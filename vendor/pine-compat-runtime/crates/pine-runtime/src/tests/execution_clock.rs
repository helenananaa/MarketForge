use pine_syntax::SourceFile;

use super::*;

fn compile(source: &str) -> pine_ir::HirProgram {
    let analysis = pine_sema::analyze_source(&SourceFile::new("timenow.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.expect("timenow HIR")
}

fn timed_bar(time: i64) -> Bar {
    Bar {
        time,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 1.0,
    }
}

fn assert_plot_values(actual: &[PineValue], expected: &[PineValue]) {
    assert_eq!(actual, expected);
}

#[test]
fn historical_execution_times_are_series_values_with_history() {
    let program = compile("//@version=6\nindicator(\"clock\")\nplot(timenow)\nplot(timenow[1])\n");
    let bars = [timed_bar(10), timed_bar(20), timed_bar(30)];
    let result = run_historical_with_execution_times(&program, &bars, &[101, 205, 333])
        .expect("explicit execution timestamps should run");

    assert_plot_values(
        &result.plots[0].values,
        &[
            PineValue::Int(101),
            PineValue::Int(205),
            PineValue::Int(333),
        ],
    );
    assert_plot_values(
        &result.plots[1].values,
        &[PineValue::Na, PineValue::Int(101), PineValue::Int(205)],
    );
}

#[test]
fn missing_or_misaligned_execution_times_fail_closed() {
    let program = compile("//@version=4\nstudy(\"clock\")\nplot(timenow)\n");
    let bars = [timed_bar(10), timed_bar(20)];

    let missing = run_historical(&program, &bars)
        .expect_err("reading timenow without a host timestamp should fail");
    assert_eq!(
        missing.message,
        "timenow requires an explicit execution timestamp for this script execution"
    );

    let mut runtime = HistoricalRuntime::new(&program);
    let mismatch = runtime
        .append_bars_with_execution_times(&bars, &[101])
        .expect_err("timestamp count must match the batch");
    assert_eq!(
        mismatch.message,
        "execution timestamp count 1 does not match bar count 2"
    );
    assert_eq!(runtime.profile().bars, 0);
}

#[test]
fn an_unreached_timenow_read_does_not_require_host_input() {
    let program = compile("//@version=6\nindicator(\"clock\")\nplot(false ? timenow : 7)\n");
    let result = run_historical(&program, &[timed_bar(10)])
        .expect("unreached timenow branch should not require a timestamp");
    assert_plot_values(&result.plots[0].values, &[PineValue::Int(7)]);
}

#[test]
fn incremental_execution_time_matches_batch_execution() {
    let program = compile("//@version=4\nstudy(\"clock\")\nplot(timenow - time)\n");
    let bars = [timed_bar(100), timed_bar(200)];
    let batch = run_historical_with_execution_times(&program, &bars, &[150, 275])
        .expect("batch execution timestamps");

    let mut incremental = HistoricalRuntime::new(&program);
    incremental
        .append_bar_with_execution_time(bars[0], 150)
        .expect("first incremental execution timestamp");
    incremental
        .append_bar_with_execution_time(bars[1], 275)
        .expect("second incremental execution timestamp");

    assert_eq!(incremental.result(), batch);
}

#[test]
fn realtime_execution_time_recomputes_forming_updates_and_rolls_back_history() {
    let program = compile("//@version=6\nindicator(\"clock\")\nplot(timenow)\nplot(timenow[1])\n");
    let mut runtime = RealtimeRuntime::new(&program);

    let historical = runtime
        .update_with_execution_time(BarUpdate::historical(timed_bar(10)), 100)
        .expect("historical execution timestamp");
    assert_plot_values(&historical.plots[0].values, &[PineValue::Int(100)]);

    let first_forming = runtime
        .update_with_execution_time(BarUpdate::forming(timed_bar(20)), 200)
        .expect("first forming execution timestamp");
    assert_plot_values(
        &first_forming.plots[0].values,
        &[PineValue::Int(100), PineValue::Int(200)],
    );
    assert_plot_values(
        &first_forming.plots[1].values,
        &[PineValue::Na, PineValue::Int(100)],
    );

    let second_forming = runtime
        .update_with_execution_time(BarUpdate::forming(timed_bar(20)), 300)
        .expect("second forming execution timestamp");
    assert_plot_values(
        &second_forming.plots[0].values,
        &[PineValue::Int(100), PineValue::Int(300)],
    );
    assert_plot_values(
        &runtime.confirmed_result().plots[0].values,
        &[PineValue::Int(100)],
    );

    let confirmed = runtime
        .update_with_execution_time(BarUpdate::confirmed(timed_bar(20)), 400)
        .expect("confirmed execution timestamp");
    assert_plot_values(
        &confirmed.plots[0].values,
        &[PineValue::Int(100), PineValue::Int(400)],
    );
    assert_plot_values(
        &confirmed.plots[1].values,
        &[PineValue::Na, PineValue::Int(100)],
    );
}
