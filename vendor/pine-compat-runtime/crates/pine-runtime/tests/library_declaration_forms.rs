use pine_runtime::{
    Bar, BarUpdate, HistoricalRuntime, RealtimeRuntime, public_runtime_result_json,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

#[test]
fn inline_switch_assignments_match_explicit_blocks_across_execution_modes() {
    for version in [5, 6] {
        let source = format!(
            "//@version={version}\nindicator(\"switch state\")\nlatch(on,off) =>\n    var bool state=false\n    switch\n        on => state := true\n        off => state := false\n    state\nplot(latch(close>2,close<1) ? 1 : 0)\n"
        );
        let block_source = source
            .replace("on => state := true", "on =>\n            state := true")
            .replace(
                "off => state := false",
                "off =>\n            state := false",
            );
        let analysis = analyze_source(&SourceFile::new("inline.pine", source));
        let block = analyze_source(&SourceFile::new("block.pine", block_source));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        assert!(block.diagnostics.is_empty(), "{:?}", block.diagnostics);
        let hir = analysis.hir.unwrap();
        let block_hir = block.hir.unwrap();
        let bars: Vec<_> = [0.5, 3.0, 2.0, 0.5]
            .into_iter()
            .enumerate()
            .map(|(i, close)| Bar {
                time: i as i64 * 60_000,
                open: close,
                high: 4.0,
                low: 0.0,
                close,
                volume: 1.0,
            })
            .collect();
        let mut runtime = HistoricalRuntime::new(&hir);
        runtime.append_bars(&bars).unwrap();
        let output = public_runtime_result_json(&runtime.result());
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["plots"][0]["values"], serde_json::json!([0, 1, 1, 0]));
        let mut explicit = HistoricalRuntime::new(&block_hir);
        explicit.append_bars(&bars).unwrap();
        assert_eq!(public_runtime_result_json(&explicit.result()), output);
        let mut incremental = HistoricalRuntime::new(&hir);
        for bar in &bars {
            incremental.append_bar(*bar).unwrap();
        }
        assert_eq!(public_runtime_result_json(&incremental.result()), output);
        let mut realtime = RealtimeRuntime::new(&hir);
        realtime.seed_historical(&bars[..2]).unwrap();
        for close in [0.5, 3.0, 2.0] {
            let mut forming = bars[2];
            forming.close = close;
            realtime.update(BarUpdate::forming(forming)).unwrap();
        }
        realtime.update(BarUpdate::confirmed(bars[2])).unwrap();
        let final_result = realtime.update(BarUpdate::confirmed(bars[3])).unwrap();
        assert_eq!(public_runtime_result_json(&final_result), output);
    }
}
