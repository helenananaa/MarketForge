#[test]
fn manual_scheduler_uses_committed_cursors_without_truncating_catch_up() {
    use crate::bots::{BotHistoryCursor, BotMarketDataRequest};
    use std::sync::{Arc, Mutex};
    struct Factory { descriptor: crate::BotDescriptor, seen: Arc<Mutex<Vec<usize>>> }
    struct Bot { state: PersistedAgentKindState, seen: Arc<Mutex<Vec<usize>>> }
    impl crate::ScheduledBot for Bot {
        fn decide(&mut self, observation: &crate::ParticipantObservation) -> Result<Vec<OrderAction>, crate::BotError> {
            let data = observation.bot_market_data.as_ref().unwrap();
            self.seen.lock().unwrap().push(data.candles.len());
            let proof = data.history_proof.as_ref().unwrap();
            let digest = data.candles.iter().fold(proof.prefix_digest().unwrap(),
                |digest, candle| crate::candle_history_append(&digest, candle));
            let PersistedAgentKindState::Plugin { data: state, .. } = &mut self.state else { unreachable!() };
            *state = serde_json::json!({"count":proof.prefix_bars + data.candles.len() as u64,
                "sha256":crate::candle_history_hex(&digest)}).into();
            Ok(vec![])
        }
        fn snapshot(&self) -> PersistedAgentKindState { self.state.clone() }
    }
    impl crate::BotFactory for Factory {
        fn descriptor(&self) -> &crate::BotDescriptor { &self.descriptor }
        fn market_data_request(&self, _: &AgentTemplate) -> Result<Option<BotMarketDataRequest>, crate::BotError> {
            Ok(Some(BotMarketDataRequest { interval_ms: 1000, max_bars: 2049 }))
        }
        fn checkpoint_history_cursors(&self, _: &AgentTemplate, state: &PersistedAgentKindState)
            -> Result<Option<Vec<BotHistoryCursor>>, crate::BotError> {
            let PersistedAgentKindState::Plugin { data, .. } = state else { unreachable!() };
            Ok(data["count"].as_u64().map(|count| vec![BotHistoryCursor {
                instrument_id: "V-BTC-SPOT".into(), interval_ms: 1000, count,
                digest: crate::candle_history_parse(data["sha256"].as_str().unwrap()).unwrap(),
            }]))
        }
        fn create(&self, _: &AgentTemplate, state: &PersistedAgentKindState)
            -> Result<Box<dyn crate::ScheduledBot>, crate::BotError> {
            Ok(Box::new(Bot { state: state.clone(), seen: self.seen.clone() }))
        }
    }
    fn append_bars(rooms: &mut RoomManager, from: u64, to: u64) {
        for index in from..to {
            for (side, account, offset) in [(Side::Sell, 30, 1), (Side::Buy, 20, 2)] {
                rooms.apply("sched-room", crate::Command::NewOrder(crate::NewOrder {
                    position_side: crate::PositionSide::Both, order_id: index * 2 + offset,
                    account_id: account, side, kind: crate::OrderKind::Limit { price_tick: 100 },
                    qty: 1, reduce_only: false,
                })).unwrap();
            }
            rooms.advance_clock("sched-room", 1).unwrap();
        }
    }
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut registry = crate::BotRegistry::with_builtins();
    registry.register(Factory {
        descriptor: crate::BotDescriptor { id: "window-test".into(), name: "Window".into(), version: "1".into(),
            protocol_version: crate::BOT_PROTOCOL_VERSION.into(), state_version: 1, runtime: "native".into(), parameters: Default::default() },
        seen: seen.clone(),
    }).unwrap();
    let mut spec = scenario();
    spec.accounts = vec![
        ScenarioAccount::Spot { account_id: 20, cash_balance: 1_000_000, position_qty: 0 },
        ScenarioAccount::Spot { account_id: 30, cash_balance: 1_000_000, position_qty: 10000 },
    ];
    let mut rooms = RoomManager::new();
    rooms.create_room(spec).unwrap();
    append_bars(&mut rooms, 0, 100);
    let template = AgentTemplate::Plugin(crate::BotConfig {
        participant: participant("window-test", 20), plugin_id: "window-test".into(), plugin_version: "1".into(),
        state_version: 1, config_version: 1, seed: 7, config: serde_json::json!({}),
    });
    let state = SchedulerState::new("sched-room", vec![template], SchedulerMode::Manual);
    let mut next_order = 1000;
    let cold = run_scheduler_step_with_registry(&mut rooms, &mut next_order, state, CrashPoint::None, &registry).unwrap();
    let warm = run_scheduler_step_with_registry(&mut rooms, &mut next_order, cold.state, CrashPoint::None, &registry).unwrap();
    append_bars(&mut rooms, 100, 180);
    let caught_up = run_scheduler_step_with_registry(&mut rooms, &mut next_order, warm.state, CrashPoint::None, &registry).unwrap();
    assert_eq!(*seen.lock().unwrap(), vec![100, 64, 80]);
    let PersistedAgentKindState::Plugin { data, .. } = &caught_up.state.agents[0].kind_state else { unreachable!() };
    assert_eq!(data["count"], 180);
}
