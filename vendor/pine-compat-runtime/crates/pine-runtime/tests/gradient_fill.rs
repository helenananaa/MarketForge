use pine_runtime::{
    Bar, BarUpdate, FillAction, OutputRetention, RealtimeRuntime, RuntimeReplica,
    public_runtime_changes_json, public_runtime_result_json, run_historical,
    runtime_changes_from_json, runtime_result_from_json,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn hir(source: &str) -> pine_ir::HirProgram {
    let analysis = analyze_source(&SourceFile::new("gradient.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.unwrap()
}
fn bar(i: i64, close: f64) -> Bar {
    Bar {
        time: i * 60000,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.0,
    }
}

#[test]
fn gradient_stops_and_masks_survive_named_overload_binding() {
    for version in [5, 6] {
        let prefix = format!(
            "//@version={version}\nindicator(\"gradient\")\na=plot(close+2)\nb=plot(close-2)\n"
        );
        let positional = hir(&(prefix.clone()
            + "fill(a,b,close+1,close-1,color.green,color.red,title=\"gradient\")\n"));
        let named = hir(&(prefix
            + "fill(bottom_color=color.red,plot2=b,top_value=close+1,plot1=a,bottom_value=close-1,top_color=color.green,title=\"gradient\")\n"));
        let bars = [bar(0, 10.0), bar(1, 20.0)];
        let result = run_historical(&positional, &bars).unwrap();
        assert_eq!(result, run_historical(&named, &bars).unwrap());
        let fill = &result.fills[0];
        assert_eq!(fill.first_id, result.plots[0].id);
        assert_eq!(fill.second_id, result.plots[1].id);
        assert_eq!(fill.gradient.as_ref().unwrap()[1].top_value, Some(21.0));
        assert_eq!(fill.gradient.as_ref().unwrap()[1].bottom_value, Some(19.0));
        let json = public_runtime_result_json(&result);
        assert_eq!(
            public_runtime_result_json(&runtime_result_from_json(&json).unwrap()),
            json
        );
    }
}

#[test]
fn gradient_retention_and_wire_replica_match_after_physical_pruning_and_rollback() {
    let program = hir(include_str!(
        "../../../tests/fixtures/runtime/gradient_fill.pine"
    ));
    let mut runtime = RealtimeRuntime::new(&program);
    runtime.set_output_retention(OutputRetention::keep_confirmed_bars(7));
    runtime.update(BarUpdate::historical(bar(0, 10.0))).unwrap();
    let mut replica = RuntimeReplica::new(runtime.result(), runtime.revision());
    for i in 1..540 {
        for update in [
            BarUpdate::forming(bar(i, 20.0)),
            BarUpdate::forming(bar(i, 30.0)),
            BarUpdate::confirmed(bar(i, 40.0)),
        ] {
            let changes = runtime.apply_update(update).unwrap();
            let decoded =
                runtime_changes_from_json(&public_runtime_changes_json(&changes)).unwrap();
            replica.apply(&decoded).unwrap();
            let result = runtime.result();
            assert_eq!(
                public_runtime_result_json(replica.result()),
                public_runtime_result_json(&result)
            );
            let samples = result.fills[0].gradient.as_ref().unwrap();
            assert!(samples.len() <= 8);
            assert_eq!(samples.len(), result.fills[0].colors.len());
            assert_eq!(
                samples.last().unwrap().bottom_value,
                Some(
                    if changes.visibility == pine_runtime::StreamingVisibility::Confirmed {
                        39.0
                    } else if result.plots[0].values.last().unwrap().as_f64() == Some(32.0) {
                        29.0
                    } else {
                        19.0
                    }
                )
            );
        }
    }
}

#[test]
fn malformed_and_old_schema_gradients_are_rejected() {
    let program = hir(include_str!(
        "../../../tests/fixtures/runtime/gradient_fill.pine"
    ));
    let mut runtime = RealtimeRuntime::new(&program);
    let changes = runtime
        .apply_update(BarUpdate::historical(bar(0, 10.0)))
        .unwrap();
    let mut wire: serde_json::Value =
        serde_json::from_str(&public_runtime_changes_json(&changes)).unwrap();
    wire["schemaVersion"] = 3.into();
    assert!(runtime_changes_from_json(&wire.to_string()).is_err());
    let mut native_old = changes.clone();
    native_old.schema_version = 3;
    let mut replica = RuntimeReplica::new(Default::default(), 0);
    assert!(replica.apply(&native_old).is_err());
    assert_eq!(replica.revision(), 0);
    let mut result: serde_json::Value =
        serde_json::from_str(&public_runtime_result_json(&runtime.result())).unwrap();
    result["schemaVersion"] = 8.into();
    assert!(runtime_result_from_json(&result.to_string()).is_err());
    result["schemaVersion"] = 9.into();
    result["fills"][0]["gradient"][0]
        .as_object_mut()
        .unwrap()
        .remove("topColor");
    assert!(runtime_result_from_json(&result.to_string()).is_err());
    assert!(
        changes.fills.iter().any(
            |change| matches!(&change.action, FillAction::Add(fill) if fill.gradient.is_some())
        )
    );
}

#[test]
fn legacy_schema8_snapshot_remains_readable() {
    let original = include_str!("../../../tests/fixtures/legacy_runtime_schema8.json");
    let decoded = runtime_result_from_json(original).unwrap();
    let mut expected: serde_json::Value = serde_json::from_str(original).unwrap();
    expected["schemaVersion"] = 9.into();
    let current: serde_json::Value =
        serde_json::from_str(&public_runtime_result_json(&decoded)).unwrap();
    assert_eq!(current, expected);
}
