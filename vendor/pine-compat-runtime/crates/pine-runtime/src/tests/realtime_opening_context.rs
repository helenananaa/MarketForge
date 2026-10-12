use super::*;

#[test]
fn attaching_mid_bar_uses_host_opening_fact_without_resetting_varip() {
    let source = pine_syntax::SourceFile::new(
        "opening.pine",
        r#"//@version=6
indicator("Opening")
varip int n = 0
if barstate.isnew
    n := 0
n += 1
plot(barstate.isnew ? 1 : 0)
plot(n)
"#,
    );
    let hir = analyze_source(&source).hir.unwrap();
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime
        .update(BarUpdate::historical(at_time(0, bar(10.0))))
        .unwrap();
    let first = runtime
        .update_with_context(
            BarUpdate::forming(at_time(60000, bar(11.0))),
            RealtimeUpdateContext {
                opening_update: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
    assert_values_close(&first.plots[0].values, &[1.0, 0.0]);
    assert_values_close(&first.plots[1].values, &[1.0, 2.0]);
    let before = public_runtime_result_json(&runtime.result());
    assert!(
        runtime
            .update_with_context(
                BarUpdate::forming(at_time(60000, bar(12.0))),
                RealtimeUpdateContext {
                    opening_update: Some(true),
                    ..Default::default()
                },
            )
            .unwrap_err()
            .message
            .contains("opening_update cannot repeat")
    );
    assert_eq!(public_runtime_result_json(&runtime.result()), before);
    let replaced = runtime
        .update(BarUpdate::forming(at_time(60000, bar(12.0))))
        .unwrap();
    assert_values_close(&replaced.plots[1].values, &[1.0, 3.0]);
}

#[test]
fn historical_observations_cannot_be_marked_as_non_opening() {
    let source = pine_syntax::SourceFile::new(
        "opening.pine",
        "//@version=6\nindicator(\"Opening\")\nplot(barstate.isnew ? 1 : 0)\n",
    );
    let hir = analyze_source(&source).hir.unwrap();
    let mut runtime = RealtimeRuntime::new(&hir);
    let before = public_runtime_result_json(&runtime.result());
    assert!(
        runtime
            .update_with_context(
                BarUpdate::historical(at_time(0, bar(10.0))),
                RealtimeUpdateContext {
                    opening_update: Some(false),
                    ..Default::default()
                },
            )
            .is_err()
    );
    assert_eq!(public_runtime_result_json(&runtime.result()), before);
    runtime
        .update(BarUpdate::historical(at_time(0, bar(10.0))))
        .unwrap();
}

fn at_time(time: i64, mut bar: Bar) -> Bar {
    bar.time = time;
    bar
}
