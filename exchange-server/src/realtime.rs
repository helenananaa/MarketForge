//! Live markets never wait for a trader's decision. Only clock/order commits
//! enter the writer lane; observations and returned bot state are immutable inputs.
use super::*;
use crate::scheduler_delta::{AgentDelta, SchedulerDelta};
use exchange_core::{BotError, BotExecutionPolicy, PersistedAgent, SchedulerPhase, SchedulerState};

// Bound writer-lane occupancy without dropping decisions or splitting a bot's
// atomic action set. Ready results beyond this batch stay in the JoinSet.
const MAX_READY_DECISIONS: usize = 64;

#[derive(Clone, Default)]
pub(super) struct Control {
    pub stop: Arc<AtomicBool>,
    pub epoch: Arc<AtomicU64>,
}

#[derive(Clone)]
pub(super) struct Decision {
    pub prior: PersistedAgent,
    pub next: exchange_core::PersistedAgentKindState,
    pub actions: Vec<OrderAction>,
}

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
        prior: &PersistedAgent,
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
        self.find_pending(scheduler, prior, &BTreeMap::new())
    }
}

fn matches_pending(
    scheduler: &SchedulerState,
    changes: &BTreeMap<usize, AgentDelta>,
    index: usize,
    prior: &PersistedAgent,
) -> bool {
    let agent = &scheduler.agents[index];
    match changes.get(&index) {
        None => agent == prior,
        Some(change) => {
            agent.version == prior.version
                && agent.config_version == prior.config_version
                && agent.template == prior.template
                && change.kind_state == prior.kind_state
                && change.unfinished_actions == prior.unfinished_actions
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
                        .any(|agent| agent == &decision.prior)
            }
            Self::Bots {
                control,
                epoch,
                decisions,
            } => {
                scheduler.bots_enabled
                    && control.epoch.load(Ordering::Acquire) == *epoch
                    && decisions
                        .iter()
                        .any(|decision| scheduler.agents.contains(&decision.prior))
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

    pub fn apply(
        self,
        rooms: &mut RoomManager,
        next_order_id: &mut u64,
        scheduler: &SchedulerState,
        policy: &mut ServerBotPolicy,
        submissions: &mut BTreeMap<u64, (String, AccountId)>,
    ) -> Result<SchedulerDelta, exchange_core::SchedulerError> {
        use exchange_core::SchedulerError;
        let mut changes = BTreeMap::new();
        match self {
            Self::Bots {
                control,
                epoch,
                decisions,
            } => {
                // Index once per batch, preserving queue order and duplicate-id
                // compatibility. Applying actions never changes the agent roster.
                // Tiny batches do not amortize building an index. The outer
                // is_current check also keeps its short-circuit scan.
                let index = (decisions.len() >= 8).then(|| DecisionIndex::new(&scheduler.agents));
                for decision in decisions {
                    if !scheduler.bots_enabled
                        || control.stop.load(Ordering::Acquire)
                        || control.epoch.load(Ordering::Acquire) != epoch
                    {
                        continue;
                    }
                    let position = match &index {
                        Some(index) => index.find_pending(scheduler, &decision.prior, &changes),
                        None => scheduler
                            .agents
                            .iter()
                            .enumerate()
                            .find(|(index, _)| {
                                matches_pending(scheduler, &changes, *index, &decision.prior)
                            })
                            .map(|(index, _)| index),
                    };
                    if let Some(position) = position {
                        let change = apply_decision(
                            rooms,
                            next_order_id,
                            &scheduler.room_id,
                            policy,
                            submissions,
                            decision,
                            position,
                        )?;
                        changes.insert(position, change);
                    }
                }
            }
            Self::Clock(_) => {
                let previous = rooms
                    .execution_history_len(&scheduler.room_id)
                    .map_err(SchedulerError::Room)?;
                rooms
                    .advance_clock(&scheduler.room_id, 1)
                    .map_err(SchedulerError::Room)?;
                // Clock-triggered liquidations also belong to the training evidence.
                if let Some(run) = &mut policy.training {
                    for execution in rooms
                        .execution_history_from(&scheduler.room_id, previous)
                        .map_err(SchedulerError::Room)?
                    {
                        apply_training_execution(
                            run,
                            execution,
                            None,
                            execution_account_id(execution),
                        );
                    }
                }
            }
            Self::Bot { decision, .. } => {
                let index = scheduler
                    .agents
                    .iter()
                    .position(|agent| *agent == decision.prior)
                    .expect("decision validated under the writer lock");
                let change = apply_decision(
                    rooms,
                    next_order_id,
                    &scheduler.room_id,
                    policy,
                    submissions,
                    *decision,
                    index,
                )?;
                changes.insert(index, change);
            }
        }
        let mut state = SchedulerDelta::metadata(scheduler);
        state.phase = SchedulerPhase::StepComplete {
            step: rooms
                .clock(&scheduler.room_id)
                .map_err(SchedulerError::Room)?
                .step(),
        };
        state.lagged = false;
        Ok(SchedulerDelta {
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
        })
    }
}

fn apply_decision(
    rooms: &mut RoomManager,
    next_order_id: &mut u64,
    room_id: &str,
    policy: &mut ServerBotPolicy,
    submissions: &mut BTreeMap<u64, (String, AccountId)>,
    decision: Decision,
    index: usize,
) -> Result<AgentDelta, exchange_core::SchedulerError> {
    use exchange_core::SchedulerError;
    if decision.actions.len() > exchange_core::MAX_BOT_ACTIONS {
        return Err(SchedulerError::Bot(BotError(
            "bot exceeded action limit".into(),
        )));
    }
    let config = decision.prior.config();
    let instrument = decision.prior.requires_instrument()?.to_string();
    for action in decision.actions {
        if order_action_allocates_id(&action) && *next_order_id >= SYSTEM_LIQUIDATION_ORDER_ID_BASE
        {
            return Err(SchedulerError::Bot(BotError(
                "API order-id range is exhausted".into(),
            )));
        }
        let request = GatewayRequest {
            participant_id: config.participant_id.clone(),
            room_id: room_id.to_string(),
            instrument_id: Some(instrument.clone()),
            account_id: config.account_id,
            action,
        };
        // The gateway still performs all live market/account checks.
        // Only training policy needs a second full public snapshot.
        let observation = if policy.training.is_some() {
            let observation = rooms
                .participant_observation(room_id, &instrument, config.account_id)
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
        submissions.insert(
            execution.execution.command_seq,
            (execution.participant_id.clone(), execution.account_id),
        );
        if let Some(observation) = observation {
            policy.after_action(&execution, &observation.book);
        }
    }
    Ok(AgentDelta {
        index,
        kind_state: decision.next,
        unfinished_actions: Vec::new(),
    })
}

// Alternate priority only when both sources are ready. A due clock cannot
// wait behind an entire completed population, and a busy clock cannot starve
// already completed decisions. Pending decisions still never delay a tick.
enum Wake<T> {
    Poll,
    Clock,
    Decision(Option<Result<T, tokio::task::JoinError>>),
}

async fn next_wake<T: Send + 'static>(
    ticks: &mut tokio::time::Interval,
    stop_poll: &mut tokio::time::Interval,
    decisions: &mut tokio::task::JoinSet<T>,
    prefer_clock: bool,
) -> Wake<T> {
    if prefer_clock {
        tokio::select! {
            biased;
            _ = stop_poll.tick() => Wake::Poll,
            _ = ticks.tick() => Wake::Clock,
            completed = decisions.join_next(), if !decisions.is_empty() => Wake::Decision(completed),
        }
    } else {
        tokio::select! {
            biased;
            _ = stop_poll.tick() => Wake::Poll,
            completed = decisions.join_next(), if !decisions.is_empty() => Wake::Decision(completed),
            _ = ticks.tick() => Wake::Clock,
        }
    }
}

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
    let mut decisions = tokio::task::JoinSet::<(String, u64, Result<Decision, String>)>::new();
    let mut in_flight = BTreeSet::new();
    let mut failed = BTreeSet::new();
    let mut prefer_clock = false;
    loop {
        if control.stop.load(Ordering::Acquire) || !shared.lifecycle.is_accepting_durable_writes() {
            return Ok(());
        }
        match next_wake(&mut ticks, &mut stop_poll, &mut decisions, prefer_clock).await {
            Wake::Poll => {}
            Wake::Decision(completed) => {
                prefer_clock = true;
                let mut ready = vec![completed.unwrap()];
                while ready.len() < MAX_READY_DECISIONS {
                    let Some(completed) = decisions.try_join_next() else {
                        break;
                    };
                    ready.push(completed);
                }
                let mut wave = Vec::new();
                let current_epoch = control.epoch.load(Ordering::Acquire);
                for completed in ready {
                    let (id, epoch, result) = completed.map_err(|e| e.to_string())?;
                    in_flight.remove(&id);
                    if control.stop.load(Ordering::Acquire) || epoch != current_epoch {
                        continue;
                    }
                    match result {
                        Ok(decision) => wave.push(decision),
                        Err(error) => {
                            failed.insert(id.clone());
                            shared.lifecycle.record_agent_error();
                            *last_error.lock().unwrap() = Some(format!("bot {id}: {error}"));
                            bot_errors.lock().unwrap().insert(id, error);
                        }
                    }
                }
                if wave.is_empty() {
                    continue;
                }
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
                if let Err((status, body)) = result {
                    if status != StatusCode::BAD_REQUEST {
                        return Err(body.0.error);
                    }
                    // No part of the failed wave was committed. Retrying its members
                    // preserves each bot's atomicity and existing error isolation.
                    for decision in wave {
                        let id = decision.prior.template.participant_id().to_string();
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
                            if status != StatusCode::BAD_REQUEST {
                                return Err(body.0.error);
                            }
                            let error = body.0.error;
                            failed.insert(id.clone());
                            shared.lifecycle.record_agent_error();
                            *last_error.lock().unwrap() = Some(format!("bot {id}: {error}"));
                            bot_errors.lock().unwrap().insert(id, error);
                        }
                    }
                }
            }
            Wake::Clock => {
                prefer_clock = false;
                let status = {
                    let app = shared.app.lock_room(&room_id).await;
                    app.rooms
                        .status(&room_id)
                        .map_err(|error| format!("{error:?}"))?
                };
                match status {
                    MarketStatus::Closed => return Ok(()),
                    MarketStatus::Paused => {
                        *lifecycle.lock().unwrap() = AgentWorkerLifecycle::Paused;
                        continue;
                    }
                    MarketStatus::Running => {}
                }
                *lifecycle.lock().unwrap() = AgentWorkerLifecycle::Running;
                commit_realtime_work(
                    shared.clone(),
                    room_id.clone(),
                    Work::Clock(control.clone()),
                )
                .await
                .map_err(|(_, body)| body.0.error)?;
                let (rooms, registry, agents, epoch) = {
                    let app = shared.app.lock_room(&room_id).await;
                    if control.stop.load(Ordering::Acquire)
                        || app.rooms.status(&room_id).map_err(|e| format!("{e:?}"))?
                            != MarketStatus::Running
                    {
                        continue;
                    }
                    let scheduler = app
                        .schedulers
                        .get(&room_id)
                        .ok_or_else(|| format!("room {room_id} has no scheduler state"))?;
                    let agents: Vec<_> = scheduler
                        .agents
                        .iter()
                        .filter(|agent| {
                            scheduler.bots_enabled
                                && !in_flight.contains(agent.template.participant_id())
                                && !failed.contains(agent.template.participant_id())
                        })
                        .cloned()
                        .collect();
                    (
                        app.rooms.clone(),
                        app.bot_registry.clone(),
                        agents,
                        control.epoch.load(Ordering::Acquire),
                    )
                };
                // Observations and plugin data requests use one frozen market
                // version without retaining the exchange execution lane.
                let mut observations = rooms.observation_batch(&room_id);
                let mut inputs = Vec::with_capacity(agents.len());
                for agent in agents {
                    let observation = agent
                        .requires_instrument()
                        .map_err(|e| format!("{e:?}"))
                        .and_then(|instrument| {
                            let request = registry
                                .market_data_request(&agent.template)
                                .map_err(|e| e.to_string())?;
                            let related = registry
                                .related_instruments(&agent.template)
                                .map_err(|e| e.to_string())?;
                            observations
                                .bot_observation(instrument, agent.account_id(), request, &related)
                                .map_err(|e| format!("{e:?}"))
                        });
                    inputs.push((agent, observation, registry.clone(), epoch));
                }
                for (prior, observation, registry, epoch) in inputs {
                    let id = prior.template.participant_id().to_string();
                    in_flight.insert(id.clone());
                    // Includes factory construction and snapshotting: no plugin code runs
                    // while holding AppState. One outstanding task per bot bounds backlog.
                    decisions.spawn_blocking(move || {
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let observation = observation?;
                            let mut bot = registry
                                .create(&prior.template, &prior.kind_state)
                                .map_err(|error| error.to_string())?;
                            let actions = bot.decide(&observation).map_err(|e| e.to_string())?;
                            if actions.len() > exchange_core::MAX_BOT_ACTIONS {
                                return Err("bot exceeded action limit".to_string());
                            }
                            Ok(Decision {
                                next: bot.snapshot(),
                                prior,
                                actions,
                            })
                        }))
                        .unwrap_or_else(|_| Err("bot panicked".to_string()));
                        (id, epoch, result)
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod index_tests {
    use super::*;
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
            match next_wake(&mut clock, &mut stop_poll, &mut tasks, prefer_clock).await {
                Wake::Clock => assert!(prefer_clock),
                Wake::Decision(Some(Ok(7))) => assert!(!prefer_clock),
                _ => panic!("both sources were ready"),
            }
            // The other ready source must win on the next iteration.
            match next_wake(&mut clock, &mut stop_poll, &mut tasks, !prefer_clock).await {
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
                index.find_pending(&scheduler, &prior, &changes),
                reference.agents.iter().position(|a| a == &prior)
            );
            let found = index.find_pending(&scheduler, &prior, &changes).unwrap();
            let mut changed = prior.kind_state.clone();
            if let exchange_core::PersistedAgentKindState::Plugin { data, .. } = &mut changed {
                *data = serde_json::json!({"changed": changes.len() + position});
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
                    index.find_pending(&scheduler, prior, &changes),
                    reference.agents.iter().position(|a| a == prior)
                );
            }
        }
    }
}
