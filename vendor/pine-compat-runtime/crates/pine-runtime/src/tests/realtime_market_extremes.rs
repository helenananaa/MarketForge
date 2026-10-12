use super::*;

#[test]
fn realtime_market_orders_use_only_new_one_sided_extremes() {
    for direction in ["long", "short"] {
        for (high, low, close, expected) in [
            (106.0, 95.0, 101.0, 106.0),
            (105.0, 94.0, 99.0, 94.0),
            (105.0, 95.0, 101.0, 101.0),
            (106.0, 94.0, 101.0, 101.0),
        ] {
            let source = pine_syntax::SourceFile::new(
                "market-extreme.pine",
                format!(
                    r#"//@version=6
strategy("market", calc_on_every_tick=true)
varip int n = 0
if barstate.isrealtime
    n += 1
    if n == 1
        strategy.entry("T", strategy.{direction}, qty=1)
    if n == 2
        strategy.close("T")
plot(strategy.opentrades.entry_price(0))
plot(strategy.closedtrades.exit_price(0))
"#
                ),
            );
            let hir = analyze_source(&source).hir.unwrap();
            let mut runtime = RealtimeRuntime::new(&hir);
            runtime.update(BarUpdate::historical(bar(100.0))).unwrap();
            let mut first = bar_ohlc(100.0, 105.0, 95.0, 100.0);
            first.time = 60_000;
            runtime.update(BarUpdate::forming(first)).unwrap();
            let next = Bar {
                high,
                low,
                close,
                ..first
            };
            let entered = runtime.update(BarUpdate::forming(next)).unwrap();
            assert_eq!(entered.plots[0].values[0], PineValue::Na);
            assert_values_close(&entered.plots[0].values[1..], &[expected]);
            // Repeated cumulative extremes must not become new fill prices.
            let exited = runtime.update(BarUpdate::forming(next)).unwrap();
            assert_eq!(exited.plots[1].values[0], PineValue::Na);
            assert_values_close(&exited.plots[1].values[1..], &[close]);
        }
    }
}
