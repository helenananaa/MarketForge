use std::collections::VecDeque;

use pine_ir::HirProgram;
use pine_runtime::{
    Bar, BarUpdate, HistoricalRuntime, PineValue, RealtimeRuntime, RuntimeResult,
    public_runtime_result_json, run_historical,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

#[derive(Clone, Default)]
struct PivotOracle {
    samples: VecDeque<Option<f64>>,
}

impl PivotOracle {
    // Independent ordinary-call model: neither SharedDeque nor runtime pivot
    // helpers are used. Each side walks by logical index to its nearest NA.
    fn call(&mut self, source: Option<f64>, left: usize, right: usize, high: bool) -> Option<f64> {
        let length = left + right + 1;
        while self.samples.len() >= length {
            self.samples.pop_front();
        }
        self.samples
            .push_back(source.filter(|value| value.is_finite()));
        if self.samples.len() != length {
            return None;
        }
        let candidate = self.samples[left]?;
        let mut index = left;
        while index > 0 {
            index -= 1;
            let Some(value) = self.samples[index] else {
                break;
            };
            if (high && candidate < value) || (!high && candidate > value) {
                return None;
            }
        }
        index = left + 1;
        while index < length {
            let Some(value) = self.samples[index] else {
                break;
            };
            if (high && candidate <= value) || (!high && candidate >= value) {
                return None;
            }
            index += 1;
        }
        Some(candidate)
    }
}

fn program(source: &str) -> HirProgram {
    let analysis = analyze_source(&SourceFile::new("pivot-long.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.expect("pivot program")
}

fn bar(index: usize, close: f64, volume: f64) -> Bar {
    Bar {
        time: index as i64 * 60_000,
        open: close,
        high: close,
        low: close,
        close,
        volume,
    }
}

fn assert_outputs(result: &RuntimeResult, expected: &[Vec<Option<f64>>]) {
    assert_eq!(result.plots.len(), expected.len());
    for (plot_index, (plot, values)) in result.plots.iter().zip(expected).enumerate() {
        assert_eq!(plot.values.len(), values.len(), "plot {plot_index} length");
        for (index, (actual, expected)) in plot.values.iter().zip(values).enumerate() {
            match (actual, expected) {
                (PineValue::Na, None) => {}
                (PineValue::Float(actual), Some(expected)) => assert_eq!(
                    actual.to_bits(),
                    expected.to_bits(),
                    "plot {plot_index} at {index}"
                ),
                _ => panic!("plot {plot_index} at {index}: {actual:?} != {expected:?}"),
            }
        }
    }
}

#[test]
fn long_pivot_left_and_right_sides_match_independent_oracle() {
    let input: Vec<_> = (0..3_330)
        .map(|index| bar(index, index as f64, if index == 257 { 10.0 } else { 0.0 }))
        .collect();
    let mut oracles: [PivotOracle; 4] = std::array::from_fn(|_| PivotOracle::default());
    let mut expected: [Vec<Option<f64>>; 4] = std::array::from_fn(|_| Vec::new());
    for sample in &input {
        for (plot, source, left, right, high) in [
            (0, sample.close, 3_000, 0, true),
            (1, -sample.close, 3_000, 0, false),
            (2, sample.volume, 257, 3_000, true),
            (3, -sample.volume, 257, 3_000, false),
        ] {
            expected[plot].push(oracles[plot].call(Some(source), left, right, high));
        }
    }
    assert_eq!(expected[0][3_000], Some(3_000.0));
    assert_eq!(expected[2][3_257], Some(10.0));
    for version in [5, 6] {
        let hir = program(&format!(
            "//@version={version}\nindicator(\"long sides\")\nplot(ta.pivothigh(close,3000,0))\nplot(ta.pivotlow(-close,3000,0))\nplot(ta.pivothigh(volume,257,3000))\nplot(ta.pivotlow(-volume,257,3000))\n"
        ));
        let result = run_historical(&hir, &input).unwrap();
        assert_outputs(&result, &expected);
        let mut incremental = HistoricalRuntime::new(&hir);
        incremental.append_bars(&input[..3_130]).unwrap();
        incremental.append_bars(&input[3_130..]).unwrap();
        assert_outputs(&incremental.result(), &expected);
        assert_eq!(
            public_runtime_result_json(&result),
            public_runtime_result_json(&incremental.result())
        );
    }
}

#[test]
fn paged_pivot_nearest_na_ties_and_signed_zero_preserve_bits() {
    let mut input: Vec<_> = (0..860).map(|index| bar(index, 0.0, 1.0)).collect();
    // The farther extreme must be hidden by the immediately adjacent NA.
    for (index, close) in [
        (298, 100.0),
        (300, 5.0),
        (302, 100.0),
        (439, 7.0),
        (440, 7.0),
        (600, 9.0),
        (601, 9.0),
        (699, 0.0),
        (700, -0.0),
        (719, -0.0),
        (720, 0.0),
    ] {
        input[index].close = close;
    }
    for index in [299, 301, 400, 438, 441, 599, 698, 701, 718, 721] {
        input[index].volume = 0.0;
    }
    let mut high = PivotOracle::default();
    let mut low = PivotOracle::default();
    let mut expected = [Vec::new(), Vec::new()];
    for sample in &input {
        let source = (sample.volume != 0.0).then_some(sample.close);
        expected[0].push(high.call(source, 257, 129, true));
        expected[1].push(low.call(source.map(|value| -value), 257, 129, false));
    }
    assert_eq!(expected[0][429], Some(5.0));
    assert_eq!(expected[0][569], Some(7.0));
    assert_eq!(expected[0][729], None);
    assert_eq!(expected[0][529], None);
    assert_eq!(expected[0][829].unwrap().to_bits(), (-0.0_f64).to_bits());
    assert_eq!(expected[0][849].unwrap().to_bits(), 0.0_f64.to_bits());
    for version in [5, 6] {
        let hir = program(&format!(
            "//@version={version}\nindicator(\"paged boundaries\")\ns=volume==0?float(na):close\nplot(ta.pivothigh(s,257,129))\nplot(ta.pivotlow(-s,257,129))\n"
        ));
        assert_outputs(&run_historical(&hir, &input).unwrap(), &expected);
    }
}

fn dynamic_sides(index: usize) -> (usize, usize) {
    let left = if index < 360 {
        257
    } else if index < 420 {
        127
    } else if index < 480 {
        128
    } else if index < 540 {
        129
    } else {
        257
    };
    let right = if index < 380 {
        3
    } else if index < 450 {
        129
    } else if index < 510 {
        1
    } else if index < 570 {
        0
    } else {
        257
    };
    (left, right)
}

#[test]
fn dynamic_pivot_sides_and_same_site_loops_consume_every_call() {
    let hir = program(
        "//@version=6\nindicator(\"dynamic loop\")\ns=volume==0?float(na):close\nleft=bar_index<360?257:bar_index<420?127:bar_index<480?128:bar_index<540?129:257\nright=bar_index<380?3:bar_index<450?129:bar_index<510?1:bar_index<570?0:257\nph=float(na)\npl=float(na)\nfor i=0 to 2\n    ph:=ta.pivothigh(s+i*0.25,left,right)\n    pl:=ta.pivotlow(-s+i*0.25,left,right)\nplot(ph)\nplot(pl)\n",
    );
    let input: Vec<_> = (0..780)
        .map(|index| {
            bar(
                index,
                (index % 37) as f64 * 0.5,
                if index % 53 == 0 { 0.0 } else { 1.0 },
            )
        })
        .collect();
    let mut high = PivotOracle::default();
    let mut low = PivotOracle::default();
    let mut expected = [Vec::new(), Vec::new()];
    for (index, sample) in input.iter().enumerate() {
        let (left, right) = dynamic_sides(index);
        let source = (sample.volume != 0.0).then_some(sample.close);
        let mut values = [None, None];
        for call in 0..3 {
            let offset = call as f64 * 0.25;
            values[0] = high.call(source.map(|value| value + offset), left, right, true);
            values[1] = low.call(source.map(|value| -value + offset), left, right, false);
        }
        for (plot, value) in expected.iter_mut().zip(values) {
            plot.push(value);
        }
    }
    assert!(expected.iter().flatten().any(Option::is_some));
    assert_outputs(&run_historical(&hir, &input).unwrap(), &expected);
}

fn replay_call(oracles: &mut [PivotOracle; 2], sample: Bar) -> [Option<f64>; 2] {
    let (left, right) = if sample.volume > 1.0 {
        (257, 129)
    } else {
        (129, 3)
    };
    [
        oracles[0].call(Some(sample.close), left, right, true),
        oracles[1].call(Some(-sample.close), left, right, false),
    ]
}

fn append_expected(expected: &mut [Vec<Option<f64>>], values: [Option<f64>; 2]) {
    for (plot, value) in values.into_iter().enumerate() {
        expected[plot].push(value);
    }
}

#[test]
fn paged_pivot_realtime_replacements_failure_and_future_commits_roll_back() {
    let hir = program(
        "//@version=6\nindicator(\"pivot rollback\")\nleft=volume>1?257:129\nright=volume>1?129:3\nph=ta.pivothigh(close,left,right)\npl=ta.pivotlow(-close,left,right)\nif volume<0\n    runtime.error(\"pivot rollback\")\nplot(ph)\nplot(pl)\n",
    );
    let mut input: Vec<_> = (0..600)
        .map(|index| bar(index, if index == 471 { 50.0 } else { 0.0 }, 2.0))
        .collect();
    let mut oracles = [PivotOracle::default(), PivotOracle::default()];
    let mut expected = [Vec::new(), Vec::new()];
    for sample in &input {
        append_expected(&mut expected, replay_call(&mut oracles, *sample));
    }
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime.seed_historical(&input).unwrap();
    assert_outputs(&runtime.result(), &expected);
    let confirmed = public_runtime_result_json(&runtime.confirmed_result());
    for (close, volume) in [(20.0, 2.0), (100.0, 2.0), (25.0, 1.0), (20.0, 2.0)] {
        let sample = bar(600, close, volume);
        let mut branch = oracles.clone();
        let mut visible = expected.clone();
        append_expected(&mut visible, replay_call(&mut branch, sample));
        runtime
            .update_without_output(BarUpdate::forming(sample))
            .unwrap();
        assert_outputs(&runtime.result(), &visible);
        assert_eq!(runtime.confirmed_bar_count(), 600);
        assert_eq!(
            public_runtime_result_json(&runtime.confirmed_result()),
            confirmed
        );
    }
    assert_eq!(
        runtime.result().plots[0].values.last(),
        Some(&PineValue::Float(50.0))
    );
    let before = public_runtime_result_json(&runtime.result());
    let revision = runtime.revision();
    let profile = runtime.profile();
    let confirmed_profile = runtime.confirmed_profile();
    let error = runtime
        .update_without_output(BarUpdate::forming(bar(600, 120.0, -1.0)))
        .unwrap_err();
    assert_eq!(error.message, "pivot rollback");
    assert_eq!(runtime.revision(), revision);
    assert_eq!(runtime.confirmed_bar_count(), 600);
    assert_eq!(runtime.profile(), profile);
    assert_eq!(runtime.confirmed_profile(), confirmed_profile);
    assert_eq!(public_runtime_result_json(&runtime.result()), before);
    assert_eq!(
        public_runtime_result_json(&runtime.confirmed_result()),
        confirmed
    );
    let sample = bar(600, 60.0, 2.0);
    let mut branch = oracles.clone();
    let mut visible = expected.clone();
    append_expected(&mut visible, replay_call(&mut branch, sample));
    assert_outputs(
        &runtime.update(BarUpdate::forming(sample)).unwrap(),
        &visible,
    );
    append_expected(&mut expected, replay_call(&mut oracles, sample));
    input.push(sample);
    assert_outputs(
        &runtime.update(BarUpdate::confirmed(sample)).unwrap(),
        &expected,
    );
    for index in 601..731 {
        let sample = bar(index, 0.0, 2.0);
        append_expected(&mut expected, replay_call(&mut oracles, sample));
        input.push(sample);
        runtime
            .update_without_output(BarUpdate::confirmed(sample))
            .unwrap();
        assert_outputs(&runtime.result(), &expected);
    }
    assert_eq!(expected[0][729], Some(60.0));
    assert_eq!(runtime.confirmed_bar_count(), input.len());
    assert_outputs(&runtime.confirmed_result(), &expected);
    assert_eq!(
        public_runtime_result_json(&runtime.result()),
        public_runtime_result_json(&run_historical(&hir, &input).unwrap())
    );
}
