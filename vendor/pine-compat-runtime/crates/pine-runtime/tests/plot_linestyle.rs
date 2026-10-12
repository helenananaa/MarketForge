use pine_runtime::{
    Bar, BarUpdate, PineValue, RealtimeRuntime, RuntimeReplica, public_runtime_changes_json,
    public_runtime_result_json, runtime_changes_from_json, runtime_result_from_json,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

#[test]
fn plot_linestyle_survives_snapshots_and_first_streamed_header() {
    let source = SourceFile::new(
        "linestyle.pine",
        include_str!("../../../tests/fixtures/runtime/plot_linestyle.pine"),
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.unwrap();
    let mut runtime = RealtimeRuntime::new(&hir);
    let mut replica = RuntimeReplica::new(runtime.result(), runtime.revision());
    for i in 0..3 {
        let bar = Bar {
            time: i * 60000,
            open: 10.0,
            high: 12.0,
            low: 9.0,
            close: 11.0 + i as f64,
            volume: 1.0,
        };
        for update in [BarUpdate::forming(bar), BarUpdate::confirmed(bar)] {
            let changes = runtime.apply_update(update).unwrap();
            let decoded =
                runtime_changes_from_json(&public_runtime_changes_json(&changes)).unwrap();
            replica.apply(&decoded).unwrap();
            let result = runtime.result();
            assert_eq!(
                result.plots[0].linestyle,
                PineValue::String("plot.linestyle_dotted".into())
            );
            assert_eq!(
                result.plots[1].linestyle,
                PineValue::String("plot.linestyle_dashed".into())
            );
            let json = public_runtime_result_json(&result);
            assert_eq!(
                public_runtime_result_json(&runtime_result_from_json(&json).unwrap()),
                json
            );
            assert_eq!(public_runtime_result_json(replica.result()), json);
        }
    }
}

#[test]
fn plot_linestyle_rejects_series_and_invalid_constants() {
    for expression in [
        "close > open ? plot.linestyle_dotted : plot.linestyle_solid",
        "123",
        "\"invalid\"",
    ] {
        let source = SourceFile::new(
            "invalid.pine",
            format!("//@version=6\nindicator(\"invalid\")\nplot(close, linestyle={expression})\n"),
        );
        let analysis = analyze_source(&source);
        assert!(!analysis.diagnostics.is_empty(), "accepted {expression}");
    }
}
