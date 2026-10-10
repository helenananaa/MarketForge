use crate::*;
use serde_json::json;

pub(super) fn scenario(base_rate_ppm: i32) -> ScenarioConfig {
    serde_json::from_value(json!({
        "room_id": "funded", "market": {"Spot": {
            "instrument": {"symbol": "V-USD-SPOT", "base_asset": "V", "quote_asset": "USD", "tick_size": 1, "lot_size": 1},
            "clearing": {"maker_fee_ppm": 0, "taker_fee_ppm": 0}, "risk": {"allow_short": false}
        }}, "extra_markets": [{"Perp": {
            "instrument": {"symbol": "V-USD-PERP", "base_asset": "V", "quote_asset": "USD", "tick_size": 1, "lot_size": 1},
            "clearing": {"maker_fee_ppm": 0, "taker_fee_ppm": 0, "leverage": 10}, "risk": {}, "initial_mark_price_tick": 10000,
            "price_link": {"spot_instrument_id": "V-USD-SPOT", "max_age_ms": 20000},
            "funding": {"interval_ms": 2000, "base_rate_ppm": base_rate_ppm, "max_rate_ppm": 200000}
        }}], "accounts": [
            {"Basic": {"account_id": 10, "cash_balance": 100000}},
            {"Basic": {"account_id": 20, "cash_balance": 100000}},
            {"Spot": {"account_id": 30, "cash_balance": 10000000, "position_qty": 100}}
        ], "seed_orders": [
            {"NewOrder": {"order_id": 1, "account_id": 30, "side": "Buy", "kind": {"Limit": {"price_tick": 9999}}, "qty": 10}},
            {"NewOrder": {"order_id": 2, "account_id": 30, "side": "Sell", "kind": {"Limit": {"price_tick": 10001}}, "qty": 10}}
        ]
    })).unwrap()
}

fn limit(id: u64, account: u64, side: Side, price: i64, qty: u64) -> Command {
    Command::NewOrder(NewOrder {
        position_side: crate::model::PositionSide::Both,
        order_id: id,
        account_id: account,
        side,
        kind: OrderKind::Limit { price_tick: price },
        qty,
        reduce_only: false,
    })
}

fn apply(rooms: &mut RoomManager, command: Command) {
    let execution = rooms
        .apply_to_instrument("funded", "V-USD-PERP", command)
        .unwrap();
    assert!(
        matches!(execution.result, ActorExecutionResult::Accepted(_)),
        "{execution:?}"
    );
}

fn setup(config: ScenarioConfig) -> RoomManager {
    let mut rooms = RoomManager::new();
    rooms.create_room(config).unwrap();
    apply(&mut rooms, limit(3, 10, Side::Sell, 10000, 1));
    apply(&mut rooms, limit(4, 20, Side::Buy, 10000, 1));
    apply(&mut rooms, limit(5, 30, Side::Buy, 9999, 10));
    apply(&mut rooms, limit(6, 30, Side::Sell, 10001, 10));
    rooms
}

fn account(rooms: &RoomManager, id: u64) -> PerpAccountSnapshot {
    let AccountSnapshot::Perp(account) = rooms
        .account_snapshot_for("funded", "V-USD-PERP", id)
        .unwrap()
        .unwrap()
    else {
        panic!("perp")
    };
    account
}

fn settlements(rooms: &RoomManager) -> Vec<FundingSettlement> {
    rooms
        .execution_history("funded")
        .unwrap()
        .iter()
        .filter_map(|execution| execution.funding_settlement.clone())
        .collect()
}

#[test]
fn funding_config_is_opt_in_and_requires_a_link_and_whole_clock_intervals() {
    for invalid in [
        FundingConfig {
            interval_ms: 0,
            ..Default::default()
        },
        FundingConfig {
            interval_ms: 1500,
            ..Default::default()
        },
        FundingConfig {
            max_rate_ppm: 1000001,
            ..Default::default()
        },
        FundingConfig {
            base_rate_ppm: i32::MIN,
            ..Default::default()
        },
    ] {
        let mut config = scenario(100);
        let MarketConfig::Perp(perp) = &mut config.extra_markets[0] else {
            unreachable!()
        };
        perp.funding = Some(invalid);
        assert!(RoomManager::new().create_room(config).is_err());
    }
    let mut config = scenario(100);
    let MarketConfig::Perp(perp) = &mut config.extra_markets[0] else {
        unreachable!()
    };
    perp.price_link = None;
    assert!(RoomManager::new().create_room(config).is_err());
    let mut config = scenario(100);
    let MarketConfig::Perp(perp) = &mut config.extra_markets[0] else {
        unreachable!()
    };
    perp.funding = None;
    let mut rooms = setup(config);
    rooms.advance_clock("funded", 4).unwrap();
    assert!(settlements(&rooms).is_empty());
    assert_eq!(account(&rooms, 20).funding_pnl, 0);
}

#[test]
fn funding_positive_negative_and_zero_rates_transfer_cash_only_at_boundaries() {
    for rate in [10000, -10000, 0] {
        let mut rooms = setup(scenario(rate));
        rooms.advance_clock("funded", 1).unwrap();
        assert!(settlements(&rooms).is_empty());
        assert_eq!(account(&rooms, 20).funding_pnl, 0);
        rooms.advance_clock("funded", 1).unwrap();
        let long = account(&rooms, 20);
        let short = account(&rooms, 10);
        let delta = i128::from(rate) / 100;
        assert_eq!(long.cash_balance, 100000 - delta);
        assert_eq!(short.cash_balance, 100000 + delta);
        assert_eq!(long.funding_pnl, -delta);
        assert_eq!(short.funding_pnl, delta);
        assert_eq!((long.position_qty, short.position_qty), (1, -1));
        assert_eq!((long.realized_pnl, long.fees_paid), (0, 0));
        assert_eq!(settlements(&rooms)[0].status, FundingStatus::Settled);
        assert_eq!(settlements(&rooms)[0].total_transfer, delta.abs());
        let cash = rooms
            .simulation_room("funded")
            .unwrap()
            .primary_exchange()
            .venue_balance_snapshot(20, "USD")
            .unwrap();
        assert_eq!(cash.total, long.cash_balance);
    }
}

#[test]
fn funding_averages_premium_over_time_then_clamps_and_samples_before_actions() {
    let mut config = scenario(0);
    let MarketConfig::Perp(perp) = &mut config.extra_markets[0] else {
        unreachable!()
    };
    perp.funding.as_mut().unwrap().max_rate_ppm = 15000;
    let mut rooms = setup(config);
    rooms.advance_clock("funded", 1).unwrap();
    apply(
        &mut rooms,
        Command::CancelOrder(CancelOrder { order_id: 5 }),
    );
    apply(
        &mut rooms,
        Command::CancelOrder(CancelOrder { order_id: 6 }),
    );
    apply(&mut rooms, limit(7, 30, Side::Buy, 10399, 10));
    apply(&mut rooms, limit(8, 30, Side::Sell, 10401, 10));
    rooms.advance_clock("funded", 1).unwrap();
    // Average premium is 2%, capped to 1.5%, rather than averaging capped samples.
    assert_eq!(settlements(&rooms)[0].rate_ppm, 15000);
    assert_eq!(account(&rooms, 20).funding_pnl, -150);
    let price = rooms
        .simulation_room("funded")
        .unwrap()
        .perp_price_snapshot("V-USD-PERP")
        .unwrap()
        .unwrap();
    assert_eq!(price.funding.unwrap().next_funding_time_ms, 4000);
}

#[test]
fn funding_missing_or_stale_prices_skip_without_deferred_charges() {
    let mut rooms = setup(scenario(10000));
    rooms.advance_clock("funded", 1).unwrap();
    apply(
        &mut rooms,
        Command::CancelOrder(CancelOrder { order_id: 6 }),
    );
    rooms.advance_clock("funded", 1).unwrap();
    assert_eq!(settlements(&rooms)[0].status, FundingStatus::SkippedPrices);
    assert_eq!(settlements(&rooms)[0].covered_ms, 1000);
    assert_eq!(account(&rooms, 20).funding_pnl, 0);
    apply(&mut rooms, limit(7, 30, Side::Sell, 10001, 10));
    rooms.advance_clock("funded", 2).unwrap();
    assert_eq!(account(&rooms, 20).funding_pnl, -100);
    let mut config = scenario(10000);
    let MarketConfig::Perp(perp) = &mut config.extra_markets[0] else {
        unreachable!()
    };
    perp.price_link.as_mut().unwrap().max_age_ms = 1500;
    let mut stale = setup(config);
    stale.advance_clock("funded", 2).unwrap();
    assert_eq!(settlements(&stale)[0].status, FundingStatus::SkippedPrices);
    assert_eq!(account(&stale, 20).funding_pnl, 0);
}

#[test]
fn funding_rounding_preserves_cash_and_detects_unbalanced_positions_and_overflow() {
    let snapshot = |id, qty| {
        PerpAccount {
            hedge_positions: None,
            account_id: id,
            cash_balance: 1000,
            position_qty: qty,
            avg_entry_price_tick: 100,
            realized_pnl: 0,
            fees_paid: 0,
            funding_pnl: 0,
            reserved_margin: 0,
        }
        .snapshot(PerpClearingConfig::default(), 100)
    };
    let mut receipt = FundingSettlement {
        instrument_id: "P".into(),
        funding_time_ms: 2000,
        interval_ms: 2000,
        covered_ms: 2000,
        rate_ppm: 10000,
        mark_price_tick: 150,
        status: FundingStatus::Settled,
        total_transfer: 0,
    };
    let allocations = crate::funding::funding_allocations(
        &[
            snapshot(1, 3),
            snapshot(2, -1),
            snapshot(3, -1),
            snapshot(4, -1),
        ],
        &mut receipt,
    )
    .unwrap();
    assert_eq!(receipt.total_transfer, 4);
    assert_eq!(
        allocations.into_iter().collect::<Vec<_>>(),
        vec![(1, -4), (2, 2), (3, 1), (4, 1)]
    );
    receipt.status = FundingStatus::Settled;
    receipt.total_transfer = 0;
    assert!(
        crate::funding::funding_allocations(&[snapshot(1, 1)], &mut receipt)
            .unwrap()
            .is_empty()
    );
    assert_eq!(receipt.status, FundingStatus::UnbalancedPositions);
    receipt.status = FundingStatus::Settled;
    let mut huge = snapshot(1, 1);
    huge.position_qty = i128::MAX;
    let mut opposite = snapshot(2, -1);
    opposite.position_qty = -i128::MAX;
    assert!(crate::funding::funding_allocations(&[huge, opposite], &mut receipt).is_err());
}

#[test]
fn funding_snapshot_restores_accumulation_and_does_not_repeat_a_settlement() {
    let mut original = setup(scenario(10000));
    original.advance_clock("funded", 1).unwrap();
    let room = original.simulation_room("funded").unwrap();
    let saved: SimulationRoom = serde_json::from_slice(&serde_json::to_vec(room).unwrap()).unwrap();
    let mut restored = RoomManager::new();
    restored
        .restore_simulation_room(
            saved,
            original.execution_history("funded").unwrap().to_vec(),
        )
        .unwrap();
    original.advance_clock("funded", 3).unwrap();
    restored.advance_clock("funded", 3).unwrap();
    assert_eq!(account(&restored, 20), account(&original, 20));
    assert_eq!(settlements(&restored), settlements(&original));
    assert_eq!(account(&restored, 20).funding_pnl, -200);
    restored.advance_clock("funded", 0).unwrap();
    assert_eq!(settlements(&restored).len(), 2);
}

#[test]
fn funding_clock_overflow_rolls_back_cash_clock_and_history() {
    let mut config = scenario(10000);
    config.accounts[0] = ScenarioAccount::Basic {
        account_id: 10,
        cash_balance: i128::MAX,
    };
    let mut rooms = setup(config);
    let before = rooms.clock("funded").unwrap();
    let history_len = rooms.execution_history("funded").unwrap().len();
    assert!(rooms.advance_clock("funded", 2).is_err());
    assert_eq!(rooms.clock("funded").unwrap(), before);
    assert_eq!(
        rooms.execution_history("funded").unwrap().len(),
        history_len
    );
    assert_eq!(account(&rooms, 10).cash_balance, i128::MAX);
    assert_eq!(account(&rooms, 20).funding_pnl, 0);
}

#[test]
fn funding_can_trigger_liquidation_and_batched_clock_matches_single_steps() {
    let mut config = scenario(100000);
    config.accounts[1] = ScenarioAccount::Basic {
        account_id: 20,
        cash_balance: 1000,
    };
    let mut batch = setup(config);
    let mut singles = batch.clone();
    batch.advance_clock("funded", 4).unwrap();
    for _ in 0..4 {
        singles.advance_clock("funded", 1).unwrap();
    }
    assert_eq!(account(&batch, 20).position_qty, 0);
    assert_eq!(account(&batch, 20).funding_pnl, -1000);
    assert_eq!(settlements(&batch)[1].status, FundingStatus::Settled);
    // The liquidation buyer takes over the position; the closed account pays no more.
    assert_eq!(account(&batch, 30).funding_pnl, -1000);
    assert_eq!(account(&batch, 20), account(&singles, 20));
    assert_eq!(
        batch.execution_history("funded").unwrap(),
        singles.execution_history("funded").unwrap()
    );
    assert!(batch.execution_history("funded").unwrap().iter().any(|execution| {
        matches!(&execution.result, ActorExecutionResult::Accepted(MarketExecution::Perp(result)) if result.clearing_events.iter().any(|event| matches!(event, PerpClearingEvent::LiquidationSettled { account_id: 20, .. })))
    }));
}

#[test]
fn funding_system_command_cannot_be_submitted_by_a_trader() {
    let mut rooms = setup(scenario(10000));
    rooms.advance_clock("funded", 2).unwrap();
    let receipt = settlements(&rooms).remove(0);
    let before = account(&rooms, 20);
    let rejected = rooms
        .apply_to_instrument("funded", "V-USD-PERP", Command::SettleFunding(receipt))
        .unwrap();
    assert_eq!(
        rejected.result,
        ActorExecutionResult::Rejected(ActorRejectReason::FundingManaged)
    );
    assert_eq!(account(&rooms, 20), before);
}

#[test]
fn funding_draws_margin_without_allowing_more_risk_and_releases_closed_exposure() {
    let mut config = scenario(10000);
    config.accounts[1] = ScenarioAccount::Basic {
        account_id: 20,
        cash_balance: 1000,
    };
    let mut rooms = setup(config);
    rooms.advance_clock("funded", 2).unwrap();
    let long = account(&rooms, 20);
    assert_eq!(long.cash_balance, 900);
    assert_eq!(long.initial_margin, 1000);
    assert_eq!(long.margin_status, PerpMarginStatus::MarginCall);
    let balance = rooms
        .simulation_room("funded")
        .unwrap()
        .primary_exchange()
        .venue_balance_snapshot(20, "USD")
        .unwrap();
    assert_eq!(
        (balance.total, balance.reserved, balance.available),
        (900, 900, 0)
    );
    let attempt = rooms
        .apply_to_instrument("funded", "V-USD-PERP", limit(7, 20, Side::Buy, 10001, 1))
        .unwrap();
    assert!(
        matches!(attempt.result, ActorExecutionResult::Rejected(_))
            || matches!(&attempt.result, ActorExecutionResult::Accepted(MarketExecution::Perp(result)) if result.events.iter().any(|event| matches!(event.event, Event::RiskRejected { .. })))
    );
    assert_eq!(account(&rooms, 20).position_qty, 1);
    let mut reduce = limit(8, 20, Side::Sell, 9999, 1);
    if let Command::NewOrder(order) = &mut reduce {
        order.reduce_only = true;
        order.kind = OrderKind::Market;
    }
    apply(&mut rooms, reduce);
    assert_eq!(account(&rooms, 20).position_qty, 0);
    let restored: SimulationRoom = serde_json::from_slice(
        &serde_json::to_vec(rooms.simulation_room("funded").unwrap()).unwrap(),
    )
    .unwrap();
    let mut recovered = RoomManager::new();
    recovered
        .restore_simulation_room(restored, Vec::new())
        .unwrap();
    assert_eq!(account(&recovered, 20), account(&rooms, 20));
}

#[test]
fn funding_debt_reaches_existing_bankruptcy_waterfall_after_liquidation() {
    let mut config = scenario(500000);
    let MarketConfig::Perp(perp) = &mut config.extra_markets[0] else {
        unreachable!()
    };
    perp.funding.as_mut().unwrap().max_rate_ppm = 1000000;
    config.accounts[1] = ScenarioAccount::Basic {
        account_id: 20,
        cash_balance: 1000,
    };
    let mut rooms = setup(config);
    rooms.advance_clock("funded", 2).unwrap();
    assert_eq!(account(&rooms, 20).funding_pnl, -5000);
    assert_eq!(account(&rooms, 20).position_qty, 0);
    assert_eq!(account(&rooms, 20).cash_balance, 0);
    assert!(rooms.execution_history("funded").unwrap().iter().any(|execution| {
        matches!(&execution.result, ActorExecutionResult::Accepted(MarketExecution::Perp(result)) if result.clearing_events.iter().any(|event| matches!(event, PerpClearingEvent::LiquidationSettled { bad_debt: 4001, .. })))
    }));
}

#[test]
fn funding_without_positions_records_an_empty_period_without_cash_movement() {
    let mut rooms = RoomManager::new();
    rooms.create_room(scenario(10000)).unwrap();
    rooms.advance_clock("funded", 2).unwrap();
    assert_eq!(settlements(&rooms)[0].status, FundingStatus::NoPositions);
    assert_eq!(account(&rooms, 20).funding_pnl, 0);
}

#[test]
fn funding_coverage_threshold_excludes_gaps_and_requires_live_boundary_prices() {
    for (minimum, missing_last, expected) in [
        (900000, false, FundingStatus::Settled),
        (1000000, false, FundingStatus::SkippedPrices),
        (900000, true, FundingStatus::SkippedPrices),
    ] {
        let mut config = scenario(10000);
        let MarketConfig::Perp(perp) = &mut config.extra_markets[0] else {
            unreachable!()
        };
        let funding = perp.funding.as_mut().unwrap();
        funding.interval_ms = 10000;
        funding.min_coverage_ppm = minimum;
        let mut rooms = setup(config);
        if missing_last {
            rooms.advance_clock("funded", 9).unwrap();
            apply(
                &mut rooms,
                Command::CancelOrder(CancelOrder { order_id: 6 }),
            );
            rooms.advance_clock("funded", 1).unwrap();
        } else {
            apply(
                &mut rooms,
                Command::CancelOrder(CancelOrder { order_id: 6 }),
            );
            rooms.advance_clock("funded", 1).unwrap();
            apply(&mut rooms, limit(7, 30, Side::Sell, 10001, 10));
            rooms.advance_clock("funded", 9).unwrap();
        }
        let receipt = &settlements(&rooms)[0];
        assert_eq!(receipt.covered_ms, 9000);
        assert_eq!(receipt.rate_ppm, 10000); // missing second did not dilute the mean
        assert_eq!(receipt.status, expected);
        assert_eq!(
            account(&rooms, 20).funding_pnl,
            if expected == FundingStatus::Settled {
                -100
            } else {
                0
            }
        );
    }
    for minimum in [0, 1000001] {
        assert!(
            !FundingConfig {
                min_coverage_ppm: minimum,
                ..Default::default()
            }
            .is_valid()
        );
    }
}

#[test]
fn funding_wide_accumulators_and_cashflows_roundtrip_through_json_values() {
    let config = FundingConfig::default();
    let mut state = crate::funding::FundingState::new(&config);
    state.rate_time_sum = i128::MAX;
    let value = serde_json::to_value(&state).unwrap();
    assert_eq!(value["rate_time_sum"], i128::MAX.to_string());
    let restored: crate::funding::FundingState = serde_json::from_value(value).unwrap();
    assert_eq!(restored.rate_time_sum, i128::MAX);
    let mut rooms = setup(scenario(10000));
    rooms.advance_clock("funded", 2).unwrap();
    let mut receipt = settlements(&rooms).remove(0);
    receipt.total_transfer = i128::MAX;
    let restored: FundingSettlement =
        serde_json::from_value(serde_json::to_value(&receipt).unwrap()).unwrap();
    assert_eq!(restored, receipt);
    let mut long = account(&rooms, 20);
    long.funding_pnl = i128::MIN;
    let restored: PerpAccountSnapshot =
        serde_json::from_value(serde_json::to_value(&long).unwrap()).unwrap();
    assert_eq!(restored, long);
}
