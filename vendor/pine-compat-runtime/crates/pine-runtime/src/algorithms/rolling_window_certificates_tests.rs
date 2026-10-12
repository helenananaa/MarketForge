//! Independent frozen pre-certificate oracle. Methods below are copied from
//! commit 0052cf0583d941202bcc428c928e2a88d8077521, algorithms/rolling_window.rs (SHA256 ec5124a2eab0bb018ce91846c4436fee2d6f572ed33c4d959e7076a4147bc07c).
//! The reference uses VecDeque and never invokes production arithmetic helpers.
use std::collections::VecDeque;

use super::RollingWindowState;

#[derive(Debug, Default, Clone)]
struct OldWindow {
    values: VecDeque<Option<f64>>,
    sum: f64,
    na_count: usize,
    nonzero_count: usize,
    change_count: usize,
    open_bar: Option<usize>,
    prev_sum: f64,
    prev_na_count: usize,
    prev_nonzero_count: usize,
    prev_change_count: usize,
    evicted: VecDeque<Option<f64>>,
}

fn weighted_denominator(length: usize) -> f64 {
    // Widen before both the addition and multiplication. The exact triangular
    // number fits u128 for every supported usize, including 32-bit Wasm.
    let length = length as u128;
    (length * (length + 1) / 2) as f64
}

fn two_sum(left: f64, right: f64) -> (f64, f64) {
    let sum = left + right;
    let virtual_right = sum - left;
    let error = (left - (sum - virtual_right)) + (right - virtual_right);
    (sum, error)
}

impl OldWindow {
    pub(crate) fn push(&mut self, value: Option<f64>, length: usize) {
        if length == 0 {
            return;
        }
        let mut recover_tail = false;
        while self.values.len() >= length {
            recover_tail |= self.remove_front();
        }
        if recover_tail {
            self.recover_qualified_tail();
        }
        self.append(value);
        self.reset_zero_window_aggregates();
        self.recover_nonfinite_sum();
    }

    pub(crate) fn push_for_bar(&mut self, value: Option<f64>, length: usize, bar: usize) {
        if length == 0 {
            return;
        }
        if self.open_bar == Some(bar) {
            self.undo_open_append();
        }
        self.begin_open_append(bar);
        let mut recover_tail = false;
        while self.values.len() >= length {
            let Some(evicted) = self.values.front().copied() else {
                break;
            };
            recover_tail |= self.remove_front();
            self.evicted.push_back(evicted);
        }
        let tail_recovered = recover_tail && self.recover_qualified_tail();
        self.append(value);
        // For nearby same-sign values, Sterbenz's lemma gives an exact input
        // difference. Apply that small delta to the sum once instead of rounding
        // after both the removal and insertion. Other transitions keep the
        // existing path, including large magnitude jumps and opposite signs.
        if !tail_recovered
            && self.evicted.len() == 1
            && let (Some(incoming), Some(outgoing)) = (value, self.evicted[0])
            && incoming.is_sign_positive() == outgoing.is_sign_positive()
            && incoming.abs() >= outgoing.abs() * 0.5
            && outgoing.abs() >= incoming.abs() * 0.5
        {
            self.sum = self.prev_sum + (incoming - outgoing);
        }
        self.reset_zero_window_aggregates();
        self.recover_nonfinite_sum();
    }

    pub(crate) fn discard_for_bar(&mut self, bar: usize) {
        if self.open_bar == Some(bar) {
            self.undo_open_append();
            self.open_bar = None;
        }
    }

    pub(crate) fn pop_front(&mut self) {
        if self.remove_front() {
            self.recover_qualified_tail();
        }
        self.recover_nonfinite_sum();
    }

    fn remove_front(&mut self) -> bool {
        let mut recover_tail = false;
        if let Some(value) = self.values.pop_front() {
            if let Some(next) = self.values.front() {
                self.change_count -= usize::from(value != *next);
            }
            if let Some(value) = value {
                self.sum -= value;
                self.nonzero_count -= usize::from(value != 0.0);
                recover_tail = self.lost_tail_needs_recovery(value);
            } else {
                self.na_count = self.na_count.saturating_sub(1);
            }
            self.reset_zero_window_aggregates();
        }
        recover_tail
    }

    pub(crate) fn is_ready(&self, length: usize) -> bool {
        length != 0 && self.values.len() == length && self.na_count == 0
    }

    pub(crate) fn is_constant_ready(&self, length: usize) -> bool {
        self.is_ready(length)
            && self.change_count == 0
            && self
                .values
                .front()
                .is_some_and(|value| value.is_some_and(|value| value.is_finite()))
    }

    pub(crate) fn mean(&self, length: usize) -> f64 {
        self.sum / length as f64
    }

    pub(crate) fn variance(&self, length: usize, biased: bool) -> f64 {
        if !biased && length < 2 {
            return f64::NAN;
        }
        if length != 0 && self.is_ready(length) && self.nonzero_count == 0 {
            return 0.0;
        }
        let mean = self.mean(length);
        let squared_diff_sum = self
            .values
            .iter()
            .flatten()
            .map(|value| {
                let diff = *value - mean;
                diff * diff
            })
            .sum::<f64>();
        let denominator = if biased { length } else { length - 1 };
        (squared_diff_sum / denominator as f64).max(0.0)
    }

    pub(crate) fn weighted_mean(&self, length: usize) -> f64 {
        let weighted_sum = self
            .values
            .iter()
            .flatten()
            .enumerate()
            .map(|(index, value)| *value * (index + 1) as f64)
            .sum::<f64>();
        let mean = weighted_sum / weighted_denominator(length);
        if mean.is_finite() {
            mean
        } else {
            self.recovered_weighted_mean(length, 0).unwrap_or(mean)
        }
    }

    pub(crate) fn weighted_mean_with_tail(&self, length: usize, tail_length: usize) -> (f64, f64) {
        let tail_start = self.values.len() - tail_length;
        // Iterator::sum::<f64>() starts at -0.0; keep its signed-zero identity.
        let mut weighted_sum = -0.0;
        let mut tail_weighted_sum = -0.0;
        let mut full_weight = 0_usize;
        self.values.range(..tail_start).flatten().for_each(|value| {
            full_weight += 1;
            weighted_sum += *value * full_weight as f64;
        });
        self.values
            .range(tail_start..)
            .flatten()
            .enumerate()
            .for_each(|(index, value)| {
                full_weight += 1;
                weighted_sum += *value * full_weight as f64;
                tail_weighted_sum += *value * (index + 1) as f64;
            });
        let full_mean = weighted_sum / weighted_denominator(length);
        let tail_mean = tail_weighted_sum / weighted_denominator(tail_length);
        (
            if full_mean.is_finite() {
                full_mean
            } else {
                self.recovered_weighted_mean(length, 0).unwrap_or(full_mean)
            },
            if tail_mean.is_finite() {
                tail_mean
            } else {
                self.recovered_weighted_mean(length, tail_start)
                    .unwrap_or(tail_mean)
            },
        )
    }

    fn recovered_weighted_mean(&self, length: usize, start: usize) -> Option<f64> {
        if length == 0 || start >= length || !self.is_ready(length) {
            return None;
        }
        // A finite constant full window proves every nonempty suffix has the
        // same exact weighted mean, even when its weighted numerator overflows.
        if self.is_constant_ready(length) {
            return self
                .values
                .back()
                .copied()
                .flatten()
                .filter(|value| value.is_finite());
        }
        let mut maximum = 0.0_f64;
        for sample in self.values.range(start..) {
            let value = (*sample)?;
            if !value.is_finite() {
                return None;
            }
            maximum = maximum.max(value.abs());
        }
        if maximum == 0.0 {
            return Some(0.0);
        }
        let exponent = maximum.to_bits() & 0x7ff0_0000_0000_0000;
        let scale = if exponent == 0 {
            f64::from_bits(1_u64 << maximum.to_bits().ilog2())
        } else {
            f64::from_bits(exponent)
        };
        let mut total = 0.0;
        let mut correction = 0.0;
        let mut minimum = f64::INFINITY;
        let mut maximum = f64::NEG_INFINITY;
        for (index, sample) in self.values.range(start..).enumerate() {
            let value = (*sample)?;
            let scaled = value / scale;
            // Mixed scales are qualified only if every original input survives.
            if (scaled * scale).to_bits() != value.to_bits() {
                return None;
            }
            minimum = minimum.min(scaled);
            maximum = maximum.max(scaled);
            let weight = (index + 1) as f64;
            let term = scaled * weight;
            let product_error = scaled.mul_add(weight, -term);
            // Capture both weighted-product and addition rounding in an exact
            // two-component sum. A third component makes this window unqualified.
            for component in [term, product_error] {
                let (next, error) = two_sum(total, component);
                let (next_correction, correction_error) = two_sum(correction, error);
                if !next.is_finite() || !next_correction.is_finite() || correction_error != 0.0 {
                    return None;
                }
                total = next;
                correction = next_correction;
            }
        }
        // The exact expansion is rounded once before division.
        let rounded = two_sum(total, correction).0;
        let weighted_length = length - start;
        let denominator = weighted_denominator(weighted_length);
        let numerator = rounded * scale;
        if numerator.is_finite() && (numerator / scale).to_bits() == rounded.to_bits() {
            // Restore first when cancellation leaves a small numerator: dividing
            // in the normalized scale could otherwise round through subnormal.
            let mean = numerator / denominator;
            return mean
                .is_finite()
                .then(|| mean.clamp(minimum * scale, maximum * scale));
        }
        // The numerator can exceed f64 while its convex weighted mean fits.
        // Bound the rounded quotient before restoring the original scale.
        let normalized_mean = (rounded / denominator).clamp(minimum, maximum);
        let mean = normalized_mean * scale;
        (mean.is_finite() && (mean / scale).to_bits() == normalized_mean.to_bits()).then_some(mean)
    }

    fn append(&mut self, value: Option<f64>) {
        if let Some(previous) = self.values.back() {
            self.change_count += usize::from(*previous != value);
        }
        if let Some(value) = value {
            self.sum += value;
            self.nonzero_count += usize::from(value != 0.0);
            self.values.push_back(Some(value));
        } else {
            self.na_count += 1;
            self.values.push_back(None);
        }
    }

    fn reset_zero_window_aggregates(&mut self) {
        // Subtracting the final nonzero sample can leave a floating residual.
        // A window containing only exact zeros has exact zero aggregates.
        if self.nonzero_count == 0 && self.na_count == 0 {
            self.sum = 0.0;
        }
    }

    fn recover_nonfinite_sum(&mut self) {
        // Do this only after a complete push (including every eviction). A
        // shrinking window must not rescan its tail after each removed item.
        if !self.sum.is_finite()
            && let Some(sum) = self.reconstructed_sum(false)
        {
            self.sum = sum;
        }
    }

    fn lost_tail_needs_recovery(&self, outgoing: f64) -> bool {
        // Removing a dominant sample can round a nonzero remaining sum to
        // zero, including after an overflow recovery rounded [huge, small]
        // to huge. Front/back are constant-time eligibility checks only; the
        // cold scan must independently certify the exact final tail sum.
        if self.sum != 0.0 || self.nonzero_count == 0 || !outgoing.is_finite() {
            return false;
        }
        let small_limit = outgoing.abs() * (1.0 / 9_007_199_254_740_992.0);
        let is_small = |sample: Option<&Option<f64>>| {
            sample.is_some_and(|sample| {
                sample.is_some_and(|value| {
                    value.is_finite() && value != 0.0 && value.abs() <= small_limit
                })
            })
        };
        is_small(self.values.front()) || is_small(self.values.back())
    }

    fn recover_qualified_tail(&mut self) -> bool {
        // The qualification may occur before several later evictions. Their
        // subtracts can turn the lost zero into another finite value, so use
        // the exact final tail instead of requiring the sum to still be zero.
        // Combining qualifications prevents a shrinking balanced window from
        // rescanning its tail after every removed pair.
        if let Some(sum) = self.reconstructed_sum(true)
            && sum != self.sum
        {
            self.sum = sum;
            return true;
        }
        false
    }

    fn reconstructed_sum(&self, require_exact: bool) -> Option<f64> {
        let mut maximum = 0.0_f64;
        for &value in self.values.iter().flatten() {
            if !value.is_finite() {
                return None;
            }
            maximum = maximum.max(value.abs());
        }
        if maximum == 0.0 {
            return Some(0.0);
        }
        // An exact power of two normalizes the cold sum without overflowing
        // intermediate additions. Every sample must survive the round trip;
        // a mixed-scale window that would lose a nonzero value is unqualified.
        let exponent = maximum.to_bits() & 0x7ff0_0000_0000_0000;
        let scale = if exponent == 0 {
            f64::from_bits(1_u64 << maximum.to_bits().ilog2())
        } else {
            f64::from_bits(exponent)
        };
        let mut total = 0.0;
        let mut correction = 0.0;
        for &value in self.values.iter().flatten() {
            let scaled = value / scale;
            if (scaled * scale).to_bits() != value.to_bits() {
                return None;
            }
            let (next, error) = two_sum(total, scaled);
            let (next_correction, correction_error) = two_sum(correction, error);
            // Keep an exact two-component sum. Do not silently drop a third
            // component when even the correction's addition loses a bit.
            if !next.is_finite() || !next_correction.is_finite() || correction_error != 0.0 {
                return None;
            }
            total = next;
            correction = next_correction;
        }
        let (rounded, error) = two_sum(total, correction);
        if require_exact && error != 0.0 {
            return None;
        }
        let sum = rounded * scale;
        // Rescaling must not overflow or introduce another rounding. For a
        // non-finite aggregate, the sole permitted rounding is total+correction.
        (sum.is_finite() && (sum / scale).to_bits() == rounded.to_bits()).then_some(sum)
    }

    fn begin_open_append(&mut self, bar: usize) {
        self.open_bar = Some(bar);
        self.prev_sum = self.sum;
        self.prev_na_count = self.na_count;
        self.prev_nonzero_count = self.nonzero_count;
        self.prev_change_count = self.change_count;
        self.evicted.clear();
    }

    fn undo_open_append(&mut self) {
        self.values.pop_back();
        self.sum = self.prev_sum;
        self.na_count = self.prev_na_count;
        self.nonzero_count = self.prev_nonzero_count;
        self.change_count = self.prev_change_count;
        while let Some(value) = self.evicted.pop_back() {
            self.values.push_front(value);
        }
    }
}

fn sample_bits(samples: impl IntoIterator<Item = Option<f64>>) -> Vec<Option<u64>> {
    samples
        .into_iter()
        .map(|sample| sample.map(f64::to_bits))
        .collect()
}

fn assert_option_bits(current: Option<f64>, old: Option<f64>) {
    assert_eq!(current.map(f64::to_bits), old.map(f64::to_bits));
}

fn assert_same(current: &RollingWindowState, old: &OldWindow) {
    assert_eq!(
        sample_bits(current.values.iter().copied()),
        sample_bits(old.values.iter().copied())
    );
    assert_eq!(
        sample_bits(current.evicted.iter().copied()),
        sample_bits(old.evicted.iter().copied())
    );
    assert_eq!(current.sum.to_bits(), old.sum.to_bits());
    assert_eq!(current.na_count, old.na_count);
    assert_eq!(current.nonzero_count, old.nonzero_count);
    assert_eq!(current.change_count, old.change_count);
    assert_eq!(current.open_bar, old.open_bar);
    assert_eq!(current.prev_sum.to_bits(), old.prev_sum.to_bits());
    assert_eq!(current.prev_na_count, old.prev_na_count);
    assert_eq!(current.prev_nonzero_count, old.prev_nonzero_count);
    assert_eq!(current.prev_change_count, old.prev_change_count);
    for require_exact in [false, true] {
        assert_option_bits(
            current.reconstructed_sum(require_exact),
            old.reconstructed_sum(require_exact),
        );
    }
    let length = current.values.len();
    if length != 0 {
        for biased in [false, true] {
            assert_eq!(
                current.variance(length, biased).to_bits(),
                old.variance(length, biased).to_bits()
            );
        }
        assert_eq!(
            current.weighted_mean(length).to_bits(),
            old.weighted_mean(length).to_bits()
        );
        for tail in [1, (length / 2).max(1), length] {
            let (full, suffix) = current.weighted_mean_with_tail(length, tail);
            let (old_full, old_suffix) = old.weighted_mean_with_tail(length, tail);
            assert_eq!(full.to_bits(), old_full.to_bits());
            assert_eq!(suffix.to_bits(), old_suffix.to_bits());
        }
    }
}

fn push(current: &mut RollingWindowState, old: &mut OldWindow, value: Option<f64>, length: usize) {
    current.push(value, length);
    old.push(value, length);
    assert_same(current, old);
}

fn push_for_bar(
    current: &mut RollingWindowState,
    old: &mut OldWindow,
    value: Option<f64>,
    length: usize,
    bar: usize,
) {
    current.push_for_bar(value, length, bar);
    old.push_for_bar(value, length, bar);
    assert_same(current, old);
}

#[test]
fn constant_threshold_neighbors_and_partial_windows_keep_old_recovery_bits() {
    for length in [1_usize, 2, 3, 4, 7, 8, 15, 16, 31, 32, 127, 128, 129, 257] {
        let power = 1_usize << length.ilog2();
        let threshold = f64::MAX / power as f64;
        let neighbors = [
            f64::from_bits(threshold.to_bits() - 1),
            threshold,
            f64::from_bits(threshold.to_bits() + 1),
        ];
        for magnitude in neighbors.into_iter().filter(|value| value.is_finite()) {
            for sign in [1.0, -1.0] {
                let value = magnitude * sign;
                let mut current = RollingWindowState::default();
                let mut old = OldWindow::default();
                // The requested length is intentionally larger than the actual
                // retained count. Certificates must also cover partial warmup.
                for _ in 0..length {
                    push(&mut current, &mut old, Some(value), length + 17);
                }
                assert!(!current.is_ready(length + 17));
                assert_option_bits(current.certified_constant_value(length), Some(value));
                if length.is_power_of_two() && magnitude == threshold {
                    for require_exact in [false, true] {
                        assert_option_bits(
                            current.reconstructed_sum(require_exact),
                            Some(sign * f64::MAX),
                        );
                    }
                }
                if magnitude > threshold {
                    assert_eq!(old.reconstructed_sum(false), None);
                    assert_eq!(old.reconstructed_sum(true), None);
                }
                current.sum = f64::INFINITY.copysign(sign);
                old.sum = current.sum;
                current.recover_nonfinite_sum();
                old.recover_nonfinite_sum();
                assert_same(&current, &old);
            }
        }
    }
}

#[test]
fn na_signed_zeros_and_subnormal_constants_do_not_become_false_rejections() {
    let tiny = f64::from_bits(1);
    for values in [
        vec![None, None],
        vec![Some(-0.0), Some(0.0), Some(-0.0)],
        vec![Some(tiny), Some(tiny), Some(tiny)],
        vec![Some(-tiny), Some(-tiny), Some(-tiny)],
        vec![Some(0.25), None, Some(0.25)],
        vec![Some(1e308), None, Some(-1e308), Some(1.0)],
        vec![Some(1e308), Some(1e-308), Some(-1e308)],
        vec![Some(1.0), Some(2.0_f64.powi(-54)), Some(2.0_f64.powi(-108))],
        vec![Some(f64::INFINITY), Some(1.0)],
        vec![Some(f64::NAN), Some(1.0)],
    ] {
        let mut current = RollingWindowState::default();
        let mut old = OldWindow::default();
        for value in &values {
            push(&mut current, &mut old, *value, values.len() + 3);
        }
        current.sum = f64::INFINITY;
        old.sum = f64::INFINITY;
        current.recover_nonfinite_sum();
        old.recover_nonfinite_sum();
        assert_same(&current, &old);
        while !current.values.is_empty() {
            current.pop_front();
            old.pop_front();
            assert_same(&current, &old);
        }
    }
}

#[test]
fn pure_per_bar_eviction_replacement_discard_and_checkpoints_keep_certificates_valid() {
    let mut current = RollingWindowState {
        values: VecDeque::with_capacity(7).into(),
        ..Default::default()
    };
    let mut old = OldWindow {
        values: VecDeque::with_capacity(7),
        ..Default::default()
    };
    for bar in 0..129 {
        push_for_bar(&mut current, &mut old, Some(1e308), 129, bar);
    }
    for bar in 129..177 {
        let checkpoint = (current.clone(), old.clone());
        for (value, length) in [
            (Some(1e308), 257),
            (Some(-1e308), 3),
            (None, 129),
            (Some(-0.0), 1),
            (Some(0.1), 65),
        ] {
            push_for_bar(&mut current, &mut old, value, length, bar);
            assert!(!current.constant_certificate_invalid);
        }
        current.discard_for_bar(bar);
        old.discard_for_bar(bar);
        assert_same(&current, &old);
        assert!(!current.constant_certificate_invalid);
        (current, old) = checkpoint;
        push_for_bar(&mut current, &mut old, Some(1e308), 129, bar);
        assert!(!current.constant_certificate_invalid);
    }
}

#[test]
fn ordinary_dynamic_shrink_na_and_mixed_scale_follow_the_frozen_oracle() {
    let mut current = RollingWindowState::default();
    let mut old = OldWindow::default();
    for value in [
        Some(1e308),
        Some(1e308),
        None,
        Some(1.0),
        Some(2.0),
        Some(3.0),
    ] {
        push(&mut current, &mut old, value, 6);
    }
    for (value, length) in [
        (Some(4.0), 4),
        (Some(-0.0), 2),
        (None, 8),
        (Some(0.0), 8),
        (Some(1.0), 3),
        (Some(1e16), 4),
        (Some(1.0), 1),
    ] {
        push(&mut current, &mut old, value, length);
    }
    assert!(!current.constant_certificate_invalid);
}

#[test]
fn variance_certificates_keep_decimal_residuals_and_zero_underflow_bits() {
    for value in [100.25_f64, -100.25, 0.1, -0.1, 1e-180, -1e-180] {
        for length in [1, 3, 17, 129] {
            let mut current = RollingWindowState::default();
            let mut old = OldWindow::default();
            for _ in 0..length {
                push(&mut current, &mut old, Some(value), length);
            }
            if value.abs() == 0.1 && length == 3 {
                let residual = value - old.mean(length);
                assert_ne!(residual * residual, 0.0);
                assert_ne!(old.variance(length, true), 0.0);
            }
            if value.abs() == 100.25 {
                assert_eq!(current.variance(length, true).to_bits(), 0.0_f64.to_bits());
            }
            if value.abs() == 1e-180 {
                assert_eq!(current.variance(length, true).to_bits(), 0.0_f64.to_bits());
            }
        }
    }
}

#[test]
fn mixed_push_modes_with_stale_constant_metadata_fall_back_and_stay_invalid() {
    let value = 1e308;
    let mut current = RollingWindowState::default();
    let mut old = OldWindow::default();
    push(&mut current, &mut old, Some(value), 2);
    push(&mut current, &mut old, Some(value), 2);
    push_for_bar(&mut current, &mut old, Some(-value), 2, 7);
    // Zero length is a true no-op and must not invalidate a sound certificate.
    push(&mut current, &mut old, Some(-value), 0);
    assert!(!current.constant_certificate_invalid);
    push(&mut current, &mut old, Some(-value), 3);
    assert!(current.constant_certificate_invalid);
    push_for_bar(&mut current, &mut old, Some(-value), 4, 7);
    assert!(current.is_constant_ready(4)); // Existing metadata is stale here.
    assert_eq!(current.change_count, 0);
    assert_eq!(
        sample_bits(current.values.iter().copied()),
        sample_bits([Some(value), Some(value), Some(-value), Some(-value)])
    );
    assert_eq!(current.certified_constant_value(4), None);
    assert_eq!(current.sum.to_bits(), 0.0_f64.to_bits());
    for require_exact in [false, true] {
        assert_option_bits(current.reconstructed_sum(require_exact), Some(0.0));
    }
    current.discard_for_bar(7);
    old.discard_for_bar(7);
    assert_same(&current, &old);
    assert!(current.constant_certificate_invalid);
    assert_eq!(current.open_bar, None);
    let checkpoint = (current.clone(), old.clone());
    push(&mut current, &mut old, Some(-value), 10);
    assert!(current.constant_certificate_invalid);
    push_for_bar(&mut current, &mut old, Some(-value), 10, 8);
    assert!(current.constant_certificate_invalid);
    current.discard_for_bar(8);
    old.discard_for_bar(8);
    assert_same(&current, &old);
    (current, old) = checkpoint;
    assert_same(&current, &old);
    assert!(current.constant_certificate_invalid);
}

#[test]
fn direct_pop_during_an_open_append_permanently_disables_new_certificates() {
    let mut current = RollingWindowState::default();
    let mut old = OldWindow::default();
    push_for_bar(&mut current, &mut old, Some(1e308), 2, 0);
    push_for_bar(&mut current, &mut old, Some(1e308), 2, 1);
    assert!(!current.constant_certificate_invalid);
    current.pop_front();
    old.pop_front();
    assert_same(&current, &old);
    assert!(current.constant_certificate_invalid);
    push_for_bar(&mut current, &mut old, Some(1e308), 10, 1);
    assert!(current.constant_certificate_invalid);
    current.discard_for_bar(1);
    old.discard_for_bar(1);
    assert_same(&current, &old);
    assert!(current.constant_certificate_invalid);
    assert_eq!(current.open_bar, None);
    push(&mut current, &mut old, Some(1e308), 10);
    assert!(current.constant_certificate_invalid);
    assert_eq!(current.certified_constant_value(current.values.len()), None);
}
