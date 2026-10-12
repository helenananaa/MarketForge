use pine_runtime::{Bar, HistoricalRuntime, RealtimeRuntime, public_runtime_result_json};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

#[test]
fn timestamped_seed_retains_batch_context_and_failure_atomicity() {
    let analysis = analyze_source(&SourceFile::new(
        "clock.pine",
        "//@version=6\nindicator(\"clock\")\nplot(timenow)\nplot(timenow[1])\nplot(last_bar_index)\n",
    ));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.unwrap();
    let bars: Vec<_> = (1..=2)
        .map(|n| Bar {
            time: n * 60000,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
        })
        .collect();
    let mut realtime = RealtimeRuntime::new(&hir);
    let empty = public_runtime_result_json(&realtime.result());
    assert!(
        realtime
            .seed_historical_with_execution_times(&bars, &[1000])
            .is_err()
    );
    assert_eq!(public_runtime_result_json(&realtime.result()), empty);
    assert!(realtime.seed_historical(&bars).is_err());
    assert_eq!(public_runtime_result_json(&realtime.result()), empty);
    let result = realtime
        .seed_historical_with_execution_times(&bars, &[1000, 2000])
        .unwrap();
    let mut historical = HistoricalRuntime::new(&hir);
    historical
        .append_bars_with_execution_times(&bars, &[1000, 2000])
        .unwrap();
    assert_eq!(
        public_runtime_result_json(&result),
        public_runtime_result_json(&historical.result())
    );
    assert_eq!(
        result.plots[2]
            .values
            .iter()
            .map(|v| v.as_i64())
            .collect::<Vec<_>>(),
        [Some(1), Some(1)]
    );
}
