use std::{
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
};

use pine_runtime::{
    Bar, BarUpdate, HistoricalRuntime, PineValue, RealtimeRuntime, ResourceLimits, RuntimeChanges,
    RuntimeError, public_runtime_result_json,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn program() -> pine_ir::HirProgram {
    let source = concat!(
        "//@version=6\nindicator(\"intrabar preparation resources\")\n",
        "type Counter\n    varip int ticks\n    int regular\n",
        "var counter=Counter.new(0,0)\ncounter.ticks+=1\n",
        "a=array.new_float(int(close),0)\nplot(counter.ticks)\nplot(array.size(a))\n",
    );
    let analysis = analyze_source(&SourceFile::new("intrabar-resources.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.unwrap()
}

fn limit(values: usize) -> ResourceLimits {
    ResourceLimits {
        max_collection_bytes_per_bar: Some(values * size_of::<PineValue>()),
        ..ResourceLimits::default()
    }
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

fn apply_without_panic(
    runtime: &mut RealtimeRuntime<'_>,
    update: BarUpdate,
) -> Result<RuntimeChanges, RuntimeError> {
    match catch_unwind(AssertUnwindSafe(|| runtime.apply_update(update))) {
        Ok(result) => result,
        Err(_) => panic!("an intrabar resource failure must return an error, not panic"),
    }
}

type PublishedState = (
    String,
    String,
    Option<RuntimeChanges>,
    u64,
    Option<i64>,
    Option<i64>,
);

fn published_state(runtime: &RealtimeRuntime<'_>) -> PublishedState {
    (
        public_runtime_result_json(&runtime.result()),
        public_runtime_result_json(&runtime.confirmed_result()),
        runtime.last_changes().cloned(),
        runtime.revision(),
        runtime.last_confirmed_bar_time(),
        runtime.forming_bar_time(),
    )
}

fn ticks(runtime: &RealtimeRuntime<'_>) -> i64 {
    runtime.result().plots[0]
        .values
        .last()
        .unwrap()
        .as_i64()
        .unwrap()
}

#[test]
fn a_new_update_does_not_inherit_the_previous_bars_spent_allowance() {
    let hir = program();
    let mut runtime = RealtimeRuntime::new(&hir).with_resource_limits(limit(6));
    // Two constructor fields, one field write and three array elements spend
    // the exact allowance on the confirmed bar.
    runtime
        .seed_historical_without_output(&[bar(0, 3.0)])
        .unwrap();
    apply_without_panic(&mut runtime, BarUpdate::forming(bar(60_000, 2.0))).unwrap();
    assert_eq!(ticks(&runtime), 2);
    // Replacement: copy two committed fields, overlay one retained field,
    // write that field in the script, then allocate two array elements = six.
    apply_without_panic(&mut runtime, BarUpdate::forming(bar(60_000, 2.0))).unwrap();
    assert_eq!(ticks(&runtime), 3);
    apply_without_panic(&mut runtime, BarUpdate::confirmed(bar(60_000, 2.0))).unwrap();
    assert_eq!(ticks(&runtime), 4);
    assert_eq!(runtime.last_confirmed_bar_time(), Some(60_000));
}

#[test]
fn preparation_and_script_spend_one_allowance_without_refunding_the_overlay() {
    let hir = program();
    let mut runtime = RealtimeRuntime::new(&hir).with_resource_limits(limit(5));
    runtime
        .seed_historical_without_output(&[bar(0, 2.0)])
        .unwrap();
    apply_without_panic(&mut runtime, BarUpdate::forming(bar(60_000, 1.0))).unwrap();
    let before = published_state(&runtime);
    // The overlay spends three values and the script needs three more.
    let error =
        apply_without_panic(&mut runtime, BarUpdate::forming(bar(60_000, 2.0))).unwrap_err();
    assert!(
        error.message.contains("E_RESOURCE_BUDGET"),
        "{}",
        error.message
    );
    assert_eq!(published_state(&runtime), before);
    // The failed attempt consumed no published varip update. Retry needs five
    // values, exactly the fresh allowance, and advances ticks only once.
    apply_without_panic(&mut runtime, BarUpdate::forming(bar(60_000, 1.0))).unwrap();
    assert_eq!(ticks(&runtime), 3);
}

#[test]
fn an_overlay_budget_failure_returns_an_error_and_the_session_can_retry() {
    let hir = program();
    let mut runtime = RealtimeRuntime::new(&hir).with_resource_limits(limit(6));
    runtime
        .seed_historical_without_output(&[bar(0, 2.0)])
        .unwrap();
    apply_without_panic(&mut runtime, BarUpdate::forming(bar(60_000, 1.0))).unwrap();
    runtime = runtime.with_resource_limits(limit(2));
    let before = published_state(&runtime);
    // Copying the committed two-field object fits; copying the retained field
    // does not. This fails during preparation, before script execution.
    let error =
        apply_without_panic(&mut runtime, BarUpdate::confirmed(bar(60_000, 1.0))).unwrap_err();
    assert!(
        error.message.contains("E_RESOURCE_BUDGET"),
        "{}",
        error.message
    );
    assert_eq!(published_state(&runtime), before);
    runtime = runtime.with_resource_limits(limit(5));
    apply_without_panic(&mut runtime, BarUpdate::confirmed(bar(60_000, 1.0))).unwrap();
    assert_eq!(ticks(&runtime), 3);
    assert_eq!(runtime.forming_bar_time(), None);
}

#[test]
fn ordinary_historical_append_still_resets_once_for_each_bar() {
    let hir = program();
    let mut runtime = HistoricalRuntime::new(&hir).with_resource_limits(limit(6));
    for time in [0, 60_000, 120_000] {
        runtime.append_bar(bar(time, 3.0)).unwrap();
    }
    assert_eq!(
        runtime.result().plots[0].values,
        vec![PineValue::Int(1), PineValue::Int(2), PineValue::Int(3)]
    );
}
