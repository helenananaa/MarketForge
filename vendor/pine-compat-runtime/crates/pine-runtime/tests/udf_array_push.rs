use pine_runtime::{Bar, BarUpdate, RealtimeRuntime, run_historical};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn bar(index: i64, close: f64) -> Bar {
    Bar {
        time: index * 60_000,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.0,
    }
}

#[test]
fn udf_push_native_reference_and_incremental_agree() {
    let original = include_str!("../../../tests/fixtures/runtime/udf_array_push.pine");
    for version in [5, 6] {
        let source = original.replace("version=6", &format!("version={version}"));
        let analysis = analyze_source(&SourceFile::new("oracle.pine", source));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.unwrap();
        let bars: Vec<_> = (0..8).map(|i| bar(i, 10.0 + i as f64)).collect();
        let result = run_historical(&hir, &bars).unwrap();
        let json = pine_runtime::public_runtime_result_json(&result);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        // Native v5/v6 chart legend captured separately; first five rows are
        // explicit arithmetic controls, not claimed as independently exported.
        for (plot, expected) in [1.0, 3.0, 19.0, 9.0, 10.0, 5.0].iter().enumerate() {
            assert_eq!(value["plots"][plot]["values"][7].as_f64(), Some(*expected));
        }
        let mut incremental = RealtimeRuntime::new(&hir);
        for b in &bars {
            incremental.update(BarUpdate::historical(*b)).unwrap();
        }
        assert_eq!(incremental.result(), result);
    }
}

#[test]
fn udf_push_rolls_back_var_but_preserves_varip_and_callsite_identity() {
    for version in [5, 6] {
        let source = format!(
            r#"//@version={version}
indicator("push rollback")
appendValue(array<float> values, float value) =>
    values.push(value)
    array.sum(values)
var ordinary = array.new_float()
varip intrabar = array.new_float()
plot(appendValue(ordinary, close))
plot(appendValue(intrabar, close))
plot(array.size(ordinary))
plot(array.size(intrabar))
"#
        );
        let analysis = analyze_source(&SourceFile::new("rollback.pine", source));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.unwrap();
        let mut runtime = RealtimeRuntime::new(&hir);
        runtime.update(BarUpdate::historical(bar(0, 10.0))).unwrap();
        for (update, expected) in [
            (BarUpdate::forming(bar(1, 20.0)), [30.0, 30.0, 2.0, 2.0]),
            (BarUpdate::forming(bar(1, 30.0)), [40.0, 60.0, 2.0, 3.0]),
            (BarUpdate::confirmed(bar(1, 40.0)), [50.0, 100.0, 2.0, 4.0]),
        ] {
            let result = runtime.update(update).unwrap();
            let value: serde_json::Value =
                serde_json::from_str(&pine_runtime::public_runtime_result_json(&result)).unwrap();
            for (index, expected) in expected.iter().enumerate() {
                assert_eq!(value["plots"][index]["values"][1].as_f64(), Some(*expected));
            }
        }
    }
}

#[test]
fn udf_push_global_aliases_match_native_reference() {
    let source = include_str!("../../../tests/fixtures/runtime/udf_array_push_global.pine");
    let analysis = analyze_source(&SourceFile::new("global.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let bars: Vec<_> = (0..8).map(|i| bar(i, 10.0)).collect();
    let result = run_historical(&analysis.hir.unwrap(), &bars).unwrap();
    let value: serde_json::Value =
        serde_json::from_str(&pine_runtime::public_runtime_result_json(&result)).unwrap();
    assert_eq!(
        value["plots"][0]["values"],
        serde_json::json!([5, 10, 15, 20, 25, 25, 25, 25])
    );
    assert_eq!(
        value["plots"][1]["values"],
        serde_json::json!([2, 4, 6, 8, 10, 10, 10, 10])
    );
}

#[test]
fn udf_push_to_a_copy_does_not_mutate_caller() {
    let source = "//@version=6\nindicator(\"copy\")\nf(values) =>\n    array.copy(values).push(3)\n    array.sum(values)\nvalues = array.from(1, 2)\nplot(f(values))\nplot(array.size(values))\n";
    let analysis = analyze_source(&SourceFile::new("copy.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(&analysis.hir.unwrap(), &[bar(0, 10.0)]).unwrap();
    let value: serde_json::Value =
        serde_json::from_str(&pine_runtime::public_runtime_result_json(&result)).unwrap();
    assert_eq!(value["plots"][0]["values"], serde_json::json!([3]));
    assert_eq!(value["plots"][1]["values"], serde_json::json!([2]));
}
