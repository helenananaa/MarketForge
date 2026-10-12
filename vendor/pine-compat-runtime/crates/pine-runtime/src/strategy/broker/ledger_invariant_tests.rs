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
fn ledger_aggregates_match_when_flat() {
    let broker = BrokerState::new(100_000.0);
    broker.assert_ledger_aggregates();
    assert_eq!(
        broker.trade_ledger.computed_net_position(),
        NetPosition::default()
    );
}

#[test]
fn ledger_aggregates_match_after_long_and_short_entries() {
    let mut long_broker = BrokerState::new(100_000.0);
    assert!(long_broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    long_broker.assert_ledger_aggregates();

    let mut short_broker = BrokerState::new(100_000.0);
    assert!(short_broker.entry_short("S".to_owned(), 0, 10, 100.0, 2.0));
    short_broker.assert_ledger_aggregates();
}

#[test]
fn ledger_aggregates_match_after_pyramided_same_side_and_partial_flatten() {
    let mut broker = pyramiding_broker(2);
    broker.place_pending_market_long_entry("L1".to_owned(), 1.0, 0);
    broker.fill_pending_market_entries(1, 10, 100.0);
    broker.assert_ledger_aggregates();
    broker.place_pending_market_long_entry("L2".to_owned(), 2.0, 1);
    broker.fill_pending_market_entries(2, 20, 110.0);
    broker.assert_ledger_aggregates();
    broker.close_long_qty("L1".to_owned(), 3, 30, 120.0, 0.5);
    broker.assert_ledger_aggregates();
    broker.close_all_position(4, 40, 115.0);
    broker.assert_ledger_aggregates();
    assert_eq!(
        broker.trade_ledger.computed_net_position(),
        NetPosition::default()
    );
}

#[test]
fn ledger_aggregates_match_after_reversal_reduce_only_exit_and_margin_call() {
    let mut reversal = BrokerState::new(100_000.0);
    assert!(reversal.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    assert!(reversal.entry_short("S".to_owned(), 1, 20, 110.0, 1.0));
    reversal.assert_ledger_aggregates();

    let mut reduce = BrokerState::new(100_000.0);
    assert!(reduce.entry_long("E".to_owned(), 0, 10, 100.0, 1.0));
    reduce.place_pending_market_short_order("R".to_owned(), 3.0, 1);
    reduce.fill_pending_market_entries(2, 20, 110.0);
    reduce.assert_ledger_aggregates();

    let mut exit_broker = BrokerState::new(100_000.0);
    assert!(exit_broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    exit_broker.place_exit_stop("XL".to_owned(), "L".to_owned(), 95.0, 0);
    exit_broker.evaluate_pending_exits(1, 20, 100.0, 94.0);
    exit_broker.assert_ledger_aggregates();

    let mut margin = margin_broker(165.0, 25.0);
    assert!(margin.entry_long("L".to_owned(), 1, 20, 4.0, 100.0));
    margin.assert_ledger_aggregates();
    margin.evaluate_margin_call_long(1, 20, 3.0);
    margin.assert_ledger_aggregates();
}

#[test]
fn ledger_aggregates_match_after_price_based_entry_and_generic_order() {
    let mut entry = BrokerState::new(100_000.0);
    entry.place_pending_limit_long_entry("L".to_owned(), 2.0, 100.0, 0);
    entry.fill_pending_limit_long_entries(2, 30, 99.0);
    entry.assert_ledger_aggregates();

    let mut order = BrokerState::new(100_000.0);
    assert!(order.entry_long("E".to_owned(), 0, 10, 100.0, 1.0));
    order.place_pending_limit_long_order("O".to_owned(), 2.0, 90.0, 1);
    order.fill_pending_limit_long_entries(2, 20, 89.0);
    order.assert_ledger_aggregates();
}
