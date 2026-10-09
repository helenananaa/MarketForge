use crate::model::RiskRejectReason;
use crate::*;

fn config() -> PerpClearingConfig {
    PerpClearingConfig {
        position_mode: PositionMode::Hedge,
        leverage: 10,
        ..Default::default()
    }
}

fn engine() -> PerpTradingEngine {
    let mut e = PerpTradingEngine::new(config(), 100).unwrap();
    for id in 1..=8 {
        e.create_account(id, 10_000);
    }
    e
}

fn order(
    id: u64,
    account: u64,
    side: Side,
    leg: PositionSide,
    qty: u64,
    kind: OrderKind,
) -> Command {
    Command::NewOrder(NewOrder {
        order_id: id,
        account_id: account,
        side,
        position_side: leg,
        qty,
        kind,
        reduce_only: false,
    })
}

fn apply(e: &mut PerpTradingEngine, command: Command) -> PerpTradingExecution {
    let result = e.apply(command).unwrap();
    assert!(
        !result.events.iter().any(|r| matches!(
            r.event,
            Event::RiskRejected { .. } | Event::OrderRejected { .. }
        )),
        "{result:?}"
    );
    result
}

fn fill(
    e: &mut PerpTradingEngine,
    id: u64,
    target: (u64, Side, PositionSide),
    qty: u64,
    price: i64,
    maker: u64,
) {
    let (account, side, leg) = target;
    let maker_leg = if side == Side::Buy {
        PositionSide::Short
    } else {
        PositionSide::Long
    };
    apply(
        e,
        order(
            id,
            maker,
            side.opposite(),
            maker_leg,
            qty,
            OrderKind::Limit { price_tick: price },
        ),
    );
    apply(
        e,
        order(
            id + 1,
            account,
            side,
            leg,
            qty,
            OrderKind::ImmediateOrCancel {
                price_tick: Some(price),
            },
        ),
    );
}

fn locked(e: &mut PerpTradingEngine) {
    fill(e, 1, (1, Side::Buy, PositionSide::Long), 5, 100, 2);
    fill(e, 3, (1, Side::Sell, PositionSide::Short), 5, 120, 3);
}

fn reject(e: &mut PerpTradingEngine, command: Command, reason: RiskRejectReason) {
    let before = serde_json::to_value(e.account_snapshots()).unwrap();
    let book = e.snapshot();
    let result = e.apply(command).unwrap();
    assert_eq!(result.events.len(), 1);
    assert!(
        matches!(&result.events[0].event, Event::RiskRejected { reason: actual, .. } if *actual == reason),
        "{result:?}"
    );
    assert_eq!(serde_json::to_value(e.account_snapshots()).unwrap(), before);
    assert_eq!(e.snapshot(), book);
}

#[test]
fn hedge_independent_entries_pnl_and_gross_margin_when_net_is_zero() {
    let mut e = engine();
    locked(&mut e);
    e.set_mark_price_tick(110).unwrap();
    let a = e.account_snapshot(1).unwrap();
    assert_eq!(a.position_qty, 0);
    assert!(a.has_open_position());
    assert_eq!(a.avg_entry_price_tick, 0);
    let p = a.hedge_positions.unwrap();
    assert_eq!((p.long.qty, p.long.avg_entry_price_tick), (5, 100));
    assert_eq!((p.short.qty, p.short.avg_entry_price_tick), (5, 120));
    assert_eq!(
        (a.unrealized_pnl, a.initial_margin, a.maintenance_margin),
        (100, 110, 55)
    );
    assert_eq!(a.margin_status, PerpMarginStatus::Healthy);
}

#[test]
fn hedge_closing_one_leg_never_nets_or_reverses_the_other() {
    let mut e = engine();
    locked(&mut e);
    fill(&mut e, 5, (1, Side::Sell, PositionSide::Long), 3, 110, 4);
    let a = e.account_snapshot(1).unwrap();
    let p = a.hedge_positions.unwrap();
    assert_eq!((p.long.qty, p.short.qty), (2, 5));
    assert_eq!((p.long.realized_pnl, p.short.realized_pnl), (30, 0));
    assert_eq!((a.realized_pnl, a.cash_balance), (30, 10_030));
    fill(&mut e, 7, (1, Side::Buy, PositionSide::Short), 5, 115, 5);
    let a = e.account_snapshot(1).unwrap();
    let p = a.hedge_positions.unwrap();
    assert_eq!(
        (p.long.qty, p.short.qty, p.short.avg_entry_price_tick),
        (2, 0, 0)
    );
    assert_eq!(p.short.realized_pnl, 25);
}

#[test]
fn hedge_direction_is_required_and_cannot_overclose_or_reduce_on_open() {
    let mut e = engine();
    locked(&mut e);
    let ioc = OrderKind::ImmediateOrCancel {
        price_tick: Some(100),
    };
    reject(
        &mut e,
        order(5, 1, Side::Sell, PositionSide::Both, 1, ioc),
        RiskRejectReason::InvalidPositionSide,
    );
    reject(
        &mut e,
        order(6, 1, Side::Sell, PositionSide::Long, 6, ioc),
        RiskRejectReason::ReduceOnlyExceedsPosition,
    );
    let mut command = order(7, 1, Side::Buy, PositionSide::Long, 1, ioc);
    if let Command::NewOrder(o) = &mut command {
        o.reduce_only = true;
    }
    reject(
        &mut e,
        command,
        RiskRejectReason::ReduceOnlyWouldIncreasePosition,
    );
    reject(
        &mut e,
        order(8, 4, Side::Buy, PositionSide::Short, 1, ioc),
        RiskRejectReason::ReduceOnlyWouldIncreasePosition,
    );
    reject(
        &mut e,
        order(
            9,
            1,
            Side::Sell,
            PositionSide::Long,
            1,
            OrderKind::Limit { price_tick: 110 },
        ),
        RiskRejectReason::ReduceOnlyUnsupported,
    );
}

#[test]
fn hedge_gross_limits_and_both_resting_open_sides_reserve_margin() {
    let mut e = PerpTradingEngine::new_with_risk(
        config(),
        100,
        PerpRiskConfig {
            max_abs_position_qty: Some(10),
            ..Default::default()
        },
    )
    .unwrap();
    e.create_account(1, 100);
    apply(
        &mut e,
        order(
            1,
            1,
            Side::Buy,
            PositionSide::Long,
            5,
            OrderKind::Limit { price_tick: 99 },
        ),
    );
    apply(
        &mut e,
        order(
            2,
            1,
            Side::Sell,
            PositionSide::Short,
            5,
            OrderKind::Limit { price_tick: 101 },
        ),
    );
    let a = e.account_snapshot(1).unwrap();
    assert_eq!(a.reserved_margin, 100);
    reject(
        &mut e,
        order(
            3,
            1,
            Side::Sell,
            PositionSide::Short,
            1,
            OrderKind::Limit { price_tick: 101 },
        ),
        RiskRejectReason::MaxPositionExceeded,
    );
    apply(
        &mut e,
        Command::AmendOrder(model::AmendOrder {
            order_id: 2,
            price_tick: None,
            qty: Some(2),
        }),
    );
    assert_eq!(e.account_snapshot(1).unwrap().reserved_margin, 70);
    apply(&mut e, Command::CancelOrder(CancelOrder { order_id: 1 }));
    assert_eq!(e.account_snapshot(1).unwrap().reserved_margin, 20);
}

#[test]
fn hedge_second_leg_requires_margin_even_if_it_reduces_net_exposure() {
    let mut e = engine();
    fill(&mut e, 1, (1, Side::Buy, PositionSide::Long), 5, 100, 2);
    e.sync_cash_balance(1, 60).unwrap();
    reject(
        &mut e,
        order(
            3,
            1,
            Side::Sell,
            PositionSide::Short,
            5,
            OrderKind::Limit { price_tick: 100 },
        ),
        RiskRejectReason::InsufficientMargin,
    );
}

#[test]
fn hedge_close_remains_possible_under_margin_call() {
    let mut e = engine();
    locked(&mut e);
    e.sync_cash_balance(1, 1).unwrap();
    // Locked-in profit is 100, below the 110 initial margin but above maintenance.
    assert_eq!(
        e.account_snapshot(1).unwrap().margin_status,
        PerpMarginStatus::MarginCall
    );
    fill(&mut e, 5, (1, Side::Sell, PositionSide::Long), 1, 100, 4);
    assert_eq!(
        e.account_snapshot(1)
            .unwrap()
            .hedge_positions
            .unwrap()
            .long
            .qty,
        4
    );
}

#[test]
fn hedge_partial_maker_fills_and_restore_preserve_the_selected_leg() {
    let mut e = engine();
    apply(
        &mut e,
        order(
            1,
            1,
            Side::Sell,
            PositionSide::Short,
            8,
            OrderKind::Limit { price_tick: 100 },
        ),
    );
    apply(
        &mut e,
        order(2, 2, Side::Buy, PositionSide::Long, 3, OrderKind::Market),
    );
    let data = serde_json::to_string(&e).unwrap();
    let mut restored: PerpTradingEngine = serde_json::from_str(&data).unwrap();
    let result = apply(
        &mut restored,
        order(3, 3, Side::Buy, PositionSide::Long, 5, OrderKind::Market),
    );
    let trade = result
        .events
        .iter()
        .find_map(|r| {
            if let Event::TradePrinted(t) = &r.event {
                Some(t)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        (trade.maker_position_side, trade.taker_position_side),
        (PositionSide::Short, PositionSide::Long)
    );
    let p = restored
        .account_snapshot(1)
        .unwrap()
        .hedge_positions
        .unwrap();
    assert_eq!((p.long.qty, p.short.qty), (0, 8));
    let mut replay = engine();
    for record in restored.command_log() {
        apply(&mut replay, record.command.clone());
    }
    assert_eq!(replay.account_snapshots(), restored.account_snapshots());
}

fn funding(rate_ppm: i32) -> FundingSettlement {
    FundingSettlement {
        instrument_id: "PERP".into(),
        funding_time_ms: 1_000,
        interval_ms: 1_000,
        covered_ms: 1_000,
        rate_ppm,
        mark_price_tick: 100,
        status: FundingStatus::Settled,
        total_transfer: 0,
    }
}

#[test]
fn hedge_funding_pays_and_receives_on_each_leg_and_conserves_cash() {
    for rate in [100_000, -100_000] {
        let mut e = engine();
        locked(&mut e);
        let before: i128 = e.account_snapshots().iter().map(|a| a.cash_balance).sum();
        let result = apply(&mut e, Command::SettleFunding(funding(rate)));
        let Command::SettleFunding(receipt) = result.command.command else {
            panic!()
        };
        assert_eq!(receipt.status, FundingStatus::Settled);
        assert_eq!(receipt.total_transfer, 100);
        let p = e.account_snapshot(1).unwrap().hedge_positions.unwrap();
        let expected = if rate > 0 { -50 } else { 50 };
        assert_eq!(
            (p.long.funding_pnl, p.short.funding_pnl),
            (expected, -expected)
        );
        assert_eq!(e.account_snapshot(1).unwrap().funding_pnl, 0);
        assert_eq!(
            e.account_snapshots()
                .iter()
                .map(|a| a.cash_balance)
                .sum::<i128>(),
            before
        );
    }
}

#[test]
fn hedge_fully_locked_account_is_not_no_positions_for_funding() {
    let mut e = engine();
    // The same account has both sides, so net position is zero globally.
    fill(&mut e, 1, (1, Side::Buy, PositionSide::Long), 5, 100, 1);
    let result = apply(&mut e, Command::SettleFunding(funding(100_000)));
    let Command::SettleFunding(receipt) = result.command.command else {
        panic!()
    };
    assert_eq!(receipt.status, FundingStatus::Settled);
    assert_eq!(receipt.total_transfer, 50);
    let p = e.account_snapshot(1).unwrap().hedge_positions.unwrap();
    assert_eq!((p.long.funding_pnl, p.short.funding_pnl), (-50, 50));
}

#[test]
fn hedge_liquidation_closes_both_legs_and_resumes_after_partial_depth() {
    let mut e = PerpTradingEngine::new(
        PerpClearingConfig {
            liquidation_fee_ppm: 10_000,
            ..config()
        },
        100,
    )
    .unwrap();
    for id in 1..=5 {
        e.create_account(id, 10_000);
    }
    fill(&mut e, 1, (1, Side::Buy, PositionSide::Long), 5, 100, 2);
    fill(&mut e, 3, (1, Side::Sell, PositionSide::Short), 5, 100, 3);
    e.sync_cash_balance(1, 0).unwrap();
    assert_eq!(e.account_snapshot(1).unwrap().position_qty, 0);
    assert_eq!(
        e.account_snapshot(1).unwrap().margin_status,
        PerpMarginStatus::Liquidatable
    );
    apply(
        &mut e,
        order(
            5,
            4,
            Side::Buy,
            PositionSide::Long,
            2,
            OrderKind::Limit { price_tick: 100 },
        ),
    );
    e.liquidate_account(1, 6).unwrap();
    let p = e.account_snapshot(1).unwrap().hedge_positions.unwrap();
    assert_eq!((p.long.qty, p.short.qty), (3, 5));
    let mut e: PerpTradingEngine =
        serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
    reject(
        &mut e,
        order(7, 1, Side::Buy, PositionSide::Long, 1, OrderKind::Market),
        RiskRejectReason::InsufficientMargin,
    );
    apply(
        &mut e,
        order(
            8,
            4,
            Side::Buy,
            PositionSide::Long,
            3,
            OrderKind::Limit { price_tick: 100 },
        ),
    );
    e.liquidate_account(1, 9).unwrap();
    assert!(e.pending_liquidation(1).is_some());
    apply(
        &mut e,
        order(
            10,
            5,
            Side::Sell,
            PositionSide::Short,
            5,
            OrderKind::Limit { price_tick: 100 },
        ),
    );
    let final_execution = e.liquidate_account(1, 11).unwrap();
    assert!(e.pending_liquidation(1).is_none());
    let a = e.account_snapshot(1).unwrap();
    assert!(!a.has_open_position(), "{a:?}");
    assert_eq!(a.fees_paid, 10);
    assert_eq!(a.margin_status, PerpMarginStatus::Flat);
    assert!(final_execution.clearing_events.iter().any(|event| matches!(
        event,
        PerpClearingEvent::LiquidationSettled {
            liquidation_notional: 1_000,
            liquidation_fee: 10,
            ..
        }
    )));
}

#[test]
fn hedge_legacy_json_defaults_to_one_way_and_keeps_wire_shape() {
    let value =
        serde_json::json!({"order_id":1,"account_id":1,"side":"Buy","kind":"Market","qty":1});
    let o: NewOrder = serde_json::from_value(value).unwrap();
    assert_eq!(o.position_side, PositionSide::Both);
    assert!(
        serde_json::to_value(o)
            .unwrap()
            .get("position_side")
            .is_none()
    );
    let cfg: PerpClearingConfig = serde_json::from_value(
        serde_json::json!({"maker_fee_ppm":0,"taker_fee_ppm":0,"leverage":10}),
    )
    .unwrap();
    assert_eq!(cfg.position_mode, PositionMode::OneWay);
    assert!(
        serde_json::to_value(cfg)
            .unwrap()
            .get("position_mode")
            .is_none()
    );
    let mut e = PerpTradingEngine::new(cfg, 100).unwrap();
    e.create_account(1, 10_000);
    reject(
        &mut e,
        order(
            1,
            1,
            Side::Buy,
            PositionSide::Long,
            1,
            OrderKind::Limit { price_tick: 100 },
        ),
        RiskRejectReason::InvalidPositionSide,
    );
    let a = e.account_snapshot(1).unwrap();
    assert!(a.hedge_positions.is_none());
    assert!(
        serde_json::to_value(&a)
            .unwrap()
            .get("hedge_positions")
            .is_none()
    );
    assert_eq!(
        serde_json::from_value::<PerpAccountSnapshot>(serde_json::to_value(&a).unwrap()).unwrap(),
        a
    );
}

#[test]
fn hedge_room_gateway_and_cross_margin_keep_locked_positions_visible() {
    let scenario: ScenarioConfig = serde_json::from_value(serde_json::json!({
        "room_id":"hedge", "seed":1,
        "market":{"Perp":{"instrument":{"symbol":"PERP","tick_size":1,"lot_size":1},
            "clearing":{"maker_fee_ppm":0,"taker_fee_ppm":0,"leverage":10,"position_mode":"Hedge"},
            "risk":{},"initial_mark_price_tick":100}},
        "accounts":[{"Basic":{"account_id":1,"cash_balance":1000}}, {"Basic":{"account_id":2,"cash_balance":1000}}],
        "seed_orders":[]
    })).unwrap();
    let mut rooms = RoomManager::new();
    rooms.create_room(scenario).unwrap();
    let mut gateway = OrderGateway::new(&mut rooms, 100);
    for (account_id, side, position_side) in [
        (1, Side::Sell, PositionSide::Short),
        (2, Side::Buy, PositionSide::Long),
    ] {
        let request = GatewayRequest {
            participant_id: "test".into(),
            room_id: "hedge".into(),
            instrument_id: None,
            account_id,
            action: OrderAction::PlaceProtected {
                side,
                position_side,
                qty: 5,
                price_tick: 100,
                order_type: model::ProtectedOrderType::Limit,
                reduce_only: false,
                valid_until_market_time_ms: None,
                expires_at_market_time_ms: None,
            },
        };
        gateway.submit_action(request).unwrap();
    }
    let AccountSnapshot::Perp(a) = rooms
        .account_snapshot_for("hedge", "PERP", 1)
        .unwrap()
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(a.hedge_positions.unwrap().short.qty, 5);
}

#[test]
fn hedge_adl_selects_profitable_leg_and_preserves_its_other_leg() {
    let mut e = PerpTradingEngine::new(
        PerpClearingConfig {
            auto_deleveraging_enabled: true,
            ..config()
        },
        100,
    )
    .unwrap();
    for id in 1..=5 {
        e.create_account(id, 10_000);
    }
    fill(&mut e, 1, (1, Side::Buy, PositionSide::Long), 10, 100, 2);
    fill(&mut e, 3, (2, Side::Buy, PositionSide::Long), 5, 60, 4);
    e.sync_cash_balance(1, 10).unwrap();
    e.set_mark_price_tick(50).unwrap();
    apply(
        &mut e,
        order(
            5,
            3,
            Side::Buy,
            PositionSide::Long,
            10,
            OrderKind::Limit { price_tick: 50 },
        ),
    );
    let result = e.liquidate_account(1, 6).unwrap();
    let p = e.account_snapshot(2).unwrap().hedge_positions.unwrap();
    assert_eq!((p.long.qty, p.short.qty), (5, 0));
    assert_eq!(p.short.realized_pnl, 500);
    assert_eq!(e.account_snapshot(1).unwrap().cash_balance, 0);
    assert!(result.clearing_events.iter().any(|event| matches!(event,
        PerpClearingEvent::LiquidationSettled { auto_deleveraging_loss: 490, bad_debt: 0, auto_deleveraging_allocations, .. }
            if auto_deleveraging_allocations.iter().any(|a| a.account_id == 2 && a.position_side == PositionSide::Short))));
    assert_eq!(
        e.account_snapshots()
            .iter()
            .map(|a| a.position_qty)
            .sum::<i128>(),
        0
    );
}

#[test]
fn hedge_zero_net_liquidation_still_runs_adl_and_conserves_cash() {
    let mut e = PerpTradingEngine::new(
        PerpClearingConfig {
            auto_deleveraging_enabled: true,
            ..config()
        },
        100,
    )
    .unwrap();
    for id in 1..=5 {
        e.create_account(id, 10_000);
    }
    fill(&mut e, 1, (1, Side::Buy, PositionSide::Long), 10, 100, 2);
    fill(&mut e, 3, (1, Side::Sell, PositionSide::Short), 10, 50, 3);
    e.sync_cash_balance(1, 10).unwrap();
    e.set_mark_price_tick(50).unwrap();
    let cash_before: i128 = e.account_snapshots().iter().map(|a| a.cash_balance).sum();
    assert_eq!(e.account_snapshot(1).unwrap().position_qty, 0);
    apply(
        &mut e,
        order(
            5,
            4,
            Side::Buy,
            PositionSide::Long,
            10,
            OrderKind::Limit { price_tick: 50 },
        ),
    );
    e.liquidate_account(1, 6).unwrap();
    apply(
        &mut e,
        order(
            7,
            5,
            Side::Sell,
            PositionSide::Short,
            10,
            OrderKind::Limit { price_tick: 50 },
        ),
    );
    let result = e.liquidate_account(1, 8).unwrap();
    assert!(!e.account_snapshot(1).unwrap().has_open_position());
    assert!(result.clearing_events.iter().any(|event| matches!(
        event,
        PerpClearingEvent::LiquidationSettled {
            auto_deleveraging_loss: 490,
            bad_debt: 0,
            ..
        }
    )));
    assert_eq!(
        e.account_snapshots()
            .iter()
            .map(|a| a.cash_balance)
            .sum::<i128>(),
        cash_before
    );
    assert_eq!(
        e.account_snapshots()
            .iter()
            .map(|a| a.position_qty)
            .sum::<i128>(),
        0
    );
}

#[test]
fn hedge_fees_and_entry_averaging_are_attributed_to_the_selected_leg() {
    let mut e = PerpTradingEngine::new(
        PerpClearingConfig {
            maker_fee_ppm: 10_000,
            taker_fee_ppm: 20_000,
            ..config()
        },
        100,
    )
    .unwrap();
    for id in 1..=4 {
        e.create_account(id, 10_000);
    }
    fill(&mut e, 1, (1, Side::Buy, PositionSide::Long), 2, 100, 2);
    fill(&mut e, 3, (1, Side::Buy, PositionSide::Long), 2, 120, 3);
    fill(&mut e, 5, (1, Side::Sell, PositionSide::Short), 2, 130, 4);
    let a = e.account_snapshot(1).unwrap();
    let p = a.hedge_positions.unwrap();
    assert_eq!(
        (p.long.qty, p.long.avg_entry_price_tick, p.long.fees_paid),
        (4, 110, 8)
    );
    assert_eq!(
        (p.short.qty, p.short.avg_entry_price_tick, p.short.fees_paid),
        (2, 130, 5)
    );
    assert_eq!(a.fees_paid, p.long.fees_paid + p.short.fees_paid);
}

#[test]
fn hedge_fok_close_expires_without_modifying_either_leg_if_depth_is_short() {
    let mut e = engine();
    locked(&mut e);
    apply(
        &mut e,
        order(
            5,
            4,
            Side::Buy,
            PositionSide::Long,
            2,
            OrderKind::Limit { price_tick: 100 },
        ),
    );
    let before = e.account_snapshots();
    let result = e
        .apply(order(
            6,
            1,
            Side::Sell,
            PositionSide::Long,
            3,
            OrderKind::FillOrKill {
                price_tick: Some(100),
            },
        ))
        .unwrap();
    assert!(result.events.iter().any(|r| matches!(
        r.event,
        Event::OrderRejected {
            reason: RejectReason::FillOrKillWouldNotFill,
            ..
        }
    )));
    assert_eq!(e.account_snapshots(), before);
}

#[test]
fn hedge_liquidation_uses_short_depth_when_long_depth_is_absent() {
    let mut e = engine();
    fill(&mut e, 1, (1, Side::Buy, PositionSide::Long), 5, 100, 2);
    fill(&mut e, 3, (1, Side::Sell, PositionSide::Short), 5, 100, 3);
    e.sync_cash_balance(1, 0).unwrap();
    apply(
        &mut e,
        order(
            5,
            4,
            Side::Sell,
            PositionSide::Short,
            2,
            OrderKind::Limit { price_tick: 100 },
        ),
    );
    e.liquidate_account(1, 6).unwrap();
    let p = e.account_snapshot(1).unwrap().hedge_positions.unwrap();
    assert_eq!((p.long.qty, p.short.qty), (5, 3));
    assert!(e.pending_liquidation(1).is_some());
}

#[test]
fn hedge_room_auto_liquidates_zero_net_account_when_gross_margin_is_breached() {
    let scenario: ScenarioConfig = serde_json::from_value(serde_json::json!({
        "room_id":"hedge-auto", "seed":1,
        "market":{"Perp":{"instrument":{"symbol":"PERP","tick_size":1,"lot_size":1},
            "clearing":{"maker_fee_ppm":0,"taker_fee_ppm":0,"leverage":10,"position_mode":"Hedge"},
            "risk":{},"initial_mark_price_tick":100}},
        "accounts":[{"Basic":{"account_id":1,"cash_balance":200}}, {"Basic":{"account_id":2,"cash_balance":10000}}, {"Basic":{"account_id":3,"cash_balance":10000}}],
        "seed_orders":[]
    })).unwrap();
    let mut rooms = RoomManager::new();
    rooms.create_room(scenario).unwrap();
    for command in [
        order(
            1,
            2,
            Side::Sell,
            PositionSide::Short,
            5,
            OrderKind::Limit { price_tick: 100 },
        ),
        order(2, 1, Side::Buy, PositionSide::Long, 5, OrderKind::Market),
        order(
            3,
            3,
            Side::Buy,
            PositionSide::Long,
            5,
            OrderKind::Limit { price_tick: 100 },
        ),
        order(4, 1, Side::Sell, PositionSide::Short, 5, OrderKind::Market),
        order(
            5,
            3,
            Side::Buy,
            PositionSide::Long,
            5,
            OrderKind::Limit { price_tick: 499 },
        ),
        order(
            6,
            2,
            Side::Sell,
            PositionSide::Short,
            5,
            OrderKind::Limit { price_tick: 501 },
        ),
    ] {
        rooms.apply("hedge-auto", command).unwrap();
    }
    rooms
        .apply(
            "hedge-auto",
            Command::SetMarkPrice(SetMarkPrice { price_tick: 500 }),
        )
        .unwrap();
    // The bounded room loop may finish one leg on the next scheduler pass.
    rooms
        .advance_pending_liquidations("hedge-auto", 10)
        .unwrap();
    let AccountSnapshot::Perp(a) = rooms
        .account_snapshot_for("hedge-auto", "PERP", 1)
        .unwrap()
        .unwrap()
    else {
        panic!()
    };
    assert!(!a.has_open_position(), "{a:?}");
    assert_eq!(a.margin_status, PerpMarginStatus::Flat);
    assert!(rooms.pending_liquidations("hedge-auto").unwrap().is_empty());
}
