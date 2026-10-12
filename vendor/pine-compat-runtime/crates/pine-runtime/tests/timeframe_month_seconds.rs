use pine_runtime::{
    Bar, ChartContext, HistoricalRuntime, NoRequestDataProvider, RequestEnvironment,
    RequestTimeframe,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

#[test]
fn inverse_seconds_uses_native_canonical_units_rounding_and_clamps() {
    let cases = [
        (-1, "1S"),
        (0, "1S"),
        (2, "5S"),
        (46, "1"),
        (61, "2"),
        (86400, "1D"),
        (86401, "2D"),
        (604800, "1W"),
        (604801, "8D"),
        (2592000, "30D"),
        (2592001, "31D"),
        (2628000, "31D"),
        (2628003, "1M"),
        (2628004, "31D"),
        (7884009, "3M"),
        (31449600, "52W"),
        (31449601, "365D"),
        (31535999, "365D"),
        (31536000, "12M"),
        (63072000, "12M"),
    ];
    for version in [5, 6] {
        let mut text = format!("//@version={version}\nindicator(\"inverse seconds\")\n");
        for (seconds, expected) in cases {
            text.push_str(&format!(
                "plot(timeframe.from_seconds(input.int({seconds}))==\"{expected}\"?1:0)\n"
            ));
        }
        let analysis = analyze_source(&SourceFile::new("inverse.pine", text));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let result = pine_runtime::run_historical(
            &analysis.hir.unwrap(),
            &[Bar {
                time: 0,
                open: 1.,
                high: 1.,
                low: 1.,
                close: 1.,
                volume: 1.,
            }],
        )
        .unwrap();
        for plot in result.plots {
            assert_eq!(plot.values[0].as_f64(), Some(1.));
        }
    }
}
use std::sync::Arc;

#[test]
fn nominal_month_seconds_match_native_and_calendar_closes_stay_calendar_based() {
    for version in [5, 6] {
        let source = format!(
            "//@version={version}\nindicator(\"month seconds\")\nplot(timeframe.in_seconds(\"1M\"))\nplot(timeframe.in_seconds(\"2M\"))\nplot(timeframe.in_seconds(\"3M\"))\nplot(timeframe.in_seconds(\"6M\"))\nplot(timeframe.in_seconds(\"12M\"))\nplot(timeframe.in_seconds())\nplot(timeframe.in_seconds(\"\"))\nplot(time_close(\"M\"))\nplot(timeframe.change(\"M\")?1:0)\n"
        );
        let analysis = analyze_source(&SourceFile::new("months.pine", source));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.unwrap();
        let env = RequestEnvironment::new(
            ChartContext::new("BTC", RequestTimeframe::parse("1M").unwrap()),
            Arc::new(NoRequestDataProvider),
        );
        let bars = [1704067200000, 1706745600000].map(|time| Bar {
            time,
            open: 1.,
            high: 1.,
            low: 1.,
            close: 1.,
            volume: 1.,
        });
        let mut runtime = HistoricalRuntime::with_request_environment(&hir, env);
        runtime.append_bars(&bars).unwrap();
        let result = runtime.result();
        for (plot, seconds) in result.plots.iter().zip([
            2628003., 5256006., 7884009., 15768018., 31536036., 2628003., 2628003.,
        ]) {
            assert_eq!(
                plot.values.iter().map(|v| v.as_f64()).collect::<Vec<_>>(),
                vec![Some(seconds); 2]
            );
        }
        assert_eq!(
            result.plots[7]
                .values
                .iter()
                .map(|v| v.as_f64())
                .collect::<Vec<_>>(),
            vec![Some(1706745600000.), Some(1709251200000.)]
        );
        assert_eq!(
            result.plots[8]
                .values
                .iter()
                .map(|v| v.as_f64())
                .collect::<Vec<_>>(),
            vec![Some(0.), Some(1.)]
        );
    }
}
