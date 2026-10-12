use pine_runtime::{
    Bar, BarUpdate, HistoricalRuntime, RealtimeRuntime, public_runtime_result_json,
};
use pine_sema::{AnalysisInput, analyze_input};
use pine_syntax::SourceFile;

#[test]
fn local_arrays_preserve_callsite_state_and_realtime_rollback() {
    for version in [5, 6] {
        for imported in [false, true] {
            let functions = "accumulate(float value) =>\n    var values = array.new<float>()\n    array.push(values,value)\n    array.sum(values)\naverage(float value) =>\n    var values = array.new<float>()\n    array.clear(values)\n    array.push(values,value)\n    array.push(values,value+2)\n    array.avg(values)\n";
            let (declarations, prefix, libraries) = if imported {
                (
                    "import test/arrays/1 as lib\n".to_string(),
                    "lib.",
                    vec![(
                        "test/arrays/1".into(),
                        SourceFile::new(
                            "library.pine",
                            format!(
                                "//@version={version}\nlibrary(\"arrays\")\n{}",
                                functions
                                    .replace("accumulate(", "export accumulate(")
                                    .replace("average(", "export average(")
                            ),
                        ),
                    )],
                )
            } else {
                (functions.to_string(), "", vec![])
            };
            let root = SourceFile::new(
                "root.pine",
                format!(
                    "//@version={version}\n{declarations}indicator(\"arrays\")\nplot({prefix}accumulate(close))\nplot({prefix}accumulate(1))\nplot({prefix}average(close))\n"
                ),
            );
            let analysis =
                analyze_input(&AnalysisInput::with_library_sources(root, libraries).unwrap());
            assert!(
                analysis.diagnostics.is_empty(),
                "{:?}",
                analysis.diagnostics
            );
            let hir = analysis.hir.unwrap();
            let bars: Vec<_> = [10.0, 20.0, 30.0]
                .into_iter()
                .enumerate()
                .map(|(i, close)| Bar {
                    time: i as i64 * 60000,
                    open: close,
                    high: close + 5.0,
                    low: close - 5.0,
                    close,
                    volume: 1.0,
                })
                .collect();
            let mut history = HistoricalRuntime::new(&hir);
            history.append_bars(&bars).unwrap();
            let expected = public_runtime_result_json(&history.result());
            let output: serde_json::Value = serde_json::from_str(&expected).unwrap();
            assert_eq!(
                output["plots"][0]["values"],
                serde_json::json!([10, 30, 60])
            );
            assert_eq!(output["plots"][1]["values"], serde_json::json!([1, 2, 3]));
            assert_eq!(
                output["plots"][2]["values"],
                serde_json::json!([11, 21, 31])
            );
            let mut incremental = HistoricalRuntime::new(&hir);
            for bar in &bars {
                incremental.append_bar(*bar).unwrap();
            }
            assert_eq!(public_runtime_result_json(&incremental.result()), expected);
            let mut realtime = RealtimeRuntime::new(&hir);
            realtime.seed_historical(&bars[..2]).unwrap();
            for close in [31.0, 29.0, 30.0] {
                let result = realtime
                    .update(BarUpdate::forming(Bar { close, ..bars[2] }))
                    .unwrap();
                let output: serde_json::Value =
                    serde_json::from_str(&public_runtime_result_json(&result)).unwrap();
                assert_eq!(output["plots"][0]["values"][2], 30.0 + close);
                assert_eq!(output["plots"][1]["values"][2], 3);
                assert_eq!(output["plots"][2]["values"][2], close + 1.0);
            }
            assert_eq!(
                public_runtime_result_json(
                    &realtime.update(BarUpdate::confirmed(bars[2])).unwrap()
                ),
                expected
            );
        }
    }
}
