use super::*;

fn pyramiding_broker(pyramiding_limit: usize) -> BrokerState {
    BrokerState::new_with_account_settings_and_pyramiding(
        100_000.0,
        None,
        0.0,
        0.0,
        StrategyMarginSetting::default(),
        StrategyMarginSetting::default(),
        pyramiding_limit,
    )
}

fn margin_broker(initial_capital: f64, margin_long: f64) -> BrokerState {
    BrokerState::new_with_account_settings(
        initial_capital,
        None,
        0.0,
        0.0,
        StrategyMarginSetting::explicit(margin_long),
        StrategyMarginSetting::default(),
    )
}

#[test]
fn characterization_same_side_market_entry_adds_open_trade_under_pyramiding() {
    let mut broker = pyramiding_broker(2);

    broker.place_pending_market_long_entry("L1".to_owned(), 1.0, 0);
    broker.fill_pending_market_entries(1, 10, 100.0);
    broker.place_pending_market_long_entry("L2".to_owned(), 2.0, 1);
    broker.fill_pending_market_entries(2, 20, 110.0);

    assert_eq!(broker.orders.len(), 2);
    assert_eq!(broker.orders[0].id, "L1");
    assert_eq!(broker.orders[0].direction, "strategy.long");
    assert_eq!(broker.orders[0].qty, 1.0);
    assert_eq!(broker.orders[0].price, 100.0);
    assert_eq!(broker.orders[1].id, "L2");
    assert_eq!(broker.orders[1].direction, "strategy.long");
    assert_eq!(broker.orders[1].qty, 2.0);
    assert_eq!(broker.orders[1].price, 110.0);
    assert_eq!(broker.position_size, 3.0);
    assert_eq!(broker.avg_price, 320.0 / 3.0);
    assert_eq!(broker.open_trade_count(), 2);
    assert_eq!(broker.trade_ledger.open_count(), 2);
    assert_eq!(broker.cash, 99_680.0);
    assert!(broker.diagnostics.is_empty());
}

#[test]
fn characterization_market_entry_reversal_flattens_then_opens_opposite() {
    let mut broker = BrokerState::new(100_000.0);

    broker.place_pending_market_long_entry("L".to_owned(), 2.0, 0);
    broker.fill_pending_market_entries(1, 10, 100.0);
    broker.place_pending_market_short_entry("S".to_owned(), 1.0, 1);
    broker.fill_pending_market_entries(2, 20, 110.0);

    assert_eq!(broker.position_size, -1.0);
    assert_eq!(broker.avg_price, 110.0);
    assert_eq!(broker.open_trade_count(), 1);
    assert_eq!(broker.closed_trade_count(), 1);
    assert_eq!(broker.trades[0].id, "L");
    assert_eq!(broker.trades[0].qty, 2.0);
    assert_eq!(broker.trades[0].profit, 20.0);
    assert_eq!(broker.orders.len(), 2);
    assert_eq!(broker.orders[0].direction, "strategy.long");
    assert_eq!(broker.orders[1].id, "S");
    assert_eq!(broker.orders[1].direction, "strategy.short");
    assert_eq!(broker.orders[1].qty, 3.0);
    assert_eq!(broker.cash, 100_130.0);
    assert_eq!(broker.max_contracts_held_long(), 2.0);
    assert_eq!(broker.max_contracts_held_short(), 1.0);
    assert!(broker.diagnostics.is_empty());
}

#[test]
fn characterization_same_side_open_applies_transition_cash_once() {
    use super::fill_transition::{
        FillRequest, FillTriggerReason, PositionSnapshot, calculate_same_side_addition,
    };
    use super::types::InternalOrderKey;

    let mut broker = BrokerState::new(100_000.0);
    let before_cash = broker.cash;
    let snapshot = PositionSnapshot {
        signed_size: broker.position_size,
        avg_price: broker.avg_price,
    };
    let expected = calculate_same_side_addition(
        &snapshot,
        FillRequest {
            order_key: InternalOrderKey(0),
            bar_index: 1,
            time: 10,
            raw_price: 100.0,
            trigger_reason: FillTriggerReason::Market,
        },
        2.0,
        100.0,
        0.0,
    )
    .expect("same-side transition");

    assert!(broker.entry_long("L".to_owned(), 1, 10, 100.0, 2.0));

    assert_eq!(broker.cash, before_cash + expected.cash_delta);
    assert_eq!(broker.cash, 99_800.0);
    broker.assert_ledger_aggregates();
}

#[test]
fn characterization_same_side_market_generic_order_bypasses_pyramiding() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("E".to_owned(), 0, 10, 100.0, 1.0));

    broker.place_pending_market_long_order("O".to_owned(), 2.0, 1);
    broker.fill_pending_market_entries(2, 20, 110.0);

    assert_eq!(broker.orders.len(), 2);
    assert_eq!(broker.orders[1].id, "O");
    assert_eq!(broker.orders[1].direction, "strategy.long");
    assert_eq!(broker.orders[1].qty, 2.0);
    assert_eq!(broker.orders[1].price, 110.0);
    assert_eq!(broker.position_size, 3.0);
    assert_eq!(broker.open_trade_count(), 2);
    assert_eq!(broker.trade_ledger.open_count(), 2);
    assert!(broker.diagnostics.is_empty());
}

#[test]
fn characterization_reduce_only_applies_shared_reduction_cash() {
    use super::fill_transition::{
        FillRequest, FillTriggerReason, PositionSnapshot, calculate_reduce_only,
    };
    use super::types::InternalOrderKey;

    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("E".to_owned(), 0, 10, 100.0, 1.0));
    let before_cash = broker.cash;
    let snapshot = PositionSnapshot {
        signed_size: broker.position_size,
        avg_price: broker.avg_price,
    };
    let allocations = broker.trade_ledger.allocate_exit_fifo(None, 1.0);
    let expected = calculate_reduce_only(
        &snapshot,
        FillRequest {
            order_key: InternalOrderKey(0),
            bar_index: 2,
            time: 20,
            raw_price: 110.0,
            trigger_reason: FillTriggerReason::Market,
        },
        -1.0,
        110.0,
        0.0,
        allocations,
    )
    .expect("reduce-only transition");

    broker.place_pending_market_short_order("R".to_owned(), 1.0, 1);
    broker.fill_pending_market_entries(2, 20, 110.0);

    assert_eq!(broker.cash, before_cash + expected.cash_delta);
    assert_eq!(broker.position_size, 0.0);
    assert_eq!(expected.close_quantity, 1.0);
    broker.assert_ledger_aggregates();
}

#[test]
fn characterization_market_generic_order_crosses_zero_with_full_order_quantity() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("E".to_owned(), 0, 10, 100.0, 1.0));

    broker.place_pending_market_short_order("R".to_owned(), 3.0, 1);
    broker.fill_pending_market_entries(2, 20, 110.0);

    assert_eq!(broker.orders.len(), 2);
    assert_eq!(broker.orders[1].id, "R");
    assert_eq!(broker.orders[1].direction, "strategy.short");
    assert_eq!(broker.orders[1].qty, 3.0);
    assert_eq!(broker.orders[1].price, 110.0);
    assert_eq!(broker.position_size, -2.0);
    assert_eq!(broker.open_trade_count(), 1);
    assert_eq!(broker.closed_trade_count(), 1);
    assert_eq!(broker.trades[0].id, "E");
    assert_eq!(broker.trades[0].exit_id, "R");
    assert_eq!(broker.trades[0].profit, 10.0);
    assert_eq!(broker.trade_ledger.open_count(), 1);
    assert!(broker.diagnostics.is_empty());
    broker.assert_ledger_aggregates();
}

#[test]
fn characterization_price_based_entry_fills_at_limit_on_later_bar() {
    let mut broker = BrokerState::new(100_000.0);

    broker.place_pending_limit_long_entry("L".to_owned(), 2.0, 100.0, 0);
    broker.fill_pending_limit_long_entries(1, 20, 101.0);
    assert_eq!(broker.position_size, 0.0);
    assert!(broker.orders.is_empty());

    broker.fill_pending_limit_long_entries(2, 30, 99.0);

    assert_eq!(broker.orders.len(), 1);
    assert_eq!(broker.orders[0].id, "L");
    assert_eq!(broker.orders[0].direction, "strategy.long");
    assert_eq!(broker.orders[0].qty, 2.0);
    assert_eq!(broker.orders[0].price, 100.0);
    assert_eq!(broker.position_size, 2.0);
    assert_eq!(broker.avg_price, 100.0);
    assert_eq!(broker.open_trade_count(), 1);
    assert!(broker.diagnostics.is_empty());
}

#[test]
fn characterization_price_based_generic_order_adds_same_side_without_pyramiding() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("E".to_owned(), 0, 10, 100.0, 1.0));

    broker.place_pending_limit_long_order("O".to_owned(), 2.0, 90.0, 1);
    broker.fill_pending_limit_long_entries(2, 20, 89.0);

    assert_eq!(broker.orders.len(), 2);
    assert_eq!(broker.orders[1].id, "O");
    assert_eq!(broker.orders[1].direction, "strategy.long");
    assert_eq!(broker.orders[1].qty, 2.0);
    assert_eq!(broker.orders[1].price, 90.0);
    assert_eq!(broker.position_size, 3.0);
    assert_eq!(broker.avg_price, 280.0 / 3.0);
    assert_eq!(broker.open_trade_count(), 2);
    assert_eq!(broker.cash, 99_720.0);
    assert!(broker.diagnostics.is_empty());
}

#[test]
fn characterization_full_close_flattens_matching_entry() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));

    broker.close_long("L".to_owned(), 1, 20, 110.0);

    assert_eq!(broker.position_size, 0.0);
    assert_eq!(broker.avg_price, 0.0);
    assert_eq!(broker.open_trade_count(), 0);
    assert_eq!(broker.closed_trade_count(), 1);
    assert_eq!(broker.trades[0].id, "L");
    assert_eq!(broker.trades[0].qty, 2.0);
    assert_eq!(broker.trades[0].exit_price, 110.0);
    assert_eq!(broker.trades[0].profit, 20.0);
    assert_eq!(broker.cash, 100_020.0);
    assert_eq!(broker.trade_ledger.open_count(), 0);
    assert!(broker.diagnostics.is_empty());
}

#[test]
fn characterization_partial_close_keeps_remaining_quantity_and_average() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));

    broker.close_long_qty("L".to_owned(), 1, 20, 110.0, 0.75);

    assert_eq!(broker.position_size, 1.25);
    assert_eq!(broker.avg_price, 100.0);
    assert_eq!(broker.open_trade_count(), 1);
    assert_eq!(broker.closed_trade_count(), 1);
    assert_eq!(broker.trades[0].qty, 0.75);
    assert_eq!(broker.trades[0].profit, 7.5);
    assert_eq!(broker.trade_ledger.net_position().signed_size, 1.25);
    assert!(broker.diagnostics.is_empty());
}

#[test]
fn characterization_exit_fill_closes_from_pending_stop() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    broker.place_exit_stop("XL".to_owned(), "L".to_owned(), 95.0, 0);

    broker.evaluate_pending_exits(0, 10, 100.0, 90.0);
    assert_eq!(broker.position_size, 2.0);
    assert!(broker.trades.is_empty());

    broker.evaluate_pending_exits(1, 20, 100.0, 94.0);

    assert_eq!(broker.orders.len(), 2);
    assert_eq!(broker.orders[1].id, "XL");
    assert_eq!(broker.orders[1].direction, "strategy.exit");
    assert_eq!(broker.orders[1].price, 95.0);
    assert_eq!(broker.trades.len(), 1);
    assert_eq!(broker.trades[0].id, "L");
    assert_eq!(broker.trades[0].exit_id, "XL");
    assert_eq!(broker.trades[0].exit_price, 95.0);
    assert_eq!(broker.trades[0].profit, -10.0);
    assert_eq!(broker.position_size, 0.0);
    assert_eq!(broker.open_trade_count(), 0);
    assert!(broker.diagnostics.is_empty());
}

#[test]
fn characterization_margin_call_fill_partially_liquidates_long() {
    let mut broker = margin_broker(165.0, 25.0);
    assert!(broker.entry_long("L".to_owned(), 1, 20, 4.0, 100.0));
    broker.update_open_trade_extremes(4.0, 3.0);

    broker.evaluate_margin_call_long(1, 20, 3.0);

    assert_eq!(broker.orders.len(), 2);
    assert_eq!(broker.orders[1].id, "Margin Call");
    assert_eq!(broker.orders[1].direction, "strategy.short");
    assert_eq!(broker.orders[1].qty, 52.0);
    assert_eq!(broker.orders[1].price, 3.0);
    assert_eq!(broker.trades.len(), 1);
    assert_eq!(broker.trades[0].id, "L");
    assert_eq!(broker.trades[0].exit_id, "Margin Call");
    assert_eq!(broker.trades[0].qty, 52.0);
    assert_eq!(broker.trades[0].profit, -52.0);
    assert_eq!(broker.position_size, 48.0);
    assert_eq!(broker.avg_price, 4.0);
    assert_eq!(broker.cash, -79.0);
    assert_eq!(broker.open_trade_count(), 1);
    assert_eq!(broker.closed_trade_count(), 1);
    assert_eq!(
        broker
            .trade_ledger
            .open_trade()
            .expect("open trade after partial margin call")
            .quantity,
        48.0
    );
    assert!(broker.diagnostics.is_empty());
}
