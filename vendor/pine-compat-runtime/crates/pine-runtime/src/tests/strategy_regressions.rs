use pine_syntax::SourceFile;

use super::*;

fn run_strategy(source_name: &str, source: &str, bars: &[Bar]) -> StrategyResult {
    let source = SourceFile::new(source_name, source);
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    run_historical(&analysis.hir.expect("HIR"), bars)
        .expect("strategy regression should run")
        .strategy
        .expect("strategy output")
}

#[test]
fn entry_when_false_skips_order() {
    let strategy = run_strategy(
        "strategy_entry_when.pine",
        include_str!("../../../../tests/fixtures/runtime/strategy_entry_when.pine"),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    );

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].id, "Take");
    assert_eq!(strategy.orders[0].direction, "strategy.long");
    assert_eq!(strategy.position.last().map(|row| row.size), Some(1.0));
}

#[test]
fn market_short_order_uses_default_qty_when_omitted() {
    let strategy = run_strategy(
        "strategy_order_default_quantity_short.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_order_default_quantity_short.pine"
        ),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    );

    assert_eq!(strategy.orders.len(), 1);
    assert_eq!(strategy.orders[0].direction, "strategy.short");
    assert_eq!(strategy.orders[0].qty, 1.0);
    assert_eq!(strategy.position.last().map(|row| row.size), Some(-1.0));
}

#[test]
fn market_short_order_default_qty_reduces_long() {
    let strategy = run_strategy(
        "strategy_order_default_quantity_short_reduce_long.pine",
        include_str!(
            "../../../../tests/fixtures/runtime/strategy_order_default_quantity_short_reduce_long.pine"
        ),
        &[bar(1.0), bar(2.0), bar(3.0), bar(4.0)],
    );

    assert!(strategy.orders.len() >= 2);
    assert_eq!(strategy.position.last().map(|row| row.size), Some(0.0));
}

#[test]
fn stale_trade_key_exit_does_not_create_a_ghost_close() {
    let strategy = run_strategy(
        "strategy_stale_trade_key_exit.pine",
        include_str!("../../../../tests/fixtures/regressions/strategy_stale_trade_key_exit.pine"),
        &[
            bar(1.0),
            bar(2.0),
            bar(3.0),
            bar(4.0),
            bar(5.0),
            bar_ohlc(6.0, 12.0, 6.0, 6.0),
            bar_ohlc(7.0, 13.0, 7.0, 7.0),
        ],
    );

    assert_eq!(strategy.orders.len(), 4);
    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 5.0);
    assert_eq!(strategy.trades[0].profit, 3.0);
    assert_eq!(strategy.trades[1].entry_price, 3.0);
    assert_eq!(strategy.trades[1].exit_price, 13.0);
    assert_eq!(strategy.trades[1].profit, 10.0);
    assert_eq!(
        strategy.equity.last().map(|value| value.cash),
        Some(1000013.0)
    );
    assert_eq!(strategy.position.last().map(|value| value.size), Some(0.0));
}

#[test]
fn market_fill_loop_preserves_other_pending_entries() {
    let strategy = run_strategy(
        "strategy_multiple_market_entries.pine",
        include_str!(
            "../../../../tests/fixtures/regressions/strategy_multiple_market_entries.pine"
        ),
        &[bar(10.0), bar(10.0), bar_ohlc(10.0, 10.0, 4.0, 10.0)],
    );

    assert_eq!(strategy.orders.len(), 3);
    assert_eq!(strategy.orders[0].id, "A");
    assert_eq!(strategy.orders[1].id, "B");
    assert_eq!(strategy.orders[2].id, "C");
    assert_eq!(strategy.orders[0].bar_index, 1);
    assert_eq!(strategy.orders[1].bar_index, 1);
    assert_eq!(strategy.orders[2].bar_index, 2);
    assert_eq!(strategy.orders[2].price, 5.0);
    assert_eq!(strategy.position.last().map(|value| value.size), Some(3.0));
}

#[test]
fn close_same_id_records_each_entry_allocation() {
    let strategy = run_strategy(
        "strategy_close_same_id_allocations.pine",
        include_str!(
            "../../../../tests/fixtures/regressions/strategy_close_same_id_allocations.pine"
        ),
        &[bar(1.0), bar(2.0), bar(3.0), bar(5.0)],
    );

    assert_eq!(strategy.trades.len(), 2);
    assert_eq!(strategy.trades[0].entry_price, 2.0);
    assert_eq!(strategy.trades[0].exit_price, 5.0);
    assert_eq!(strategy.trades[0].qty, 1.0);
    assert_eq!(strategy.trades[0].profit, 3.0);
    assert_eq!(strategy.trades[1].entry_price, 3.0);
    assert_eq!(strategy.trades[1].exit_price, 5.0);
    assert_eq!(strategy.trades[1].qty, 1.0);
    assert_eq!(strategy.trades[1].profit, 2.0);
    assert_eq!(
        strategy.equity.last().map(|value| value.cash),
        Some(1000005.0)
    );
    assert_eq!(
        strategy.equity.last().map(|value| value.net_profit),
        Some(5.0)
    );
}
