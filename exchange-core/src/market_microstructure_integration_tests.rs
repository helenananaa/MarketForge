use crate::*;
use serde_json::json;

fn native(kind: &str, config: serde_json::Value) -> AgentTemplate {
    AgentTemplate::Plugin(BotConfig {
        participant: ParticipantConfig {
            participant_id: "motive".into(),
            kind: ParticipantKind::RuleAgent,
            room_id: "funded".into(),
            account_id: 20,
            instrument_id: Some("V-USD-PERP".into()),
        },
        plugin_id: kind.into(),
        plugin_version: "1".into(),
        state_version: 1,
        config_version: 1,
        seed: 7,
        config,
    })
}
fn account(rooms: &RoomManager, id: u64) -> PerpAccountSnapshot {
    let AccountSnapshot::Perp(account) = rooms
        .account_snapshot_for("funded", "V-USD-PERP", id)
        .unwrap()
        .unwrap()
    else {
        panic!()
    };
    account
}
fn quote(
    rooms: &mut RoomManager,
    instrument: &str,
    id: u64,
    owner: u64,
    side: Side,
    price: i64,
    qty: u64,
) {
    rooms
        .apply_to_instrument_from(
            "funded",
            instrument,
            Command::NewOrder(NewOrder {
                position_side: Default::default(),
                order_id: id,
                account_id: owner,
                side,
                kind: OrderKind::Limit { price_tick: price },
                qty,
                reduce_only: false,
            }),
            CommandOrigin::Scheduler,
        )
        .unwrap();
}
fn cancel(rooms: &mut RoomManager, instrument: &str, id: u64) {
    rooms
        .apply_to_instrument_from(
            "funded",
            instrument,
            Command::CancelOrder(CancelOrder { order_id: id }),
            CommandOrigin::Scheduler,
        )
        .unwrap();
}

#[test]
fn funding_trader_actual_payment_exit_and_saved_partial_submission_recover_exactly() {
    for rate in [20000, -20000] {
        let setup = || {
            let mut scenario = crate::funding_tests::scenario(rate);
            let MarketConfig::Perp(perp) = &mut scenario.extra_markets[0] else {
                panic!()
            };
            perp.funding.as_mut().unwrap().interval_ms = 10000;
            let mut rooms = RoomManager::new();
            rooms.create_room(scenario).unwrap();
            quote(&mut rooms, "V-USD-PERP", 100, 10, Side::Buy, 9999, 10);
            quote(&mut rooms, "V-USD-PERP", 101, 10, Side::Sell, 10001, 10);
            let state = SchedulerState::new(
                "funded",
                vec![native(
                    "FundingRateTrader",
                    json!({
                        "funding_entry_rate_ppm":10000,"funding_exit_rate_ppm":1000,"funding_entry_window_ms":5000,
                        "position_size":2,"max_qty":1,"jitter_ms":0,"fee_buffer_ppm":0,"max_slippage_ticks":2
                    }),
                )],
                SchedulerMode::Manual,
            );
            (rooms, state, 1000)
        };
        let (mut baseline, mut baseline_state, mut baseline_id) = setup();
        for _ in 0..12 {
            baseline_state = run_scheduler_step(
                &mut baseline,
                &mut baseline_id,
                baseline_state,
                CrashPoint::None,
            )
            .unwrap()
            .state;
        }
        assert_eq!(account(&baseline, 20).position_qty, 0);
        assert_eq!(account(&baseline, 20).funding_pnl, 400);
        assert_eq!(account(&baseline, 10).funding_pnl, -400);
        assert_eq!(
            account(&baseline, 10).cash_balance + account(&baseline, 20).cash_balance,
            200000
        );
        for crash in [
            CrashPoint::AfterDecisionPersist {
                step: 5,
                participant_index: 0,
            },
            CrashPoint::AfterActionSubmit {
                step: 5,
                participant_index: 0,
                action_index: 0,
            },
        ] {
            let (mut rooms, mut state, mut id) = setup();
            for _ in 0..4 {
                state = run_scheduler_step(&mut rooms, &mut id, state, CrashPoint::None)
                    .unwrap()
                    .state;
            }
            let outcome = run_scheduler_step(&mut rooms, &mut id, state, crash).unwrap();
            assert!(outcome.crashed);
            let actor = serde_json::from_slice(
                &serde_json::to_vec(rooms.simulation_room("funded").unwrap()).unwrap(),
            )
            .unwrap();
            let mut restored = RoomManager::new();
            restored
                .restore_simulation_room(actor, rooms.execution_history("funded").unwrap().to_vec())
                .unwrap();
            state = serde_json::from_slice(&serde_json::to_vec(&outcome.state).unwrap()).unwrap();
            for _ in 4..12 {
                state = run_scheduler_step(&mut restored, &mut id, state, CrashPoint::None)
                    .unwrap()
                    .state;
            }
            assert_eq!(state, baseline_state);
            assert_eq!(id, baseline_id);
            assert_eq!(
                restored.execution_history("funded").unwrap(),
                baseline.execution_history("funded").unwrap()
            );
            assert_eq!(account(&restored, 20), account(&baseline, 20));
        }
    }
}

#[test]
fn leveraged_trend_opens_with_venue_margin_then_trims_actual_loss_exposure() {
    let mut rooms = RoomManager::new();
    let mut scenario = crate::funding_tests::scenario(0);
    if let ScenarioAccount::Basic { cash_balance, .. } = &mut scenario.accounts[0] {
        *cash_balance = 1000000;
    }
    rooms.create_room(scenario).unwrap();
    quote(&mut rooms, "V-USD-PERP", 100, 10, Side::Buy, 9999, 100);
    quote(&mut rooms, "V-USD-PERP", 101, 10, Side::Sell, 10001, 100);
    let mut state = SchedulerState::new(
        "funded",
        vec![native(
            "LeveragedTrendTrader",
            json!({
                "target_leverage":5,"inventory_cap":100,"position_size":100,"max_qty":40,
                "jitter_ms":0,"signal_threshold_ticks":1,"fee_buffer_ppm":0,"max_slippage_ticks":5
            }),
        )],
        SchedulerMode::Manual,
    );
    let mut id = 1000;
    state = run_scheduler_step(&mut rooms, &mut id, state, CrashPoint::None)
        .unwrap()
        .state;
    for instrument in ["V-USD-SPOT", "V-USD-PERP"] {
        for order in if instrument.ends_with("SPOT") {
            [1, 2]
        } else {
            [100, 101]
        } {
            cancel(&mut rooms, instrument, order);
        }
        let owner = if instrument.ends_with("SPOT") { 30 } else { 10 };
        let first = if owner == 30 { 20 } else { 120 };
        quote(&mut rooms, instrument, first, owner, Side::Buy, 10999, 100);
        quote(
            &mut rooms,
            instrument,
            first + 1,
            owner,
            Side::Sell,
            11001,
            100,
        );
    }
    state = run_scheduler_step(&mut rooms, &mut id, state, CrashPoint::None)
        .unwrap()
        .state;
    assert_eq!(account(&rooms, 20).position_qty, 40);
    for instrument in ["V-USD-SPOT", "V-USD-PERP"] {
        let owner = if instrument.ends_with("SPOT") { 30 } else { 10 };
        let first = if owner == 30 { 20 } else { 120 };
        cancel(&mut rooms, instrument, first);
        cancel(&mut rooms, instrument, first + 1);
        quote(
            &mut rooms,
            instrument,
            first + 10,
            owner,
            Side::Buy,
            9999,
            100,
        );
        quote(
            &mut rooms,
            instrument,
            first + 11,
            owner,
            Side::Sell,
            10001,
            100,
        );
    }
    assert_eq!(account(&rooms, 20).margin_status, PerpMarginStatus::Healthy);
    state = run_scheduler_step(&mut rooms, &mut id, state, CrashPoint::None)
        .unwrap()
        .state;
    let account = account(&rooms, 20);
    assert_eq!(account.position_qty, 29);
    assert!(account.position_qty * 10000 <= account.equity * 5);
    let PersistedAgentKindState::Plugin { data, .. } = &state.agents[0].kind_state else {
        panic!()
    };
    assert_eq!(data["deleverage_decisions"], 1);
    assert_eq!(data["target_position"], "29");
}
