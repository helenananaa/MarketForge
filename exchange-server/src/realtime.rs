//! Live markets never wait for a trader's decision. Only clock/order commits
//! enter the writer lane; observations and returned bot state are immutable inputs.
use super::*;
use exchange_core::{BotError, BotExecutionPolicy, PersistedAgent, SchedulerPhase, SchedulerState};

#[derive(Clone, Default)]
pub(super) struct Control {
    pub stop: Arc<AtomicBool>,
    pub epoch: Arc<AtomicU64>,
}

pub(super) struct Decision {
    pub prior: PersistedAgent,
    pub next: exchange_core::PersistedAgentKindState,
    pub actions: Vec<OrderAction>,
}

pub(super) enum Work {
    Clock(Control),
    Bot {
        control: Control,
        epoch: u64,
        decision: Box<Decision>,
    },
}

impl Work {
    pub fn is_current(&self, scheduler: &SchedulerState) -> bool {
        let control = match self {
            Self::Clock(control) | Self::Bot { control, .. } => control,
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
                if decision.actions.len() > exchange_core::MAX_BOT_ACTIONS {
                    return Err(SchedulerError::Bot(BotError(
                        "bot exceeded action limit".into(),
                    )));
                }
                let config = decision.prior.config();
                let instrument = decision.prior.requires_instrument()?.to_string();
                for action in decision.actions {
                    if order_action_allocates_id(&action)
                        && *next_order_id >= SYSTEM_LIQUIDATION_ORDER_ID_BASE
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
                    let observation = rooms
                        .participant_observation(&scheduler.room_id, &instrument, config.account_id)
                        .map_err(SchedulerError::Room)?;
                    policy
                        .before_action(&request, &observation)
                        .map_err(SchedulerError::Bot)?;
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
                    policy.after_action(&execution, &observation.book);
                }
                let agent = scheduler
                    .agents
                    .iter_mut()
                    .find(|agent| **agent == decision.prior)
                    .expect("decision validated under the writer lock");
                agent.kind_state = decision.next;
                agent.unfinished_actions.clear();
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
    let mut decisions = tokio::task::JoinSet::new();
    let mut in_flight = BTreeSet::new();
    let mut failed = BTreeSet::new();
    loop {
        if control.stop.load(Ordering::Acquire) || !shared.lifecycle.is_accepting_durable_writes() {
            return Ok(());
        }
        tokio::select! {
            _ = stop_poll.tick() => {},
            _ = ticks.tick() => {
                let status = {
                    let app = shared.app.lock().await;
                    app.rooms.status(&room_id).map_err(|error| format!("{error:?}"))?
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
                commit_scheduler_work(shared.clone(), room_id.clone(), None, false, None,
                    Some(Work::Clock(control.clone())))
                    .await.map_err(|(_, body)| body.0.error)?;
                let inputs = {
                    let app = shared.app.lock().await;
                    if control.stop.load(Ordering::Acquire)
                        || app.rooms.status(&room_id).map_err(|e| format!("{e:?}"))? != MarketStatus::Running {
                        continue;
                    }
                    let scheduler = app.schedulers.get(&room_id)
                        .ok_or_else(|| format!("room {room_id} has no scheduler state"))?;
                    let mut inputs = Vec::new();
                    if scheduler.bots_enabled {
                        for agent in &scheduler.agents {
                            let id = agent.template.participant_id();
                            if in_flight.contains(id) || failed.contains(id) { continue; }
                            let observation = agent.requires_instrument()
                                .map_err(|e| format!("{e:?}"))
                                .and_then(|instrument| {
                                    let request = app.bot_registry.market_data_request(&agent.template)
                                        .map_err(|e| e.to_string())?;
                                    app.rooms.bot_observation(&room_id, instrument, agent.account_id(), request)
                                        .map_err(|e| format!("{e:?}"))
                                });
                            inputs.push((agent.clone(), observation, app.bot_registry.clone(),
                                control.epoch.load(Ordering::Acquire)));
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
                            let mut bot = registry.create(&prior.template, &prior.kind_state)
                                .map_err(|error| error.to_string())?;
                            let actions = bot.decide(&observation).map_err(|e| e.to_string())?;
                            if actions.len() > exchange_core::MAX_BOT_ACTIONS {
                                return Err("bot exceeded action limit".to_string());
                            }
                            Ok(Decision { next: bot.snapshot(), prior, actions })
                        })).unwrap_or_else(|_| Err("bot panicked".to_string()));
                        (id, epoch, result)
                    });
                }
            }
            completed = decisions.join_next(), if !decisions.is_empty() => {
                let (id, epoch, result) = completed.unwrap().map_err(|e| e.to_string())?;
                in_flight.remove(&id);
                if control.stop.load(Ordering::Acquire)
                    || epoch != control.epoch.load(Ordering::Acquire) { continue; }
                let result = match result {
                    Ok(decision) => commit_scheduler_work(shared.clone(), room_id.clone(), None,
                        false, None, Some(Work::Bot { control: control.clone(), epoch, decision: Box::new(decision) }))
                        .await.map(|_| ()).map_err(|(status, body)| (status, body.0.error)),
                    Err(error) => Err((StatusCode::BAD_REQUEST, error)),
                };
                if let Err((status, error)) = result {
                    // Storage/lease failures stop the writer; strategy failures stop only
                    // that trader, until the operator explicitly restarts the bots.
                    if status != StatusCode::BAD_REQUEST { return Err(error); }
                    failed.insert(id.clone());
                    shared.lifecycle.record_agent_error();
                    *last_error.lock().unwrap() = Some(format!("bot {id}: {error}"));
                    bot_errors.lock().unwrap().insert(id, error);
                }
            }
        }
    }
}
