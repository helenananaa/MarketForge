use pine_runtime::{Bar, BarUpdate, PineValue, RealtimeRuntime};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

#[test]
fn owned_runtime_seeds_historical_bars_with_the_complete_dataset_endpoint() {
    let mut runtime = RealtimeRuntime::from_program(hir(r#"//@version=6
indicator("Dataset endpoint")
plot(last_bar_index)
plot(barstate.islast ? 1 : 0)
"#));

    let result = runtime
        .seed_historical(&[bar(60_000, 1.0), bar(120_000, 2.0)])
        .expect("historical seed should run");

    assert_values(&result.plots[0].values, &[1.0, 1.0]);
    assert_values(&result.plots[1].values, &[0.0, 1.0]);
}

#[test]
fn owned_runtime_preserves_realtime_rollback_and_varip_state() {
    let mut runtime = RealtimeRuntime::from_program(hir(r#"//@version=6
indicator("Rollback")
var float regular = 0.0
varip float intrabar = 0.0
regular += 1.0
intrabar += 1.0
plot(regular)
plot(intrabar)
"#));
    runtime
        .seed_historical(&[bar(60_000, 1.0)])
        .expect("historical seed should run");

    let first = runtime
        .update(BarUpdate::forming(bar(120_000, 2.0)))
        .expect("first forming update should run");
    let second = runtime
        .update(BarUpdate::forming(bar(120_000, 3.0)))
        .expect("replacement forming update should run");

    assert_values(&first.plots[0].values, &[1.0, 2.0]);
    assert_values(&first.plots[1].values, &[1.0, 2.0]);
    assert_values(&second.plots[0].values, &[1.0, 2.0]);
    assert_values(&second.plots[1].values, &[1.0, 3.0]);
    assert_values(&runtime.confirmed_result().plots[0].values, &[1.0]);
    assert_values(&runtime.confirmed_result().plots[1].values, &[1.0]);

    let confirmed = runtime
        .update(BarUpdate::confirmed(bar(120_000, 4.0)))
        .expect("confirmed update should run");
    assert_values(&confirmed.plots[0].values, &[1.0, 2.0]);
    assert_values(&confirmed.plots[1].values, &[1.0, 4.0]);
    assert_eq!(runtime.confirmed_result(), confirmed);
}

#[test]
fn owned_runtime_carries_empty_magnifier_input_across_seed_and_forming() {
    use pine_runtime::{MagnifierChartBarInput, MagnifierInput, magnifier_input_from_groups};

    let mut runtime = RealtimeRuntime::from_program(hir(r#"//@version=6
indicator("Magnifier ownership")
plot(close)
"#))
    .with_magnifier_input(MagnifierInput::new());
    assert!(runtime.magnifier_input().is_empty());

    let seeded = runtime.seed_historical(&[bar(60_000, 1.0)]).expect("seed");
    assert_eq!(seeded.plots[0].values.len(), 1);

    let forming = runtime
        .update(BarUpdate::forming(bar(120_000, 2.0)))
        .expect("forming with empty magnifier");
    assert_eq!(forming.plots[0].values.len(), 2);

    let input = magnifier_input_from_groups(vec![MagnifierChartBarInput {
        chart_bar_index: 0,
        bars: vec![bar(60_000, 1.5)],
    }])
    .expect("valid");
    let mut with_input = RealtimeRuntime::from_program(hir(r#"//@version=6
indicator("Magnifier ownership")
plot(close)
"#))
    .with_magnifier_input(input);
    with_input
        .seed_historical(&[bar(60_000, 1.0)])
        .expect("seed with inert magnifier");
    with_input
        .update(BarUpdate::forming(bar(120_000, 2.0)))
        .expect("forming must not consume historical magnifier");
}

#[test]
fn owned_runtime_rejects_out_of_range_magnifier_on_historical_seed() {
    use pine_runtime::{MagnifierChartBarInput, magnifier_input_from_groups};

    let input = magnifier_input_from_groups(vec![MagnifierChartBarInput {
        chart_bar_index: 2,
        bars: vec![bar(1, 1.0)],
    }])
    .expect("valid");
    let error = RealtimeRuntime::from_program(hir(r#"//@version=6
indicator("Magnifier range")
plot(close)
"#))
    .with_magnifier_input(input)
    .seed_historical(&[bar(60_000, 1.0)])
    .expect_err("out of range");
    assert!(
        error.message.contains("E_MAGNIFIER_CHART_BAR_RANGE"),
        "{}",
        error.message
    );
}

fn hir(source: &str) -> pine_ir::HirProgram {
    let source = SourceFile::new("<owned-realtime-test>", source);
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        analysis.diagnostics
    );
    analysis.hir.expect("source should lower to HIR")
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

fn assert_values(actual: &[PineValue], expected: &[f64]) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        let actual = actual
            .as_f64()
            .unwrap_or_else(|| panic!("expected numeric value, got {actual:?}"));
        assert!((actual - expected).abs() < 1e-10);
    }
}
