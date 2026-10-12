use pine_runtime::{
    Bar, BarUpdate, HistoricalRuntime, RealtimeRuntime, public_runtime_result_json,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

const SOURCE: &str = include_str!("../../../tests/fixtures/runtime/strategy_pyramiding_zero.pine");

#[test]
fn zero_disables_additions_but_allows_initial_entries_and_reversals() {
    for version in [5, 6] {
        let source = SOURCE.replace("//@version=6", &format!("//@version={version}"));
        let a = analyze_source(&SourceFile::new("zero.pine", &source));
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        let hir = a.hir.unwrap();
        assert_eq!(hir.strategy_settings.pyramiding_limit, 1);
        let bars: Vec<_> = (0..6)
            .map(|i| Bar {
                time: i * 60000,
                open: 100.0,
                high: 101.0,
                low: 99.0,
                close: 100.0,
                volume: 1.0,
            })
            .collect();
        let mut batch = HistoricalRuntime::new(&hir);
        batch.append_bars(&bars).unwrap();
        let expected = public_runtime_result_json(&batch.result());
        let result: serde_json::Value = serde_json::from_str(&expected).unwrap();
        assert_eq!(
            result["plots"][0]["values"],
            serde_json::json!([0, 1, 1, -1, -1, 0])
        );
        assert_eq!(result["strategy"]["trades"].as_array().unwrap().len(), 2);
        let mut incremental = HistoricalRuntime::new(&hir);
        for bar in &bars {
            incremental.append_bar(*bar).unwrap();
        }
        assert_eq!(public_runtime_result_json(&incremental.result()), expected);
        let mut realtime = RealtimeRuntime::new(&hir);
        realtime.seed_historical(&bars[..5]).unwrap();
        realtime.update(BarUpdate::forming(bars[5])).unwrap();
        realtime.update(BarUpdate::forming(bars[5])).unwrap();
        assert_eq!(
            public_runtime_result_json(&realtime.update(BarUpdate::confirmed(bars[5])).unwrap()),
            expected
        );
        let one = analyze_source(&SourceFile::new(
            "one.pine",
            source.replace("pyramiding=0", "pyramiding=1"),
        ))
        .hir
        .unwrap();
        let mut control = HistoricalRuntime::new(&one);
        control.append_bars(&bars).unwrap();
        assert_eq!(public_runtime_result_json(&control.result()), expected);
    }
}

#[test]
fn negative_and_fractional_pyramiding_stay_rejected() {
    for value in ["-1", "0.5"] {
        let a = analyze_source(&SourceFile::new(
            "invalid.pine",
            SOURCE.replace("pyramiding=0", &format!("pyramiding={value}")),
        ));
        assert!(a.hir.is_none());
        assert!(a.diagnostics.iter().any(|d| d.code == "E_CALL_ARG_VALUE"));
    }
}
