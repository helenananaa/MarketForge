use pine_runtime::{
    Bar, BarUpdate, HistoricalRuntime, PineValue, RealtimeRuntime, public_runtime_result_json,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

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

const SOURCE: &str =
    "//@version=6\nindicator(\"plots\")\nplot(close, color=close>10 ? color.red : color.blue)\n";

#[test]
fn forked_runtime_and_returned_plot_mutations_are_independent() {
    let hir = analyze_source(&SourceFile::new("plots.pine", SOURCE))
        .hir
        .unwrap();
    let mut original = HistoricalRuntime::new(&hir);
    original.append_bar(bar(0, 10.0)).unwrap();
    let before = public_runtime_result_json(&original.result());
    let mut fork = original.clone();
    fork.append_bar(bar(60000, 20.0)).unwrap();
    assert_eq!(public_runtime_result_json(&original.result()), before);
    assert_eq!(fork.result().plots[0].values.len(), 2);
    let fork_before = public_runtime_result_json(&fork.result());
    let mut caller = fork.result();
    caller.plots[0].values[0] = PineValue::Float(999.0);
    caller.plots[0].colors.clear();
    caller.plots[0].metadata.title = PineValue::String("caller changed".into());
    assert_eq!(public_runtime_result_json(&fork.result()), fork_before);
    assert_eq!(public_runtime_result_json(&original.result()), before);
}

#[test]
fn forming_replacement_retains_previous_results_and_confirmed_colors() {
    let hir = analyze_source(&SourceFile::new("plots.pine", SOURCE))
        .hir
        .unwrap();
    let mut runtime = RealtimeRuntime::new(&hir);
    let seed = runtime.seed_historical(&[bar(0, 10.0)]).unwrap();
    let first = runtime
        .update(BarUpdate::forming(bar(60000, 20.0)))
        .unwrap();
    let first_copy = public_runtime_result_json(&first);
    let replacement = runtime.update(BarUpdate::forming(bar(60000, 5.0))).unwrap();
    assert_eq!(
        public_runtime_result_json(&runtime.confirmed_result()),
        public_runtime_result_json(&seed)
    );
    assert_ne!(first.plots[0].colors[1], replacement.plots[0].colors[1]);
    let final_result = runtime
        .update(BarUpdate::confirmed(bar(60000, 5.0)))
        .unwrap();
    assert_eq!(final_result.plots, replacement.plots);
    assert_eq!(public_runtime_result_json(&first), first_copy);
    assert_eq!(final_result.plots[0].values.len(), 2);
}
