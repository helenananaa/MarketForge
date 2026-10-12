use std::sync::Arc;

use pine_runtime::{
    Bar, ChartContext, HistoricalRuntime, InMemoryRequestDataProvider, NoRequestDataProvider,
    RealtimeRuntime, RequestEnvironment, RequestKey, RequestTimeframe, RuntimeResult,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn program(version: u16, body: &str) -> pine_ir::HirProgram {
    let source = format!("//@version={version}\nindicator(\"chart time\")\n{body}");
    let analysis = analyze_source(&SourceFile::new("chart-time.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.unwrap()
}

fn bar(time: i64) -> Bar {
    Bar {
        time,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 1.0,
    }
}

fn environment(timeframe: &str) -> RequestEnvironment {
    RequestEnvironment::new(
        ChartContext::new("CHART", RequestTimeframe::parse(timeframe).unwrap()),
        Arc::new(NoRequestDataProvider),
    )
}

fn plot_times(result: &RuntimeResult, index: usize) -> Vec<Option<i64>> {
    result.plots[index]
        .values
        .iter()
        .map(|value| value.as_i64())
        .collect()
}

#[test]
fn chart_period_controls_close_variables_empty_timeframes_and_bar_offsets() {
    for version in [5, 6] {
        let hir = program(
            version,
            concat!(
                "plot(time_close)\nplot(time_close(\"\"))\nplot(time_close(timeframe.period))\n",
                "plot(time(\"\", bars_back=1))\nplot(time_close(\"\", bars_back=1))\n",
                "plot(time(\"\", bars_back=-1))\nplot(time_close(\"\", bars_back=-1))\n",
                "plot(time(timeframe.period, bars_back=2))\nplot(time_close(timeframe.period, bars_back=2))\n",
                "plot(time(timeframe.period, bars_back=-2))\nplot(time_close(timeframe.period, bars_back=-2))\n",
            ),
        );
        for (timeframe, duration) in [("5", 300_000), ("30S", 30_000)] {
            // Fixed chart closes retain the host's open time even if it is not
            // aligned to the epoch's timeframe buckets.
            let bars = [bar(10_000), bar(10_000 + duration)];
            let mut historical =
                HistoricalRuntime::with_request_environment(&hir, environment(timeframe));
            historical.append_bars(&bars).unwrap();
            let result = historical.result();
            for index in 0..3 {
                assert_eq!(
                    plot_times(&result, index),
                    vec![Some(10_000 + duration), Some(10_000 + 2 * duration)]
                );
            }
            // Positive and negative offsets move chart periods and preserve
            // the host's fixed-period open, including its non-epoch alignment.
            let offsets = [
                (3, 1, false),
                (4, 1, true),
                (5, -1, false),
                (6, -1, true),
                (7, 2, false),
                (8, 2, true),
                (9, -2, false),
                (10, -2, true),
            ];
            for (index, bars_back, close_time) in offsets {
                let close_offset = if close_time { duration } else { 0 };
                let expected = bars
                    .iter()
                    .map(|bar| Some(bar.time - bars_back * duration + close_offset))
                    .collect::<Vec<_>>();
                assert_eq!(plot_times(&result, index), expected, "{timeframe}");
            }

            let mut realtime =
                RealtimeRuntime::with_request_environment(&hir, environment(timeframe));
            realtime.seed_historical_without_output(&bars).unwrap();
            assert_eq!(realtime.result(), result);
            let live = realtime
                .update(pine_runtime::BarUpdate::forming(bar(10_000 + 2 * duration)))
                .unwrap();
            assert_eq!(
                plot_times(&live, 0).last(),
                Some(&Some(10_000 + 3 * duration))
            );
        }
    }
}

#[test]
fn calendar_chart_closes_and_offsets_use_calendar_boundaries() {
    // February 2024 has 29 days; nominal month seconds cannot reproduce this.
    let cases = [
        (
            "M",
            1_706_745_600_000,
            1_709_251_200_000,
            1_704_067_200_000,
            1_711_929_600_000,
        ),
        (
            "3M",
            1_704_067_200_000,
            1_711_929_600_000,
            1_696_118_400_000,
            1_719_792_000_000,
        ),
        (
            "W",
            1_706_486_400_000,
            1_707_091_200_000,
            1_705_881_600_000,
            1_707_696_000_000,
        ),
        (
            "2W",
            1_705_881_600_000,
            1_707_091_200_000,
            1_704_672_000_000,
            1_708_300_800_000,
        ),
    ];
    for version in [5, 6] {
        let hir = program(
            version,
            concat!(
                "plot(time_close)\nplot(time_close(\"\"))\nplot(time_close(timeframe.period))\n",
                "plot(time(\"\", bars_back=1))\nplot(time_close(\"\", bars_back=1))\n",
                "plot(time(\"\", bars_back=-1))\nplot(time_close(\"\", bars_back=-1))\n",
            ),
        );
        for (timeframe, open, close, previous_open, next_close) in cases {
            let mut runtime =
                HistoricalRuntime::with_request_environment(&hir, environment(timeframe));
            runtime.append_bars(&[bar(open)]).unwrap();
            let result = runtime.result();
            for index in 0..3 {
                assert_eq!(plot_times(&result, index), vec![Some(close)], "{timeframe}");
            }
            assert_eq!(
                plot_times(&result, 3),
                vec![Some(previous_open)],
                "{timeframe}"
            );
            assert_eq!(plot_times(&result, 4), vec![Some(open)], "{timeframe}");
            assert_eq!(plot_times(&result, 5), vec![Some(close)], "{timeframe}");
            assert_eq!(
                plot_times(&result, 6),
                vec![Some(next_close)],
                "{timeframe}"
            );
        }
    }
}

#[test]
fn requested_time_functions_use_the_requested_chart_period() {
    for version in [5, 6] {
        for (timeframe, open, close) in [
            ("5", 0, 300_000),
            ("M", 1_706_745_600_000, 1_709_251_200_000),
            ("W", 1_706_486_400_000, 1_707_091_200_000),
        ] {
            let hir = program(
                version,
                &format!(
                    "plot(request.security(\"REMOTE\", \"{timeframe}\", time_close(\"\")))\nplot(request.security(\"REMOTE\", \"{timeframe}\", time_close(timeframe.period)))\n"
                ),
            );
            let mut provider = InMemoryRequestDataProvider::new();
            provider
                .insert(
                    RequestKey::new("REMOTE", RequestTimeframe::parse(timeframe).unwrap()),
                    vec![bar(open)],
                )
                .unwrap();
            let env = RequestEnvironment::new(
                ChartContext::new("CHART", RequestTimeframe::parse("1").unwrap()),
                Arc::new(provider),
            );
            let mut runtime = HistoricalRuntime::with_request_environment(&hir, env);
            runtime.append_bars(&[bar(close - 60_000)]).unwrap();
            let result = runtime.result();
            for index in 0..2 {
                assert_eq!(plot_times(&result, index), vec![Some(close)], "{timeframe}");
            }
        }
    }
}
