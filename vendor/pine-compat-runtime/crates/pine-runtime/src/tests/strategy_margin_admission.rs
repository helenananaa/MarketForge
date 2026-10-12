use super::*;

#[test]
fn long_stop_admission_cannot_borrow_margin_from_a_later_exit() {
    for (capital, expected_trades) in [(150, 1), (300, 2)] {
        let source = pine_syntax::SourceFile::new(
            "margin_admission.pine",
            format!(
                r#"//@version=6
strategy("Admission", initial_capital={capital}, margin_long=100, pyramiding=4)
if bar_index == 0
    strategy.entry("BASE", strategy.long, qty=1)
if bar_index == 2
    strategy.entry("NEXT", strategy.long, qty=1, stop=110)
if bar_index == 3
    strategy.close("BASE")
if bar_index == 6
    strategy.close_all()
plot(strategy.position_size)
"#
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.expect("HIR");
        let bars = [
            bar(100.0),
            bar(100.0),
            bar(100.0),
            bar(100.0),
            bar(100.0),
            bar_ohlc(100.0, 115.0, 99.0, 112.0),
            bar(112.0),
            bar(112.0),
        ];
        let result = run_historical(&hir, &bars).expect("historical run");
        let trades = &result.strategy.as_ref().expect("strategy").trades;
        assert_eq!(
            trades.len(),
            expected_trades,
            "capital={capital}: {trades:?}"
        );
        assert_eq!(trades[0].id, "BASE");
    }
}

#[test]
fn long_stop_can_be_created_after_occupied_margin_is_released() {
    let source = pine_syntax::SourceFile::new(
        "released_margin.pine",
        r#"//@version=6
strategy("Released", initial_capital=150, margin_long=100, pyramiding=4)
if bar_index == 0
    strategy.entry("BASE", strategy.long, qty=1)
if bar_index == 2
    strategy.close("BASE")
if bar_index == 3
    strategy.entry("NEXT", strategy.long, qty=1, stop=110)
if bar_index == 5
    strategy.close_all()
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.expect("HIR");
    let result = run_historical(
        &hir,
        &[
            bar(100.0),
            bar(100.0),
            bar(100.0),
            bar(100.0),
            bar_ohlc(100.0, 115.0, 99.0, 112.0),
            bar(112.0),
            bar(112.0),
        ],
    )
    .expect("historical run");
    assert_eq!(result.strategy.expect("strategy").trades.len(), 2);
}
