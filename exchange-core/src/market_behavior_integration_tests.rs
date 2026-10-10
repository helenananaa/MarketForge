use crate::*;

fn recipe() -> population::BackgroundMarket {
    serde_json::from_str(include_str!("../../scripts/fixtures/behavior_market.json")).unwrap()
}

#[test]
fn checkpoint_recent_trades_restore_without_duplicates_across_replay_and_new_trades() {
    let spec = recipe();
    let mut rooms = RoomManager::new();
    rooms.create_room(spec.scenario).unwrap();
    let mut scheduler = SchedulerState::new("behavior-market", spec.agents, SchedulerMode::Manual);
    let mut order_id = 100000;
    for _ in 0..60 {
        scheduler = run_scheduler_step(&mut rooms, &mut order_id, scheduler, CrashPoint::None)
            .unwrap()
            .state;
    }
    let history = rooms.execution_history("behavior-market").unwrap().to_vec();
    let mut receipts = Vec::new();
    for execution in &history {
        let ActorExecutionResult::Accepted(market) = &execution.result else {
            continue;
        };
        let events = match market {
            MarketExecution::Spot(m) => &m.events,
            MarketExecution::Perp(m) => &m.events,
        };
        for event in events {
            if let Event::TradePrinted(trade) = &event.event {
                receipts.push(crate::observation::BotTradeReceipt {
                    instrument_id: execution.instrument_id.clone(),
                    market_time_ms: execution.market_time_ms,
                    trade: trade.clone(),
                    buyer_fee: None,
                    seller_fee: None,
                });
            }
        }
    }
    for tail in [vec![], history[history.len().saturating_sub(8)..].to_vec()] {
        let mut restored = RoomManager::new();
        restored
            .restore_simulation_room(
                rooms.simulation_room("behavior-market").unwrap().clone(),
                tail,
            )
            .unwrap();
        // Public trade fields remain usable even if fee history is incomplete.
        restored.restore_bot_history("behavior-market", receipts.clone(), false);
        for instrument in ["V-BTC-SPOT", "V-BTC-PERP"] {
            let expected = rooms
                .participant_observation("behavior-market", instrument, 400)
                .unwrap();
            assert!(!expected.public_trades.is_empty());
            let actual = restored
                .participant_observation("behavior-market", instrument, 400)
                .unwrap();
            assert_eq!(actual, expected);
            let mut ids = actual
                .public_trades
                .iter()
                .map(|t| t.trade_id)
                .collect::<Vec<_>>();
            ids.sort();
            ids.dedup();
            assert_eq!(ids.len(), actual.public_trades.len());
        }
        let mut original_next = rooms.clone();
        for manager in [&mut original_next, &mut restored] {
            manager
                .apply_to_instrument(
                    "behavior-market",
                    "V-BTC-SPOT",
                    order(900000, Side::Buy, 1000, 1),
                )
                .unwrap();
        }
        assert_eq!(
            restored
                .participant_observation("behavior-market", "V-BTC-SPOT", 400)
                .unwrap(),
            original_next
                .participant_observation("behavior-market", "V-BTC-SPOT", 400)
                .unwrap()
        );
    }
}

fn position(rooms: &RoomManager, instrument: &str, account: u64) -> i128 {
    match rooms
        .account_snapshot_for("behavior-market", instrument, account)
        .unwrap()
        .unwrap()
    {
        AccountSnapshot::Spot(a) => a.position_qty,
        AccountSnapshot::Perp(a) => a.position_qty,
    }
}

fn order(id: u64, side: Side, price: i64, qty: u64) -> Command {
    Command::NewOrder(NewOrder {
        position_side: crate::model::PositionSide::Both,
        order_id: id,
        account_id: 200,
        side,
        kind: OrderKind::Limit { price_tick: price },
        qty,
        reduce_only: false,
    })
}

fn arbitrage_setup(depth: u64) -> (RoomManager, SchedulerState, u64) {
    let mut spec = recipe();
    spec.agents.retain(|a| a.bot_id() == "BasisArbitrageTrader");
    spec.scenario.routed_seed_orders = vec![
        ScenarioSeedOrder {
            instrument_id: Some("V-BTC-PERP".into()),
            command: order(910, Side::Buy, 110, depth),
        },
        ScenarioSeedOrder {
            instrument_id: Some("V-BTC-PERP".into()),
            command: order(911, Side::Sell, 112, 20),
        },
    ];
    let mut rooms = RoomManager::new();
    rooms.create_room(spec.scenario).unwrap();
    (
        rooms,
        SchedulerState::new("behavior-market", spec.agents, SchedulerMode::Manual),
        100000,
    )
}

#[test]
fn shared_events_are_hidden_before_publication_and_survive_full_room_restore() {
    let mut rooms = RoomManager::new();
    rooms.create_room(recipe().scenario).unwrap();
    let before = rooms
        .participant_observation("behavior-market", "V-BTC-SPOT", 20)
        .unwrap();
    assert!(before.market_events.is_empty());
    rooms.advance_clock("behavior-market", 45).unwrap();
    let visible = rooms
        .participant_observation("behavior-market", "V-BTC-SPOT", 20)
        .unwrap();
    assert_eq!(visible.market_events.len(), 1);
    assert_eq!(visible.market_events[0].id, "news-up");
    assert!(
        rooms
            .participant_observation("behavior-market", "V-BTC-PERP", 20)
            .unwrap()
            .market_events
            .is_empty()
    );
    let saved = serde_json::to_vec(rooms.simulation_room("behavior-market").unwrap()).unwrap();
    let mut restored = RoomManager::new();
    restored
        .restore_simulation_room(
            serde_json::from_slice(&saved).unwrap(),
            rooms.execution_history("behavior-market").unwrap().to_vec(),
        )
        .unwrap();
    assert_eq!(
        visible,
        restored
            .participant_observation("behavior-market", "V-BTC-SPOT", 20)
            .unwrap()
    );
    restored.advance_clock("behavior-market", 115).unwrap();
    assert_eq!(
        restored
            .participant_observation("behavior-market", "V-BTC-SPOT", 20)
            .unwrap()
            .market_events
            .len(),
        2
    );
    let mut invalid = recipe().scenario;
    invalid.market_events.push(invalid.market_events[0].clone());
    assert!(RoomManager::new().create_room(invalid).is_err());
}

#[test]
fn pov_volume_is_authoritative_and_excludes_both_sides_of_own_trades() {
    let mut rooms = RoomManager::new();
    rooms.create_room(recipe().scenario).unwrap();
    for (id, account, qty) in [(80001, 20, 5), (80002, 30, 3)] {
        rooms
            .apply_to_instrument_from(
                "behavior-market",
                "V-BTC-SPOT",
                Command::NewOrder(NewOrder {
                    position_side: crate::model::PositionSide::Both,
                    order_id: id,
                    account_id: account,
                    side: Side::Buy,
                    kind: OrderKind::Market,
                    qty,
                    reduce_only: false,
                }),
                CommandOrigin::Scheduler,
            )
            .unwrap();
    }
    let request = Some(bots::BotMarketDataRequest {
        interval_ms: 1000,
        max_bars: 4096,
    });
    let observation = rooms
        .bot_observation("behavior-market", "V-BTC-SPOT", 20, request)
        .unwrap();
    let data = observation.bot_market_data.unwrap();
    assert_eq!(data.external_volume_qty, "3");
    assert_eq!(data.own_fills.iter().map(|t| t.qty).sum::<u64>(), 5);
    let maker = rooms
        .bot_observation("behavior-market", "V-BTC-SPOT", 10, request)
        .unwrap();
    assert_eq!(maker.bot_market_data.unwrap().external_volume_qty, "0");
    let unrelated = rooms
        .bot_observation("behavior-market", "V-BTC-SPOT", 420, request)
        .unwrap();
    assert_eq!(unrelated.bot_market_data.unwrap().external_volume_qty, "8");
}

#[test]
fn actual_two_leg_fills_converge_then_unwind_when_basis_closes() {
    let (mut rooms, mut state, mut id) = arbitrage_setup(20);
    for _ in 0..8 {
        state = run_scheduler_step(&mut rooms, &mut id, state, CrashPoint::None)
            .unwrap()
            .state;
    }
    assert_eq!(position(&rooms, "V-BTC-SPOT", 400), 10);
    assert_eq!(position(&rooms, "V-BTC-PERP", 400), -10);
    for command in [
        Command::CancelOrder(CancelOrder { order_id: 910 }),
        Command::CancelOrder(CancelOrder { order_id: 911 }),
        order(912, Side::Buy, 95, 20),
        order(913, Side::Sell, 97, 20),
    ] {
        rooms
            .apply_to_instrument_from(
                "behavior-market",
                "V-BTC-PERP",
                command,
                CommandOrigin::Scheduler,
            )
            .unwrap();
    }
    for _ in 0..15 {
        state = run_scheduler_step(&mut rooms, &mut id, state, CrashPoint::None)
            .unwrap()
            .state;
    }
    assert_eq!(position(&rooms, "V-BTC-SPOT", 400), 0);
    assert_eq!(position(&rooms, "V-BTC-PERP", 400), 0);
    assert!(
        rooms
            .execution_history("behavior-market")
            .unwrap()
            .iter()
            .all(|e| !matches!(e.result, ActorExecutionResult::Rejected(_)))
    );
}

#[test]
fn actual_partial_hedge_and_no_depth_unwind_without_inventing_fills() {
    let (mut rooms, mut state, mut id) = arbitrage_setup(20);
    state = run_scheduler_step(&mut rooms, &mut id, state, CrashPoint::None)
        .unwrap()
        .state;
    rooms
        .apply_to_instrument_from(
            "behavior-market",
            "V-BTC-PERP",
            Command::CancelOrder(CancelOrder { order_id: 910 }),
            CommandOrigin::Scheduler,
        )
        .unwrap();
    rooms
        .apply_to_instrument_from(
            "behavior-market",
            "V-BTC-PERP",
            order(914, Side::Buy, 110, 1),
            CommandOrigin::Scheduler,
        )
        .unwrap();
    for _ in 0..2 {
        state = run_scheduler_step(&mut rooms, &mut id, state, CrashPoint::None)
            .unwrap()
            .state;
    }
    assert_eq!(position(&rooms, "V-BTC-SPOT", 400), 2);
    assert_eq!(position(&rooms, "V-BTC-PERP", 400), -1);
    for _ in 0..20 {
        state = run_scheduler_step(&mut rooms, &mut id, state, CrashPoint::None)
            .unwrap()
            .state;
    }
    assert_eq!(position(&rooms, "V-BTC-SPOT", 400), 0);
    assert_eq!(position(&rooms, "V-BTC-PERP", 400), 0);
}

fn canonical(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if key == "seen_order_ids"
                    && let Some(a) = v.as_array_mut()
                {
                    a.sort_by_key(|x| x.as_u64());
                }
                canonical(v);
            }
        }
        serde_json::Value::Array(a) => {
            for v in a {
                canonical(v);
            }
        }
        _ => {}
    }
}

#[test]
fn pair_recovers_after_saved_decision_and_after_first_leg_submission() {
    let (mut baseline, state, id) = arbitrage_setup(20);
    let mut baseline_state = state.clone();
    let mut baseline_id = id;
    for _ in 0..10 {
        baseline_state = run_scheduler_step(
            &mut baseline,
            &mut baseline_id,
            baseline_state,
            CrashPoint::None,
        )
        .unwrap()
        .state;
    }
    for crash in [
        CrashPoint::AfterDecisionPersist {
            step: 1,
            participant_index: 1,
        },
        CrashPoint::AfterActionSubmit {
            step: 1,
            participant_index: 1,
            action_index: 0,
        },
        CrashPoint::AfterActionSubmit {
            step: 2,
            participant_index: 0,
            action_index: 0,
        },
    ] {
        let (mut rooms, mut state, mut order_id) = arbitrage_setup(20);
        let crash_step = match crash {
            CrashPoint::AfterDecisionPersist { step, .. }
            | CrashPoint::AfterActionSubmit { step, .. } => step,
            _ => unreachable!(),
        };
        for _ in 1..crash_step {
            state = run_scheduler_step(&mut rooms, &mut order_id, state, CrashPoint::None)
                .unwrap()
                .state;
        }
        let outcome = run_scheduler_step(&mut rooms, &mut order_id, state, crash).unwrap();
        assert!(outcome.crashed);
        let saved_room =
            serde_json::to_vec(rooms.simulation_room("behavior-market").unwrap()).unwrap();
        let saved_state = serde_json::to_vec(&outcome.state).unwrap();
        let mut restored = RoomManager::new();
        restored
            .restore_simulation_room(
                serde_json::from_slice(&saved_room).unwrap(),
                rooms.execution_history("behavior-market").unwrap().to_vec(),
            )
            .unwrap();
        state = run_scheduler_step(
            &mut restored,
            &mut order_id,
            serde_json::from_slice(&saved_state).unwrap(),
            CrashPoint::None,
        )
        .unwrap()
        .state;
        for _ in crash_step..10 {
            state = run_scheduler_step(&mut restored, &mut order_id, state, CrashPoint::None)
                .unwrap()
                .state;
        }
        assert_eq!(state, baseline_state);
        assert_eq!(order_id, baseline_id);
        assert_eq!(
            restored.execution_history("behavior-market").unwrap(),
            baseline.execution_history("behavior-market").unwrap()
        );
        let mut actual =
            serde_json::to_value(restored.simulation_room("behavior-market").unwrap()).unwrap();
        let mut expected =
            serde_json::to_value(baseline.simulation_room("behavior-market").unwrap()).unwrap();
        canonical(&mut actual);
        canonical(&mut expected);
        assert_eq!(actual, expected);
    }
}

#[test]
fn full_behavior_population_has_real_flows_finite_assets_and_durable_state() {
    let spec = recipe();
    assert_eq!(spec.agents.len(), 33);
    let base_asset = spec.scenario.market.instrument().base_asset.clone();
    let mut rooms = RoomManager::new();
    rooms.create_room(spec.scenario).unwrap();
    let initial = rooms.net_worth_snapshot("behavior-market").unwrap();
    let mut state = SchedulerState::new("behavior-market", spec.agents, SchedulerMode::Manual);
    let mut id = 100000;
    for _ in 0..260 {
        state = run_scheduler_step(&mut rooms, &mut id, state, CrashPoint::None)
            .unwrap()
            .state;
    }
    // Total base inventory is conserved; fees and funding do not mint base assets.
    let base_total = |n: &RoomNetWorthSnapshot| {
        n.accounts
            .iter()
            .flat_map(|a| a.assets.iter())
            .filter(|a| a.asset_id == base_asset)
            .map(|a| a.total)
            .sum::<i128>()
    };
    assert!(base_total(&initial) > 0);
    assert_eq!(
        base_total(&initial),
        base_total(&rooms.net_worth_snapshot("behavior-market").unwrap())
    );
    for a in &state.agents {
        let PersistedAgentKindState::Plugin { data, .. } = &a.kind_state else {
            panic!()
        };
        if a.template.bot_id() == "MarketEventTrader" {
            assert_eq!(data["received_event_ids"].as_array().unwrap().len(), 2);
        }
        if a.template.bot_id() == "PovExecutionTrader" {
            assert!(data["completed_qty"].as_u64().unwrap() > 0);
            assert_eq!(data["deadline_reached"], true);
        }
    }
    for e in rooms.execution_history("behavior-market").unwrap() {
        if let ActorExecutionResult::Accepted(market) = &e.result {
            let events = match market {
                MarketExecution::Spot(m) => &m.events,
                MarketExecution::Perp(m) => &m.events,
            };
            for e in events {
                if let Event::TradePrinted(t) = &e.event {
                    assert_ne!(t.maker_account_id, t.taker_account_id);
                }
            }
        }
    }
}
