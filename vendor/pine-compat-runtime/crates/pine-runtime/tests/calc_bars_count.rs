use pine_runtime::{Bar, HistoricalRuntime, run_historical};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn bar(index: i64) -> Bar {
    Bar {
        time: (1_609_459_200 + index * 86_400) * 1_000,
        open: index as f64,
        high: index as f64,
        low: index as f64,
        close: index as f64,
        volume: 1.0,
    }
}

#[test]
fn batch_window_restarts_index_and_history_then_accepts_new_bars() {
    let source = SourceFile::new(
        "calc_bars_count.pine",
        "//@version=6\nindicator(\"window\", calc_bars_count=3)\nplot(bar_index)\nplot(close)\nplot(close[1])\nplot(timeframe.change(\"7M\") ? 1 : 0)\n",
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let program = analysis.hir.expect("HIR");
    assert_eq!(program.calc_bars_count, Some(3));
    let bars: Vec<_> = (0..5).map(bar).collect();
    let result = run_historical(&program, &bars).expect("batch result");
    let values = |index: usize| {
        result.plots[index]
            .values
            .iter()
            .map(|value| value.as_f64())
            .collect::<Vec<_>>()
    };
    assert_eq!(values(0), [Some(0.0), Some(1.0), Some(2.0)]);
    assert_eq!(values(1), [Some(2.0), Some(3.0), Some(4.0)]);
    assert_eq!(values(2), [None, Some(2.0), Some(3.0)]);
    assert_eq!(values(3), [Some(0.0), Some(0.0), Some(0.0)]);

    let mut incremental = HistoricalRuntime::new(&program);
    incremental.append_bars(&bars).expect("initial batch");
    incremental.append_bar(bar(5)).expect("next bar");
    let values = incremental.result().plots[0]
        .values
        .iter()
        .map(|value| value.as_f64())
        .collect::<Vec<_>>();
    assert_eq!(values, [Some(0.0), Some(1.0), Some(2.0), Some(3.0)]);
}

#[test]
fn zero_and_invalid_count_are_explicit() {
    let zero = analyze_source(&SourceFile::new(
        "zero.pine",
        "//@version=6\nindicator(\"all\", calc_bars_count=0)\nplot(bar_index)\n",
    ));
    assert!(zero.diagnostics.is_empty(), "{:?}", zero.diagnostics);
    let result = run_historical(&zero.hir.unwrap(), &(0..5).map(bar).collect::<Vec<_>>()).unwrap();
    assert_eq!(result.plots[0].values.len(), 5);
    let negative = analyze_source(&SourceFile::new(
        "negative.pine",
        "//@version=6\nindicator(\"bad\", calc_bars_count=-1)\nplot(close)\n",
    ));
    assert!(
        negative
            .diagnostics
            .iter()
            .any(|diag| diag.code == "E_CALL_ARG_VALUE")
    );
}
