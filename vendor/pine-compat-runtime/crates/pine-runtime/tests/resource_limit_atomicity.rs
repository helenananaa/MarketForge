use pine_runtime::{
    Bar, BarUpdate, HistoricalRuntime, PineValue, RealtimeRuntime, public_runtime_result_json,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

const OVER_LIMIT: &str = r#"//@version=6
indicator("last-bar resource limit")
if bar_index >= 2
    while true
        x = close
plot(close)
"#;

#[test]
fn realtime_resource_limit_keeps_prior_results_and_session_state() {
    let mut runtime = RealtimeRuntime::from_program(hir(OVER_LIMIT));
    let seed = runtime
        .seed_historical(&[bar(60_000, 1.0), bar(120_000, 2.0)])
        .expect("prefix should run");
    let seed_json = public_runtime_result_json(&seed);
    assert_eq!(seed.plots[0].values.len(), 2);
    let before = public_runtime_result_json(&runtime.result());

    let error = runtime
        .update(BarUpdate::forming(bar(180_000, 3.0)))
        .expect_err("last-bar while loop should exceed the iteration ceiling");
    assert!(
        error.message.contains("exceeded maximum iteration"),
        "{}",
        error.message
    );
    assert_eq!(public_runtime_result_json(&runtime.result()), before);
    assert_eq!(
        public_runtime_result_json(&runtime.confirmed_result()),
        seed_json
    );
    assert_eq!(public_runtime_result_json(&seed), seed_json);

    let retry = runtime
        .update(BarUpdate::forming(bar(180_000, 4.0)))
        .expect_err("the session must still reject a later last-bar update");
    assert!(retry.message.contains("exceeded maximum iteration"));
    assert_eq!(
        public_runtime_result_json(&runtime.confirmed_result()),
        seed_json
    );
    assert_eq!(seed.plots[0].values.len(), 2);
}

#[test]
fn historical_resource_limit_does_not_commit_the_failed_bar_or_mutate_old_results() {
    let hir = hir(OVER_LIMIT);
    let mut runtime = HistoricalRuntime::new(&hir);
    runtime.append_bar(bar(60_000, 1.0)).expect("bar 0");
    runtime.append_bar(bar(120_000, 2.0)).expect("bar 1");
    let owned = runtime.result();
    let owned_json = public_runtime_result_json(&owned);
    let error = runtime
        .append_bar(bar(180_000, 3.0))
        .expect_err("bar 2 should hit the while-loop ceiling");
    assert!(error.message.contains("exceeded maximum iteration"));
    let after = runtime.result();
    assert_eq!(after.plots[0].values.len(), 2);
    assert_eq!(public_runtime_result_json(&owned), owned_json);
    let mut caller = owned;
    caller.plots[0].values[0] = PineValue::Float(999.0);
    assert_eq!(runtime.result().plots[0].values.len(), 2);
    assert_ne!(runtime.result().plots[0].values[0], PineValue::Float(999.0));
}

fn hir(source: &str) -> pine_ir::HirProgram {
    let analysis = analyze_source(&SourceFile::new("limit.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.expect("hir")
}

fn bar(time: i64, close: f64) -> Bar {
    Bar {
        time,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.0,
    }
}
