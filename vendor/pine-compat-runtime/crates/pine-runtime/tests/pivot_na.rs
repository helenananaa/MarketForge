use pine_runtime::{Bar, BarUpdate, HistoricalRuntime, RealtimeRuntime, run_historical};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn bar(i: i64) -> Bar {
    Bar {
        time: i * 60000,
        open: 10.0,
        high: 10.0,
        low: 10.0,
        close: 10.0,
        volume: 1.0,
    }
}

#[test]
fn pivot_na_native_reference_and_realtime_replacements_match() {
    let original = include_str!("../../../tests/fixtures/runtime/pivot_na_boundaries.pine");
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/pivot_na_native_values.json"
    ))
    .unwrap();
    for version in [5, 6] {
        let source = original.replace("version=6", &format!("version={version}"));
        let analysis = analyze_source(&SourceFile::new("native.pine", source));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.unwrap();
        let bars: Vec<_> = (0..109).map(bar).collect();
        let result = run_historical(&hir, &bars).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&pine_runtime::public_runtime_result_json(&result)).unwrap();
        for plot in json["plots"].as_array().unwrap() {
            let name = plot["title"].as_str().unwrap();
            let values = plot["values"].as_array().unwrap();
            let targets = expected[name].as_array().unwrap();
            assert_eq!(values.len(), targets.len());
            for (index, (a, b)) in values.iter().zip(targets).enumerate() {
                assert_eq!(a.as_f64(), b.as_f64(), "v{version} {name} at {index}");
            }
        }
        let mut history = HistoricalRuntime::new(&hir);
        history.append_bars(&bars).unwrap();
        assert_eq!(result, history.result());
        let mut realtime = RealtimeRuntime::new(&hir);
        realtime.update(BarUpdate::historical(bars[0])).unwrap();
        for bar in &bars[1..] {
            realtime.update(BarUpdate::forming(*bar)).unwrap();
            realtime.update(BarUpdate::forming(*bar)).unwrap();
            realtime.update(BarUpdate::confirmed(*bar)).unwrap();
        }
        assert_eq!(result, realtime.result());
    }
}
