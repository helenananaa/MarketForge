//! Live markets never wait for a trader's decision. Only clock/order commits
//! enter the writer lane; observations and returned bot state are immutable inputs.
use super::*;
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
    fn find(&self, scheduler: &SchedulerState, prior: &PersistedAgent) -> Option<usize> {
        self.0
            .get(prior.template.participant_id())?
            .iter()
            .copied()
            .find(|&position| scheduler.agents[position] == *prior)
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

    pub fn apply(
        self,
        rooms: &mut RoomManager,
        next_order_id: &mut u64,
        mut scheduler: SchedulerState,
        policy: &mut ServerBotPolicy,
        submissions: &mut BTreeMap<u64, (String, AccountId)>,
    ) -> Result<exchange_core::SchedulerStepOutcome, exchange_core::SchedulerError> {
        use exchange_core::SchedulerError;
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
                        Some(index) => index.find(&scheduler, &decision.prior),
                        None => scheduler
                            .agents
                            .iter()
                            .position(|agent| *agent == decision.prior),
                    };
                    if let Some(position) = position {
                        apply_decision(
                            rooms,
                            next_order_id,
                            &mut scheduler,
                            policy,
                            submissions,
                            decision,
                            position,
                        )?;
                    }
                }
            }
            Self::Clock(_) => {
                let previous = rooms
                    .execution_history(&scheduler.room_id)
                    .map_err(SchedulerError::Room)?
                    .len();
                rooms
                    .advance_clock(&scheduler.room_id, 1)
                    .map_err(SchedulerError::Room)?;
                // Clock-triggered liquidations also belong to the training evidence.
                if let Some(run) = &mut policy.training {
                    for execution in &rooms
                        .execution_history(&scheduler.room_id)
                        .map_err(SchedulerError::Room)?[previous..]
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
                apply_decision(
                    rooms,
                    next_order_id,
                    &mut scheduler,
                    policy,
                    submissions,
                    *decision,
                    index,
                )?;
            }
        }
        scheduler.phase = SchedulerPhase::StepComplete {
            step: rooms
                .clock(&scheduler.room_id)
                .map_err(SchedulerError::Room)?
                .step(),
        };
        scheduler.lagged = false;
        Ok(exchange_core::SchedulerStepOutcome {
            state: scheduler,
            crashed: false,
            crash_point: exchange_core::CrashPoint::None,
        })
    }
}

fn apply_decision(
    rooms: &mut RoomManager,
    next_order_id: &mut u64,
    scheduler: &mut SchedulerState,
    policy: &mut ServerBotPolicy,
    submissions: &mut BTreeMap<u64, (String, AccountId)>,
    decision: Decision,
    index: usize,
) -> Result<(), exchange_core::SchedulerError> {
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
            room_id: scheduler.room_id.clone(),
            instrument_id: Some(instrument.clone()),
            account_id: config.account_id,
            action,
        };
        // The gateway still performs all live market/account checks.
        // Only training policy needs a second full public snapshot.
        let observation = if policy.training.is_some() {
            let observation = rooms
                .participant_observation(&scheduler.room_id, &instrument, config.account_id)
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
    let agent = &mut scheduler.agents[index];
    agent.kind_state = decision.next;
    agent.unfinished_actions.clear();
    Ok(())
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
                let result = commit_scheduler_work(
                    shared.clone(),
                    room_id.clone(),
                    None,
                    false,
                    None,
                    Some(Work::Bots {
                        control: control.clone(),
                        epoch: current_epoch,
                        decisions: wave.clone(),
                    }),
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
                        let result = commit_scheduler_work(
                            shared.clone(),
                            room_id.clone(),
                            None,
                            false,
                            None,
                            Some(Work::Bot {
                                control: control.clone(),
                                epoch: current_epoch,
                                decision: Box::new(decision),
                            }),
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
                    let app = shared.app.lock().await;
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
                commit_scheduler_work(
                    shared.clone(),
                    room_id.clone(),
                    None,
                    false,
                    None,
                    Some(Work::Clock(control.clone())),
                )
                .await
                .map_err(|(_, body)| body.0.error)?;
                let inputs = {
                    let app = shared.app.lock().await;
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
                    let mut inputs = Vec::new();
                    let mut observations = app.rooms.observation_batch(&room_id);
                    if scheduler.bots_enabled {
                        for agent in &scheduler.agents {
                            let id = agent.template.participant_id();
                            if in_flight.contains(id) || failed.contains(id) {
                                continue;
                            }
                            let observation = agent
                                .requires_instrument()
                                .map_err(|e| format!("{e:?}"))
                                .and_then(|instrument| {
                                    let request = app
                                        .bot_registry
                                        .market_data_request(&agent.template)
                                        .map_err(|e| e.to_string())?;
                                    let related = app
                                        .bot_registry
                                        .related_instruments(&agent.template)
                                        .map_err(|e| e.to_string())?;
                                    observations
                                        .bot_observation(
                                            instrument,
                                            agent.account_id(),
                                            request,
                                            &related,
                                        )
                                        .map_err(|e| format!("{e:?}"))
                                });
                            inputs.push((
                                agent.clone(),
                                observation,
                                app.bot_registry.clone(),
                                control.epoch.load(Ordering::Acquire),
                            ));
                        }
                    }
                    inputs
                };
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
}
