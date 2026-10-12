use pine_runtime::{
    Bar, BarUpdate, HistoricalRuntime, RealtimeRuntime, public_runtime_result_json,
};
use pine_sema::{AnalysisInput, analyze_input, analyze_source};
use pine_syntax::SourceFile;

const SOURCE: &str = include_str!("../../../tests/fixtures/runtime/series_scalar_parameters.pine");

fn bars() -> Vec<Bar> {
    [10.0, 20.0, 30.0]
        .into_iter()
        .enumerate()
        .map(|(i, close)| Bar {
            time: i as i64 * 60_000,
            open: close,
            high: close + 2.0,
            low: close - 2.0,
            close,
            volume: 1.0,
        })
        .collect()
}

#[test]
fn scalar_series_parameters_keep_history_and_realtime_rollback() {
    for version in [5, 6] {
        let analysis = analyze_source(&SourceFile::new(
            "series.pine",
            SOURCE.replace("//@version=6", &format!("//@version={version}")),
        ));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.unwrap();
        let input = bars();
        let mut batch = HistoricalRuntime::new(&hir);
        batch.append_bars(&input).unwrap();
        let expected = public_runtime_result_json(&batch.result());
        let result: serde_json::Value = serde_json::from_str(&expected).unwrap();
        assert_eq!(
            result["plots"][0]["values"],
            serde_json::json!([null, 3, 3])
        );
        assert_eq!(
            result["plots"][1]["values"],
            serde_json::json!([null, 10, 20])
        );
        assert_eq!(
            result["plots"][2]["values"],
            serde_json::json!([1.5, 1.5, 1.5])
        );
        assert_eq!(
            result["plots"][3]["values"],
            serde_json::json!([10, 30, 60])
        );
        assert_eq!(result["plots"][4]["values"], serde_json::json!([1, 1, 1]));
        let mut incremental = HistoricalRuntime::new(&hir);
        for bar in &input {
            incremental.append_bar(*bar).unwrap();
        }
        assert_eq!(public_runtime_result_json(&incremental.result()), expected);
        let mut realtime = RealtimeRuntime::new(&hir);
        realtime.seed_historical(&input[..2]).unwrap();
        for close in [31.0, 29.0, 32.0, 30.0] {
            let mut forming = input[2];
            forming.close = close;
            let result = realtime.update(BarUpdate::forming(forming)).unwrap();
            let result: serde_json::Value =
                serde_json::from_str(&public_runtime_result_json(&result)).unwrap();
            assert_eq!(result["plots"][3]["values"][2], 30.0 + close);
        }
        let confirmed = realtime.update(BarUpdate::confirmed(input[2])).unwrap();
        assert_eq!(public_runtime_result_json(&confirmed), expected);
    }
}

#[test]
fn imported_series_parameters_keep_independent_callsite_history() {
    let root = SourceFile::new(
        "root.pine",
        "//@version=6\nimport test/series/1 as first\nimport test/series/1 as second\nindicator(\"import\")\nplot(first.previous(3))\nplot(second.previous(close))\n",
    );
    let library = SourceFile::new(
        "library.pine",
        "//@version=6\nlibrary(\"series\")\nexport previous(series float value) => value[1]\n",
    );
    let input =
        AnalysisInput::with_library_sources(root, vec![("test/series/1".into(), library)]).unwrap();
    let analysis = analyze_input(&input);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.unwrap();
    let mut runtime = HistoricalRuntime::new(&hir);
    runtime.append_bars(&bars()).unwrap();
    let result: serde_json::Value =
        serde_json::from_str(&public_runtime_result_json(&runtime.result())).unwrap();
    assert_eq!(
        result["plots"][0]["values"],
        serde_json::json!([null, 3, 3])
    );
    assert_eq!(
        result["plots"][1]["values"],
        serde_json::json!([null, 10, 20])
    );
}
