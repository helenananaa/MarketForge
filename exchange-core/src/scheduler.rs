use serde::{Deserialize, Serialize};

use crate::{
    agents::{
        AGENT_CONFIG_VERSION, AGENT_STATE_VERSION, AgentTemplate, DcaTrader, GridTrader,
        NoiseTrader, PersistedAgentKindState,
    },
    gateway::{GatewayError, GatewayRequest, OrderAction, OrderGateway, TradingApi},
    model::AccountId,
    observation::ParticipantObservation,
    participant::{Participant, ParticipantConfig},
    room::{RoomManager, RoomManagerError},
};

pub const SCHEDULER_STATE_VERSION: u16 = 1;
pub const DEFAULT_CATCH_UP_LIMIT: u32 = 8;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum AgentContinuity {
    Continuous,
    LegacyNonContinuous,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SchedulerMode {
    Manual,
    Auto { interval_ms: u64 },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SchedulerPhase {
    Idle,
    AwaitingParticipant {
        step: u64,
        participant_index: usize,
    },
    Decided {
        step: u64,
        participant_index: usize,
    },
    Submitting {
        step: u64,
        participant_index: usize,
        action_index: usize,
    },
    StepComplete {
        step: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CrashPoint {
    None,
    BeforeDecisionPersist {
        step: u64,
        participant_index: usize,
    },
    AfterDecisionPersist {
        step: u64,
        participant_index: usize,
    },
    BeforeActionSubmit {
        step: u64,
        participant_index: usize,
        action_index: usize,
    },
    AfterActionSubmit {
        step: u64,
        participant_index: usize,
        action_index: usize,
    },
    BeforeStepComplete {
        step: u64,
    },
    AfterStepComplete {
        step: u64,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PersistedAgent {
    pub version: u16,
    pub config_version: u16,
    pub template: AgentTemplate,
    pub kind_state: PersistedAgentKindState,
    pub unfinished_actions: Vec<OrderAction>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SchedulerState {
    pub version: u16,
    pub room_id: String,
    pub mode: SchedulerMode,
    pub catch_up_limit: u32,
    pub lagged: bool,
    pub continuity: AgentContinuity,
    pub phase: SchedulerPhase,
    pub agents: Vec<PersistedAgent>,
}

impl SchedulerState {
    pub fn new(
        room_id: impl Into<String>,
        templates: Vec<AgentTemplate>,
        mode: SchedulerMode,
    ) -> Self {
        let mut agents = templates
            .into_iter()
            .map(PersistedAgent::from_template)
            .collect::<Vec<_>>();
        agents.sort_by(|left, right| {
            left.template
                .participant_id()
                .cmp(right.template.participant_id())
        });
        Self {
            version: SCHEDULER_STATE_VERSION,
            room_id: room_id.into(),
            mode,
            catch_up_limit: DEFAULT_CATCH_UP_LIMIT,
            lagged: false,
            continuity: AgentContinuity::Continuous,
            phase: SchedulerPhase::Idle,
            agents,
        }
    }

    pub fn legacy_non_continuous(
        room_id: impl Into<String>,
        templates: Vec<AgentTemplate>,
        mode: SchedulerMode,
    ) -> Self {
        let mut state = Self::new(room_id, templates, mode);
        state.continuity = AgentContinuity::LegacyNonContinuous;
        state
    }

    pub fn participant_ids(&self) -> Vec<String> {
        self.agents
            .iter()
            .map(|agent| agent.template.participant_id().to_string())
            .collect()
    }
}

impl PersistedAgent {
    pub fn from_template(template: AgentTemplate) -> Self {
        let kind_state = match &template {
            AgentTemplate::NoiseTrader(config) => PersistedAgentKindState::Noise {
                rng_state: config.seed.max(1),
            },
            AgentTemplate::DcaTrader(_) => PersistedAgentKindState::Dca { observed_steps: 0 },
            AgentTemplate::GridTrader(_) => PersistedAgentKindState::Grid {
                has_seeded_grid: false,
            },
        };
        Self {
            version: AGENT_STATE_VERSION,
            config_version: AGENT_CONFIG_VERSION,
            template,
            kind_state,
            unfinished_actions: Vec::new(),
        }
    }

    pub fn requires_instrument(&self) -> Result<&str, SchedulerError> {
        let instrument = match &self.template {
            AgentTemplate::NoiseTrader(config) => config.participant.instrument_id.as_deref(),
            AgentTemplate::DcaTrader(config) => config.participant.instrument_id.as_deref(),
            AgentTemplate::GridTrader(config) => config.participant.instrument_id.as_deref(),
        };
        instrument.ok_or_else(|| SchedulerError::MissingInstrument {
            participant_id: self.template.participant_id().to_string(),
        })
    }

    pub fn account_id(&self) -> AccountId {
        match &self.template {
            AgentTemplate::NoiseTrader(config) => config.participant.account_id,
            AgentTemplate::DcaTrader(config) => config.participant.account_id,
            AgentTemplate::GridTrader(config) => config.participant.account_id,
        }
    }

    pub fn config(&self) -> &ParticipantConfig {
        match &self.template {
            AgentTemplate::NoiseTrader(config) => &config.participant,
            AgentTemplate::DcaTrader(config) => &config.participant,
            AgentTemplate::GridTrader(config) => &config.participant,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SchedulerError {
    Room(RoomManagerError),
    Gateway(GatewayError),
    MissingInstrument { participant_id: String },
    Closed,
    NotPausedForManualStep,
    UnknownCrashRestore,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionKey {
    pub room_id: String,
    pub step: u64,
    pub participant_id: String,
    pub action_index: usize,
}

#[derive(Clone, Debug)]
pub struct SchedulerStepOutcome {
    pub state: SchedulerState,
    pub crashed: bool,
    pub crash_point: CrashPoint,
}

enum RuntimeAgent {
    Noise(NoiseTrader),
    Dca(DcaTrader),
    Grid(GridTrader),
}

impl RuntimeAgent {
    fn from_persisted(agent: &PersistedAgent) -> Result<Self, SchedulerError> {
        let mut runtime = match agent.template.clone() {
            AgentTemplate::NoiseTrader(config) => Self::Noise(NoiseTrader::new(config)),
            AgentTemplate::DcaTrader(config) => Self::Dca(DcaTrader::new(config)),
            AgentTemplate::GridTrader(config) => Self::Grid(GridTrader::new(config)),
        };
        if !runtime.restore(&agent.kind_state) {
            return Err(SchedulerError::UnknownCrashRestore);
        }
        Ok(runtime)
    }

    fn restore(&mut self, state: &PersistedAgentKindState) -> bool {
        match self {
            Self::Noise(agent) => agent.restore_kind_state(state),
            Self::Dca(agent) => agent.restore_kind_state(state),
            Self::Grid(agent) => agent.restore_kind_state(state),
        }
    }

    fn persist(&self) -> PersistedAgentKindState {
        match self {
            Self::Noise(agent) => agent.persist_kind_state(),
            Self::Dca(agent) => agent.persist_kind_state(),
            Self::Grid(agent) => agent.persist_kind_state(),
        }
    }

    fn observe(&mut self, view: &ParticipantObservation) {
        match self {
            Self::Noise(agent) => agent.observe(view),
            Self::Dca(agent) => agent.observe(view),
            Self::Grid(agent) => agent.observe(view),
        }
    }

    fn decide(&mut self) -> Vec<OrderAction> {
        match self {
            Self::Noise(agent) => agent.decide(),
            Self::Dca(agent) => agent.decide(),
            Self::Grid(agent) => agent.decide(),
        }
    }

    fn as_participant(&mut self) -> &mut dyn Participant {
        match self {
            Self::Noise(agent) => agent,
            Self::Dca(agent) => agent,
            Self::Grid(agent) => agent,
        }
    }
}

fn matches_crash(point: CrashPoint, expected: CrashPoint) -> bool {
    point != CrashPoint::None && point == expected
}

/// Drive one authoritative simulation step against `rooms`.
///
/// Order: advance clock → stable participant_id order observe/decide → submit
/// actions. `crash_at` stops before applying the named side effect so tests can
/// restore `state` and continue.
pub fn run_scheduler_step(
    rooms: &mut RoomManager,
    next_order_id: &mut u64,
    state: SchedulerState,
    crash_at: CrashPoint,
) -> Result<SchedulerStepOutcome, SchedulerError> {
    if rooms.status(&state.room_id).map_err(SchedulerError::Room)?
        == crate::actor::MarketStatus::Closed
    {
        return Err(SchedulerError::Closed);
    }

    let mut state = state;
    let mut agents = state
        .agents
        .iter()
        .map(RuntimeAgent::from_persisted)
        .collect::<Result<Vec<_>, _>>()?;

    match state.phase.clone() {
        SchedulerPhase::Idle | SchedulerPhase::StepComplete { .. } => {
            rooms
                .advance_clock(&state.room_id, 1)
                .map_err(SchedulerError::Room)?;
            let step = rooms
                .clock(&state.room_id)
                .map_err(SchedulerError::Room)?
                .step();
            state.phase = SchedulerPhase::AwaitingParticipant {
                step,
                participant_index: 0,
            };
        }
        SchedulerPhase::AwaitingParticipant { .. }
        | SchedulerPhase::Decided { .. }
        | SchedulerPhase::Submitting { .. } => {}
    }

    let step = match state.phase {
        SchedulerPhase::AwaitingParticipant { step, .. }
        | SchedulerPhase::Decided { step, .. }
        | SchedulerPhase::Submitting { step, .. } => step,
        SchedulerPhase::Idle | SchedulerPhase::StepComplete { .. } => rooms
            .clock(&state.room_id)
            .map_err(SchedulerError::Room)?
            .step(),
    };

    let start_index = match state.phase {
        SchedulerPhase::AwaitingParticipant {
            participant_index, ..
        }
        | SchedulerPhase::Decided {
            participant_index, ..
        }
        | SchedulerPhase::Submitting {
            participant_index, ..
        } => participant_index,
        SchedulerPhase::Idle | SchedulerPhase::StepComplete { .. } => 0,
    };

    for participant_index in start_index..agents.len() {
        let instrument_id = state.agents[participant_index]
            .requires_instrument()?
            .to_string();
        let account_id = state.agents[participant_index].account_id();
        let participant_id = state.agents[participant_index]
            .template
            .participant_id()
            .to_string();
        let room_id = state.room_id.clone();

        let already_decided = matches!(
            state.phase,
            SchedulerPhase::Decided {
                participant_index: index,
                ..
            }
            | SchedulerPhase::Submitting {
                participant_index: index,
                ..
            } if index == participant_index
        );

        if !already_decided {
            if matches_crash(
                crash_at,
                CrashPoint::BeforeDecisionPersist {
                    step,
                    participant_index,
                },
            ) {
                persist_runtime(&mut state, &agents);
                return Ok(SchedulerStepOutcome {
                    state,
                    crashed: true,
                    crash_point: crash_at,
                });
            }
            let observation = rooms
                .participant_observation(&room_id, &instrument_id, account_id)
                .map_err(SchedulerError::Room)?;
            agents[participant_index].observe(&observation);
            let actions = agents[participant_index].decide();
            state.agents[participant_index].kind_state = agents[participant_index].persist();
            state.agents[participant_index].unfinished_actions = actions;
            state.phase = SchedulerPhase::Decided {
                step,
                participant_index,
            };
            persist_runtime(&mut state, &agents);
            if matches_crash(
                crash_at,
                CrashPoint::AfterDecisionPersist {
                    step,
                    participant_index,
                },
            ) {
                return Ok(SchedulerStepOutcome {
                    state,
                    crashed: true,
                    crash_point: crash_at,
                });
            }
        }

        let start_action = match state.phase {
            SchedulerPhase::Submitting {
                action_index,
                participant_index: index,
                ..
            } if index == participant_index => action_index,
            _ => 0,
        };
        let actions = state.agents[participant_index].unfinished_actions.clone();
        for (action_index, action) in actions.iter().enumerate().skip(start_action) {
            if matches_crash(
                crash_at,
                CrashPoint::BeforeActionSubmit {
                    step,
                    participant_index,
                    action_index,
                },
            ) {
                persist_runtime(&mut state, &agents);
                return Ok(SchedulerStepOutcome {
                    state,
                    crashed: true,
                    crash_point: crash_at,
                });
            }
            let mut gateway = OrderGateway::new_scheduler(rooms, *next_order_id);
            gateway
                .submit_action(GatewayRequest {
                    participant_id: participant_id.clone(),
                    room_id: room_id.clone(),
                    instrument_id: Some(instrument_id.clone()),
                    account_id,
                    action: action.clone(),
                })
                .map_err(SchedulerError::Gateway)?;
            *next_order_id = gateway.next_order_id();
            state.phase = SchedulerPhase::Submitting {
                step,
                participant_index,
                action_index: action_index + 1,
            };
            persist_runtime(&mut state, &agents);
            if matches_crash(
                crash_at,
                CrashPoint::AfterActionSubmit {
                    step,
                    participant_index,
                    action_index,
                },
            ) {
                return Ok(SchedulerStepOutcome {
                    state,
                    crashed: true,
                    crash_point: crash_at,
                });
            }
        }
        state.agents[participant_index].unfinished_actions.clear();
        state.phase = SchedulerPhase::AwaitingParticipant {
            step,
            participant_index: participant_index + 1,
        };
        persist_runtime(&mut state, &agents);
        let _ = agents[participant_index].as_participant();
    }

    if matches_crash(crash_at, CrashPoint::BeforeStepComplete { step }) {
        persist_runtime(&mut state, &agents);
        return Ok(SchedulerStepOutcome {
            state,
            crashed: true,
            crash_point: crash_at,
        });
    }
    state.phase = SchedulerPhase::StepComplete { step };
    persist_runtime(&mut state, &agents);
    if matches_crash(crash_at, CrashPoint::AfterStepComplete { step }) {
        return Ok(SchedulerStepOutcome {
            state,
            crashed: true,
            crash_point: crash_at,
        });
    }

    Ok(SchedulerStepOutcome {
        state,
        crashed: false,
        crash_point: CrashPoint::None,
    })
}

fn persist_runtime(state: &mut SchedulerState, agents: &[RuntimeAgent]) {
    for (persisted, runtime) in state.agents.iter_mut().zip(agents.iter()) {
        persisted.kind_state = runtime.persist();
        persisted.version = AGENT_STATE_VERSION;
        persisted.config_version = AGENT_CONFIG_VERSION;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DcaTraderConfig, GridTraderConfig, ParticipantKind, Side, SpotRiskConfig,
        market::{InstrumentConfig, MarketConfig, SpotMarketConfig},
        scenario::{ScenarioAccount, ScenarioConfig},
        spot::SpotClearingConfig,
    };

    fn participant(id: &str, account_id: u64) -> ParticipantConfig {
        ParticipantConfig {
            participant_id: id.to_string(),
            kind: ParticipantKind::RuleAgent,
            room_id: "sched-room".to_string(),
            account_id,
            instrument_id: Some("V-BTC-SPOT".to_string()),
        }
    }

    fn scenario() -> ScenarioConfig {
        ScenarioConfig {
            room_id: "sched-room".to_string(),
            venue_preset: None,
            venue_rules: crate::VenueRuleConfig::default(),
            venue_asset_policy: crate::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: MarketConfig::Spot(SpotMarketConfig {
                instrument: InstrumentConfig::new("V-BTC-SPOT", 1, 1).unwrap(),
                clearing: SpotClearingConfig::default(),
                risk: SpotRiskConfig {
                    allow_short: true,
                    ..SpotRiskConfig::default()
                },
            }),
            extra_markets: Vec::new(),
            initial_portfolios: Vec::new(),
            initial_allocations: Vec::new(),
            routed_initial_allocations: Vec::new(),
            accounts: vec![
                ScenarioAccount::Basic {
                    account_id: 20,
                    cash_balance: 10_000,
                },
                ScenarioAccount::Spot {
                    account_id: 30,
                    cash_balance: 10_000,
                    position_qty: 100,
                },
            ],
            seed_orders: vec![],
            routed_seed_orders: Vec::new(),
        }
    }

    fn templates() -> Vec<AgentTemplate> {
        vec![
            AgentTemplate::GridTrader(GridTraderConfig {
                participant: participant("grid-1", 30),
                center_price_tick: 100,
                grid_spacing_ticks: 5,
                levels: 1,
                qty_per_level: 2,
            }),
            AgentTemplate::DcaTrader(DcaTraderConfig {
                participant: participant("dca-1", 20),
                interval_steps: 1,
                order_qty: 1,
                use_market_order: false,
                limit_offset_ticks: 10,
                fallback_price_tick: 100,
                side: Side::Buy,
            }),
        ]
    }

    fn run_uninterrupted() -> (RoomManager, SchedulerState, u64) {
        let mut rooms = RoomManager::new();
        rooms.create_room(scenario()).unwrap();
        let mut next_order_id = 1;
        let mut state = SchedulerState::new("sched-room", templates(), SchedulerMode::Manual);
        let outcome =
            run_scheduler_step(&mut rooms, &mut next_order_id, state, CrashPoint::None).unwrap();
        state = outcome.state;
        (rooms, state, next_order_id)
    }

    #[test]
    fn participants_run_in_stable_id_order_after_clock_advance() {
        let (rooms, state, _) = run_uninterrupted();
        assert_eq!(rooms.clock("sched-room").unwrap().step(), 1);
        assert_eq!(state.agents[0].template.participant_id(), "dca-1");
        assert_eq!(state.agents[1].template.participant_id(), "grid-1");
        assert!(matches!(
            state.phase,
            SchedulerPhase::StepComplete { step: 1 }
        ));
        assert_eq!(rooms.execution_history("sched-room").unwrap().len(), 3);
    }

    #[test]
    fn second_account_privacy_does_not_change_first_observation() {
        let mut rooms = RoomManager::new();
        rooms.create_room(scenario()).unwrap();
        let first = rooms
            .participant_observation("sched-room", "V-BTC-SPOT", 20)
            .unwrap();
        assert!(first.own_account.is_some());
        match first.own_account.as_ref().unwrap() {
            crate::actor::AccountSnapshot::Spot(account) => {
                assert_eq!(account.account_id, 20);
            }
            crate::actor::AccountSnapshot::Perp(_) => panic!("expected spot"),
        }
        assert!(
            first
                .own_account
                .as_ref()
                .is_some_and(|snapshot| match snapshot {
                    crate::actor::AccountSnapshot::Spot(account) => account.account_id != 30,
                    crate::actor::AccountSnapshot::Perp(account) => account.account_id != 30,
                })
        );
        let second = rooms
            .participant_observation("sched-room", "V-BTC-SPOT", 30)
            .unwrap();
        assert_eq!(first.book, second.book);
        assert_eq!(first.public_trades, second.public_trades);
        assert_ne!(first.own_account, second.own_account);
    }

    #[test]
    fn crash_after_decision_then_resume_matches_uninterrupted() {
        let (expected_rooms, expected_state, expected_order_id) = run_uninterrupted();
        let mut rooms = RoomManager::new();
        rooms.create_room(scenario()).unwrap();
        let mut next_order_id = 1;
        let state = SchedulerState::new("sched-room", templates(), SchedulerMode::Manual);
        let crashed = run_scheduler_step(
            &mut rooms,
            &mut next_order_id,
            state,
            CrashPoint::AfterDecisionPersist {
                step: 1,
                participant_index: 0,
            },
        )
        .unwrap();
        assert!(crashed.crashed);
        let resumed = run_scheduler_step(
            &mut rooms,
            &mut next_order_id,
            crashed.state,
            CrashPoint::None,
        )
        .unwrap();
        assert!(!resumed.crashed);
        assert_eq!(
            rooms.execution_history("sched-room").unwrap().len(),
            expected_rooms
                .execution_history("sched-room")
                .unwrap()
                .len()
        );
        assert_eq!(resumed.state.phase, expected_state.phase);
        assert_eq!(next_order_id, expected_order_id);
        assert_eq!(
            resumed.state.agents[0].kind_state,
            expected_state.agents[0].kind_state
        );
        assert_eq!(
            resumed.state.agents[1].kind_state,
            expected_state.agents[1].kind_state
        );
    }

    #[test]
    fn crash_before_and_after_action_submit_does_not_double_trade() {
        let (expected_rooms, _, _) = run_uninterrupted();
        let expected_len = expected_rooms
            .execution_history("sched-room")
            .unwrap()
            .len();

        for crash in [
            CrashPoint::BeforeActionSubmit {
                step: 1,
                participant_index: 0,
                action_index: 0,
            },
            CrashPoint::AfterActionSubmit {
                step: 1,
                participant_index: 0,
                action_index: 0,
            },
            CrashPoint::BeforeStepComplete { step: 1 },
        ] {
            let mut rooms = RoomManager::new();
            rooms.create_room(scenario()).unwrap();
            let mut next_order_id = 1;
            let state = SchedulerState::new("sched-room", templates(), SchedulerMode::Manual);
            let crashed = run_scheduler_step(&mut rooms, &mut next_order_id, state, crash).unwrap();
            assert!(crashed.crashed, "{crash:?}");
            let resumed = run_scheduler_step(
                &mut rooms,
                &mut next_order_id,
                crashed.state,
                CrashPoint::None,
            )
            .unwrap();
            assert!(!resumed.crashed);
            assert_eq!(
                rooms.execution_history("sched-room").unwrap().len(),
                expected_len,
                "{crash:?}"
            );
        }
    }

    #[test]
    fn pause_blocks_external_orders_but_manual_step_still_trades() {
        let mut rooms = RoomManager::new();
        rooms.create_room(scenario()).unwrap();
        rooms.pause_room("sched-room").unwrap();
        let rejected = rooms
            .apply(
                "sched-room",
                crate::Command::NewOrder(crate::NewOrder {
                    order_id: 1,
                    account_id: 20,
                    side: Side::Buy,
                    kind: crate::OrderKind::Limit { price_tick: 100 },
                    qty: 1,
                    reduce_only: false,
                }),
            )
            .unwrap();
        assert!(matches!(
            rejected.result,
            crate::ActorExecutionResult::Rejected(crate::ActorRejectReason::MarketPaused)
        ));

        let mut next_order_id = 1;
        let state = SchedulerState::new("sched-room", templates(), SchedulerMode::Manual);
        let outcome =
            run_scheduler_step(&mut rooms, &mut next_order_id, state, CrashPoint::None).unwrap();
        assert!(!outcome.crashed);
        assert!(
            rooms
                .execution_history("sched-room")
                .unwrap()
                .iter()
                .any(|execution| matches!(
                    execution.result,
                    crate::ActorExecutionResult::Accepted(_)
                ))
        );
    }
}
