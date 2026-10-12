//! Mathematical recovery cases are separate from the legacy finite-path bit
//! comparisons: the legacy overflowing or lost-tail sums are known errors.
use std::collections::VecDeque;

use super::*;

fn assert_constant_result(window: &RollingWindowState, value: f64, length: usize) {
    assert!(window.is_ready(length));
    assert_eq!(window.sum, value * length as f64);
    assert_eq!(window.mean(length), value);
    assert_eq!(window.variance(length, true).to_bits(), 0.0_f64.to_bits());
    assert_eq!(window.variance(length, false).to_bits(), 0.0_f64.to_bits());
    let stdev = window.variance(length, true).sqrt();
    assert_eq!(stdev, 0.0);
    assert_eq!(window.mean(length) + 2.0 * stdev, value);
    assert_eq!(window.mean(length) - 2.0 * stdev, value);
}

#[test]
fn overflow_recovers_small_tail_after_both_dominant_samples_leave() {
    for sign in [1.0, -1.0] {
        for per_bar in [false, true] {
            let mut window = RollingWindowState::default();
            for (bar, sample) in [1e308, 1e308, 1.0, 1.0, 1.0].into_iter().enumerate() {
                if per_bar {
                    window.push_for_bar(Some(sign * sample), 2, bar);
                } else {
                    window.push(Some(sign * sample), 2);
                }
                match bar {
                    1 => assert!(window.sum.is_infinite()),
                    2 => assert_eq!(window.sum, sign * 1e308),
                    3 | 4 => assert_constant_result(&window, sign, 2),
                    _ => {}
                }
            }
        }
    }
}

#[test]
fn finite_dominant_removal_recovers_exact_tail_and_direct_pop() {
    for sign in [1.0, -1.0] {
        let mut window = RollingWindowState::default();
        window.push(Some(sign * 1e16), 2);
        window.push(Some(sign), 2);
        assert_eq!(window.sum, sign * 1e16);
        window.pop_front();
        assert_eq!(window.values, VecDeque::from([Some(sign)]));
        assert_eq!(window.sum, sign);
        window.push(Some(sign), 2);
        assert_constant_result(&window, sign, 2);
    }
}

#[test]
fn overflow_recovery_ignores_na_but_readiness_waits_for_a_complete_window() {
    let mut window = RollingWindowState::default();
    for sample in [Some(1e308), Some(1e308), None, Some(1.0)] {
        window.push(sample, 3);
    }
    assert_eq!(
        window.values,
        VecDeque::from([Some(1e308), None, Some(1.0)])
    );
    assert_eq!(window.sum, 1e308);
    assert!(!window.is_ready(3));
    window.pop_front();
    assert_eq!(window.sum, 1.0);
    assert_eq!(window.na_count, 1);
    window.push(Some(1.0), 3);
    assert!(!window.is_ready(3));
    window.push(Some(1.0), 3);
    assert_constant_result(&window, 1.0, 3);
}

#[test]
fn dynamic_shrink_recovers_after_all_evictions_and_append() {
    for per_bar in [false, true] {
        let mut window = RollingWindowState::default();
        for (bar, sample) in [
            Some(1e308),
            Some(1e308),
            None,
            Some(1.0),
            Some(2.0),
            Some(3.0),
        ]
        .into_iter()
        .enumerate()
        {
            if per_bar {
                window.push_for_bar(sample, 6, bar);
            } else {
                window.push(sample, 6);
            }
        }
        assert!(window.sum.is_infinite());
        let before = window.clone();
        if per_bar {
            window.push_for_bar(Some(4.0), 4, 6);
        } else {
            window.push(Some(4.0), 4);
        }
        assert_eq!(
            window.values,
            VecDeque::from([Some(1.0), Some(2.0), Some(3.0), Some(4.0)])
        );
        assert_eq!(window.sum, 10.0);
        assert_eq!(window.variance(4, true), 1.25);
        if per_bar {
            window.discard_for_bar(6);
            assert_eq!(window.values, before.values);
            assert_eq!(window.sum.to_bits(), before.sum.to_bits());
            assert_eq!(window.na_count, before.na_count);
            window.push_for_bar(Some(5.0), 6, 6);
            assert!(window.sum.is_finite());
        }
    }
}

#[test]
fn same_bar_replacement_discard_and_checkpoint_restore_exact_prior_aggregates() {
    let mut window = RollingWindowState::default();
    window.push_for_bar(Some(1e308), 2, 0);
    window.push_for_bar(Some(1e308), 2, 1);
    let before = window.clone();
    window.push_for_bar(Some(1.0), 2, 2);
    assert_eq!(window.sum, 1e308);
    let checkpoint = window.clone();
    window.push_for_bar(Some(-1e308), 2, 2);
    assert_eq!(window.sum.to_bits(), 0.0_f64.to_bits());
    window.discard_for_bar(2);
    assert_eq!(window.values, before.values);
    assert_eq!(window.sum.to_bits(), before.sum.to_bits());
    window = checkpoint;
    window.push_for_bar(None, 2, 2);
    assert_eq!(window.values, VecDeque::from([Some(1e308), None]));
    assert_eq!(window.sum, 1e308);
    window.push_for_bar(Some(1.0), 2, 2);
    window.push_for_bar(Some(1.0), 2, 3);
    assert_constant_result(&window, 1.0, 2);
    window.push_for_bar(Some(-0.0), 2, 3);
    assert_eq!(window.sum, 1.0);
    window.discard_for_bar(3);
    assert_eq!(window.values, VecDeque::from([Some(1e308), Some(1.0)]));
    assert_eq!(window.sum, 1e308);
}

#[test]
fn shrink_combines_dominant_loss_and_repeated_balanced_tail_qualifications() {
    for length in [2, 3] {
        let mut window = RollingWindowState::default();
        for value in [1e16, 1.0, 1.0, 1.0] {
            window.push(Some(value), 4);
        }
        window.push(Some(1.0), length);
        assert_eq!(window.sum.to_bits(), (length as f64).to_bits());
        assert_constant_result(&window, 1.0, length);
    }
    let mut near_dominant = RollingWindowState::default();
    for value in [1e16, 1.0, 1.0, 1.0] {
        near_dominant.push(Some(value), 4);
    }
    let checkpoint = near_dominant.clone();
    near_dominant.push_for_bar(Some(1.1e16), 4, 4);
    assert_eq!(near_dominant.sum.to_bits(), (1.1e16_f64 + 3.0).to_bits());
    near_dominant.discard_for_bar(4);
    assert_eq!(near_dominant.values, checkpoint.values);
    assert_eq!(near_dominant.sum.to_bits(), checkpoint.sum.to_bits());
    let mut window = RollingWindowState::default();
    for _ in 0..2500 {
        window.push(Some(1.0), 5002);
        window.push(Some(-1.0), 5002);
    }
    window.push(Some(1e-20), 5002);
    window.push(Some(-1e-20), 5002);
    let before = window.clone();
    window.push_for_bar(Some(0.0), 2, 5002);
    assert_eq!(window.values, VecDeque::from([Some(-1e-20), Some(0.0)]));
    assert_eq!(window.sum.to_bits(), (-1e-20_f64).to_bits());
    window.push_for_bar(Some(2.0), 5002, 5002);
    let mut expected = before.clone();
    expected.push_for_bar(Some(2.0), 5002, 5002);
    assert_eq!(window, expected);
    window.discard_for_bar(5002);
    assert_eq!(window.values, before.values);
    assert_eq!(window.sum.to_bits(), before.sum.to_bits());
    assert_eq!(window.change_count, before.change_count);
}

#[test]
fn actual_nonfinite_samples_cannot_be_certified_until_they_leave() {
    for nonfinite in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        let mut window = RollingWindowState::default();
        window.push(Some(nonfinite), 2);
        window.push(Some(1.0), 2);
        assert!(!window.sum.is_finite());
        assert_eq!(window.reconstructed_sum(false), None);
        window.push(Some(1.0), 2);
        assert_constant_result(&window, 1.0, 2);
    }
}

fn samples(values: &[f64]) -> RollingWindowState {
    RollingWindowState {
        values: values.iter().copied().map(Some).collect(),
        nonzero_count: values.iter().filter(|value| **value != 0.0).count(),
        change_count: values.windows(2).filter(|pair| pair[0] != pair[1]).count(),
        ..Default::default()
    }
}

#[test]
fn cold_sum_qualifies_subnormals_signed_zero_and_compensated_cancellation() {
    let tiny = f64::from_bits(1);
    for sign in [1.0, -1.0] {
        let window = samples(&[sign * tiny, -sign * tiny, -0.0, sign * tiny]);
        assert_eq!(window.reconstructed_sum(true), Some(sign * tiny));
        let window = samples(&[
            sign * 1e308,
            sign * 1e308,
            -sign * 1e308,
            -sign * 1e308,
            sign,
        ]);
        assert_eq!(window.reconstructed_sum(true), Some(sign));
    }
    assert_eq!(
        samples(&[-0.0, 0.0, -0.0]).reconstructed_sum(true),
        Some(0.0)
    );
}

#[test]
fn cold_sum_rejects_irreversible_scaling_third_component_and_unrepresentable_result() {
    assert_eq!(
        samples(&[1e308, 1e-308, -1e308]).reconstructed_sum(false),
        None
    );
    // The exact dyadic sum needs three nonoverlapping f64 components. The
    // second correction update must not silently lose 2^-108.
    assert_eq!(
        samples(&[1.0, 2.0_f64.powi(-54), 2.0_f64.powi(-108)]).reconstructed_sum(false),
        None
    );
    assert_eq!(samples(&[1e308, 1e308]).reconstructed_sum(false), None);
    let inexact = samples(&[1.0, 2.0_f64.powi(-54)]);
    assert_eq!(inexact.reconstructed_sum(false), Some(1.0));
    assert_eq!(inexact.reconstructed_sum(true), None);
}

#[test]
fn genuine_balanced_zero_and_unqualified_finite_residual_keep_old_bits() {
    let mut window = RollingWindowState::default();
    // The dominant removal qualifies the scan, but its exact tail sum is zero.
    for value in [1e308, 1.0, -1.0] {
        window.push(Some(value), 3);
    }
    window.pop_front();
    assert_eq!(window.sum.to_bits(), 0.0_f64.to_bits());
    for _ in 0..1024 {
        window.push(Some(1.0), 2);
        window.push(Some(-1.0), 2);
        assert_eq!(window.sum.to_bits(), 0.0_f64.to_bits());
    }
    // A nonzero finite aggregate outside the zero-removal gate is preserved.
    let mut residual = samples(&[1.0, -1.0]);
    residual.sum = f64::from_bits(1);
    residual.nonzero_count = 2;
    residual.recover_nonfinite_sum();
    assert_eq!(residual.sum.to_bits(), 1);
}

#[test]
fn ready_zero_variance_matches_independent_old_scan_bits_with_wrap_na_and_undo() {
    fn old_variance(window: &RollingWindowState, length: usize, biased: bool) -> f64 {
        if !biased && length < 2 {
            return f64::NAN;
        }
        let mean = window.sum / length as f64;
        let squared = window
            .values
            .iter()
            .flatten()
            .map(|value| {
                let difference = *value - mean;
                difference * difference
            })
            .sum::<f64>();
        let denominator = if biased { length } else { length - 1 };
        (squared / denominator as f64).max(0.0)
    }
    let mut window = RollingWindowState {
        values: VecDeque::with_capacity(7).into(),
        ..Default::default()
    };
    let mut wrapped = false;
    for bar in 0..256 {
        let length = [1, 7, 3, 11][(bar / 32) % 4];
        let value = if bar.is_multiple_of(23) {
            None
        } else {
            Some(if bar.is_multiple_of(2) { -0.0 } else { 0.0 })
        };
        window.push_for_bar(value, length, bar);
        wrapped |= window.values.has_split_storage();
        for biased in [false, true] {
            assert_eq!(
                window.variance(length, biased).to_bits(),
                old_variance(&window, length, biased).to_bits()
            );
        }
        if bar.is_multiple_of(7) {
            let before = window.clone();
            window.push_for_bar(Some(1.0), length, bar);
            window = before;
            window.discard_for_bar(bar);
            for biased in [false, true] {
                assert_eq!(
                    window.variance(length, biased).to_bits(),
                    old_variance(&window, length, biased).to_bits()
                );
            }
        }
    }
    assert!(wrapped);
}

#[test]
fn constant_proof_matches_independent_adjacent_scan_after_eviction_and_undo() {
    fn assert_proof(window: &RollingWindowState, length: usize) {
        let values = window.values.iter().copied().collect::<Vec<_>>();
        let changes = values.windows(2).filter(|pair| pair[0] != pair[1]).count();
        assert_eq!(window.change_count, changes);
        let expected = values.len() == length
            && values.first().is_some_and(|first| {
                first.is_some_and(|number| number.is_finite())
                    && values.iter().all(|value| value == first)
            });
        assert_eq!(window.is_constant_ready(length), expected);
    }
    let mut window = RollingWindowState {
        values: VecDeque::with_capacity(7).into(),
        ..Default::default()
    };
    let inputs = [
        Some(-0.0),
        Some(0.0),
        None,
        None,
        Some(3.0),
        Some(3.0),
        Some(4.0),
    ];
    let mut wrapped = false;
    for bar in 0..256 {
        for (pass, length) in [7, 3, 11, 1, 6].into_iter().enumerate() {
            let value = inputs[(bar + pass) % inputs.len()];
            window.push_for_bar(value, length, bar);
            assert_proof(&window, length);
            wrapped |= window.values.has_split_storage();
            if pass == 2 {
                let checkpoint = window.clone();
                window.push_for_bar(Some(9.0), 2, bar);
                assert_proof(&window, 2);
                window = checkpoint;
                assert_proof(&window, length);
            }
        }
        if bar.is_multiple_of(3) {
            window.discard_for_bar(bar);
            assert_proof(&window, 6);
        }
    }
    assert!(wrapped);
    for length in [1_usize, 2, 7] {
        let mut zeros = RollingWindowState::default();
        for index in 0..length {
            zeros.push(
                Some(if index.is_multiple_of(2) { -0.0 } else { 0.0 }),
                length,
            );
        }
        assert!(zeros.is_constant_ready(length));
        assert_eq!(zeros.change_count, 0);
        zeros.pop_front();
        assert_proof(&zeros, length);
    }
    for number in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut nonfinite = RollingWindowState::default();
        nonfinite.push(Some(number), 1);
        assert!(!nonfinite.is_constant_ready(1));
    }
}
