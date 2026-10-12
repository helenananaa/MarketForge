use std::{collections::VecDeque, sync::Arc};

use super::*;

fn program(text: &str) -> pine_ir::HirProgram {
    let analysis = analyze_source(&SourceFile::new("ta_scan.pine", text));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.expect("HIR")
}

fn timed_bar(index: usize, close: f64, volume: f64) -> Bar {
    Bar {
        time: index as i64 * 60_000,
        volume,
        ..bar(close)
    }
}

fn finite(value: f64) -> PineValue {
    if value.is_finite() {
        PineValue::Float(value)
    } else {
        PineValue::Na
    }
}

// Independent ordinary queue and the pre-optimization collected-Vec formulas.
// Neither the state model nor its scans use a production rolling helper.
#[derive(Default)]
struct LegacyWindow(VecDeque<Option<f64>>);

impl LegacyWindow {
    fn push(&mut self, source: Option<f64>, length: usize) {
        if length == 0 {
            return;
        }
        while self.0.len() >= length {
            self.0.pop_front();
        }
        self.0.push_back(source.filter(|value| value.is_finite()));
    }

    fn ready_values(&self, length: usize) -> Option<Vec<f64>> {
        if length == 0 || self.0.len() != length {
            return None;
        }
        self.0.iter().copied().collect()
    }

    fn linreg(&self, length: usize, offset: i64) -> PineValue {
        let Some(values) = self.ready_values(length) else {
            return PineValue::Na;
        };
        if values.len() == 1 {
            return finite(values[0]);
        }
        let n = length as f64;
        let mut sum_x = 0.0;
        let mut sum_y = 0.0;
        let mut sum_x_squared = 0.0;
        let mut sum_xy = 0.0;
        for (index, value) in values.iter().enumerate() {
            let x = index as f64;
            sum_x += x;
            sum_y += value;
            sum_x_squared += x * x;
            sum_xy += x * value;
        }
        let denominator = n * sum_x_squared - sum_x * sum_x;
        if denominator == 0.0 {
            return PineValue::Na;
        }
        let slope = (n * sum_xy - sum_x * sum_y) / denominator;
        let intercept = (sum_y - slope * sum_x) / n;
        finite(intercept + slope * (length as f64 - 1.0 - offset as f64))
    }

    fn swma(&self) -> PineValue {
        let Some(values) = self.ready_values(4) else {
            return PineValue::Na;
        };
        finite((values[0] + 2.0 * values[1] + 2.0 * values[2] + values[3]) / 6.0)
    }
}

fn assert_value_bits(actual: &PineValue, expected: &PineValue) {
    match (actual, expected) {
        (PineValue::Float(actual), PineValue::Float(expected)) => {
            assert_eq!(
                actual.to_bits(),
                expected.to_bits(),
                "{actual} != {expected}"
            );
        }
        _ => assert_eq!(actual, expected),
    }
}

fn assert_plots(actual: &RuntimeResult, expected: &[Vec<PineValue>]) {
    assert_eq!(actual.plots.len(), expected.len());
    for (plot, values) in actual.plots.iter().zip(expected) {
        assert_eq!(plot.values.len(), values.len());
        for (actual, expected) in plot.values.iter().zip(values) {
            assert_value_bits(actual, expected);
        }
    }
}

fn sample(index: usize) -> f64 {
    let coarse = (index * 37 % 211) as f64 - 105.0;
    coarse * 0.125 + (index % 13) as f64 * 0.1
}

#[test]
fn linreg_fixed_windows_keep_collected_vec_bits_across_page_boundaries() {
    let bars: Vec<_> = (0..720)
        .map(|index| timed_bar(index, sample(index), 1.0))
        .collect();
    let offsets = [0, 1, -1, i64::MAX];
    for length in [1, 2, 3, 127, 128, 129, 257] {
        let mut source = String::from("indicator(\"LinReg boundaries\")\n");
        for offset in offsets {
            source.push_str(&format!("plot(ta.linreg(close, {length}, {offset}))\n"));
        }
        let hir = program(&source);
        let actual = run_historical(&hir, &bars).unwrap();
        let mut window = LegacyWindow::default();
        let mut expected = vec![Vec::new(); offsets.len()];
        for bar in &bars {
            window.push(Some(bar.close), length);
            for (values, offset) in expected.iter_mut().zip(offsets) {
                values.push(window.linreg(length, offset));
            }
        }
        assert_plots(&actual, &expected);
    }
}

#[test]
fn linreg_dynamic_shrink_expand_and_na_keep_logical_window_bounds() {
    let hir = program(
        "indicator(\"LinReg dynamic storage\")\nlength = int(volume)\nsource = bar_index % 700 == 430 ? na : close\nplot(ta.linreg(source, length, 0))\nplot(ta.linreg(offset=na, source=source, length=length))\nplot(ta.linreg(source, -1, 0))\n",
    );
    let bars: Vec<_> = (0..1700)
        .map(|index| {
            let length = if index % 211 == 9 {
                0
            } else {
                match index % 700 {
                    0..350 => 300,
                    350..370 => 1,
                    370..390 => 4,
                    390..570 => 129,
                    _ => 257,
                }
            };
            timed_bar(index, sample(index), length as f64)
        })
        .collect();
    let actual = run_historical(&hir, &bars).unwrap();
    let mut window = LegacyWindow::default();
    let mut values = Vec::new();
    for (index, bar) in bars.iter().enumerate() {
        let source = (index % 700 != 430).then_some(bar.close);
        let length = bar.volume as usize;
        window.push(source, length);
        values.push(window.linreg(length, 0));
    }
    assert_plots(
        &actual,
        &[values.clone(), values, vec![PineValue::Na; bars.len()]],
    );
}

#[test]
fn swma_preserves_signed_zero_subnormal_cancellation_and_overflow_bits() {
    let hir = program("indicator(\"SWMA exact bits\")\nplot(ta.swma(close))\n");
    for inputs in [
        vec![-0.0; 40],
        vec![0.0; 40],
        (0..80)
            .map(|index| {
                [
                    f64::from_bits(1),
                    -f64::from_bits(1),
                    0.1,
                    -0.0,
                    f64::MAX,
                    -f64::MAX,
                    1e308,
                    -1e308,
                ][index % 8]
            })
            .collect(),
    ] {
        let bars: Vec<_> = inputs
            .iter()
            .enumerate()
            .map(|(index, &value)| timed_bar(index, value, 1.0))
            .collect();
        let actual = run_historical(&hir, &bars).unwrap();
        let mut window = LegacyWindow::default();
        let expected: Vec<_> = inputs
            .into_iter()
            .map(|value| {
                window.push(Some(value), 4);
                window.swma()
            })
            .collect();
        assert_plots(&actual, &[expected]);
    }
}

#[test]
fn scans_preserve_na_nonfinite_warmup_and_recovery_masks() {
    let hir = program(
        "indicator(\"Scan missing sources\")\nsource = bar_index % 17 == 4 ? close * 1e308 : bar_index % 17 == 8 ? (close - close) / (close - close) : bar_index % 17 == 12 ? na : close\nplot(ta.linreg(source, 3, -1))\nplot(ta.swma(source))\n",
    );
    let bars: Vec<_> = (0..120)
        .map(|index| timed_bar(index, index as f64 + 2.0, 1.0))
        .collect();
    let actual = run_historical(&hir, &bars).unwrap();
    let mut regression = LegacyWindow::default();
    let mut symmetric = LegacyWindow::default();
    let mut expected = vec![Vec::new(); 2];
    for (index, bar) in bars.iter().enumerate() {
        let source = (!matches!(index % 17, 4 | 8 | 12)).then_some(bar.close);
        regression.push(source, 3);
        symmetric.push(source, 4);
        expected[0].push(regression.linreg(3, -1));
        expected[1].push(symmetric.swma());
    }
    assert_plots(&actual, &expected);
}

#[test]
fn linreg_extreme_windows_keep_legacy_nonfinite_result_and_single_value_bits() {
    for inputs in [
        vec![-0.0; 180],
        vec![f64::MAX; 180],
        (0..180)
            .map(|index| [f64::MAX, -f64::MAX, 1e308, -1e308][index % 4])
            .collect(),
        (0..180)
            .map(|index| f64::from_bits((index % 23 + 1) as u64))
            .collect(),
    ] {
        let bars: Vec<_> = inputs
            .iter()
            .enumerate()
            .map(|(index, &value)| timed_bar(index, value, 1.0))
            .collect();
        for length in [1, 2, 3, 129] {
            let hir = program(&format!(
                "indicator(\"LinReg extremes\")\nplot(ta.linreg(close, {length}, -1))\n"
            ));
            let actual = run_historical(&hir, &bars).unwrap();
            let mut window = LegacyWindow::default();
            let expected: Vec<_> = inputs
                .iter()
                .map(|&value| {
                    window.push(Some(value), length);
                    window.linreg(length, -1)
                })
                .collect();
            assert_plots(&actual, &[expected]);
        }
    }
}

#[test]
fn repeated_loop_calls_keep_each_invocation_in_the_window() {
    let hir = program(
        "indicator(\"Scan loop calls\")\nfloat regression = na\nfloat symmetric = na\nsource = bar_index % 37 == 9 ? na : close\nfor index = 0 to 3\n    regression := ta.linreg(source + index * 0.125, 129, 0)\n    symmetric := ta.swma(source + index * 0.125)\nplot(regression)\nplot(symmetric)\n",
    );
    let bars: Vec<_> = (0..350)
        .map(|index| timed_bar(index, sample(index), 1.0))
        .collect();
    let actual = run_historical(&hir, &bars).unwrap();
    let mut regression = LegacyWindow::default();
    let mut symmetric = LegacyWindow::default();
    let mut expected = vec![Vec::new(); 2];
    for (index, bar) in bars.iter().enumerate() {
        for inner in 0..4 {
            let source = (index % 37 != 9).then_some(bar.close + inner as f64 * 0.125);
            regression.push(source, 129);
            symmetric.push(source, 4);
        }
        expected[0].push(regression.linreg(129, 0));
        expected[1].push(symmetric.swma());
    }
    assert_plots(&actual, &expected);
}

const REALTIME_SOURCE: &str = "indicator(\"Scan realtime storage\")\nlength = volume == 0 ? 129 : int(volume)\nsource = volume == 0 ? na : close\nplot(ta.linreg(source, length, -1))\nplot(ta.swma(source))\n";

fn realtime_reference(bars: &[Bar]) -> Vec<Vec<PineValue>> {
    let mut regression = LegacyWindow::default();
    let mut symmetric = LegacyWindow::default();
    let mut expected = vec![Vec::new(); 2];
    for bar in bars {
        let length = if bar.volume == 0.0 {
            129
        } else {
            bar.volume as usize
        };
        let source = (bar.volume != 0.0).then_some(bar.close);
        regression.push(source, length);
        symmetric.push(source, 4);
        expected[0].push(regression.linreg(length, -1));
        expected[1].push(symmetric.swma());
    }
    expected
}

#[test]
fn realtime_borrowed_deltas_no_output_confirmation_correction_and_replay_keep_bits() {
    let hir = program(REALTIME_SOURCE);
    let prepared = PreparedProgram::new(hir.clone());
    let mut runtime = RealtimeRuntime::from_prepared(&prepared);
    let mut no_output = RealtimeRuntime::new(&hir);
    let seed: Vec<_> = (0..300)
        .map(|index| timed_bar(index, sample(index), 257.0))
        .collect();
    runtime.seed_historical_without_output(&seed).unwrap();
    no_output.seed_historical_without_output(&seed).unwrap();
    let mut committed = seed.clone();
    let mut replica = runtime.replica();
    for (close, volume, confirmed) in [
        (-0.0, 1.0, false),
        (2.1, 4.0, false),
        (3.1, 129.0, false),
        (4.1, 300.0, false),
        (5.1, 0.0, false),
        (6.1, 129.0, true),
        (7.1, 257.0, false),
        (8.1, 4.0, true),
        (9.1, 129.0, false),
    ] {
        let bar = timed_bar(committed.len(), close, volume);
        let update = if confirmed {
            BarUpdate::confirmed(bar)
        } else {
            BarUpdate::forming(bar)
        };
        replica
            .apply(runtime.apply_update_ref(update).unwrap())
            .unwrap();
        no_output.update_without_output(update).unwrap();
        let actual = runtime.result();
        assert_eq!(
            public_runtime_result_json(&actual),
            public_runtime_result_view_json(&runtime.result_view())
        );
        assert_eq!(
            public_runtime_result_json(replica.result()),
            public_runtime_result_json(&actual)
        );
        assert_eq!(
            public_runtime_result_json(&no_output.result()),
            public_runtime_result_json(&actual)
        );
        let mut visible = committed.clone();
        visible.push(bar);
        let reference = realtime_reference(&visible);
        assert_plots(&actual, &reference);
        assert_plots(replica.result(), &reference);
        assert_plots(&no_output.result(), &reference);
        assert_plots(&run_historical(&hir, &visible).unwrap(), &reference);
        if confirmed {
            committed.push(bar);
        }
    }
    let corrected: Vec<_> = (0..310)
        .map(|index| timed_bar(index, sample(index) + 0.25, 129.0))
        .collect();
    runtime.correct_historical(0, &corrected).unwrap();
    no_output
        .correct_historical_without_output(0, &corrected)
        .unwrap();
    assert_plots(&runtime.result(), &realtime_reference(&corrected));
    assert_eq!(
        public_runtime_result_json(&runtime.result()),
        public_runtime_result_json(&no_output.result())
    );
    runtime.replay_historical(&seed).unwrap();
    no_output.replay_historical_without_output(&seed).unwrap();
    let next = BarUpdate::forming(timed_bar(seed.len(), -0.0, 1.0));
    let mut replica = runtime.replica();
    replica
        .apply(runtime.apply_update_ref(next).unwrap())
        .unwrap();
    no_output.update_without_output(next).unwrap();
    let mut visible = seed;
    visible.push(next.bar);
    let reference = realtime_reference(&visible);
    assert_plots(&runtime.result(), &reference);
    assert_plots(replica.result(), &reference);
    assert_plots(&no_output.result(), &reference);
    assert_eq!(
        public_runtime_result_json(replica.result()),
        public_runtime_result_json(&runtime.result())
    );
    assert_eq!(
        public_runtime_result_json(&no_output.result()),
        public_runtime_result_json(&runtime.result())
    );
}

#[test]
fn prepared_requested_scans_keep_independent_provider_and_chart_windows() {
    let hir = program(
        "indicator(\"Requested scan windows\")\nplot(request.security(\"ALT\", \"1\", ta.linreg(close, 129, -1)))\nplot(request.security(\"ALT\", \"1\", ta.swma(close)))\nplot(ta.linreg(close, 129, -1))\nplot(ta.swma(close))\n",
    );
    let chart: Vec<_> = (0..350)
        .map(|index| timed_bar(index, sample(index) + 20.0, 1.0))
        .collect();
    let requested: Vec<_> = (0..350)
        .map(|index| timed_bar(index, sample(index * 3) + 100.0, 1.0))
        .collect();
    let timeframe = RequestTimeframe::parse("1").unwrap();
    let provider = InMemoryRequestDataProvider::from_streams([(
        RequestKey::new("ALT", timeframe.clone()),
        requested.clone(),
    )])
    .unwrap();
    let environment =
        RequestEnvironment::new(ChartContext::new("CHART", timeframe), Arc::new(provider));
    let prepared = PreparedProgram::new(hir);
    let mut runtime = HistoricalRuntime::from_prepared_with_request_environment_and_input_overrides(
        &prepared,
        environment,
        InputOverrides::default(),
    );
    runtime.append_bars(&chart).unwrap();
    let mut expected = Vec::new();
    for bars in [&requested, &chart] {
        let mut regression = LegacyWindow::default();
        let mut symmetric = LegacyWindow::default();
        let mut regression_values = Vec::new();
        let mut symmetric_values = Vec::new();
        for bar in bars {
            regression.push(Some(bar.close), 129);
            symmetric.push(Some(bar.close), 4);
            regression_values.push(regression.linreg(129, -1));
            symmetric_values.push(symmetric.swma());
        }
        expected.extend([regression_values, symmetric_values]);
    }
    assert_plots(&runtime.result(), &expected);
}
