use super::*;

fn program(text: &str) -> pine_ir::HirProgram {
    let source = pine_syntax::SourceFile::new("paired_statistics.pine", text);
    let analysis = pine_sema::analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.unwrap()
}

fn bar(index: i64, left: f64, right: f64) -> Bar {
    Bar {
        time: index * 60_000,
        open: right,
        high: left.max(right),
        low: left.min(right),
        close: left,
        volume: 100.0,
    }
}

fn run_pairs(pairs: &[(f64, f64)], length: usize) -> RuntimeResult {
    let program = program(&format!(
        "//@version=6\nindicator(\"paired\")\nplot(ta.correlation(close, open, {length}))\nplot(ta.covariance(close, open, {length}))\n"
    ));
    let mut runtime = HistoricalRuntime::new(&program);
    for (index, &(left, right)) in pairs.iter().enumerate() {
        runtime.append_bar(bar(index as i64, left, right)).unwrap();
    }
    runtime.result()
}

fn assert_close(value: &PineValue, expected: f64) {
    let PineValue::Float(value) = value else {
        panic!("expected {expected}, got {value:?}");
    };
    if expected == 0.0 {
        assert_eq!(*value, 0.0);
    } else {
        assert!(
            (*value - expected).abs() <= 1e-13 * expected.abs(),
            "{value} != {expected}"
        );
    }
}

fn window(values: &[f64]) -> RollingWindowState {
    let mut window = RollingWindowState::default();
    for &value in values {
        window.push(Some(value), values.len());
    }
    window
}

#[test]
fn partial_precision_recovery_uses_centered_scale_for_multiple_periods_and_offsets() {
    for period in [37, 127, 257, 511] {
        let variance = (period * period - 1) as f64 / 12.0;
        for (left_offset, right_offset, slope) in
            [(1e8, 2e8, 1.0), (-1e8, 3e8, 2.0), (1e9, -2e9, -3.0)]
        {
            let pairs = (0..2 * period)
                .map(|index| {
                    let phase = (index % period) as f64;
                    (left_offset + phase, right_offset + slope * phase)
                })
                .collect::<Vec<_>>();
            let result = run_pairs(&pairs, period);
            for index in 0..pairs.len() {
                if index + 1 < period {
                    assert_eq!(result.plots[0].values[index], PineValue::Na);
                    assert_eq!(result.plots[1].values[index], PineValue::Na);
                } else {
                    assert_close(&result.plots[0].values[index], slope.signum());
                    assert_close(&result.plots[1].values[index], slope * variance);
                }
            }
        }
    }

    // Distinct permutations of 0..256 have the same variance 5504. Doubling
    // modulo the odd period gives covariance exactly half that variance.
    let pairs = (0..514)
        .map(|index| (1e8 + (index % 257) as f64, -2e8 + (2 * index % 257) as f64))
        .collect::<Vec<_>>();
    let result = run_pairs(&pairs, 257);
    for index in 256..pairs.len() {
        assert_close(&result.plots[0].values[index], 0.5);
        assert_close(&result.plots[1].values[index], 2752.0);
    }

    let orthogonal = [
        (1e8 - 1.0, -2e8 - 3.0),
        (1e8 - 1.0, -2e8 + 3.0),
        (1e8 + 1.0, -2e8 - 3.0),
        (1e8 + 1.0, -2e8 + 3.0),
    ];
    let result = run_pairs(&orthogonal, 4);
    assert_close(result.plots[0].values.last().unwrap(), 0.0);
    assert_close(result.plots[1].values.last().unwrap(), 0.0);

    // The previously missed window fails the old complete-loss heuristic.
    assert!(cancellation_budget(5502.0, 1e16, 1e16, 257).is_none());
    assert!(partial_precision_budget(5502.0, 1e16, 1e16).unwrap() >= 5504.0);
}

#[test]
fn partial_precision_screen_preserves_moderate_offsets_and_constant_residual_bits() {
    let pairs = (0..257)
        .map(|index| (1e6 + 0.1 + index as f64, 2e6 + 0.3 + 1.25 * index as f64))
        .collect::<Vec<_>>();
    let length = pairs.len();
    let left = window(&pairs.iter().map(|pair| pair.0).collect::<Vec<_>>());
    let right = window(&pairs.iter().map(|pair| pair.1).collect::<Vec<_>>());
    let product = window(&pairs.iter().map(|pair| pair.0 * pair.1).collect::<Vec<_>>());
    let mean_product = left.mean(length) * right.mean(length);
    let covariance = product.mean(length) - mean_product;
    let denominator = (left.variance(length, true) * right.variance(length, true)).sqrt();
    assert!(partial_precision_budget(covariance, product.mean(length), mean_product).is_none());
    let budget = partial_precision_budget(0.0, product.mean(length), mean_product).unwrap();
    assert!(budget < denominator);
    let result = run_pairs(&pairs, length);
    assert_eq!(
        result.plots[1]
            .values
            .last()
            .unwrap()
            .as_f64()
            .unwrap()
            .to_bits(),
        covariance.to_bits()
    );
    assert_eq!(
        result.plots[0]
            .values
            .last()
            .unwrap()
            .as_f64()
            .unwrap()
            .to_bits(),
        (covariance / denominator).to_bits()
    );

    let pairs = [(0.1, 0.2), (0.1, 0.4), (0.1, 0.6)];
    let left = window(&[0.1, 0.1, 0.1]);
    let right = window(&[0.2, 0.4, 0.6]);
    let product = window(&pairs.map(|pair| pair.0 * pair.1));
    let covariance = product.mean(3) - left.mean(3) * right.mean(3);
    assert_ne!(covariance, 0.0);
    let result = run_pairs(&pairs, 3);
    assert_eq!(result.plots[0].values.last(), Some(&PineValue::Na));
    assert_eq!(
        result.plots[1]
            .values
            .last()
            .unwrap()
            .as_f64()
            .unwrap()
            .to_bits(),
        covariance.to_bits()
    );

    // Large raw operands do not alone certify small centered variance. A
    // sparsely placed outlier can escape the constant-time sampled screen;
    // the full centered SD product still rejects numerical recovery.
    let mut left_sparse = vec![1e8; 257];
    let mut right_sparse = vec![1e8; 257];
    left_sparse[73] += 1e7;
    left_sparse[74] -= 1e7;
    right_sparse[75] += 1e7;
    right_sparse[76] -= 1e7;
    let left_sparse = window(&left_sparse);
    let right_sparse = window(&right_sparse);
    let budget = partial_precision_budget(0.0, 1e16, 1e16).unwrap();
    assert!(!observed_spread_excludes_cancellation(
        &left_sparse,
        &right_sparse,
        257,
        budget
    ));
    assert!(
        centered_pair_moments(&left_sparse, &right_sparse, 257)
            .unwrap()
            .standard_deviation_product()
            > budget
    );
    let pairs = left_sparse
        .values
        .iter()
        .zip(&right_sparse.values)
        .map(|(left, right)| (left.unwrap(), right.unwrap()))
        .collect::<Vec<_>>();
    let product = window(&pairs.iter().map(|pair| pair.0 * pair.1).collect::<Vec<_>>());
    let mean_product = left_sparse.mean(257) * right_sparse.mean(257);
    let covariance = product.mean(257) - mean_product;
    assert!(partial_precision_budget(covariance, product.mean(257), mean_product).is_some());
    let result = run_pairs(&pairs, 257);
    assert_eq!(
        result.plots[1]
            .values
            .last()
            .unwrap()
            .as_f64()
            .unwrap()
            .to_bits(),
        covariance.to_bits()
    );
}

#[test]
fn cancellation_recovery_handles_distinct_shifted_and_opposite_sources() {
    // For two points covariance = (x1-x0)*(y1-y0)/4, independent
    // of either offset. No identity shortcut can produce these three answers.
    for (pairs, expected_covariance, expected_correlation) in [
        ([(1e8, 2e8), (1e8 + 1.0, 2e8 + 1.0)], 0.25, 1.0),
        ([(1e8, 2e8), (1e8 + 1.0, 2e8 + 3.0)], 0.75, 1.0),
        ([(1e8, 3e8), (1e8 + 1.0, 3e8 - 2.0)], -0.5, -1.0),
    ] {
        let result = run_pairs(&pairs, 2);
        assert_eq!(result.plots[0].values[0], PineValue::Na);
        assert_eq!(result.plots[1].values[0], PineValue::Na);
        assert_close(result.plots[0].values.last().unwrap(), expected_correlation);
        assert_close(result.plots[1].values.last().unwrap(), expected_covariance);
    }
    // Uniform points 0,1,2 have population variance 2/3; y=3x gives covariance 2.
    let result = run_pairs(
        &[(1e8, 2e8), (1e8 + 1.0, 2e8 + 3.0), (1e8 + 2.0, 2e8 + 6.0)],
        3,
    );
    assert_close(result.plots[0].values.last().unwrap(), 1.0);
    assert_close(result.plots[1].values.last().unwrap(), 2.0);

    // Every full period contains the arithmetic progression 0..36 exactly
    // once; its population variance is (37²-1)/12 = 114. The raw moment
    // subtraction formerly returned 116 despite the correct centered variance.
    let pairs = (0..74)
        .map(|index| {
            let value = 1e8 + (index % 37) as f64;
            (value, value)
        })
        .collect::<Vec<_>>();
    let result = run_pairs(&pairs, 37);
    for index in 0..pairs.len() {
        if index < 36 {
            assert_eq!(result.plots[0].values[index], PineValue::Na);
            assert_eq!(result.plots[1].values[index], PineValue::Na);
        } else {
            assert_close(&result.plots[0].values[index], 1.0);
            assert_close(&result.plots[1].values[index], 114.0);
        }
    }
}

#[test]
fn ordinary_results_keep_the_original_float_order_and_zero_covariance_fast_reject() {
    let pairs = [(1.0, 2.0), (2.0, 3.0), (4.0, 6.0), (8.0, 10.0)];
    let result = run_pairs(&pairs, 4);
    let left_mean = pairs
        .iter()
        .map(|pair| pair.0)
        .fold(0.0, |sum, value| sum + value)
        / 4.0;
    let right_mean = pairs
        .iter()
        .map(|pair| pair.1)
        .fold(0.0, |sum, value| sum + value)
        / 4.0;
    let product_mean = pairs
        .iter()
        .map(|pair| pair.0 * pair.1)
        .fold(0.0, |sum, value| sum + value)
        / 4.0;
    let left_variance = pairs
        .iter()
        .map(|pair| (pair.0 - left_mean).powi(2))
        .sum::<f64>()
        / 4.0;
    let right_variance = pairs
        .iter()
        .map(|pair| (pair.1 - right_mean).powi(2))
        .sum::<f64>()
        / 4.0;
    let covariance = product_mean - left_mean * right_mean;
    let correlation = covariance / (left_variance * right_variance).sqrt();
    assert_eq!(
        result.plots[1]
            .values
            .last()
            .unwrap()
            .as_f64()
            .unwrap()
            .to_bits(),
        covariance.to_bits()
    );
    assert_eq!(
        result.plots[0]
            .values
            .last()
            .unwrap()
            .as_f64()
            .unwrap()
            .to_bits(),
        correlation.to_bits()
    );
    assert_eq!(covariance, 8.3125);
    assert!(cancellation_budget(covariance, product_mean, left_mean * right_mean, 4).is_none());

    let orthogonal = [(99.0, 199.0), (99.0, 201.0), (101.0, 199.0), (101.0, 201.0)];
    let left = window(&[99.0, 99.0, 101.0, 101.0]);
    let right = window(&[199.0, 201.0, 199.0, 201.0]);
    let budget = cancellation_budget(0.0, 20_000.0, 20_000.0, 4).unwrap();
    assert!(observed_spread_excludes_cancellation(
        &left, &right, 4, budget
    ));
    let result = run_pairs(&orthogonal, 4);
    assert_eq!(result.plots[0].values.last(), Some(&PineValue::Float(0.0)));
    assert_eq!(result.plots[1].values.last(), Some(&PineValue::Float(0.0)));

    // With length 512, front/middle/back alone alias the same x phase for
    // half the rotations. Every full period is balanced and orthogonal, so
    // no rotation should need a centered scan just to preserve covariance 0.
    for phase in 0..4 {
        let pairs = (0..512)
            .map(|index| orthogonal[(index + phase) % orthogonal.len()])
            .collect::<Vec<_>>();
        let left = window(&pairs.iter().map(|pair| pair.0).collect::<Vec<_>>());
        let right = window(&pairs.iter().map(|pair| pair.1).collect::<Vec<_>>());
        let budget = cancellation_budget(0.0, 20_000.0, 20_000.0, 512).unwrap();
        assert!(observed_spread_excludes_cancellation(
            &left, &right, 512, budget
        ));
        let result = run_pairs(&pairs, 512);
        assert_eq!(result.plots[0].values.last(), Some(&PineValue::Float(0.0)));
        assert_eq!(result.plots[1].values.last(), Some(&PineValue::Float(0.0)));
    }

    // The additional partial-loss criterion must also retain a constant-time
    // rejection for ordinary large windows, in every phase of the sources.
    for phase in 0..4 {
        let pairs = (0..8192)
            .map(|index| orthogonal[(index + phase) % orthogonal.len()])
            .collect::<Vec<_>>();
        let left = window(&pairs.iter().map(|pair| pair.0).collect::<Vec<_>>());
        let right = window(&pairs.iter().map(|pair| pair.1).collect::<Vec<_>>());
        let budget = cancellation_budget(0.0, 20_000.0, 20_000.0, 8192)
            .unwrap()
            .max(partial_precision_budget(0.0, 20_000.0, 20_000.0).unwrap());
        assert!(observed_spread_excludes_cancellation(
            &left, &right, 8192, budget
        ));
    }
}

#[test]
fn normalized_centering_handles_subnormal_means_and_extreme_derived_products() {
    let smallest = f64::from_bits(1);
    for pairs in [
        [(0.0, smallest), (smallest, 0.0)],
        [(1e-200, 2e-200), (2e-200, 1e-200)],
    ] {
        let result = run_pairs(&pairs, 2);
        assert_close(result.plots[0].values.last().unwrap(), -1.0);
        assert_close(result.plots[1].values.last().unwrap(), 0.0);
    }
    for (pairs, expected_correlation) in [
        ([(1e200, 2e200), (2e200, 1e200)], -1.0),
        ([(1e308, 1e308), (-1e308, -1e308)], 1.0),
    ] {
        let result = run_pairs(&pairs, 2);
        assert_close(result.plots[0].values.last().unwrap(), expected_correlation);
        assert_eq!(result.plots[1].values.last(), Some(&PineValue::Na));
    }
    let invalid = window(&[f64::INFINITY, f64::INFINITY]);
    assert!(centered_pair_moments(&invalid, &window(&[1.0, 2.0]), 2).is_none());
    let mut missing = RollingWindowState::default();
    missing.push(Some(1e200), 2);
    missing.push(None, 2);
    assert!(centered_pair_moments(&missing, &window(&[1e200, 2e200]), 2).is_none());
}

#[test]
fn constant_sources_preserve_zero_covariance_and_undefined_correlation() {
    for pairs in [
        [(10.0, 20.0), (10.0, 20.0)],
        [(1e200, 2e200), (1e200, 3e200)],
        [(0.0, 1.0), (-0.0, 2.0)],
    ] {
        let result = run_pairs(&pairs, 2);
        assert_eq!(result.plots[0].values.last(), Some(&PineValue::Na));
        assert_close(result.plots[1].values.last().unwrap(), 0.0);
    }
}

#[test]
fn named_arguments_evaluate_each_expression_once_in_the_existing_kernel_order() {
    let program = program(
        r#"//@version=6
indicator("paired side effects")
var trace = array.new_int()
mark_source(float value, int tag) =>
    array.push(trace, tag)
    value
mark_length(int value) =>
    array.push(trace, 3)
    value
x = 1e8 + bar_index % 2
y = 2e8 + 3 * (bar_index % 2)
corr = ta.correlation(length=mark_length(2), source2=mark_source(y, 2), source1=mark_source(x, 1))
cov = ta.covariance(source2=mark_source(y, 2), length=mark_length(2), source1=mark_source(x, 1))
n = array.size(trace)
plot(corr)
plot(cov)
plot(n)
plot(array.get(trace,n-6)*100000 + array.get(trace,n-5)*10000 + array.get(trace,n-4)*1000 + array.get(trace,n-3)*100 + array.get(trace,n-2)*10 + array.get(trace,n-1))
"#,
    );
    let mut runtime = HistoricalRuntime::new(&program);
    for index in 0..3 {
        runtime.append_bar(bar(index, 1.0, 2.0)).unwrap();
    }
    let result = runtime.result();
    assert_eq!(
        result.plots[2].values,
        [PineValue::Int(6), PineValue::Int(12), PineValue::Int(18)]
    );
    assert_eq!(result.plots[3].values, vec![PineValue::Int(123123); 3]);
    assert_close(result.plots[0].values.last().unwrap(), 1.0);
    assert_close(result.plots[1].values.last().unwrap(), 0.75);
}

#[test]
fn repeated_loop_calls_keep_the_existing_per_invocation_window_updates() {
    let program = program(
        r#"//@version=6
indicator("paired repeated calls")
corr = 0.0
cov = 0.0
for i = 0 to 1
    corr := ta.correlation(1e8 + i, 2e8 + 3*i, 2)
    cov := ta.covariance(1e8 + i, 2e8 + 3*i, 2)
plot(corr)
plot(cov)
"#,
    );
    let mut runtime = HistoricalRuntime::new(&program);
    for index in 0..3 {
        runtime.append_bar(bar(index, 1.0, 2.0)).unwrap();
    }
    let result = runtime.result();
    for value in &result.plots[0].values {
        assert_close(value, 1.0);
    }
    for value in &result.plots[1].values {
        assert_close(value, 0.75);
    }
}

#[test]
fn forming_replacements_restore_na_and_dynamic_length_in_chart_and_request_windows() {
    let program = program(
        r#"//@version=6
indicator("paired forming state")
length = close > 1e8 + 3 ? 2 : 3
source = bar_index == 3 and close < 1e8 + 2 ? na : close
plot(ta.correlation(source1=source, source2=open, length=length))
plot(ta.covariance(length=length, source2=open, source1=source))
plot(request.security(syminfo.tickerid, timeframe.period, ta.correlation(source, open, length)))
plot(request.security(syminfo.tickerid, timeframe.period, ta.covariance(source, open, length)))
"#,
    );
    let mut runtime = RealtimeRuntime::new(&program);
    let make_bar = |index, delta| bar(index, 1e8 + delta, 2e8 + 2.0 * delta);
    runtime
        .seed_historical(&[make_bar(0, 0.0), make_bar(1, 1.0), make_bar(2, 2.0)])
        .unwrap();
    for (delta, confirmed, expected_covariance) in [
        (3.0, false, Some(4.0 / 3.0)),
        (5.0, false, Some(4.5)),
        (1.5, false, None),
        (4.0, false, Some(2.0)),
        (3.0, true, Some(4.0 / 3.0)),
    ] {
        let update = if confirmed {
            BarUpdate::confirmed(make_bar(3, delta))
        } else {
            BarUpdate::forming(make_bar(3, delta))
        };
        runtime.apply_update(update).unwrap();
        let result = runtime.result();
        for index in [0, 2] {
            let value = result.plots[index].values.last().unwrap();
            if expected_covariance.is_some() {
                assert_close(value, 1.0);
            } else {
                assert_eq!(*value, PineValue::Na);
            }
        }
        for index in [1, 3] {
            let value = result.plots[index].values.last().unwrap();
            if let Some(expected) = expected_covariance {
                assert_close(value, expected);
            } else {
                assert_eq!(*value, PineValue::Na);
            }
        }
    }
}

#[test]
fn partial_precision_recovery_restores_large_forming_windows_after_length_and_na_changes() {
    let program = program(
        r#"//@version=6
indicator("partial precision forming state")
length = close > 1e8 + 300 ? 127 : 257
source = close < 1e8 ? na : close
plot(ta.correlation(source, open, length))
plot(ta.covariance(source, open, length))
plot(request.security(syminfo.tickerid, timeframe.period, ta.correlation(source, open, length)))
plot(request.security(syminfo.tickerid, timeframe.period, ta.covariance(source, open, length)))
"#,
    );
    let mut confirmed = (0..514)
        .map(|index| {
            bar(
                index,
                1e8 + (index % 257) as f64,
                2e8 + (2 * index % 257) as f64,
            )
        })
        .collect::<Vec<_>>();
    let mut runtime = RealtimeRuntime::new(&program);
    runtime.seed_historical(&confirmed).unwrap();
    for (left_delta, right_delta, commit) in [
        (128.5, 64.25, false),
        (400.0, 200.5, false),
        (-1.0, 0.0, false),
        (255.5, 140.5, false),
        (129.0, 101.0, true),
    ] {
        let current = bar(514, 1e8 + left_delta, 2e8 + right_delta);
        runtime
            .apply_update(if commit {
                BarUpdate::confirmed(current)
            } else {
                BarUpdate::forming(current)
            })
            .unwrap();
        let result = runtime.result();
        if left_delta < 0.0 {
            for plot in &result.plots {
                assert_eq!(plot.values.last(), Some(&PineValue::Na));
            }
            continue;
        }
        let length = if left_delta > 300.0 { 127 } else { 257 };
        let mut reference = confirmed.clone();
        reference.push(current);
        // Independent offset-free two-pass arithmetic uses the actual tail
        // after every replacement, without any production rolling helper.
        let reference = &reference[reference.len() - length..];
        let left_mean = reference.iter().map(|bar| bar.close - 1e8).sum::<f64>() / length as f64;
        let right_mean = reference.iter().map(|bar| bar.open - 2e8).sum::<f64>() / length as f64;
        let mut cross_sum = 0.0;
        let mut left_square_sum = 0.0;
        let mut right_square_sum = 0.0;
        for sample in reference {
            let left = sample.close - 1e8 - left_mean;
            let right = sample.open - 2e8 - right_mean;
            cross_sum += left * right;
            left_square_sum += left * left;
            right_square_sum += right * right;
        }
        let covariance = cross_sum / length as f64;
        let correlation = cross_sum / (left_square_sum * right_square_sum).sqrt();
        for index in [0, 2] {
            assert_close(result.plots[index].values.last().unwrap(), correlation);
        }
        for index in [1, 3] {
            assert_close(result.plots[index].values.last().unwrap(), covariance);
        }
        if commit {
            confirmed.push(current);
        }
    }
}

#[test]
fn length_aware_screening_recovers_seeded_realtime_drift_without_bridging_na() {
    let program = program(
        r#"//@version=6
indicator("seeded paired drift")
x = bar_index % 7 == 0 ? na : 100000000.0 + close
y = -x
plot(ta.correlation(x, y, 4))
plot(ta.covariance(x, y, 4))
"#,
    );
    let make_bar = |index: usize, offset: f64| {
        let open = 100.0 + (index % 17) as f64 / 16.0;
        bar(index as i64, open + offset, open)
    };
    let mut confirmed = (0..257)
        .map(|index| make_bar(index, 0.25))
        .collect::<Vec<_>>();
    let check = |result: &RuntimeResult, bars: &[Bar]| {
        // A separate bar-count reference retains NA positions. Dyadic input
        // sums are exact here; direct centered squares supply the mathematical
        // covariance for y=-x without using any production rolling helper.
        let mut reference = std::collections::VecDeque::new();
        assert_eq!(result.plots[0].values.len(), bars.len());
        assert_eq!(result.plots[1].values.len(), bars.len());
        for (index, bar) in bars.iter().enumerate() {
            let value = (index % 7 != 0).then_some(100000000.0 + bar.close);
            reference.push_back(value);
            if reference.len() > 4 {
                reference.pop_front();
            }
            if reference.len() < 4 || reference.iter().any(Option::is_none) {
                assert_eq!(result.plots[0].values[index], PineValue::Na);
                assert_eq!(result.plots[1].values[index], PineValue::Na);
                continue;
            }
            let mean = reference.iter().flatten().sum::<f64>() / 4.0;
            let variance = reference
                .iter()
                .flatten()
                .map(|value| (value - mean) * (value - mean))
                .sum::<f64>()
                / 4.0;
            assert!(variance > 0.0);
            assert_close(&result.plots[0].values[index], -1.0);
            assert_close(&result.plots[1].values[index], -variance);
        }
    };
    let mut runtime = RealtimeRuntime::new(&program);
    runtime.seed_historical(&confirmed).unwrap();
    check(&runtime.result(), &confirmed);
    for index in 257..265 {
        for (offset, commit) in [(0.5, false), (-0.5, false), (0.25, false), (-0.25, true)] {
            let current = make_bar(index, offset);
            let mut expected = confirmed.clone();
            expected.push(current);
            runtime
                .apply_update(if commit {
                    BarUpdate::confirmed(current)
                } else {
                    BarUpdate::forming(current)
                })
                .unwrap();
            check(&runtime.result(), &expected);
            if commit {
                confirmed.push(current);
            }
        }
    }
}
