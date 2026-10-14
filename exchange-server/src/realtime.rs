//! Live markets never wait for a trader's decision. Only clock/order commits
//! enter the writer lane; observations and returned bot state are immutable inputs.
use super::*;
use crate::scheduler_delta::{AgentDelta, SchedulerDelta};
use exchange_core::{BotError, BotExecutionPolicy, PersistedAgent, SchedulerPhase, SchedulerState};
mod wake_index;
use wake_index::WakeIndex;
mod commit_trace;
pub(super) use commit_trace::CommitTrace;

#[cfg(feature = "risk-benchmark")]
#[path = "risk_benchmark_realtime.rs"]
pub(super) mod risk_benchmark;

// Bound writer-lane occupancy without dropping decisions or splitting a bot's
// atomic action set. Excess results stay queued; external refill is bounded.
const MAX_READY_DECISIONS: usize = 64;
const NATIVE_DECISIONS_PER_TASK: usize = 32;

#[derive(Clone, Default)]
pub(super) struct Control {
    pub stop: Arc<AtomicBool>,
    pub epoch: Arc<AtomicU64>,
}

#[derive(Clone)]
pub(super) struct SharedAgent {
    version: u16,
    config_version: u16,
    pub template: Arc<exchange_core::AgentTemplate>,
    kind_state: Arc<exchange_core::PersistedAgentKindState>,
    unfinished_actions: Arc<[OrderAction]>,
}

impl From<PersistedAgent> for SharedAgent {
    fn from(agent: PersistedAgent) -> Self {
        Self {
            version: agent.version,
            config_version: agent.config_version,
            template: Arc::new(agent.template),
            kind_state: Arc::new(agent.kind_state),
            unfinished_actions: agent.unfinished_actions.into(),
        }
    }
}

impl SharedAgent {
    fn matches(&self, agent: &PersistedAgent) -> bool {
        self.version == agent.version
            && self.config_version == agent.config_version
            && *self.template == agent.template
            && *self.kind_state == agent.kind_state
            && *self.unfinished_actions == agent.unfinished_actions
    }
    fn config(&self) -> &exchange_core::ParticipantConfig {
        self.template.config()
    }
    fn requires_instrument(&self) -> Result<&str, exchange_core::SchedulerError> {
        self.config().instrument_id.as_deref().ok_or_else(|| {
            exchange_core::SchedulerError::MissingInstrument {
                participant_id: self.template.participant_id().to_owned(),
            }
        })
    }
    fn after(&self, next: Arc<exchange_core::PersistedAgentKindState>) -> Self {
        Self {
            kind_state: next,
            unfinished_actions: Arc::default(),
            ..self.clone()
        }
    }
}

#[derive(Clone)]
pub(super) struct Decision {
    pub prior: SharedAgent,
    pub next: Arc<exchange_core::PersistedAgentKindState>,
    pub actions: Arc<[OrderAction]>,
    pub state_hint: Option<crate::scheduler_delta::PineStateHint>,
    pub trace_id: Option<u64>,
}

struct ObservationRequest {
    history: Option<exchange_core::bots::BotMarketDataRequest>,
    related: Vec<String>,
    intervals: Vec<u64>,
    account_events: bool,
    event_filter: Option<exchange_core::bots::BotEventFilter>,
}

struct CachedBot {
    epoch: u64,
    expected: SharedAgent,
    registry: exchange_core::BotRegistry,
    request: Arc<ObservationRequest>,
    bot: Box<dyn exchange_core::ScheduledBot>,
    event_stamp: Option<[u8; 32]>,
    #[cfg(feature = "bench-execution-timing")]
    screen_submitted_at: Option<Instant>,
}

impl CachedBot {
    fn matches(
        &self,
        epoch: u64,
        agent: &PersistedAgent,
        registry: &exchange_core::BotRegistry,
    ) -> bool {
        self.epoch == epoch
            && self.expected.matches(agent)
            && registry.same_factory(&agent.template, &self.registry)
    }
}

type ComputedDecision = Result<(Decision, Option<CachedBot>), String>;
// A screened candidate has not executed a decision or advanced durable state.
// Its index routes it to the ordinary bounded worker queue, where admission and
// event checks still use the latest committed predecessor.
type DecisionResult = (String, u64, ComputedDecision, bool, Option<usize>);

// Exact state comparisons remain mandatory; this narrows candidates by id
// instead of comparing every full template/state for every completed bot.
struct DecisionIndex(BTreeMap<String, Vec<usize>>);
impl DecisionIndex {
    fn new(agents: &[PersistedAgent]) -> Self {
        let mut index = BTreeMap::<String, Vec<usize>>::new();
        for (position, agent) in agents.iter().enumerate() {
            index
                .entry(agent.template.participant_id().to_owned())
                .or_default()
                .push(position);
        }
        Self(index)
    }
    fn find_pending(
        &self,
        scheduler: &SchedulerState,
        prior: &SharedAgent,
        changes: &BTreeMap<usize, AgentDelta>,
    ) -> Option<usize> {
        self.0
            .get(prior.template.participant_id())?
            .iter()
            .copied()
            .find(|&position| matches_pending(scheduler, changes, position, prior))
    }

    #[cfg(test)]
    fn find(&self, scheduler: &SchedulerState, prior: &PersistedAgent) -> Option<usize> {
        self.find_pending(scheduler, &prior.clone().into(), &BTreeMap::new())
    }
}

fn matches_pending(
    scheduler: &SchedulerState,
    changes: &BTreeMap<usize, AgentDelta>,
    index: usize,
    prior: &SharedAgent,
) -> bool {
    let agent = &scheduler.agents[index];
    match changes.get(&index) {
        None => prior.matches(agent),
        Some(change) => {
            agent.version == prior.version
                && agent.config_version == prior.config_version
                && agent.template == *prior.template
                && change.kind_state == *prior.kind_state
                && change.unfinished_actions == *prior.unfinished_actions
        }
    }
}

pub(super) enum Work {
    Clock(Control),
    Bot {
        control: Control,
        epoch: u64,
        decision: Box<Decision>,
    },
    Bots {
        control: Control,
        epoch: u64,
        decisions: Vec<Decision>,
    },
}

impl Work {
    pub(super) fn state_hints(
        &self,
        scheduler: &SchedulerState,
    ) -> BTreeMap<usize, crate::scheduler_delta::PineStateHint> {
        let decisions: &[Decision] = match self {
            Self::Clock(_) => return BTreeMap::new(),
            Self::Bot { decision, .. } => std::slice::from_ref(decision),
            Self::Bots { decisions, .. } => decisions,
        };
        if !decisions.iter().any(|d| d.state_hint.is_some()) {
            return BTreeMap::new();
        }
        let index = DecisionIndex::new(&scheduler.agents);
        decisions
            .iter()
            .filter_map(|decision| {
                let hint = decision.state_hint.as_ref()?;
                index
                    .find_pending(scheduler, &decision.prior, &BTreeMap::new())
                    .map(|index| (index, hint.clone()))
            })
            .collect()
    }

    pub fn is_current(&self, scheduler: &SchedulerState) -> bool {
        let control = match self {
            Self::Clock(control) | Self::Bot { control, .. } | Self::Bots { control, .. } => {
                control
            }
        };
        if control.stop.load(Ordering::Acquire) {
            return false;
        }
        match self {
            Self::Clock(_) => true,
            Self::Bot {
                epoch, decision, ..
            } => {
                scheduler.bots_enabled
                    && control.epoch.load(Ordering::Acquire) == *epoch
                    && scheduler
                        .agents
                        .iter()
                        .any(|agent| decision.prior.matches(agent))
            }
            Self::Bots {
                control,
                epoch,
                decisions,
            } => {
                scheduler.bots_enabled
                    && control.epoch.load(Ordering::Acquire) == *epoch
                    && decisions.iter().any(|decision| {
                        scheduler
                            .agents
                            .iter()
                            .any(|agent| decision.prior.matches(agent))
                    })
            }
        }
    }

    pub fn order_id_budget(&self) -> u64 {
        let count = |decision: &Decision| {
            decision
                .actions
                .iter()
                .filter(|action| order_action_allocates_id(action))
                .count() as u64
        };
        match self {
            Self::Clock(_) => 0,
            Self::Bot { decision, .. } => count(decision),
            Self::Bots { decisions, .. } => decisions.iter().map(count).sum(),
        }
    }

    pub fn is_state_only(&self) -> bool {
        match self {
            Self::Clock(_) => false,
            Self::Bot { decision, .. } => decision.actions.is_empty(),
            Self::Bots { decisions, .. } => decisions.iter().all(|d| d.actions.is_empty()),
        }
    }

    pub fn apply_state_only_traced(
        self,
        scheduler: &SchedulerState,
        step: u64,
        trace: &mut CommitTrace,
    ) -> Result<SchedulerDelta, exchange_core::SchedulerError> {
        assert!(self.is_state_only());
        let mut discarded = CommitTrace::default();
        let changes = self.apply_agents(
            scheduler,
            |decision, index| {
                // Preserve validation even for a bot that returned no orders.
                decision.prior.requires_instrument()?;
                trace.state(decision.trace_id);
                Ok(AgentDelta {
                    index,
                    kind_state: Arc::unwrap_or_clone(decision.next),
                    unfinished_actions: Vec::new(),
                })
            },
            &mut discarded,
        )?;
        trace.extend(discarded);
        Ok(finish_delta(scheduler, changes, step))
    }

    #[cfg(test)]
    pub fn apply(
        self,
        rooms: &mut RoomManager,
        next_order_id: &mut u64,
        scheduler: &SchedulerState,
        policy: &mut ServerBotPolicy,
        submissions: &mut BTreeMap<u64, (String, AccountId)>,
    ) -> Result<SchedulerDelta, exchange_core::SchedulerError> {
        self.apply_traced(
            rooms,
            next_order_id,
            scheduler,
            policy,
            submissions,
            &mut CommitTrace::default(),
        )
    }

    pub fn apply_traced(
        self,
        rooms: &mut RoomManager,
        next_order_id: &mut u64,
        scheduler: &SchedulerState,
        policy: &mut ServerBotPolicy,
        submissions: &mut BTreeMap<u64, (String, AccountId)>,
        trace: &mut CommitTrace,
    ) -> Result<SchedulerDelta, exchange_core::SchedulerError> {
        use exchange_core::SchedulerError;
        let mut discarded = CommitTrace::default();
        let changes = if matches!(self, Self::Clock(_)) {
            let previous = rooms
                .execution_history_len(&scheduler.room_id)
                .map_err(SchedulerError::Room)?;
            rooms
                .advance_clock(&scheduler.room_id, 1)
                .map_err(SchedulerError::Room)?;
            if let Some(run) = &mut policy.training {
                for execution in rooms
                    .execution_history_from(&scheduler.room_id, previous)
                    .map_err(SchedulerError::Room)?
                {
                    apply_training_execution(run, execution, None, execution_account_id(execution));
                }
            }
            BTreeMap::new()
        } else {
            self.apply_agents(
                scheduler,
                |decision, index| {
                    apply_decision(
                        rooms,
                        next_order_id,
                        &scheduler.room_id,
                        policy,
                        submissions,
                        decision,
                        index,
                        trace,
                    )
                },
                &mut discarded,
            )?
        };
        trace.extend(discarded);
        let step = rooms
            .clock(&scheduler.room_id)
            .map_err(SchedulerError::Room)?
            .step();
        Ok(finish_delta(scheduler, changes, step))
    }

    fn apply_agents(
        self,
        scheduler: &SchedulerState,
        mut apply: impl FnMut(Decision, usize) -> Result<AgentDelta, exchange_core::SchedulerError>,
        discarded: &mut CommitTrace,
    ) -> Result<BTreeMap<usize, AgentDelta>, exchange_core::SchedulerError> {
        let mut changes = BTreeMap::new();
        match self {
            Self::Bots {
                control,
                epoch,
                decisions,
            } => {
                let index = (decisions.len() >= 8).then(|| DecisionIndex::new(&scheduler.agents));
                for decision in decisions {
                    if !scheduler.bots_enabled
                        || control.stop.load(Ordering::Acquire)
                        || control.epoch.load(Ordering::Acquire) != epoch
                    {
                        discarded.discard(decision.trace_id, "stale_epoch_or_disabled");
                        continue;
                    }
                    let position = match &index {
                        Some(index) => index.find_pending(scheduler, &decision.prior, &changes),
                        None => scheduler
                            .agents
                            .iter()
                            .enumerate()
                            .find(|(i, _)| {
                                matches_pending(scheduler, &changes, *i, &decision.prior)
                            })
                            .map(|(i, _)| i),
                    };
                    if let Some(position) = position {
                        changes.insert(position, apply(decision, position)?);
                    } else {
                        discarded.discard(decision.trace_id, "stale_predecessor");
                    }
                }
            }
            Self::Bot { decision, .. } => {
                let index = scheduler
                    .agents
                    .iter()
                    .position(|a| decision.prior.matches(a))
                    .expect("decision validated under the writer lock");
                changes.insert(index, apply(*decision, index)?);
            }
            Self::Clock(_) => unreachable!("clock work needs the market transaction"),
        }
        Ok(changes)
    }
}

fn finish_delta(
    scheduler: &SchedulerState,
    changes: BTreeMap<usize, AgentDelta>,
    step: u64,
) -> SchedulerDelta {
    let mut state = SchedulerDelta::metadata(scheduler);
    state.phase = SchedulerPhase::StepComplete { step };
    state.lagged = false;
    SchedulerDelta {
        version: 1,
        base_revision: scheduler.revision,
        agent_count: scheduler.agents.len(),
        state,
        changes: changes
            .into_values()
            .filter(|change| {
                let prior = &scheduler.agents[change.index];
                prior.kind_state != change.kind_state
                    || prior.unfinished_actions != change.unfinished_actions
            })
            .collect(),
        pine_changes: Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)] // Transaction collaborators and its buffered receipt are explicit.
fn apply_decision(
    rooms: &mut RoomManager,
    next_order_id: &mut u64,
    room_id: &str,
    policy: &mut ServerBotPolicy,
    submissions: &mut BTreeMap<u64, (String, AccountId)>,
    decision: Decision,
    index: usize,
    trace: &mut CommitTrace,
) -> Result<AgentDelta, exchange_core::SchedulerError> {
    use exchange_core::SchedulerError;
    if decision.actions.len() > exchange_core::MAX_BOT_ACTIONS {
        return Err(SchedulerError::Bot(BotError(
            "bot exceeded action limit".into(),
        )));
    }
    let config = decision.prior.config();
    let instrument = decision.prior.requires_instrument()?.to_string();
    let mut next = Arc::unwrap_or_clone(decision.next);
    for (action_index, action) in decision.actions.iter().enumerate() {
        let (target, resolved) = exchange_core::bots::resolve_bot_order(&instrument, action)
            .map_err(SchedulerError::Bot)?;
        if order_action_allocates_id(action) && *next_order_id >= SYSTEM_LIQUIDATION_ORDER_ID_BASE {
            return Err(SchedulerError::Bot(BotError(
                "API order-id range is exhausted".into(),
            )));
        }
        let request = GatewayRequest {
            participant_id: config.participant_id.clone(),
            room_id: room_id.to_string(),
            instrument_id: Some(target.to_string()),
            account_id: config.account_id,
            action: resolved.clone(),
        };
        // The gateway still performs all live market/account checks.
        // Only training policy needs a second full public snapshot.
        let observation = if policy.training.is_some() {
            let observation = rooms
                .participant_observation(room_id, target, config.account_id)
                .map_err(SchedulerError::Room)?;
            policy
                .before_action(&request, &observation)
                .map_err(SchedulerError::Bot)?;
            Some(observation)
        } else {
            validate_bot_action_precision(&request).map_err(SchedulerError::Bot)?;
            None
        };
        // Live bots obey the same running-market gateway as human traders.
        let mut gateway = OrderGateway::new(rooms, *next_order_id);
        let execution = gateway
            .submit_action(request)
            .map_err(SchedulerError::Gateway)?;
        *next_order_id = gateway.next_order_id();
        exchange_core::bots::record_bot_execution(&mut next, action, &execution);
        trace.action(decision.trace_id, action_index, &execution.execution);
        submissions.insert(
            execution.execution.command_seq,
            (execution.participant_id.clone(), execution.account_id),
        );
        if let Some(observation) = observation {
            policy.after_action(&execution, &observation.book);
        }
    }
    if decision.actions.is_empty() {
        trace.state(decision.trace_id);
    }
    Ok(AgentDelta {
        index,
        kind_state: next,
        unfinished_actions: Vec::new(),
    })
}

// Alternate priority only when both sources are ready. A due clock cannot
// wait behind an entire completed population, and a busy clock cannot starve
// already completed decisions. Pending decisions still never delay a tick.
enum Wake<T> {
    Poll,
    Clock,
    Ready,
    Decision(Option<Result<T, tokio::task::JoinError>>),
    Committed(CommitCompletion),
}

async fn next_wake<T: Send + 'static>(
    ticks: &mut tokio::time::Interval,
    stop_poll: &mut tokio::time::Interval,
    decisions: &mut tokio::task::JoinSet<T>,
    prefer_clock: bool,
    has_ready: bool,
) -> Wake<T> {
    if prefer_clock {
        tokio::select! {
            biased;
            _ = stop_poll.tick() => Wake::Poll,
            _ = ticks.tick() => Wake::Clock,
            _ = async {}, if has_ready => Wake::Ready,
            completed = decisions.join_next(), if !decisions.is_empty() => Wake::Decision(completed),
        }
    } else {
        tokio::select! {
            biased;
            _ = stop_poll.tick() => Wake::Poll,
            _ = async {}, if has_ready => Wake::Ready,
            completed = decisions.join_next(), if !decisions.is_empty() => Wake::Decision(completed),
            _ = ticks.tick() => Wake::Clock,
        }
    }
}

#[path = "realtime_shutdown.rs"]
mod shutdown;

pub(super) async fn run(
    shared: SharedState,
    room_id: RoomId,
    interval: Duration,
    control: Control,
    lifecycle: Arc<Mutex<AgentWorkerLifecycle>>,
    last_error: Arc<Mutex<Option<String>>>,
    bot_errors: Arc<Mutex<BTreeMap<String, String>>>,
) -> Result<(), String> {
    let mut ticks = tokio::time::interval(interval);
    // No replay storm after a busy writer lane; bot computation never changes cadence.
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut stop_poll = tokio::time::interval(Duration::from_millis(25));
    let mut decisions = tokio::task::JoinSet::<Vec<DecisionResult>>::new();
    let mut completed_results = VecDeque::new();
    let mut ready_decisions = VecDeque::new();
    let mut writer = tokio::task::JoinSet::<Result<CommitCompletion, String>>::new();
    let mut clock_due = false;
    let mut in_flight = BTreeSet::new();
    let mut failed = BTreeSet::new();
    let mut instances = BTreeMap::<String, CachedBot>::new();
    let mut cache_epoch = control.epoch.load(Ordering::Acquire);
    let mut prefer_clock = false;
    let mut dispatch_offset = 0usize;
    let mut pending_external = ExternalQueue::new(cache_epoch);
    let mut external_observations = ObservationWaveCache::default();
    let mut wake_index = WakeIndex::default();
    static EXTERNAL_PERMITS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    let external_permits = EXTERNAL_PERMITS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(16)))
        .clone();
    loop {
        if control.stop.load(Ordering::Acquire) || !shared.lifecycle.is_accepting_durable_writes() {
            break;
        }
        let wake = if clock_due && writer.is_empty() && (prefer_clock || ready_decisions.is_empty())
        {
            clock_due = false;
            Wake::Clock
        } else {
            tokio::select! {
                biased;
                completed = writer.join_next(), if !writer.is_empty() => {
                    Wake::Committed(completed.unwrap().map_err(|e| e.to_string())??)
                }
                wake = next_wake(&mut ticks, &mut stop_poll, &mut decisions, prefer_clock,
                    !completed_results.is_empty()) => wake,
            }
        };
        let wake = match wake {
            Wake::Decision(completed) => {
                completed_results.extend(completed.unwrap().map_err(|e| e.to_string())?);
                Wake::Ready
            }
            other => other,
        };
        match wake {
            Wake::Poll => {}
            Wake::Decision(_) => unreachable!(),
            Wake::Ready => {
                prefer_clock = true;
                while completed_results.len() < MAX_READY_DECISIONS {
                    let Some(completed) = decisions.try_join_next() else {
                        break;
                    };
                    completed_results.extend(completed.map_err(|e| e.to_string())?);
                }
                let ready: Vec<_> = completed_results
                    .drain(..completed_results.len().min(MAX_READY_DECISIONS))
                    .collect();
                let mut wave = Vec::new();
                let current_epoch = control.epoch.load(Ordering::Acquire);
                for completed in ready {
                    let (id, epoch, result, skipped, screened) = completed;
                    if skipped || screened.is_some() || result.is_err() || epoch != current_epoch {
                        in_flight.remove(&id);
                    }
                    if control.stop.load(Ordering::Acquire) || epoch != current_epoch {
                        if let Ok((decision, _)) = &result {
                            commit_trace::discard(
                                decision.trace_id,
                                "epoch_changed_before_admission",
                            );
                        } else if result.is_err() {
                            // A pre-ID error can arrive across an epoch/stop
                            // change. The trace auditor cannot see it otherwise.
                            crate::bot_runtime_diagnostics::shutdown_failed();
                        }
                        continue;
                    }
                    match result {
                        Ok((decision, cached)) => {
                            if let Some(index) = screened {
                                #[cfg(feature = "bench-execution-timing")]
                                let submitted_at = cached
                                    .as_ref()
                                    .and_then(|cached| cached.screen_submitted_at);
                                if let Some(cached) = cached {
                                    instances.insert(id.clone(), cached);
                                }
                                if !skipped {
                                    #[cfg(feature = "bench-execution-timing")]
                                    if let Some(started) = submitted_at {
                                        crate::bench_execution_timing::BOT_SCREEN_PREFIFO
                                            .record(started);
                                    }
                                    pending_external.push(index, id);
                                }
                            } else {
                                if let Some(cached) = cached {
                                    instances.insert(id.clone(), cached);
                                }
                                if !skipped {
                                    wave.push(decision);
                                }
                            }
                        }
                        Err(error) => {
                            failed.insert(id.clone());
                            shared.lifecycle.record_agent_error();
                            *last_error.lock().unwrap() = Some(format!("bot {id}: {error}"));
                            bot_errors.lock().unwrap().insert(id, error);
                        }
                    }
                }
                ready_decisions.extend(wave.into_iter().map(|decision| (current_epoch, decision)));
            }
            Wake::Clock => {
                if !writer.is_empty() {
                    clock_due = true;
                } else {
                    prefer_clock = false;
                    let published = shared.app.read_room(&room_id).await;
                    match published
                        .rooms
                        .status(&room_id)
                        .map_err(|e| format!("{e:?}"))?
                    {
                        MarketStatus::Closed => break,
                        MarketStatus::Paused => {
                            *lifecycle.lock().unwrap() = AgentWorkerLifecycle::Paused;
                            continue;
                        }
                        MarketStatus::Running => {}
                    }
                    *lifecycle.lock().unwrap() = AgentWorkerLifecycle::Running;
                    let (shared, room, control) =
                        (shared.clone(), room_id.clone(), control.clone());
                    writer.spawn(async move {
                        commit_realtime_work(shared, room, Work::Clock(control))
                            .await
                            .map_err(|(_, body)| body.0.error)?;
                        Ok(CommitCompletion {
                            clock: true,
                            ids: Vec::new(),
                            errors: Vec::new(),
                        })
                    });
                }
            }
            Wake::Committed(completed) => {
                for id in completed.ids {
                    in_flight.remove(&id);
                    wake_index.committed(&id);
                }
                for (id, error) in completed.errors {
                    failed.insert(id.clone());
                    shared.lifecycle.record_agent_error();
                    *last_error.lock().unwrap() = Some(format!("bot {id}: {error}"));
                    bot_errors.lock().unwrap().insert(id, error);
                }
                if completed.clock {
                    let epoch_now = control.epoch.load(Ordering::Acquire);
                    if cache_epoch != epoch_now {
                        instances.clear(); // Destructors also stay outside the writer lane.
                        cache_epoch = epoch_now;
                    }
                    let (rooms, registry, agents, screening, epoch) = {
                        let app = shared
                            .app
                            .read_dispatch(&room_id)
                            .await
                            .ok_or_else(|| format!("room {room_id} has no published state"))?;
                        if control.stop.load(Ordering::Acquire)
                            || app.rooms.status(&room_id).map_err(|e| format!("{e:?}"))?
                                != MarketStatus::Running
                        {
                            continue;
                        }
                        let scheduler = app
                            .dispatch
                            .as_ref()
                            .ok_or_else(|| format!("room {room_id} has no scheduler state"))?;
                        let registry = &scheduler.registry;
                        let epoch = control.epoch.load(Ordering::Acquire);
                        wake_index.refresh(&app.rooms, &room_id, scheduler, epoch)?;
                        let mut candidates: Vec<_> = wake_index
                            .candidates()
                            .into_iter()
                            .filter_map(|index| {
                                scheduler.agents.get(index).map(|agent| (index, agent))
                            })
                            .filter(|(_, agent)| {
                                scheduler.bots_enabled
                                    && !in_flight.contains(agent.template.participant_id())
                                    && !failed.contains(agent.template.participant_id())
                            })
                            .collect();
                        if !candidates.is_empty() {
                            let count = candidates.len();
                            candidates.rotate_left(dispatch_offset % count);
                            dispatch_offset = dispatch_offset.wrapping_add(16);
                        }
                        pending_external.reset_epoch(epoch);
                        let mut screening = Vec::new();
                        let agents: Vec<_> = candidates
                            .into_iter()
                            .filter_map(|(index, agent)| {
                                if !registry.supports_native_batching(&agent.template) {
                                    let id = agent.template.participant_id();
                                    if pending_external.queued.contains(id) {
                                        return None;
                                    }
                                    wake_index.consumed(index, &agent.kind_state);
                                    let screen = instances.get(id).is_some_and(|cached| {
                                        cached.matches(epoch, agent, registry)
                                            && cached.request.event_filter.is_some()
                                            && cached.event_stamp.is_some()
                                    });
                                    if screen {
                                        let cached = instances.remove(id).unwrap();
                                        screening.push((index, cached.expected.clone(), cached));
                                        return None;
                                    }
                                    pending_external.push(index, id.to_owned());
                                    return None;
                                }
                                let cached = instances.remove(agent.template.participant_id());
                                let valid = cached
                                    .as_ref()
                                    .is_some_and(|c| c.matches(epoch, agent, registry));
                                let prior = if valid {
                                    cached.as_ref().unwrap().expected.clone()
                                } else {
                                    agent.as_ref().clone().into()
                                };
                                // Return stale instances too: do not drop plugin objects under the lock.
                                Some((prior, cached, valid))
                            })
                            .collect();
                        (app.rooms, registry.clone(), agents, screening, epoch)
                    };
                    let frozen = if screening.is_empty() {
                        // Native-only waves retain their original snapshot lifetime.
                        Arc::new(
                            rooms
                                .as_ref()
                                .clone()
                                .into_native_observation_wave(room_id.clone()),
                        )
                    } else {
                        external_observations.get(rooms, &room_id)
                    };
                    let mut screening_batch = Vec::new();
                    for item in screening {
                        in_flight.insert(item.1.template.participant_id().to_owned());
                        screening_batch.push(item);
                        if screening_batch.len() == NATIVE_DECISIONS_PER_TASK {
                            spawn_event_screening(
                                &mut decisions,
                                frozen.clone(),
                                epoch,
                                std::mem::take(&mut screening_batch),
                            );
                        }
                    }
                    if !screening_batch.is_empty() {
                        spawn_event_screening(
                            &mut decisions,
                            frozen.clone(),
                            epoch,
                            screening_batch,
                        );
                    }
                    let mut native = Vec::new();
                    for (prior, cached, valid) in agents {
                        let id = prior.template.participant_id().to_string();
                        let input = (prior, cached.filter(|_| valid));
                        in_flight.insert(id);
                        native.push(input);
                        if native.len() == NATIVE_DECISIONS_PER_TASK {
                            spawn_decision_batch(
                                &mut decisions,
                                frozen.clone(),
                                registry.clone(),
                                std::mem::take(&mut native),
                                epoch,
                                true,
                                None,
                            );
                        }
                    }
                    if !native.is_empty() {
                        spawn_decision_batch(
                            &mut decisions,
                            frozen,
                            registry,
                            native,
                            epoch,
                            true,
                            None,
                        );
                    }
                }
            }
        }
        // Bound completed-but-uncommitted work while keeping independent
        // calculations active during the single ordered writer transaction.
        if writer.is_empty() && !ready_decisions.is_empty() && !(clock_due && prefer_clock) {
            let current_epoch = control.epoch.load(Ordering::Acquire);
            let wave: Vec<_> = ready_decisions
                .drain(..ready_decisions.len().min(MAX_READY_DECISIONS))
                .filter_map(|(epoch, decision)| {
                    if epoch == current_epoch {
                        Some(decision)
                    } else {
                        in_flight.remove(decision.prior.template.participant_id());
                        commit_trace::discard(decision.trace_id, "epoch_changed_in_ready_queue");
                        None
                    }
                })
                .collect();
            if !wave.is_empty() {
                writer.spawn(commit_wave(
                    shared.clone(),
                    room_id.clone(),
                    control.clone(),
                    current_epoch,
                    wave,
                ));
            }
            prefer_clock = true;
        }
        if ready_decisions.len() < MAX_READY_DECISIONS * 4 {
            dispatch_external_pending(
                &shared,
                &room_id,
                &control,
                &external_permits,
                &mut pending_external,
                &mut decisions,
                &mut in_flight,
                &failed,
                &mut instances,
                &mut external_observations,
            )
            .await?;
        }
    }
    // Every successful loop exit (stop, global shutdown, or closed market)
    // reaches the same diagnostic drain. An early Err is marked by the worker.
    if crate::bot_runtime_diagnostics::shutdown_requested() {
        let drained = shutdown::drain(
            &mut writer,
            &mut decisions,
            &mut completed_results,
            &mut ready_decisions,
            commit_trace::discard,
        )
        .await;
        drained?;
        if !failed.is_empty() {
            return Err("trace shutdown follows contained bot/commit errors".into());
        }
    }
    Ok(())
}

struct CommitCompletion {
    clock: bool,
    ids: Vec<String>,
    errors: Vec<(String, String)>,
}

async fn commit_wave(
    shared: SharedState,
    room_id: RoomId,
    control: Control,
    current_epoch: u64,
    wave: Vec<Decision>,
) -> Result<CommitCompletion, String> {
    let ids = wave
        .iter()
        .map(|d| d.prior.template.participant_id().to_owned())
        .collect();
    let mut errors = Vec::new();
    let result = commit_realtime_work(
        shared.clone(),
        room_id.clone(),
        Work::Bots {
            control: control.clone(),
            epoch: current_epoch,
            decisions: wave.clone(),
        },
    )
    .await;
    #[cfg(feature = "bench-execution-timing")]
    if result.is_ok() {
        for decision in &wave {
            if let exchange_core::PersistedAgentKindState::Plugin {
                plugin_id, data, ..
            } = decision.next.as_ref()
                && plugin_id == "pine.strategy"
            {
                let time = data
                    .get("_host_observation")
                    .and_then(|v| v.get(1))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                crate::bench_execution_timing::trace(
                    "commit_return",
                    decision.prior.template.participant_id(),
                    time,
                    0,
                );
            }
        }
    }
    if let Err((status, body)) = result {
        if status != StatusCode::BAD_REQUEST {
            for decision in &wave {
                commit_trace::failed(decision.trace_id, "unrecoverable_batch_failure");
            }
            return Err(body.0.error);
        }
        // No part of the failed wave was committed. Retrying its members
        // preserves each bot's atomicity and existing error isolation.
        let mut pending = wave.into_iter();
        while let Some(decision) = pending.next() {
            let id = decision.prior.template.participant_id().to_string();
            let trace_id = decision.trace_id;
            let result = commit_realtime_work(
                shared.clone(),
                room_id.clone(),
                Work::Bot {
                    control: control.clone(),
                    epoch: current_epoch,
                    decision: Box::new(decision),
                },
            )
            .await;
            if let Err((status, body)) = result {
                commit_trace::failed(trace_id, "individual_transaction_failed");
                if status != StatusCode::BAD_REQUEST {
                    for decision in pending {
                        commit_trace::failed(decision.trace_id, "batch_aborted");
                    }
                    return Err(body.0.error);
                }
                let error = body.0.error;
                errors.push((id.clone(), error.clone()));
            }
        }
    }

    Ok(CommitCompletion {
        clock: false,
        ids,
        errors,
    })
}

fn spawn_event_screening(
    decisions: &mut tokio::task::JoinSet<Vec<DecisionResult>>,
    frozen: Arc<exchange_core::room::NativeObservationWave>,
    epoch: u64,
    inputs: Vec<(usize, SharedAgent, CachedBot)>,
) {
    // One small blocking batch checks immutable event inputs before any worker
    // permit is acquired. No plugin hook/decision runs here, and unchanged
    // candidates neither enter the worker FIFO nor produce journal revisions.
    #[cfg(feature = "bench-execution-timing")]
    let screen_submitted_at = Instant::now();
    decisions.spawn_blocking(move || {
        let mut observations = frozen.batch();
        inputs
            .into_iter()
            .map(|(index, prior, cached)| {
                #[cfg(feature = "bench-execution-timing")]
                let cached = CachedBot {
                    screen_submitted_at: Some(screen_submitted_at),
                    ..cached
                };
                let id = prior.template.participant_id().to_owned();
                let mut skipped = false;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    #[cfg(feature = "bench-execution-timing")]
                    let _timer = crate::bench_execution_timing::Timer::start(
                        &crate::bench_execution_timing::BOT_EVENT_PREFILTER,
                    );
                    let request = &cached.request;
                    let (stamp, time) = observations
                        .bot_event_stamp(
                            prior.requires_instrument().map_err(|e| format!("{e:?}"))?,
                            prior.config().account_id,
                            request.history,
                            &request.related,
                            &request.intervals,
                            request.account_events,
                            request.event_filter.expect("screened event filter"),
                        )
                        .map_err(|e| format!("{e:?}"))?;
                    skipped = stamp.is_some()
                        && stamp == cached.event_stamp
                        && !event_wake_is_due(&prior.kind_state, time);
                    #[cfg(feature = "bench-execution-timing")]
                    if skipped {
                        let _timer = crate::bench_execution_timing::Timer::start(
                            &crate::bench_execution_timing::BOT_EVENT_SKIP,
                        );
                    }
                    Ok((
                        Decision {
                            next: prior.kind_state.clone(),
                            actions: Arc::default(),
                            prior,
                            state_hint: None,
                            trace_id: None,
                        },
                        Some(cached),
                    ))
                }))
                .unwrap_or_else(|_| Err("bot event screening panicked".to_owned()));
                (id, epoch, result, skipped, Some(index))
            })
            .collect()
    });
}

// Pending work outlives market ticks. A new tick adds eligible instances but
// cannot move an older candidate behind instances that already completed.
struct ExternalQueue {
    epoch: u64,
    items: VecDeque<(usize, String, Instant)>,
    queued: BTreeSet<String>,
}

#[derive(Default)]
struct ObservationWaveCache {
    current: Option<(
        Arc<exchange_core::RoomManager>,
        Arc<exchange_core::room::NativeObservationWave>,
    )>,
}
impl ObservationWaveCache {
    fn get(
        &mut self,
        snapshot: Arc<exchange_core::RoomManager>,
        room_id: &str,
    ) -> Arc<exchange_core::room::NativeObservationWave> {
        if let Some((previous, wave)) = &self.current
            && Arc::ptr_eq(previous, &snapshot)
        {
            #[cfg(feature = "bench-execution-timing")]
            let _timer = crate::bench_execution_timing::Timer::start(
                &crate::bench_execution_timing::BOT_WAVE_REUSE,
            );
            return wave.clone();
        }
        #[cfg(feature = "bench-execution-timing")]
        let _timer = crate::bench_execution_timing::Timer::start(
            &crate::bench_execution_timing::BOT_WAVE_BUILD,
        );
        let rooms = snapshot.as_ref().clone();
        let wave = Arc::new(match &self.current {
            Some((_, previous)) => {
                rooms.into_native_observation_wave_after(room_id.to_owned(), previous)
            }
            None => rooms.into_native_observation_wave(room_id.to_owned()),
        });
        self.current = Some((snapshot, wave.clone()));
        wave
    }
}
impl ExternalQueue {
    fn new(epoch: u64) -> Self {
        Self {
            epoch,
            items: VecDeque::new(),
            queued: BTreeSet::new(),
        }
    }
    fn clear(&mut self) {
        self.items.clear();
        self.queued.clear();
    }
    fn reset_epoch(&mut self, epoch: u64) {
        if self.epoch != epoch {
            self.clear();
            self.epoch = epoch;
        }
    }
    fn push(&mut self, index: usize, id: String) {
        if self.queued.insert(id.clone()) {
            self.items.push_back((index, id, Instant::now()));
        }
    }
    fn pop(&mut self) -> Option<(usize, String, Instant)> {
        let item = self.items.pop_front()?;
        self.queued.remove(&item.1);
        Some(item)
    }
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_external_pending(
    shared: &SharedState,
    room_id: &str,
    control: &Control,
    permits: &Arc<tokio::sync::Semaphore>,
    pending: &mut ExternalQueue,
    decisions: &mut tokio::task::JoinSet<Vec<DecisionResult>>,
    in_flight: &mut BTreeSet<String>,
    failed: &BTreeSet<String>,
    instances: &mut BTreeMap<String, CachedBot>,
    observations: &mut ObservationWaveCache,
) -> Result<(), String> {
    if pending.items.is_empty() {
        return Ok(());
    }
    let (rooms, registry, inputs, epoch) = {
        let Some(app) = shared.app.read_dispatch(room_id).await else {
            pending.clear();
            return Ok(());
        };
        let epoch = control.epoch.load(Ordering::Acquire);
        if control.stop.load(Ordering::Acquire)
            || pending.epoch != epoch
            || app.rooms.status(room_id).map_err(|e| format!("{e:?}"))? != MarketStatus::Running
        {
            pending.clear();
            return Ok(());
        }
        let scheduler = app
            .dispatch
            .as_ref()
            .ok_or_else(|| format!("room {room_id} has no scheduler state"))?;
        let registry = &scheduler.registry;
        let mut inputs = Vec::new();
        while !pending.items.is_empty() {
            // Reserve BEFORE copying any durable transcript. Completed tasks
            // refill this budget without waiting for the next market tick.
            let Ok(permit) = permits.clone().try_acquire_owned() else {
                break;
            };
            let (index, id, _queued) = pending.pop().unwrap();
            #[cfg(feature = "bench-execution-timing")]
            {
                crate::bench_execution_timing::BOT_DISPATCH_QUEUE.record(_queued);
                crate::bench_execution_timing::trace(
                    "dispatch",
                    &id,
                    app.rooms.clock(room_id).unwrap().market_time_ms(),
                    _queued.elapsed().as_micros() as u64,
                );
            }
            let Some(agent) = scheduler
                .agents
                .get(index)
                .filter(|a| a.template.participant_id() == id)
            else {
                continue;
            };
            if !scheduler.bots_enabled || in_flight.contains(&id) || failed.contains(&id) {
                continue;
            }
            #[cfg(feature = "bench-execution-timing")]
            let copy_timer = crate::bench_execution_timing::Timer::start(
                &crate::bench_execution_timing::BOT_STATE_COPY,
            );
            let cached = instances.remove(&id);
            let valid = cached
                .as_ref()
                .is_some_and(|c| c.matches(epoch, agent, registry));
            let prior = if valid {
                cached.as_ref().unwrap().expected.clone()
            } else {
                agent.as_ref().clone().into()
            };
            #[cfg(feature = "bench-execution-timing")]
            drop(copy_timer);
            // Stale plugin destructors must run after releasing the room lane.
            inputs.push((id, prior, cached, valid, permit));
        }
        if inputs.is_empty() {
            return Ok(());
        }
        (app.rooms.clone(), registry.clone(), inputs, epoch)
    };
    let frozen = observations.get(rooms, room_id);
    for (id, prior, cached, valid, permit) in inputs {
        in_flight.insert(id);
        spawn_decision_batch(
            decisions,
            frozen.clone(),
            registry.clone(),
            vec![(prior, cached.filter(|_| valid))],
            epoch,
            false,
            Some(permit),
        );
    }
    Ok(())
}

fn spawn_decision_batch(
    decisions: &mut tokio::task::JoinSet<Vec<DecisionResult>>,
    frozen: Arc<exchange_core::room::NativeObservationWave>,
    registry: exchange_core::BotRegistry,
    inputs: Vec<(SharedAgent, Option<CachedBot>)>,
    epoch: u64,
    native: bool,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
) {
    // Observation construction, metadata hooks, strategy calls and destructors
    // all run outside both the room lane and the clock's async task.
    #[cfg(feature = "bench-execution-timing")]
    let blocking_timer = (!native).then(|| {
        crate::bench_execution_timing::Timer::start(
            &crate::bench_execution_timing::BOT_BLOCKING_WAIT,
        )
    });
    decisions.spawn_blocking(move || {
        #[cfg(feature = "bench-execution-timing")]
        drop(blocking_timer);
        let _permit = permit;
        #[cfg(feature = "pine-in-process")]
        let _source_wave = (!native && crate::pine_inprocess::source_wave::enabled())
            .then(|| crate::pine_inprocess::source_wave::enter(&frozen));
        let mut observations = frozen.batch();
        inputs
            .into_iter()
            .map(|(prior, mut cached)| {
                let id = prior.template.participant_id().to_string();
                #[cfg(feature = "bench-execution-timing")]
                let _turn_timer =
                    (!native).then(crate::bench_execution_timing::BotTurnTimer::start);
                let mut skipped = false;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    #[cfg(all(test, feature = "memory-diagnostics"))]
                    let delivery_scope = crate::memory_diagnostics::Scope::enter(
                        crate::memory_diagnostics::Origin::Delivery,
                    );
                    let request = match &cached {
                        Some(cached) => cached.request.clone(),
                        None => Arc::new(ObservationRequest {
                            history: registry
                                .market_data_request(&prior.template)
                                .map_err(|e| e.to_string())?,
                            related: registry
                                .related_instruments(&prior.template)
                                .map_err(|e| e.to_string())?,
                            intervals: registry
                                .additional_intervals(&prior.template)
                                .map_err(|e| e.to_string())?,
                            account_events: registry.account_events(&prior.template),
                            event_filter: registry.event_filter(&prior.template),
                        }),
                    };
                    let instrument = prior.requires_instrument().map_err(|e| format!("{e:?}"))?;
                    let event_stamp = if let Some(filter) = request.event_filter {
                        #[cfg(feature = "bench-execution-timing")]
                        let _filter_timer = crate::bench_execution_timing::Timer::start(
                            &crate::bench_execution_timing::BOT_EVENT_PREFILTER,
                        );
                        let (stamp, time) = observations
                            .bot_event_stamp(
                                instrument,
                                prior.config().account_id,
                                request.history,
                                &request.related,
                                &request.intervals,
                                request.account_events,
                                filter,
                            )
                            .map_err(|e| format!("{e:?}"))?;
                        if stamp.is_some()
                            && cached.as_ref().is_some_and(|c| c.event_stamp == stamp)
                            && !event_wake_is_due(&prior.kind_state, time)
                        {
                            #[cfg(feature = "bench-execution-timing")]
                            let _skip_timer = crate::bench_execution_timing::Timer::start(
                                &crate::bench_execution_timing::BOT_EVENT_SKIP,
                            );
                            skipped = true;
                            return Ok((
                                Decision {
                                    next: prior.kind_state.clone(),
                                    actions: Arc::default(),
                                    prior,
                                    state_hint: None,
                                    trace_id: None,
                                },
                                cached,
                            ));
                        }
                        stamp
                    } else {
                        None
                    };
                    let mut effective_history = request.history;
                    if !native
                        && let Some(cursors) = registry
                            .checkpoint_history_cursors(&prior.template, &prior.kind_state)
                            .map_err(|e| e.to_string())?
                    {
                        let max_bars = observations
                            .checkpoint_window_max_bars(&cursors)
                            .map_err(|e| e.to_string())?;
                        if let Some(history) = &mut effective_history {
                            history.max_bars = max_bars;
                        }
                    }
                    #[cfg(feature = "bench-execution-timing")]
                    let observation_timer = (!native).then(|| {
                        crate::bench_execution_timing::Timer::start(
                            &crate::bench_execution_timing::BOT_OBSERVATION,
                        )
                    });
                    let observation =
                        if native && request.intervals.is_empty() && !request.account_events {
                            DecisionObservation::Shared(
                                observations
                                    .shared_bot_observation(
                                        instrument,
                                        prior.config().account_id,
                                        effective_history,
                                        &request.related,
                                    )
                                    .map_err(|e| format!("{e:?}"))?,
                            )
                        } else {
                            let mut view = observations
                                .bot_observation(
                                    instrument,
                                    prior.config().account_id,
                                    effective_history,
                                    &request.related,
                                )
                                .map_err(|e| format!("{e:?}"))?;
                            observations
                                .enrich_data(
                                    &mut view,
                                    effective_history,
                                    &request.related,
                                    &request.intervals,
                                    request.account_events,
                                    prior.config().account_id,
                                )
                                .map_err(|e| format!("{e:?}"))?;
                            DecisionObservation::Owned(view)
                        };
                    #[cfg(all(test, feature = "memory-diagnostics"))]
                    drop(delivery_scope);
                    #[cfg(feature = "bench-execution-timing")]
                    drop(observation_timer);
                    let due = if let DecisionObservation::Owned(view) = &observation {
                        #[cfg(feature = "bench-execution-timing")]
                        let _trigger_timer = (!native).then(|| {
                            crate::bench_execution_timing::Timer::start(
                                &crate::bench_execution_timing::BOT_TRIGGER,
                            )
                        });
                        registry
                            .decision_is_due(&prior.template, &prior.kind_state, view)
                            .map_err(|e| e.to_string())?
                    } else {
                        true
                    };
                    if !due {
                        skipped = true;
                        if let Some(cached) = &mut cached {
                            cached.event_stamp = event_stamp;
                        }
                        return Ok((
                            Decision {
                                next: prior.kind_state.clone(),
                                actions: Arc::default(),
                                prior,
                                state_hint: None,
                                trace_id: None,
                            },
                            cached,
                        ));
                    }
                    #[cfg(feature = "bench-execution-timing")]
                    let _evaluate_timer = (!native).then(|| {
                        crate::bench_execution_timing::Timer::start(
                            &crate::bench_execution_timing::BOT_EVALUATE,
                        )
                    });
                    let mut result = compute_decision_input(
                        prior,
                        observation,
                        registry.clone(),
                        cached,
                        request,
                        epoch,
                    );
                    if let Ok((_, Some(cached))) = &mut result {
                        cached.event_stamp = event_stamp;
                    }
                    result
                }))
                .unwrap_or_else(|_| Err("bot panicked".to_string()));
                (id, epoch, result, skipped, None)
            })
            .collect()
    });
}

fn event_wake_is_due(state: &exchange_core::PersistedAgentKindState, time: u64) -> bool {
    match state {
        exchange_core::PersistedAgentKindState::Plugin { data, .. } => data
            .get("_next_wake_ms")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|deadline| deadline <= time),
        _ => false,
    }
}

// This value is consumed on the blocking worker's stack, never queued. Boxing
// would add an allocation to every decision solely to shrink a short-lived enum.
#[allow(clippy::large_enum_variant)]
enum DecisionObservation {
    Owned(exchange_core::ParticipantObservation),
    Shared(exchange_core::observation::SharedParticipantObservation),
}

#[cfg(any(test, feature = "risk-benchmark"))]
fn compute_decision(
    prior: SharedAgent,
    observation: exchange_core::ParticipantObservation,
    registry: exchange_core::BotRegistry,
    cached: Option<CachedBot>,
    request: Arc<ObservationRequest>,
    epoch: u64,
) -> ComputedDecision {
    compute_decision_input(
        prior,
        DecisionObservation::Owned(observation),
        registry,
        cached,
        request,
        epoch,
    )
}

fn compute_decision_input(
    prior: SharedAgent,
    observation: DecisionObservation,
    registry: exchange_core::BotRegistry,
    cached: Option<CachedBot>,
    request: Arc<ObservationRequest>,
    epoch: u64,
) -> ComputedDecision {
    #[cfg(all(test, feature = "memory-diagnostics"))]
    let _scope = crate::memory_diagnostics::Scope::enter(crate::memory_diagnostics::Origin::Bot);
    let reusable = cached.is_some() || registry.supports_instance_reuse(&prior.template);
    let mut bot = match cached {
        Some(cached) => {
            let mut bot = cached.bot;
            bot.prepare_reused_call(&prior.kind_state)
                .map_err(|e| e.to_string())?;
            bot
        }
        None => registry
            .create(&prior.template, &prior.kind_state)
            .map_err(|e| e.to_string())?,
    };
    if registry.supports_native_batching(&prior.template) {
        crate::bot_runtime_diagnostics::record_route("native", None);
    }
    let actions = match &observation {
        DecisionObservation::Owned(view) => bot.decide(view),
        DecisionObservation::Shared(view) => bot.decide_shared(view),
    }
    .map_err(|e| e.to_string())?;
    if actions.len() > exchange_core::MAX_BOT_ACTIONS {
        return Err("bot exceeded action limit".to_string());
    }
    let next = Arc::new(bot.take_snapshot());
    exchange_core::bots::validate_bot_orders(
        prior.requires_instrument().map_err(|e| format!("{e:?}"))?,
        &request.related,
        &actions,
        &next,
    )
    .map_err(|e| e.to_string())?;
    let state_hint = crate::scheduler_delta::PineStateHint::prepare(&prior.kind_state, &next);
    let trace_id = crate::bot_runtime_diagnostics::next_decision_id();
    commit_trace::decision(trace_id, &prior, &next, &observation, actions.len());
    // Speculative state is reusable ONLY after exact equality with committed state.
    // A failed journal write, rejected action, stale epoch or config change forces restore.
    let cached = reusable.then(|| CachedBot {
        expected: prior.after(next.clone()),
        epoch,
        registry,
        request,
        bot,
        event_stamp: None,
        #[cfg(feature = "bench-execution-timing")]
        screen_submitted_at: None,
    });
    Ok((
        Decision {
            prior,
            next,
            actions: actions.into(),
            state_hint,
            trace_id,
        },
        cached,
    ))
}

#[cfg(test)]
#[path = "native_delivery_benchmark.rs"]
mod delivery_benchmark;

#[cfg(test)]
mod index_tests {
    #[test]
    fn observation_wave_reuses_only_the_exact_immutable_market_snapshot() {
        let mut cache = super::ObservationWaveCache::default();
        let snapshot = std::sync::Arc::new(exchange_core::RoomManager::new());
        let first = cache.get(snapshot.clone(), "room");
        assert!(std::sync::Arc::ptr_eq(
            &first,
            &cache.get(snapshot.clone(), "room")
        ));
        // Even identical contents in a newly published snapshot invalidate all
        // history/account caches; no heuristic revision or last-bar shortcut.
        let changed = std::sync::Arc::new(snapshot.as_ref().clone());
        let next = cache.get(changed.clone(), "room");
        assert!(!std::sync::Arc::ptr_eq(&first, &next));
        assert!(std::sync::Arc::ptr_eq(&next, &cache.get(changed, "room")));
    }

    #[test]
    fn external_queue_preserves_older_candidates_across_ticks_and_resets_on_epoch() {
        let mut queue = super::ExternalQueue::new(3);
        queue.push(0, "completed".into());
        queue.push(1, "older".into());
        assert_eq!(queue.pop().unwrap().1, "completed");
        queue.reset_epoch(3); // Next tick must not discard the still waiting bot.
        queue.push(1, "older".into());
        queue.push(0, "completed".into());
        assert_eq!(queue.items.len(), 2);
        assert_eq!(queue.pop().unwrap().1, "older");
        queue.reset_epoch(4); // Reconfiguration cannot retain stale roster indices.
        assert!(queue.pop().is_none());
        queue.push(7, "new-roster".into());
        assert_eq!(queue.pop().unwrap().0, 7);
    }
    use super::*;

    struct EventFactory {
        descriptor: exchange_core::BotDescriptor,
        calls: Arc<AtomicUsize>,
    }
    struct EventBot {
        calls: Arc<AtomicUsize>,
        state: exchange_core::PersistedAgentKindState,
    }
    impl exchange_core::BotFactory for EventFactory {
        fn descriptor(&self) -> &exchange_core::BotDescriptor {
            &self.descriptor
        }
        fn supports_instance_reuse(&self) -> bool {
            true
        }
        fn event_filter(&self) -> Option<exchange_core::bots::BotEventFilter> {
            Some(exchange_core::bots::BotEventFilter {
                closed_bars: true,
                account: true,
                interval_ms: None,
            })
        }
        fn market_data_request(
            &self,
            _: &exchange_core::AgentTemplate,
        ) -> Result<Option<exchange_core::bots::BotMarketDataRequest>, BotError> {
            Ok(Some(exchange_core::bots::BotMarketDataRequest {
                interval_ms: 10_000,
                max_bars: 512,
            }))
        }
        fn create(
            &self,
            _: &exchange_core::AgentTemplate,
            state: &exchange_core::PersistedAgentKindState,
        ) -> Result<Box<dyn exchange_core::ScheduledBot>, BotError> {
            Ok(Box::new(EventBot {
                calls: self.calls.clone(),
                state: state.clone(),
            }))
        }
    }
    impl exchange_core::ScheduledBot for EventBot {
        fn decide(
            &mut self,
            view: &exchange_core::ParticipantObservation,
        ) -> Result<Vec<OrderAction>, BotError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let exchange_core::PersistedAgentKindState::Plugin { data, .. } = &mut self.state {
                let calls = data
                    .get("calls")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0)
                    + 1;
                *data = serde_json::json!({ "calls": calls, "_next_wake_ms": view.market_time_ms + 5000 }).into();
            }
            Ok(vec![])
        }
        fn snapshot(&self) -> exchange_core::PersistedAgentKindState {
            self.state.clone()
        }
    }

    #[tokio::test]
    async fn early_event_skip_preserves_state_and_timed_wake_and_cold_restore() {
        let value: serde_json::Value =
            serde_json::from_str(include_str!("../../scripts/fixtures/f6_batch_spec.json"))
                .unwrap();
        let mut rooms = RoomManager::new();
        rooms
            .create_room(serde_json::from_value(value["scenario"].clone()).unwrap())
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut registry = exchange_core::BotRegistry::default();
        registry
            .register(EventFactory {
                descriptor: exchange_core::BotDescriptor {
                    id: "event.test".into(),
                    name: "Event test".into(),
                    version: "1".into(),
                    protocol_version: "bot.v1".into(),
                    state_version: 1,
                    runtime: "test".into(),
                    parameters: BTreeMap::new(),
                },
                calls: calls.clone(),
            })
            .unwrap();
        let template = exchange_core::AgentTemplate::Plugin(exchange_core::BotConfig {
            participant: exchange_core::ParticipantConfig {
                participant_id: "event-bot".into(),
                room_id: "f6-batch".into(),
                account_id: 20,
                instrument_id: Some("V-BTC-SPOT".into()),
                kind: exchange_core::ParticipantKind::RuleAgent,
            },
            plugin_id: "event.test".into(),
            plugin_version: "1".into(),
            state_version: 1,
            config_version: 1,
            seed: 1,
            config: serde_json::json!({}),
        });
        let mut prior = SharedAgent::from(PersistedAgent::from_template(template));
        let mut cached = None;
        for (advance, expected_skipped, expected_calls) in [
            (0, false, 1),
            (1, true, 1),
            (3, true, 1),
            (1, false, 2),
            (1, true, 2),
            (0, false, 3),
        ] {
            if expected_calls == 3 {
                cached = None;
            } // Process/recovery cache is optional.
            rooms.advance_clock("f6-batch", advance).unwrap();
            let frozen = Arc::new(
                rooms
                    .clone()
                    .into_native_observation_wave("f6-batch".into()),
            );
            if let Some(instance) = cached.take() {
                let calls_before = calls.load(Ordering::SeqCst);
                let mut screening = tokio::task::JoinSet::new();
                spawn_event_screening(
                    &mut screening,
                    frozen.clone(),
                    7,
                    vec![(9, prior.clone(), instance)],
                );
                let screened = screening.join_next().await.unwrap().unwrap().pop().unwrap();
                assert_eq!(screened.3, expected_skipped);
                assert_eq!(screened.4, Some(9));
                let (unchanged, instance) = screened.2.unwrap();
                assert!(Arc::ptr_eq(&unchanged.next, &prior.kind_state));
                assert!(unchanged.actions.is_empty());
                assert_eq!(
                    calls.load(Ordering::SeqCst),
                    calls_before,
                    "screening must never call a plugin or advance its state"
                );
                cached = instance;
            }
            let mut tasks = tokio::task::JoinSet::new();
            spawn_decision_batch(
                &mut tasks,
                frozen,
                registry.clone(),
                vec![(prior.clone(), cached)],
                7,
                false,
                None,
            );
            let result = tasks.join_next().await.unwrap().unwrap().pop().unwrap();
            assert_eq!(result.3, expected_skipped);
            let (decision, returned) = result.2.unwrap();
            if expected_skipped {
                assert!(Arc::ptr_eq(&decision.next, &prior.kind_state));
            }
            assert!(decision.actions.is_empty());
            prior = prior.after(decision.next);
            cached = returned;
            assert_eq!(calls.load(Ordering::SeqCst), expected_calls);
        }
    }

    #[test]
    fn indexed_wakes_keep_busy_accounts_dirty_and_track_deadlines_and_bar_groups() {
        let value: serde_json::Value =
            serde_json::from_str(include_str!("../../scripts/fixtures/f6_batch_spec.json"))
                .unwrap();
        let mut rooms = RoomManager::new();
        rooms
            .create_room(serde_json::from_value(value["scenario"].clone()).unwrap())
            .unwrap();
        let mut registry = exchange_core::BotRegistry::default();
        registry
            .register(EventFactory {
                descriptor: exchange_core::BotDescriptor {
                    id: "event.test".into(),
                    name: "Event".into(),
                    version: "1".into(),
                    protocol_version: "bot.v1".into(),
                    state_version: 1,
                    runtime: "test".into(),
                    parameters: BTreeMap::new(),
                },
                calls: Arc::new(AtomicUsize::new(0)),
            })
            .unwrap();
        let agents: im::Vector<_> = [20, 999]
            .into_iter()
            .map(|account| {
                PersistedAgent::from_template(exchange_core::AgentTemplate::Plugin(
                    exchange_core::BotConfig {
                        participant: exchange_core::ParticipantConfig {
                            participant_id: format!("event-{account}"),
                            room_id: "f6-batch".into(),
                            account_id: account,
                            instrument_id: Some("V-BTC-SPOT".into()),
                            kind: exchange_core::ParticipantKind::RuleAgent,
                        },
                        plugin_id: "event.test".into(),
                        plugin_version: "1".into(),
                        config_version: 1,
                        state_version: 1,
                        seed: 1,
                        config: serde_json::json!({}),
                    },
                ))
            })
            .map(Arc::new)
            .collect();
        let mut scheduler = crate::room_runtime::DispatchState {
            roster: Arc::new(()),
            revision: 0,
            bots_enabled: true,
            agents,
            registry,
        };
        let mut index = WakeIndex::default();
        index
            .refresh(&Arc::new(rooms.clone()), "f6-batch", &scheduler, 7)
            .unwrap();
        assert_eq!(index.candidates(), BTreeSet::from([0, 1]));
        for i in 0..2 {
            index.consumed(i, &scheduler.agents[i].kind_state);
        }
        rooms.advance_clock("f6-batch", 1).unwrap();
        index
            .refresh(&Arc::new(rooms.clone()), "f6-batch", &scheduler, 7)
            .unwrap();
        assert!(index.candidates().is_empty());
        rooms
            .apply(
                "f6-batch",
                Command::NewOrder(exchange_core::NewOrder {
                    order_id: 777,
                    account_id: 20,
                    side: exchange_core::Side::Buy,
                    qty: 1,
                    kind: exchange_core::OrderKind::Limit { price_tick: 90 },
                    position_side: exchange_core::PositionSide::Both,
                    reduce_only: false,
                }),
            )
            .unwrap();
        index
            .refresh(&Arc::new(rooms.clone()), "f6-batch", &scheduler, 7)
            .unwrap();
        assert_eq!(index.candidates(), BTreeSet::from([0]));
        rooms.advance_clock("f6-batch", 1).unwrap();
        index
            .refresh(&Arc::new(rooms.clone()), "f6-batch", &scheduler, 7)
            .unwrap();
        assert_eq!(
            index.candidates(),
            BTreeSet::from([0]),
            "an in-flight account must retain its wake"
        );
        let mut state = scheduler.agents[0].kind_state.clone();
        if let exchange_core::PersistedAgentKindState::Plugin { data, .. } = &mut state {
            *data = serde_json::json!({"_next_wake_ms":3000}).into();
        }
        index.consumed(0, &state);
        assert!(index.candidates().is_empty());
        rooms.advance_clock("f6-batch", 1).unwrap();
        index
            .refresh(&Arc::new(rooms.clone()), "f6-batch", &scheduler, 7)
            .unwrap();
        assert_eq!(index.candidates(), BTreeSet::from([0]));
        index.consumed(0, &scheduler.agents[0].kind_state);
        rooms.advance_clock("f6-batch", 7).unwrap();
        index
            .refresh(&Arc::new(rooms.clone()), "f6-batch", &scheduler, 7)
            .unwrap();
        assert_eq!(index.candidates(), BTreeSet::from([0, 1]));
        for i in 0..2 {
            index.consumed(i, &scheduler.agents[i].kind_state);
        }
        scheduler.roster = Arc::new(());
        index
            .refresh(&Arc::new(rooms.clone()), "f6-batch", &scheduler, 7)
            .unwrap();
        assert_eq!(
            index.candidates(),
            BTreeSet::from([0, 1]),
            "roster replacement resets all subscriptions"
        );
    }
    #[test]
    fn cached_runtime_requires_exact_committed_state_epoch_and_factory_identity() {
        let spec: exchange_core::population::BackgroundMarket = serde_json::from_str(include_str!(
            "../../scripts/fixtures/microstructure_market.json"
        ))
        .unwrap();
        let mut rooms = RoomManager::new();
        let room = spec.scenario.room_id.clone();
        rooms.create_room(spec.scenario).unwrap();
        let registry = exchange_core::BotRegistry::with_builtins();
        let prior = PersistedAgent::from_template(spec.agents[0].clone());
        let request = Arc::new(ObservationRequest {
            intervals: vec![],
            account_events: false,
            event_filter: None,
            history: registry.market_data_request(&prior.template).unwrap(),
            related: registry.related_instruments(&prior.template).unwrap(),
        });
        let observation = rooms
            .observation_batch(&room)
            .bot_observation(
                prior.requires_instrument().unwrap(),
                prior.account_id(),
                request.history,
                &request.related,
            )
            .unwrap();
        let (decision, cached) = compute_decision(
            prior.clone().into(),
            observation,
            registry.clone(),
            None,
            request,
            7,
        )
        .unwrap();
        let cached = cached.unwrap();
        let mut committed = prior.clone();
        committed.kind_state = (*decision.next).clone();
        committed.unfinished_actions.clear();
        assert!(cached.matches(7, &committed, &registry));
        assert!(Arc::ptr_eq(
            &decision.prior.template,
            &cached.expected.template
        ));
        assert!(Arc::ptr_eq(&decision.next, &cached.expected.kind_state));
        let copy = decision.clone();
        assert!(Arc::ptr_eq(&copy.actions, &decision.actions));
        assert!(!cached.matches(8, &committed, &registry));
        assert!(!cached.matches(7, &committed, &exchange_core::BotRegistry::with_builtins()));
        // State and config edits can preserve both schema version numbers.
        let mut changed = committed.clone();
        changed.kind_state = exchange_core::PersistedAgentKindState::Noise { rng_state: 999 };
        assert!(!cached.matches(7, &changed, &registry));
        changed = committed.clone();
        if let exchange_core::AgentTemplate::Plugin(config) = &mut changed.template {
            config.seed += 1;
        }
        assert!(!cached.matches(7, &changed, &registry));
        changed = committed;
        changed
            .unfinished_actions
            .push(OrderAction::Cancel { order_id: 123 });
        assert!(!cached.matches(7, &changed, &registry));
        if prior.kind_state != *decision.next {
            assert!(
                !cached.matches(7, &prior, &registry),
                "uncommitted speculative state must be discarded"
            );
        }
    }

    #[tokio::test]
    async fn queued_packet_results_and_due_clock_alternate() {
        for prefer_clock in [true, false] {
            let mut tasks = tokio::task::JoinSet::<u8>::new();
            let mut clock = tokio::time::interval(Duration::from_secs(3600));
            let mut stop_poll = tokio::time::interval(Duration::from_secs(3600));
            stop_poll.tick().await;
            match next_wake(&mut clock, &mut stop_poll, &mut tasks, prefer_clock, true).await {
                Wake::Clock => assert!(prefer_clock),
                Wake::Ready => assert!(!prefer_clock),
                _ => panic!("buffered results and clock were ready"),
            }
            match next_wake(
                &mut clock,
                &mut stop_poll,
                &mut tasks,
                !prefer_clock,
                prefer_clock,
            )
            .await
            {
                Wake::Clock => assert!(!prefer_clock),
                Wake::Ready => assert!(prefer_clock),
                _ => panic!("other source must not be starved"),
            }
        }
    }

    #[tokio::test]
    async fn ready_clock_and_ready_results_each_receive_their_turn() {
        for prefer_clock in [true, false] {
            let mut tasks = tokio::task::JoinSet::new();
            let (tx, rx) = tokio::sync::oneshot::channel();
            tasks.spawn(async move {
                tx.send(()).unwrap();
                7u8
            });
            rx.await.unwrap(); // Current-thread executor: task has returned.
            let mut clock = tokio::time::interval(Duration::from_secs(3600));
            let mut stop_poll = tokio::time::interval(Duration::from_secs(3600));
            stop_poll.tick().await;
            match next_wake(&mut clock, &mut stop_poll, &mut tasks, prefer_clock, false).await {
                Wake::Clock => assert!(prefer_clock),
                Wake::Decision(Some(Ok(7))) => assert!(!prefer_clock),
                _ => panic!("both sources were ready"),
            }
            // The other ready source must win on the next iteration.
            match next_wake(&mut clock, &mut stop_poll, &mut tasks, !prefer_clock, false).await {
                Wake::Clock => assert!(!prefer_clock),
                Wake::Decision(Some(Ok(7))) => assert!(prefer_clock),
                _ => panic!("other source must not be starved"),
            }
        }
    }
    #[test]
    fn candidate_index_matches_full_scan_with_stale_states_and_duplicate_ids() {
        let spec: exchange_core::population::BackgroundMarket = serde_json::from_str(include_str!(
            "../../scripts/fixtures/microstructure_market.json"
        ))
        .unwrap();
        let mut scheduler = SchedulerState::new(
            "index-test",
            spec.agents,
            exchange_core::SchedulerMode::Manual,
        );
        scheduler.agents.reverse(); // Legacy restored order need not be sorted.
        let duplicated = scheduler.agents[0].clone();
        scheduler.agents.push(duplicated.clone());
        let index = DecisionIndex::new(&scheduler.agents);
        for prior in &scheduler.agents {
            assert_eq!(
                index.find(&scheduler, prior),
                scheduler.agents.iter().position(|a| a == prior)
            );
            let mut stale = prior.clone();
            stale.config_version += 1;
            assert_eq!(index.find(&scheduler, &stale), None);
            stale = prior.clone();
            stale
                .unfinished_actions
                .push(OrderAction::Cancel { order_id: 123 });
            assert_eq!(index.find(&scheduler, &stale), None);
        }
        scheduler.agents[0].config_version += 1;
        assert_eq!(
            index.find(&scheduler, &duplicated),
            Some(scheduler.agents.len() - 1)
        );
        assert_eq!(index.find(&scheduler, &scheduler.agents[0]), Some(0));
    }

    #[test]
    fn pending_changes_match_mutated_full_roster_for_duplicate_and_sequential_results() {
        let spec: exchange_core::population::BackgroundMarket = serde_json::from_str(include_str!(
            "../../scripts/fixtures/microstructure_market.json"
        ))
        .unwrap();
        let mut scheduler = SchedulerState::new(
            "pending-index",
            spec.agents,
            exchange_core::SchedulerMode::Manual,
        );
        scheduler.agents.reverse();
        scheduler.agents.push(scheduler.agents[0].clone());
        let index = DecisionIndex::new(&scheduler.agents);
        let mut reference = scheduler.clone();
        let mut changes = BTreeMap::new();
        for position in [0, 0, scheduler.agents.len() - 1, 4, 4] {
            let prior = reference.agents[position].clone();
            assert_eq!(
                index.find_pending(&scheduler, &prior.clone().into(), &changes),
                reference.agents.iter().position(|a| a == &prior)
            );
            let found = index
                .find_pending(&scheduler, &prior.clone().into(), &changes)
                .unwrap();
            let mut changed = prior.kind_state.clone();
            if let exchange_core::PersistedAgentKindState::Plugin { data, .. } = &mut changed {
                *data = serde_json::json!({"changed": changes.len() + position}).into();
            }
            reference.agents[found].kind_state = changed.clone();
            reference.agents[found].unfinished_actions.clear();
            changes.insert(
                found,
                AgentDelta {
                    index: found,
                    kind_state: changed,
                    unfinished_actions: vec![],
                },
            );
            for prior in scheduler.agents.iter().chain(&reference.agents) {
                assert_eq!(
                    index.find_pending(&scheduler, &prior.clone().into(), &changes),
                    reference.agents.iter().position(|a| a == prior)
                );
            }
        }
    }
}
