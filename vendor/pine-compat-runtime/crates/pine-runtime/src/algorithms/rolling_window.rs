use pine_ir::CallSiteId;

use super::rolling_window_certificates::{constant_sum_overflows, exact_constant_weighted_mean};
use super::shared_deque::SharedDeque;

fn weighted_denominator(length: usize) -> f64 {
    // Widen before both the addition and multiplication. The exact triangular
    // number fits u128 for every supported usize, including 32-bit Wasm.
    let length = length as u128;
    (length * (length + 1) / 2) as f64
}

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct RollingWindowState {
    pub(crate) values: SharedDeque<Option<f64>>,
    pub(crate) sum: f64,
    pub(crate) na_count: usize,
    nonzero_count: usize,
    /// Number of unequal adjacent samples; signed zeros compare as equal.
    change_count: usize,
    /// Ordinary pushes interleaved with an open append can invalidate its saved
    /// aggregate counters. Such windows keep using the existing scans; a bar
    /// change or discard does not prove those counters trustworthy again.
    constant_certificate_invalid: bool,
    /// Bar that owns the uncommitted tail sample, if any.
    open_bar: Option<usize>,
    /// Exact aggregates from before the open append. Restored by assignment on
    /// same-bar undo so subtract/add cannot accumulate roundoff.
    prev_sum: f64,
    prev_na_count: usize,
    prev_nonzero_count: usize,
    prev_change_count: usize,
    /// Items evicted by the open append only. Short logs reuse their allocation;
    /// large logs share bounded pages across checkpoints until committed.
    evicted: SharedDeque<Option<f64>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RollingWindowKey {
    Single(CallSiteId),
    MathSum(CallSiteId),
    VwmaWeighted(CallSiteId),
    VwmaVolume(CallSiteId),
    MfiPositive(CallSiteId),
    MfiNegative(CallSiteId),
    CmoPositive(CallSiteId),
    CmoNegative(CallSiteId),
    AoFast(CallSiteId),
    AoSlow(CallSiteId),
    CorrelationLeft(CallSiteId),
    CorrelationRight(CallSiteId),
    CorrelationProduct(CallSiteId),
    CovarianceLeft(CallSiteId),
    CovarianceRight(CallSiteId),
    CovarianceProduct(CallSiteId),
    StochHigh(CallSiteId),
    StochLow(CallSiteId),
    WprHigh(CallSiteId),
    WprLow(CallSiteId),
    HmaFull(CallSiteId),
    HmaSmooth(CallSiteId),
    Rma { call_site: CallSiteId, channel: u8 },
    Macd { call_site: CallSiteId, channel: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowExtreme {
    Highest,
    Lowest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RisingFallingMode {
    Rising,
    Falling,
}

impl RollingWindowState {
    pub(crate) fn retained_values(&self) -> usize {
        self.values.len() + self.evicted.len()
    }

    pub(crate) fn retained_capacity(&self) -> usize {
        self.values.capacity() + self.evicted.capacity()
    }

    pub(crate) fn push(&mut self, value: Option<f64>, length: usize) {
        if length == 0 {
            return;
        }
        self.constant_certificate_invalid |= self.open_bar.is_some();
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

    /// Append `value` as the sample for `bar`.
    ///
    /// A second call with the same `bar` undoes the open append (restoring the
    /// exact pre-append aggregates and only the items that append evicted) and
    /// then applies `value`. A different `bar` commits the previous append.
    /// `None` is an ordinary NA sample, not a discard.
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

    /// Undo the open append when it belongs to `bar`, leaving no sample for
    /// that bar. A later `push_for_bar` on the same bar starts a new append.
    pub(crate) fn discard_for_bar(&mut self, bar: usize) {
        if self.open_bar == Some(bar) {
            self.undo_open_append();
            self.open_bar = None;
        }
    }

    #[cfg(test)]
    pub(crate) fn pop_front(&mut self) {
        self.constant_certificate_invalid |= self.open_bar.is_some() && !self.values.is_empty();
        if self.remove_front() {
            self.recover_qualified_tail();
        }
        self.recover_nonfinite_sum();
    }

    /// Remove one sample in constant time and qualify a possible lost tail.
    /// A push combines these flags and reconstructs once after all evictions.
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

    fn certified_constant_value(&self, length: usize) -> Option<f64> {
        if self.constant_certificate_invalid || !self.is_constant_ready(length) {
            return None;
        }
        self.values.front().copied().flatten()
    }

    pub(crate) fn variance(&self, length: usize, biased: bool) -> f64 {
        if !biased && length < 2 {
            return f64::NAN;
        }
        if length != 0 && self.is_ready(length) && self.nonzero_count == 0 {
            return 0.0;
        }
        let mean = self.mean(length);
        if let Some(value) = self.certified_constant_value(length) {
            let diff = value - mean;
            // Keep the original mean and subtraction. Only an exactly zero
            // squared residual proves every term in the old scan is +0.0.
            if diff * diff == 0.0 {
                return 0.0;
            }
        }
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

    pub(crate) fn extreme(&self, mode: WindowExtreme) -> Option<f64> {
        self.values
            .iter()
            .flatten()
            .copied()
            .reduce(|current, value| match mode {
                WindowExtreme::Highest => current.max(value),
                WindowExtreme::Lowest => current.min(value),
            })
    }

    pub(crate) fn range(&self) -> Option<f64> {
        let mut values = self.values.iter().flatten().copied();
        let first = values.next()?;
        // Keep each original oldest-to-newest max/min chain, including its
        // first-sample initialization and signed-zero/nonfinite behavior.
        let (highest, lowest) = values.fold((first, first), |(highest, lowest), value| {
            (highest.max(value), lowest.min(value))
        });
        Some(highest - lowest)
    }

    pub(crate) fn mean_absolute_deviation(&self, length: usize) -> f64 {
        let mean = self.mean(length);
        self.values
            .iter()
            .flatten()
            .map(|value| (*value - mean).abs())
            .sum::<f64>()
            / length as f64
    }

    pub(crate) fn center_of_gravity(&self, length: usize) -> f64 {
        let numerator = self
            .values
            .iter()
            .flatten()
            .enumerate()
            .map(|(index, value)| *value * (length - index) as f64)
            .sum::<f64>();
        -numerator / self.sum
    }

    pub(crate) fn weighted_mean(&self, length: usize) -> f64 {
        if let Some(mean) = self
            .certified_constant_value(length)
            .and_then(|value| exact_constant_weighted_mean(value, length))
        {
            return mean;
        }
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

    /// Full and tail weighted means in one traversal. Each accumulator keeps
    /// the original oldest-to-newest multiplication and addition order.
    pub(crate) fn weighted_mean_with_tail(&self, length: usize, tail_length: usize) -> (f64, f64) {
        if tail_length != 0
            && tail_length <= length
            && let Some(value) = self.certified_constant_value(length)
            && let (Some(full), Some(tail)) = (
                exact_constant_weighted_mean(value, length),
                exact_constant_weighted_mean(value, tail_length),
            )
        {
            return (full, tail);
        }
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

    /// Recover an overflowing weighted scan only for a complete finite window.
    /// A suffix uses its own weights, as in HMA's half window. This is a cold,
    /// stateless calculation; the finite legacy scan above keeps its exact bits.
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
        if let Some(value) = self.certified_constant_value(self.values.len())
            && constant_sum_overflows(value, self.values.len())
        {
            // A lower bound already exceeds the representable range. Both
            // forms of the existing reconstruction must reject this sum.
            return None;
        }
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
            self.values.restore_front(value);
        }
    }
}

#[cfg(test)]
#[path = "rolling_window_certificates_tests.rs"]
mod certificates_tests;

fn two_sum(left: f64, right: f64) -> (f64, f64) {
    let sum = left + right;
    let virtual_right = sum - left;
    let error = (left - (sum - virtual_right)) + (right - virtual_right);
    (sum, error)
}

#[cfg(test)]
#[path = "rolling_window_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "rolling_window_legacy_tests.rs"]
mod legacy_equivalence_tests;

#[cfg(test)]
#[path = "rolling_window_recovery_tests.rs"]
mod recovery_tests;

#[cfg(test)]
#[path = "rolling_window_weighted_recovery_tests.rs"]
mod weighted_recovery_tests;

#[cfg(test)]
#[path = "rolling_window_length_tests.rs"]
mod length_tests;

#[cfg(test)]
#[path = "rolling_range_tests.rs"]
mod range_tests;
