use pine_ir::{StrategyCloseEntriesRule, StrategyCommission, StrategyMarginSetting};

use super::fill_transition::{
    FillRequest, FillTriggerReason, PositionSnapshot, calculate_netting_transition,
};
use super::ledger::TradeDirection;
use super::pending_entries::{PendingEntryDirection, PendingEntryKind};
use super::types::{InternalOrderKey, StrategyCommandOrigin, StrategyOrderMetadata};
use super::*;

fn request() -> FillRequest {
    FillRequest {
        order_key: InternalOrderKey(1),
        bar_index: 2,
        time: 20,
        raw_price: 110.0,
        trigger_reason: FillTriggerReason::Market,
    }
}

fn snapshot(signed_size: f64) -> PositionSnapshot {
    PositionSnapshot {
        signed_size,
        avg_price: 100.0,
    }
}

fn allocation(direction: TradeDirection, quantity: f64) -> super::ledger::TradeAllocation {
    super::ledger::TradeAllocation {
        trade_index: 0,
        trade_key: 0,
        entry_id: "E".to_owned(),
        direction,
        entry_price: 100.0,
        entry_bar_index: 0,
        entry_time: 10,
        quantity,
        entry_commission: 0.0,
        entry_metadata: StrategyOrderMetadata::default(),
    }
}

/// Generic-order signed delta against both sides for the five netting shapes.
#[test]
fn generic_order_netting_matrix_covers_both_directions_and_five_shapes() {
    struct Case {
        position: f64,
        delta: f64,
        close_qty: f64,
        open_qty: f64,
        close_dir: Option<TradeDirection>,
        open_dir: Option<TradeDirection>,
        routable: bool,
    }
    let cases = [
        Case {
            position: 0.0,
            delta: 2.0,
            close_qty: 0.0,
            open_qty: 2.0,
            close_dir: None,
            open_dir: Some(TradeDirection::Long),
            routable: true,
        },
        Case {
            position: 0.0,
            delta: -2.0,
            close_qty: 0.0,
            open_qty: 2.0,
            close_dir: None,
            open_dir: Some(TradeDirection::Short),
            routable: true,
        },
        Case {
            position: 1.0,
            delta: 2.0,
            close_qty: 0.0,
            open_qty: 2.0,
            close_dir: None,
            open_dir: Some(TradeDirection::Long),
            routable: true,
        },
        Case {
            position: -1.0,
            delta: -2.0,
            close_qty: 0.0,
            open_qty: 2.0,
            close_dir: None,
            open_dir: Some(TradeDirection::Short),
            routable: true,
        },
        Case {
            position: 3.0,
            delta: -1.0,
            close_qty: 1.0,
            open_qty: 0.0,
            close_dir: Some(TradeDirection::Long),
            routable: true,
            open_dir: None,
        },
        Case {
            position: -3.0,
            delta: 1.0,
            close_qty: 1.0,
            open_qty: 0.0,
            close_dir: Some(TradeDirection::Short),
            open_dir: None,
            routable: true,
        },
        Case {
            position: 3.0,
            delta: -3.0,
            close_qty: 3.0,
            open_qty: 0.0,
            close_dir: Some(TradeDirection::Long),
            open_dir: None,
            routable: true,
        },
        Case {
            position: -2.0,
            delta: 2.0,
            close_qty: 2.0,
            open_qty: 0.0,
            close_dir: Some(TradeDirection::Short),
            open_dir: None,
            routable: true,
        },
        Case {
            position: 2.0,
            delta: -5.0,
            close_qty: 2.0,
            open_qty: 3.0,
            close_dir: Some(TradeDirection::Long),
            open_dir: Some(TradeDirection::Short),
            routable: false,
        },
        Case {
            position: -2.0,
            delta: 5.0,
            close_qty: 2.0,
            open_qty: 3.0,
            close_dir: Some(TradeDirection::Short),
            open_dir: Some(TradeDirection::Long),
            routable: false,
        },
    ];
    for case in cases {
        let allocations = if case.close_qty > 0.0 {
            vec![allocation(
                case.close_dir.expect("close dir"),
                case.close_qty,
            )]
        } else {
            Vec::new()
        };
        let transition = calculate_netting_transition(
            &snapshot(case.position),
            request(),
            case.delta,
            110.0,
            0.0,
            0.0,
            allocations,
        )
        .expect("netting");
        assert_eq!(transition.close_quantity, case.close_qty);
        assert_eq!(transition.open_quantity, case.open_qty);
        assert_eq!(
            transition.opened_trade.map(|opened| opened.direction),
            case.open_dir
        );
        assert_eq!(transition.filled_quantity, case.delta.abs());
        assert_eq!(transition.routable, case.routable);
    }
}

#[test]
fn production_generic_short_order_against_long_crosses_zero() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("E".to_owned(), 0, 10, 100.0, 1.0));
    broker.place_pending_market_short_order("R".to_owned(), 3.0, 1);
    broker.fill_pending_market_entries(2, 20, 110.0);
    assert_eq!(broker.position_size, -2.0);
    assert_eq!(broker.closed_trade_count(), 1);
    assert_eq!(broker.open_trade_count(), 1);
    assert_eq!(broker.orders.last().map(|order| order.qty), Some(3.0));
    assert_eq!(broker.trades[0].exit_id, "R");
    broker.assert_ledger_aggregates();
}

#[test]
fn production_generic_long_order_against_short_nets_and_opens_remainder() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_market_short_entry("S".to_owned(), 1.0, 0);
    broker.fill_pending_market_entries(1, 10, 100.0);
    assert_eq!(broker.position_size, -1.0);
    broker.place_pending_market_long_order("O".to_owned(), 2.0, 1);
    broker.fill_pending_market_entries(2, 20, 110.0);
    assert_eq!(broker.position_size, 1.0);
    assert_eq!(broker.closed_trade_count(), 1);
    assert_eq!(broker.open_trade_count(), 1);
    assert_eq!(broker.orders.last().map(|order| order.qty), Some(2.0));
    broker.assert_ledger_aggregates();
}

#[test]
fn production_generic_market_order_covers_flat_increase_reduce_flatten_and_cross_zero() {
    struct Case {
        setup_long: Option<f64>,
        setup_short: Option<f64>,
        order_long: Option<f64>,
        order_short: Option<f64>,
        size: f64,
        closed: usize,
        open: usize,
        order_qty: f64,
    }
    let cases = [
        Case {
            setup_long: None,
            setup_short: None,
            order_long: Some(2.0),
            order_short: None,
            size: 2.0,
            closed: 0,
            open: 1,
            order_qty: 2.0,
        },
        Case {
            setup_long: None,
            setup_short: None,
            order_long: None,
            order_short: Some(2.0),
            size: -2.0,
            closed: 0,
            open: 1,
            order_qty: 2.0,
        },
        Case {
            setup_long: Some(1.0),
            setup_short: None,
            order_long: Some(2.0),
            order_short: None,
            size: 3.0,
            closed: 0,
            open: 2,
            order_qty: 2.0,
        },
        Case {
            setup_long: None,
            setup_short: Some(1.0),
            order_long: None,
            order_short: Some(2.0),
            size: -3.0,
            closed: 0,
            open: 2,
            order_qty: 2.0,
        },
        Case {
            setup_long: Some(3.0),
            setup_short: None,
            order_long: None,
            order_short: Some(1.0),
            size: 2.0,
            closed: 1,
            open: 1,
            order_qty: 1.0,
        },
        Case {
            setup_long: None,
            setup_short: Some(3.0),
            order_long: Some(1.0),
            order_short: None,
            size: -2.0,
            closed: 1,
            open: 1,
            order_qty: 1.0,
        },
        Case {
            setup_long: Some(2.0),
            setup_short: None,
            order_long: None,
            order_short: Some(2.0),
            size: 0.0,
            closed: 1,
            open: 0,
            order_qty: 2.0,
        },
        Case {
            setup_long: None,
            setup_short: Some(2.0),
            order_long: Some(2.0),
            order_short: None,
            size: 0.0,
            closed: 1,
            open: 0,
            order_qty: 2.0,
        },
        Case {
            setup_long: Some(2.0),
            setup_short: None,
            order_long: None,
            order_short: Some(5.0),
            size: -3.0,
            closed: 1,
            open: 1,
            order_qty: 5.0,
        },
        Case {
            setup_long: None,
            setup_short: Some(2.0),
            order_long: Some(5.0),
            order_short: None,
            size: 3.0,
            closed: 1,
            open: 1,
            order_qty: 5.0,
        },
    ];
    for case in cases {
        let mut broker = BrokerState::new(100_000.0);
        if let Some(qty) = case.setup_long {
            assert!(broker.entry_long("E".to_owned(), 0, 10, 100.0, qty));
        }
        if let Some(qty) = case.setup_short {
            broker.place_pending_market_short_entry("S".to_owned(), qty, 0);
            broker.fill_pending_market_entries(1, 10, 100.0);
        }
        if let Some(qty) = case.order_long {
            broker.place_pending_market_long_order("O".to_owned(), qty, 1);
        }
        if let Some(qty) = case.order_short {
            broker.place_pending_market_short_order("O".to_owned(), qty, 1);
        }
        broker.fill_pending_market_entries(2, 20, 110.0);
        assert_eq!(broker.position_size, case.size);
        assert_eq!(broker.closed_trade_count() as usize, case.closed);
        assert_eq!(broker.open_trade_count(), case.open as i64);
        assert_eq!(
            broker.orders.last().map(|order| order.qty),
            Some(case.order_qty)
        );
        broker.assert_ledger_aggregates();
    }
}

#[test]
fn production_generic_market_order_rejects_unaffordable_remainder_atomically() {
    let mut broker = BrokerState::new_with_account_settings(
        100.0,
        None,
        0.0,
        0.0,
        StrategyMarginSetting::default(),
        StrategyMarginSetting::explicit(100.0),
    );
    assert!(broker.entry_long("E".to_owned(), 0, 10, 100.0, 1.0));
    let cash = broker.cash;
    broker.place_pending_market_short_order("R".to_owned(), 3.0, 1);
    broker.fill_pending_market_entries(2, 20, 110.0);
    assert_eq!(broker.position_size, 1.0);
    assert_eq!(broker.cash, cash);
    assert_eq!(broker.closed_trade_count(), 0);
    assert_eq!(broker.open_trade_count(), 1);
    assert!(broker.orders.iter().all(|order| order.id != "R"));
    assert_eq!(broker.diagnostics.len(), 1);
    assert_eq!(broker.diagnostics[0].code, "E_STRATEGY_MARGIN");
    broker.assert_ledger_aggregates();
}

#[test]
fn production_generic_market_order_applies_side_slippage_and_commission() {
    let mut long_order = BrokerState::new_with_commission_and_slippage(
        100_000.0,
        Some(StrategyCommission::CashPerContract(1.5)),
        1.0,
    );
    long_order.place_pending_market_long_order("O".to_owned(), 2.0, 0);
    long_order.fill_pending_market_entries(1, 10, 100.0);
    assert_eq!(long_order.orders[0].price, 101.0);
    assert_eq!(long_order.position_size, 2.0);
    assert_eq!(long_order.cash, 100_000.0 - 2.0 * 101.0 - 3.0);
    long_order.assert_ledger_aggregates();

    let mut reduce = BrokerState::new_with_commission_and_slippage(
        100_000.0,
        Some(StrategyCommission::CashPerContract(1.5)),
        1.0,
    );
    assert!(reduce.entry_long("E".to_owned(), 0, 10, 100.0, 1.0));
    reduce.place_pending_market_short_order("R".to_owned(), 1.0, 1);
    reduce.fill_pending_market_entries(2, 20, 110.0);
    assert_eq!(reduce.orders.last().map(|order| order.price), Some(109.0));
    assert_eq!(reduce.position_size, 0.0);
    assert_eq!(reduce.closed_trade_count(), 1);
    assert_eq!(reduce.trades[0].profit, 5.0);
    reduce.assert_ledger_aggregates();
}

#[test]
fn production_generic_market_order_updates_max_held_and_bypasses_pyramiding() {
    let mut broker = BrokerState::new_with_account_settings_and_pyramiding(
        100_000.0,
        None,
        0.0,
        0.0,
        StrategyMarginSetting::default(),
        StrategyMarginSetting::default(),
        1,
    );
    assert!(broker.entry_long("E".to_owned(), 0, 10, 100.0, 1.0));
    broker.place_pending_market_short_order("R".to_owned(), 3.0, 1);
    broker.fill_pending_market_entries(2, 20, 110.0);
    assert_eq!(broker.position_size, -2.0);
    assert_eq!(broker.open_trade_count(), 1);
    assert_eq!(broker.max_contracts_held_long(), 1.0);
    assert_eq!(broker.max_contracts_held_short(), 2.0);

    let mut increase = BrokerState::new_with_account_settings_and_pyramiding(
        100_000.0,
        None,
        0.0,
        0.0,
        StrategyMarginSetting::default(),
        StrategyMarginSetting::default(),
        1,
    );
    increase.place_pending_market_short_entry("S".to_owned(), 1.0, 0);
    increase.fill_pending_market_entries(1, 10, 100.0);
    increase.place_pending_market_short_order("O".to_owned(), 2.0, 1);
    increase.fill_pending_market_entries(2, 20, 110.0);
    assert_eq!(increase.position_size, -3.0);
    assert_eq!(increase.open_trade_count(), 2);
    increase.assert_ledger_aggregates();
}

#[test]
fn production_generic_limit_order_does_not_net_until_trigger() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_market_short_entry("S".to_owned(), 1.0, 0);
    broker.fill_pending_market_entries(1, 10, 100.0);
    broker.place_pending_limit_long_order("O".to_owned(), 2.0, 90.0, 1);
    broker.fill_pending_limit_long_entries(1, 20, 90.0);
    assert_eq!(broker.position_size, -1.0);
    assert_eq!(broker.closed_trade_count(), 0);
    broker.fill_pending_limit_long_entries(2, 30, 91.0);
    assert_eq!(broker.position_size, -1.0);
    assert_eq!(broker.closed_trade_count(), 0);
}

#[test]
fn production_generic_stop_order_does_not_net_until_trigger() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("E".to_owned(), 0, 10, 100.0, 1.0));
    broker.place_pending_stop_short_order("O".to_owned(), 2.0, 90.0, 1);
    broker.fill_pending_stop_short_entries(1, 20, 89.0);
    assert_eq!(broker.position_size, 1.0);
    assert_eq!(broker.closed_trade_count(), 0);
    broker.fill_pending_stop_short_entries(2, 30, 91.0);
    assert_eq!(broker.position_size, 1.0);
    assert_eq!(broker.closed_trade_count(), 0);
}

#[test]
fn production_generic_stop_limit_activates_without_filling() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("E".to_owned(), 0, 10, 100.0, 1.0));
    broker.place_pending_stop_limit_short_order("O".to_owned(), 2.0, 90.0, 95.0, 1);
    broker.fill_pending_stop_limit_short_entries(2, 20, 96.0, 89.0);
    assert_eq!(broker.position_size, 1.0);
    assert_eq!(broker.closed_trade_count(), 0);
    assert_eq!(
        broker
            .order_book
            .entries()
            .current()
            .map(|entry| entry.kind),
        Some(super::pending_entries::PendingEntryKind::StopLimit {
            stop_price: 90.0,
            limit_price: 95.0,
            activated_bar_index: Some(2),
        })
    );
}

#[test]
fn production_generic_stop_and_stop_limit_cancel_before_fill_does_not_mutate() {
    let mut stop = BrokerState::new(100_000.0);
    assert!(stop.entry_long("E".to_owned(), 0, 10, 100.0, 1.0));
    let cash = stop.cash;
    stop.place_pending_stop_short_order("O".to_owned(), 2.0, 90.0, 1);
    stop.cancel_pending_order("O");
    stop.fill_pending_stop_short_entries(2, 20, 80.0);
    assert_eq!(stop.position_size, 1.0);
    assert_eq!(stop.cash, cash);
    assert_eq!(stop.closed_trade_count(), 0);

    let mut stop_limit = BrokerState::new(100_000.0);
    assert!(stop_limit.entry_long("E".to_owned(), 0, 10, 100.0, 1.0));
    stop_limit.place_pending_stop_limit_short_order("O".to_owned(), 2.0, 90.0, 95.0, 1);
    stop_limit.fill_pending_stop_limit_short_entries(2, 20, 91.0, 89.0);
    stop_limit.cancel_pending_order("O");
    stop_limit.fill_pending_stop_limit_short_entries(3, 30, 95.0, 94.0);
    assert_eq!(stop_limit.position_size, 1.0);
    assert_eq!(stop_limit.closed_trade_count(), 0);
}

#[test]
fn production_generic_stop_order_nets_after_trigger_in_both_directions() {
    let mut short_against_long = BrokerState::new(100_000.0);
    assert!(short_against_long.entry_long("E".to_owned(), 0, 10, 100.0, 1.0));
    short_against_long.place_pending_stop_short_order("O".to_owned(), 2.0, 90.0, 1);
    short_against_long.fill_pending_stop_short_entries(2, 20, 89.0);
    assert_eq!(short_against_long.position_size, -1.0);
    assert_eq!(short_against_long.closed_trade_count(), 1);
    assert_eq!(
        short_against_long.orders.last().map(|order| order.price),
        Some(90.0)
    );
    short_against_long.assert_ledger_aggregates();

    let mut long_against_short = BrokerState::new(100_000.0);
    long_against_short.place_pending_market_short_entry("S".to_owned(), 1.0, 0);
    long_against_short.fill_pending_market_entries(1, 10, 100.0);
    long_against_short.place_pending_stop_long_order("O".to_owned(), 2.0, 110.0, 1);
    long_against_short.fill_pending_stop_long_entries(2, 20, 111.0);
    assert_eq!(long_against_short.position_size, 1.0);
    assert_eq!(long_against_short.closed_trade_count(), 1);
    assert_eq!(
        long_against_short.orders.last().map(|order| order.price),
        Some(110.0)
    );
    long_against_short.assert_ledger_aggregates();
}

#[test]
fn production_generic_stop_limit_order_nets_after_activation_and_limit() {
    let mut short_against_long = BrokerState::new(100_000.0);
    assert!(short_against_long.entry_long("E".to_owned(), 0, 10, 100.0, 1.0));
    short_against_long.place_pending_stop_limit_short_order("O".to_owned(), 2.0, 90.0, 95.0, 1);
    short_against_long.fill_pending_stop_limit_short_entries(2, 20, 91.0, 89.0);
    assert_eq!(short_against_long.position_size, 1.0);
    assert_eq!(short_against_long.closed_trade_count(), 0);
    short_against_long.fill_pending_stop_limit_short_entries(3, 30, 95.0, 94.0);
    assert_eq!(short_against_long.position_size, -1.0);
    assert_eq!(short_against_long.closed_trade_count(), 1);
    assert_eq!(
        short_against_long.orders.last().map(|order| order.price),
        Some(95.0)
    );
    short_against_long.assert_ledger_aggregates();

    let mut long_against_short = BrokerState::new(100_000.0);
    long_against_short.place_pending_market_short_entry("S".to_owned(), 1.0, 0);
    long_against_short.fill_pending_market_entries(1, 10, 100.0);
    long_against_short.place_pending_stop_limit_long_order("O".to_owned(), 2.0, 110.0, 90.0, 1);
    long_against_short.fill_pending_stop_limit_long_entries(2, 20, 111.0, 109.0);
    assert_eq!(long_against_short.position_size, -1.0);
    long_against_short.fill_pending_stop_limit_long_entries(3, 30, 91.0, 89.0);
    assert_eq!(long_against_short.position_size, 1.0);
    assert_eq!(long_against_short.closed_trade_count(), 1);
    assert_eq!(
        long_against_short.orders.last().map(|order| order.price),
        Some(90.0)
    );
    long_against_short.assert_ledger_aggregates();
}

#[test]
fn production_generic_stop_order_rejects_unaffordable_remainder_atomically() {
    let mut broker = BrokerState::new_with_account_settings(
        100.0,
        None,
        0.0,
        0.0,
        StrategyMarginSetting::default(),
        StrategyMarginSetting::explicit(100.0),
    );
    assert!(broker.entry_long("E".to_owned(), 0, 10, 100.0, 1.0));
    let cash = broker.cash;
    broker.place_pending_stop_short_order("O".to_owned(), 3.0, 90.0, 1);
    broker.fill_pending_stop_short_entries(2, 20, 89.0);
    assert_eq!(broker.position_size, 1.0);
    assert_eq!(broker.cash, cash);
    assert_eq!(broker.closed_trade_count(), 0);
    assert_eq!(broker.diagnostics[0].code, "E_STRATEGY_MARGIN");
    broker.assert_ledger_aggregates();
}

#[test]
fn production_generic_stop_order_records_netting_metadata() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("E".to_owned(), 0, 10, 100.0, 1.0));
    broker.order_fill_alerts.clear();
    broker.place_pending_stop_short_order_with_metadata(
        "O".to_owned(),
        2.0,
        90.0,
        1,
        StrategyOrderMetadata {
            comment: Some("stop comment".to_owned()),
            alert_message: Some("stop alert".to_owned()),
            disable_alert: false,
        },
    );
    broker.fill_pending_stop_short_entries(2, 20, 89.0);
    assert_eq!(broker.closed_trade_count(), 1);
    assert_eq!(broker.closed_trade_exit_comment(0), Some("stop comment"));
    assert_eq!(
        broker.order_fill_alerts.last().map(|alert| alert.qty),
        Some(2.0)
    );
    assert_eq!(
        broker
            .order_fill_alerts
            .last()
            .map(|alert| alert.message.as_str()),
        Some("stop alert")
    );
}

#[test]
fn production_generic_limit_order_cancel_before_fill_does_not_mutate() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_market_short_entry("S".to_owned(), 1.0, 0);
    broker.fill_pending_market_entries(1, 10, 100.0);
    let cash = broker.cash;
    broker.place_pending_limit_long_order("O".to_owned(), 2.0, 90.0, 1);
    broker.cancel_pending_order("O");
    broker.fill_pending_limit_long_entries(2, 20, 89.0);
    assert_eq!(broker.position_size, -1.0);
    assert_eq!(broker.cash, cash);
    assert_eq!(broker.closed_trade_count(), 0);
    assert!(broker.orders.iter().all(|order| order.id != "O"));
    broker.assert_ledger_aggregates();
}

#[test]
fn production_generic_limit_order_nets_after_limit_trigger_in_both_directions() {
    struct Case {
        setup_long: Option<f64>,
        setup_short: Option<f64>,
        order_long: Option<f64>,
        order_short: Option<f64>,
        limit: f64,
        extreme: f64,
        size: f64,
        closed: usize,
        order_qty: f64,
        fill_price: f64,
    }
    let cases = [
        Case {
            setup_long: None,
            setup_short: Some(1.0),
            order_long: Some(2.0),
            order_short: None,
            limit: 90.0,
            extreme: 89.0,
            size: 1.0,
            closed: 1,
            order_qty: 2.0,
            fill_price: 90.0,
        },
        Case {
            setup_long: Some(1.0),
            setup_short: None,
            order_long: None,
            order_short: Some(2.0),
            limit: 110.0,
            extreme: 111.0,
            size: -1.0,
            closed: 1,
            order_qty: 2.0,
            fill_price: 110.0,
        },
        Case {
            setup_long: None,
            setup_short: Some(2.0),
            order_long: Some(1.0),
            order_short: None,
            limit: 90.0,
            extreme: 89.0,
            size: -1.0,
            closed: 1,
            order_qty: 1.0,
            fill_price: 90.0,
        },
        Case {
            setup_long: Some(2.0),
            setup_short: None,
            order_long: None,
            order_short: Some(1.0),
            limit: 110.0,
            extreme: 111.0,
            size: 1.0,
            closed: 1,
            order_qty: 1.0,
            fill_price: 110.0,
        },
        Case {
            setup_long: None,
            setup_short: Some(2.0),
            order_long: Some(2.0),
            order_short: None,
            limit: 90.0,
            extreme: 89.0,
            size: 0.0,
            closed: 1,
            order_qty: 2.0,
            fill_price: 90.0,
        },
        Case {
            setup_long: Some(2.0),
            setup_short: None,
            order_long: None,
            order_short: Some(2.0),
            limit: 110.0,
            extreme: 111.0,
            size: 0.0,
            closed: 1,
            order_qty: 2.0,
            fill_price: 110.0,
        },
    ];
    for case in cases {
        let mut broker = BrokerState::new(100_000.0);
        if let Some(qty) = case.setup_long {
            assert!(broker.entry_long("E".to_owned(), 0, 10, 100.0, qty));
        }
        if let Some(qty) = case.setup_short {
            broker.place_pending_market_short_entry("S".to_owned(), qty, 0);
            broker.fill_pending_market_entries(1, 10, 100.0);
        }
        if let Some(qty) = case.order_long {
            broker.place_pending_limit_long_order("O".to_owned(), qty, case.limit, 1);
            broker.fill_pending_limit_long_entries(2, 20, case.extreme);
        }
        if let Some(qty) = case.order_short {
            broker.place_pending_limit_short_order("O".to_owned(), qty, case.limit, 1);
            broker.fill_pending_limit_short_entries(2, 20, case.extreme);
        }
        assert_eq!(broker.position_size, case.size);
        assert_eq!(broker.closed_trade_count() as usize, case.closed);
        assert_eq!(
            broker.orders.last().map(|order| order.qty),
            Some(case.order_qty)
        );
        assert_eq!(
            broker.orders.last().map(|order| order.price),
            Some(case.fill_price)
        );
        broker.assert_ledger_aggregates();
    }
}

#[test]
fn production_price_based_entry_does_not_reverse_until_trigger() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_market_short_entry("S".to_owned(), 1.0, 0);
    broker.fill_pending_market_entries(1, 10, 100.0);
    broker.place_pending_limit_long_entry("L".to_owned(), 1.0, 90.0, 1);
    broker.fill_pending_limit_long_entries(1, 20, 90.0);
    assert_eq!(broker.position_size, -1.0);
    assert_eq!(broker.closed_trade_count(), 0);
    broker.fill_pending_limit_long_entries(2, 30, 91.0);
    assert_eq!(broker.position_size, -1.0);
    assert_eq!(broker.closed_trade_count(), 0);
}

#[test]
fn production_price_based_entry_cancel_before_fill_does_not_mutate() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_market_short_entry("S".to_owned(), 1.0, 0);
    broker.fill_pending_market_entries(1, 10, 100.0);
    let cash = broker.cash;
    broker.place_pending_limit_long_entry("L".to_owned(), 1.0, 90.0, 1);
    broker.cancel_pending_order("L");
    broker.fill_pending_limit_long_entries(2, 20, 89.0);
    assert_eq!(broker.position_size, -1.0);
    assert_eq!(broker.cash, cash);
    assert_eq!(broker.closed_trade_count(), 0);
}

#[test]
fn production_price_based_entry_reverses_flatten_then_open_requested_qty() {
    let mut limit = BrokerState::new(100_000.0);
    limit.place_pending_market_short_entry("S".to_owned(), 2.0, 0);
    limit.fill_pending_market_entries(1, 10, 100.0);
    limit.place_pending_limit_long_entry("L".to_owned(), 1.0, 90.0, 1);
    limit.fill_pending_limit_long_entries(2, 20, 89.0);
    assert_eq!(limit.position_size, 1.0);
    assert_eq!(limit.closed_trade_count(), 1);
    assert_eq!(limit.open_trade_count(), 1);
    assert_eq!(limit.orders.last().map(|order| order.qty), Some(3.0));
    assert_eq!(limit.orders.last().map(|order| order.price), Some(90.0));
    limit.assert_ledger_aggregates();

    let mut stop = BrokerState::new(100_000.0);
    assert!(stop.entry_long("E".to_owned(), 0, 10, 100.0, 2.0));
    stop.place_pending_stop_short_entry("S".to_owned(), 1.0, 90.0, 1);
    stop.fill_pending_stop_short_entries(2, 20, 89.0);
    assert_eq!(stop.position_size, -1.0);
    assert_eq!(stop.closed_trade_count(), 1);
    assert_eq!(stop.orders.last().map(|order| order.qty), Some(3.0));
    stop.assert_ledger_aggregates();

    let mut stop_limit = BrokerState::new(100_000.0);
    assert!(stop_limit.entry_long("E".to_owned(), 0, 10, 100.0, 2.0));
    stop_limit.place_pending_stop_limit_short_entry("S".to_owned(), 1.0, 90.0, 95.0, 1);
    stop_limit.fill_pending_stop_limit_short_entries(2, 20, 91.0, 89.0);
    assert_eq!(stop_limit.position_size, 2.0);
    stop_limit.fill_pending_stop_limit_short_entries(3, 30, 95.0, 94.0);
    assert_eq!(stop_limit.position_size, -1.0);
    assert_eq!(stop_limit.closed_trade_count(), 1);
    assert_eq!(
        stop_limit.orders.last().map(|order| order.price),
        Some(95.0)
    );
    stop_limit.assert_ledger_aggregates();
}

#[test]
fn production_price_based_entry_reversal_applies_pyramiding_to_new_side() {
    let mut broker = BrokerState::new_with_account_settings_and_pyramiding(
        100_000.0,
        None,
        0.0,
        0.0,
        StrategyMarginSetting::default(),
        StrategyMarginSetting::default(),
        1,
    );
    broker.place_pending_market_short_entry("S".to_owned(), 2.0, 0);
    broker.fill_pending_market_entries(1, 10, 100.0);
    broker.place_pending_limit_long_entry("L1".to_owned(), 1.0, 90.0, 1);
    broker.place_pending_limit_long_entry("L2".to_owned(), 1.0, 90.0, 1);
    broker.fill_pending_limit_long_entries(2, 20, 89.0);
    assert_eq!(broker.position_size, 2.0);
    assert_eq!(broker.open_trade_count(), 2);
    broker.assert_ledger_aggregates();
}

#[test]
fn production_price_based_entry_reversal_clears_opposite_exits() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_market_short_entry("S".to_owned(), 1.0, 0);
    broker.fill_pending_market_entries(1, 10, 100.0);
    broker.place_exit_stop("XS".to_owned(), "S".to_owned(), 110.0, 1);
    assert!(broker.order_book.exits().count() > 0);
    broker.place_pending_limit_long_entry("L".to_owned(), 1.0, 90.0, 1);
    broker.fill_pending_limit_long_entries(2, 20, 89.0);
    assert_eq!(broker.position_size, 1.0);
    assert_eq!(broker.order_book.exits().count(), 0);
}

fn pending_order(broker: &BrokerState, id: &str) -> super::pending_entries::PendingEntry {
    broker
        .order_book
        .entries()
        .find_by_id(id)
        .cloned()
        .unwrap_or_else(|| panic!("missing pending {id}"))
}

#[test]
fn production_generic_order_same_id_does_not_fill_until_replaced_kind_triggers() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_order("O".to_owned(), 1.0, 90.0, 0);
    broker.place_pending_stop_long_order("O".to_owned(), 2.0, 110.0, 1);
    let replaced = pending_order(&broker, "O");
    assert_eq!(replaced.quantity, 2.0);
    assert_eq!(replaced.kind, PendingEntryKind::Stop { price: 110.0 });
    assert_eq!(replaced.origin, StrategyCommandOrigin::Order);
    broker.fill_pending_limit_long_entries(2, 20, 89.0);
    assert_eq!(broker.position_size, 0.0);
    assert_eq!(broker.closed_trade_count(), 0);
    broker.fill_pending_stop_long_entries(2, 20, 111.0);
    assert_eq!(broker.position_size, 2.0);
}

#[test]
fn production_generic_order_same_id_stop_limit_replace_resets_activation() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_stop_limit_long_order("O".to_owned(), 1.0, 90.0, 95.0, 0);
    broker.fill_pending_stop_limit_long_entries(1, 10, 91.0, 89.0);
    assert_eq!(broker.position_size, 0.0);
    match pending_order(&broker, "O").kind {
        PendingEntryKind::StopLimit {
            activated_bar_index,
            ..
        } => assert_eq!(activated_bar_index, Some(1)),
        other => panic!("expected stop-limit, got {other:?}"),
    }
    broker.place_pending_stop_limit_long_order("O".to_owned(), 2.0, 90.0, 95.0, 1);
    match pending_order(&broker, "O").kind {
        PendingEntryKind::StopLimit {
            activated_bar_index,
            ..
        } => assert_eq!(activated_bar_index, None),
        other => panic!("expected replaced stop-limit, got {other:?}"),
    }
    broker.fill_pending_stop_limit_long_entries(1, 10, 95.0, 94.0);
    assert_eq!(broker.position_size, 0.0);
    broker.fill_pending_stop_limit_long_entries(2, 20, 91.0, 89.0);
    broker.fill_pending_stop_limit_long_entries(3, 30, 95.0, 94.0);
    assert_eq!(broker.position_size, 2.0);
    broker.assert_ledger_aggregates();
}

#[test]
fn production_generic_order_same_id_direction_change_cancels_then_places() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_order("O".to_owned(), 1.0, 90.0, 0);
    let original_key = pending_order(&broker, "O").key;
    broker.place_pending_limit_short_order("O".to_owned(), 2.0, 110.0, 1);
    let replaced = pending_order(&broker, "O");
    assert_ne!(replaced.key, original_key);
    assert_eq!(replaced.direction, PendingEntryDirection::Short);
    assert_eq!(replaced.quantity, 2.0);
    assert_eq!(broker.order_book.entries().count(), 1);
}

#[test]
fn production_generic_order_cancel_shared_id_clears_exit_and_order() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 1.0));
    broker.place_exit_stop("X".to_owned(), "L".to_owned(), 90.0, 1);
    broker.place_pending_limit_long_order("X".to_owned(), 1.0, 90.0, 1);
    assert_eq!(broker.order_book.exits().count(), 1);
    assert!(broker.order_book.entries().find_by_id("X").is_some());
    let cash = broker.cash;
    broker.cancel_pending_order("X");
    assert_eq!(broker.order_book.exits().count(), 0);
    assert!(broker.order_book.entries().find_by_id("X").is_none());
    assert_eq!(broker.position_size, 1.0);
    assert_eq!(broker.cash, cash);
    assert_eq!(broker.closed_trade_count(), 0);
}

#[test]
fn production_generic_order_fifo_reduces_oldest_open_entry() {
    let mut broker = BrokerState::new_with_account_settings_and_pyramiding(
        100_000.0,
        None,
        0.0,
        0.0,
        StrategyMarginSetting::default(),
        StrategyMarginSetting::default(),
        2,
    );
    assert!(broker.entry_long("A".to_owned(), 0, 10, 100.0, 1.0));
    assert!(broker.entry_long("B".to_owned(), 1, 20, 110.0, 1.0));
    broker.place_pending_market_short_order("O".to_owned(), 1.0, 2);
    broker.fill_pending_market_entries(3, 30, 120.0);
    assert_eq!(broker.closed_trade_count(), 1);
    assert_eq!(broker.trades[0].id, "A");
    assert_eq!(
        broker
            .trade_ledger
            .open_at(0)
            .map(|trade| trade.id.as_str()),
        Some("B")
    );
    assert_eq!(broker.position_size, 1.0);
    broker.assert_ledger_aggregates();
}

#[test]
fn production_generic_order_any_matching_id_reduces_that_entry() {
    let mut broker = BrokerState::new_with_account_settings_and_pyramiding(
        100_000.0,
        None,
        0.0,
        0.0,
        StrategyMarginSetting::default(),
        StrategyMarginSetting::default(),
        2,
    )
    .with_close_entries_rule(StrategyCloseEntriesRule::Any);
    assert!(broker.entry_long("A".to_owned(), 0, 10, 100.0, 1.0));
    assert!(broker.entry_long("B".to_owned(), 1, 20, 110.0, 1.0));
    broker.place_pending_market_short_order("B".to_owned(), 1.0, 2);
    broker.fill_pending_market_entries(3, 30, 120.0);
    assert_eq!(broker.closed_trade_count(), 1);
    assert_eq!(broker.trades[0].id, "B");
    assert_eq!(
        broker
            .trade_ledger
            .open_at(0)
            .map(|trade| trade.id.as_str()),
        Some("A")
    );
    assert_eq!(broker.position_size, 1.0);
    broker.assert_ledger_aggregates();
}

#[test]
fn production_generic_order_any_unmatched_id_stays_fifo() {
    let mut broker = BrokerState::new_with_account_settings_and_pyramiding(
        100_000.0,
        None,
        0.0,
        0.0,
        StrategyMarginSetting::default(),
        StrategyMarginSetting::default(),
        2,
    )
    .with_close_entries_rule(StrategyCloseEntriesRule::Any);
    assert!(broker.entry_long("A".to_owned(), 0, 10, 100.0, 1.0));
    assert!(broker.entry_long("B".to_owned(), 1, 20, 110.0, 1.0));
    broker.place_pending_market_short_order("O".to_owned(), 1.0, 2);
    broker.fill_pending_market_entries(3, 30, 120.0);
    assert_eq!(broker.closed_trade_count(), 1);
    assert_eq!(broker.trades[0].id, "A");
    assert_eq!(
        broker
            .trade_ledger
            .open_at(0)
            .map(|trade| trade.id.as_str()),
        Some("B")
    );
    assert_eq!(broker.position_size, 1.0);
    broker.assert_ledger_aggregates();
}
