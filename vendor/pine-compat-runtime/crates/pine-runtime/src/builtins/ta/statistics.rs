use super::*;

impl<'a> HistoricalRuntime<'a> {
    pub(crate) fn eval_rci(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let (source, length) = self.eval_source_length(args)?;
        if length < 2 {
            return Ok(PineValue::Na);
        }

        let Some(length) = usize::try_from(length).ok() else {
            return Ok(PineValue::Na);
        };
        let window = self.update_rolling_window(call_site_id, source, length);
        if !window.is_ready(length) {
            return Ok(PineValue::Na);
        }

        let window = &self.ta_state.rolling_windows[&RollingWindowKey::Single(call_site_id)];
        let rci = self
            .selection_scratch
            .rci(window.values.iter().flatten().copied());
        Ok(finite_float_or_na(rci))
    }

    pub(crate) fn eval_stdev(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        match self.eval_window_variance(call_site_id, args)? {
            PineValue::Float(value) => Ok(finite_float_or_na(value.sqrt())),
            value => Ok(value),
        }
    }

    pub(crate) fn eval_variance(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        self.eval_window_variance(call_site_id, args)
    }

    pub(crate) fn eval_range(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let (source, length) = self.eval_source_length(args)?;
        if length <= 0 {
            return Ok(PineValue::Na);
        }

        let Some(length) = usize::try_from(length).ok() else {
            return Ok(PineValue::Na);
        };
        let window = self.update_rolling_window(call_site_id, source, length);
        if !window.is_ready(length) {
            return Ok(PineValue::Na);
        }

        Ok(window.range().map_or(PineValue::Na, PineValue::Float))
    }

    pub(crate) fn eval_dev(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let (source, length) = self.eval_source_length(args)?;
        if length <= 0 {
            return Ok(PineValue::Na);
        }

        let Some(length) = usize::try_from(length).ok() else {
            return Ok(PineValue::Na);
        };
        let window = self.update_rolling_window(call_site_id, source, length);
        if !window.is_ready(length) {
            return Ok(PineValue::Na);
        }

        Ok(finite_float_or_na(window.mean_absolute_deviation(length)))
    }

    fn eval_source_length(
        &mut self,
        args: RuntimeArgs<'_>,
    ) -> Result<(PineValue, i64), RuntimeError> {
        let source = ta_arg(args, 0, "source")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .unwrap_or(PineValue::Na);
        let length = ta_arg(args, 1, "length")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        Ok((source, length))
    }

    pub(crate) fn eval_correlation(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let (left, right, length) = self.eval_pair_sources_length(args)?;
        if length <= 0 {
            return Ok(PineValue::Na);
        }

        let Some(length) = usize::try_from(length).ok() else {
            return Ok(PineValue::Na);
        };
        let left = left.as_f64();
        let right = right.as_f64();
        let product = left.zip(right).map(|(left, right)| left * right);
        self.update_rolling_window_key(
            RollingWindowKey::CorrelationLeft(call_site_id),
            left,
            length,
        );
        self.update_rolling_window_key(
            RollingWindowKey::CorrelationRight(call_site_id),
            right,
            length,
        );
        self.update_rolling_window_key(
            RollingWindowKey::CorrelationProduct(call_site_id),
            product,
            length,
        );

        let left = self
            .ta_state
            .rolling_windows
            .get(&RollingWindowKey::CorrelationLeft(call_site_id));
        let right = self
            .ta_state
            .rolling_windows
            .get(&RollingWindowKey::CorrelationRight(call_site_id));
        let product = self
            .ta_state
            .rolling_windows
            .get(&RollingWindowKey::CorrelationProduct(call_site_id));
        let (Some(left), Some(right), Some(product)) = (left, right, product) else {
            return Ok(PineValue::Na);
        };
        if !left.is_ready(length) || !right.is_ready(length) {
            return Ok(PineValue::Na);
        }
        // Finite paired sources can overflow only their derived product. A
        // missing source still fails readiness above and is never recovered.
        if !product.is_ready(length) {
            return Ok(centered_pair_moments(left, right, length)
                .and_then(CenteredPairMoments::correlation)
                .map_or(PineValue::Na, finite_float_or_na));
        }

        let left_variance = left.variance(length, true);
        let right_variance = right.variance(length, true);
        let denominator = (left_variance * right_variance).sqrt();
        if denominator == 0.0 || !denominator.is_finite() {
            return Ok(centered_pair_moments(left, right, length)
                .and_then(CenteredPairMoments::correlation)
                .map_or(PineValue::Na, finite_float_or_na));
        }

        let product_mean = product.mean(length);
        let mean_product = left.mean(length) * right.mean(length);
        let covariance = product_mean - mean_product;
        let budget = cancellation_budget(covariance, product_mean, mean_product, length)
            .unwrap_or(0.0)
            .max(partial_precision_budget(covariance, product_mean, mean_product).unwrap_or(0.0));
        let cancellation = left_variance > 0.0 && right_variance > 0.0 && budget >= denominator;
        if !covariance.is_finite() || !mean_product.is_finite() || cancellation {
            return Ok(centered_pair_moments(left, right, length)
                .and_then(CenteredPairMoments::correlation)
                .map_or(PineValue::Na, finite_float_or_na));
        }
        Ok(finite_float_or_na(covariance / denominator))
    }

    pub(crate) fn eval_covariance(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let (left, right, length) = self.eval_pair_sources_length(args)?;
        if length <= 0 {
            return Ok(PineValue::Na);
        }

        let Some(length) = usize::try_from(length).ok() else {
            return Ok(PineValue::Na);
        };
        let left = left.as_f64();
        let right = right.as_f64();
        let product = left.zip(right).map(|(left, right)| left * right);
        self.update_rolling_window_key(
            RollingWindowKey::CovarianceLeft(call_site_id),
            left,
            length,
        );
        self.update_rolling_window_key(
            RollingWindowKey::CovarianceRight(call_site_id),
            right,
            length,
        );
        self.update_rolling_window_key(
            RollingWindowKey::CovarianceProduct(call_site_id),
            product,
            length,
        );

        let left = self
            .ta_state
            .rolling_windows
            .get(&RollingWindowKey::CovarianceLeft(call_site_id));
        let right = self
            .ta_state
            .rolling_windows
            .get(&RollingWindowKey::CovarianceRight(call_site_id));
        let product = self
            .ta_state
            .rolling_windows
            .get(&RollingWindowKey::CovarianceProduct(call_site_id));
        let (Some(left), Some(right), Some(product)) = (left, right, product) else {
            return Ok(PineValue::Na);
        };
        if !left.is_ready(length) || !right.is_ready(length) {
            return Ok(PineValue::Na);
        }
        if !product.is_ready(length) {
            return Ok(centered_pair_moments(left, right, length)
                .and_then(CenteredPairMoments::covariance)
                .map_or(PineValue::Na, finite_float_or_na));
        }

        let product_mean = product.mean(length);
        let mean_product = left.mean(length) * right.mean(length);
        let covariance = product_mean - mean_product;
        if !covariance.is_finite() || !mean_product.is_finite() {
            return Ok(centered_pair_moments(left, right, length)
                .and_then(CenteredPairMoments::covariance)
                .map_or(PineValue::Na, finite_float_or_na));
        }
        let budget = cancellation_budget(covariance, product_mean, mean_product, length)
            .unwrap_or(0.0)
            .max(partial_precision_budget(covariance, product_mean, mean_product).unwrap_or(0.0));
        if budget > 0.0 {
            if left.is_constant_ready(length) || right.is_constant_ready(length) {
                // A finite constant-source result keeps the existing raw
                // arithmetic, including small residuals in golden outputs.
                return Ok(finite_float_or_na(covariance));
            }
            if !observed_spread_excludes_cancellation(left, right, length, budget)
                && let Some(centered) = centered_pair_moments(left, right, length)
                && budget >= centered.standard_deviation_product()
            {
                return Ok(centered
                    .covariance()
                    .map_or(PineValue::Na, finite_float_or_na));
            }
        }
        Ok(finite_float_or_na(covariance))
    }

    fn eval_pair_sources_length(
        &mut self,
        args: RuntimeArgs<'_>,
    ) -> Result<(PineValue, PineValue, i64), RuntimeError> {
        let left = ta_arg(args, 0, "source1")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .unwrap_or(PineValue::Na);
        let right = ta_arg(args, 1, "source2")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .unwrap_or(PineValue::Na);
        let length = ta_arg(args, 2, "length")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        Ok((left, right, length))
    }

    pub(crate) fn eval_median(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let (source, length) = self.eval_source_length(args)?;
        if length <= 0 {
            return Ok(PineValue::Na);
        }

        let Some(length) = usize::try_from(length).ok() else {
            return Ok(PineValue::Na);
        };
        let window = self.update_rolling_window(call_site_id, source, length);
        if !window.is_ready(length) {
            return Ok(PineValue::Na);
        }

        let middle = length / 2;
        let lower = if length.is_multiple_of(2) {
            middle - 1
        } else {
            middle
        };
        let window = &self.ta_state.rolling_windows[&RollingWindowKey::Single(call_site_id)];
        let (low, high) = self.selection_scratch.select_pair(
            window.values.iter().flatten().copied(),
            lower,
            middle,
        );
        let median = if length.is_multiple_of(2) {
            (low + high) / 2.0
        } else {
            high
        };
        Ok(finite_float_or_na(median))
    }

    pub(crate) fn eval_mode(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let (source, length) = self.eval_source_length(args)?;
        if length <= 0 {
            return Ok(PineValue::Na);
        }

        let Some(length) = usize::try_from(length).ok() else {
            return Ok(PineValue::Na);
        };
        let window = self.update_rolling_window(call_site_id, source, length);
        if !window.is_ready(length) {
            return Ok(PineValue::Na);
        }

        let window = &self.ta_state.rolling_windows[&RollingWindowKey::Single(call_site_id)];
        let best_value = self
            .selection_scratch
            .mode(window.values.iter().flatten().copied());
        Ok(finite_float_or_na(best_value))
    }

    pub(crate) fn eval_percentile(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
        mode: ArrayPercentileMode,
    ) -> Result<PineValue, RuntimeError> {
        let (source, length, percentage) = self.eval_percentile_source_length_percentage(args)?;
        if length <= 0 {
            return Ok(PineValue::Na);
        }
        let Some(percentage) = percentage else {
            return Ok(PineValue::Na);
        };
        if !(0.0..=100.0).contains(&percentage) {
            return Ok(PineValue::Na);
        }

        let Some(length) = usize::try_from(length).ok() else {
            return Ok(PineValue::Na);
        };
        let window = self.update_rolling_window(call_site_id, source, length);
        // Native linear interpolation retains the bar-count window but omits
        // NA samples within it. Nearest-rank behavior is qualified separately.
        let ready = match mode {
            ArrayPercentileMode::LinearInterpolation => window.values.len() == length,
            ArrayPercentileMode::NearestRank => window.is_ready(length),
        };
        if !ready {
            return Ok(PineValue::Na);
        }

        let count = window.values.len() - window.na_count;
        if count == 0 {
            return Ok(PineValue::Na);
        }
        let window = &self.ta_state.rolling_windows[&RollingWindowKey::Single(call_site_id)];
        match mode {
            ArrayPercentileMode::NearestRank => {
                let rank = ((percentage / 100.0) * count as f64).ceil();
                let index = (rank as usize).saturating_sub(1).min(count - 1);
                let (value, _) = self.selection_scratch.select_pair(
                    window.values.iter().flatten().copied(),
                    index,
                    index,
                );
                Ok(finite_float_or_na(value))
            }
            ArrayPercentileMode::LinearInterpolation => {
                if count == 1 {
                    return Ok(finite_float_or_na(
                        *window.values.iter().flatten().next().unwrap(),
                    ));
                }
                let rank = (percentage / 100.0) * (count - 1) as f64;
                let lower = rank.floor() as usize;
                let upper = rank.ceil() as usize;
                let fraction = rank - lower as f64;
                let (low, high) = self.selection_scratch.select_pair(
                    window.values.iter().flatten().copied(),
                    lower,
                    upper,
                );
                let value = low + (high - low) * fraction;
                Ok(finite_float_or_na(value))
            }
        }
    }

    pub(crate) fn eval_percentrank(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let (source, length) = self.eval_source_length(args)?;
        if length <= 0 {
            return Ok(PineValue::Na);
        }

        let Some(length) = usize::try_from(length).ok() else {
            return Ok(PineValue::Na);
        };
        let target = source.as_f64();
        let window = self.update_rolling_window(call_site_id, source, length);
        let Some(target) = target else {
            return Ok(PineValue::Na);
        };
        if !window.is_ready(length) {
            return Ok(PineValue::Na);
        }

        let count = window
            .values
            .iter()
            .flatten()
            .filter(|value| **value <= target || (**value - target).abs() < f64::EPSILON)
            .count();
        Ok(finite_float_or_na(count as f64 / length as f64 * 100.0))
    }

    pub(crate) fn eval_window_variance(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let (source, length) = self.eval_source_length(args)?;
        let biased = if let Some(arg) = ta_arg(args, 2, "biased") {
            matches!(self.eval_expr(arg)?, PineValue::Bool(true))
        } else {
            true
        };
        if length <= 0 {
            return Ok(PineValue::Na);
        }

        let Some(length) = usize::try_from(length).ok() else {
            return Ok(PineValue::Na);
        };
        let window = self.update_rolling_window(call_site_id, source, length);
        if !window.is_ready(length) || (!biased && length < 2) {
            return Ok(PineValue::Na);
        }

        Ok(finite_float_or_na(window.variance(length, biased)))
    }

    fn eval_percentile_source_length_percentage(
        &mut self,
        args: RuntimeArgs<'_>,
    ) -> Result<(PineValue, i64, Option<f64>), RuntimeError> {
        let (source, length) = self.eval_source_length(args)?;
        let percentage = ta_arg(args, 2, "percentage")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_f64());
        Ok((source, length, percentage))
    }
}

// A finite raw-moment result is reconsidered only when subtraction is within
// four machine epsilons per window sample of its operands AND that uncertainty
// covers the whole standard-deviation product. Length accounts heuristically
// for accumulation; this is not a bound on all rolling-sum error. Keep this
// complete-loss screen alongside the independent partial-precision screen below.
fn cancellation_budget(
    covariance: f64,
    product_mean: f64,
    mean_product: f64,
    length: usize,
) -> Option<f64> {
    if !covariance.is_finite() || !product_mean.is_finite() || !mean_product.is_finite() {
        return None;
    }
    let budget =
        (4.0 * f64::EPSILON) * length.max(1) as f64 * product_mean.abs().max(mean_product.abs());
    (budget > 0.0 && covariance.abs() <= budget).then_some(budget)
}

// The complete-loss screen above misses partial cancellation, e.g. operands
// near 1e16 whose difference is 5504 but is quantized in steps of 2. Independently
// qualify windows where one operand epsilon is at least 2^-20 of the centered
// SD product: raw subtraction then has at most about 20 effective binary bits
// at that covariance scale. This bounded precision heuristic is independent of
// length and is not a complete rolling-sum error bound. The sampled spread only
// rejects candidates; the full centered SD product must confirm eligibility.
// Resolved raw covariance outside that budget retains the cheap legacy path.
fn partial_precision_budget(covariance: f64, product_mean: f64, mean_product: f64) -> Option<f64> {
    const MIN_RELATIVE_PRECISION: f64 = 1.0 / 1_048_576.0;
    let budget =
        (f64::EPSILON / MIN_RELATIVE_PRECISION) * product_mean.abs().max(mean_product.abs());
    (budget > 0.0 && covariance.abs() <= budget).then_some(budget)
}

fn observed_spread_excludes_cancellation(
    left: &RollingWindowState,
    right: &RollingWindowState,
    length: usize,
    budget: f64,
) -> bool {
    let spread = |window: &RollingWindowState| {
        let first = window.values.front().copied().flatten()?;
        let next = window.values.get(1.min(length - 1)).copied().flatten()?;
        let third = window.values.get(2.min(length - 1)).copied().flatten()?;
        let middle = window.values.get(length / 2).copied().flatten()?;
        let last = window.values.back().copied().flatten()?;
        // Neighboring samples keep periodic sources from aliasing all three
        // widely spaced observations to the same phase. This is still only a
        // lower bound; a failed screen retains the complete centered scan.
        let range = first.max(next).max(third).max(middle).max(last)
            - first.min(next).min(third).min(middle).min(last);
        range.is_finite().then_some(range)
    };
    let (Some(left_range), Some(right_range)) = (spread(left), spread(right)) else {
        return false;
    };
    // Two samples separated by r contribute at least r²/2 to the centered
    // sum of squares, regardless of the unknown mean. Thus observed ranges
    // lower-bound the SD product. An extra factor of two leaves rounding slack.
    let lower_bound = (left_range / (2.0 * length as f64)) * right_range * 0.5;
    budget < lower_bound
}

#[derive(Clone, Copy)]
struct CenteredPairMoments {
    left_scale: f64,
    right_scale: f64,
    left_square_sum: f64,
    right_square_sum: f64,
    cross_sum: f64,
    length: usize,
}

impl CenteredPairMoments {
    fn correlation(self) -> Option<f64> {
        if self.left_square_sum == 0.0 || self.right_square_sum == 0.0 {
            return None;
        }
        let value = self.cross_sum / self.left_square_sum.sqrt() / self.right_square_sum.sqrt();
        // Exact centered correlation is in [-1, 1]; remove only cold-path
        // accumulation/division roundoff at the endpoints.
        value.is_finite().then(|| value.clamp(-1.0, 1.0))
    }

    fn covariance(self) -> Option<f64> {
        let value = self.restore_scales(self.cross_sum / self.length as f64);
        value.is_finite().then_some(value)
    }

    fn standard_deviation_product(self) -> f64 {
        self.restore_scales(
            self.left_square_sum.sqrt() / (self.length as f64).sqrt()
                * (self.right_square_sum.sqrt() / (self.length as f64).sqrt()),
        )
    }

    fn restore_scales(self, value: f64) -> f64 {
        // The larger factor goes first so a tiny covariance is not rounded to
        // zero before multiplication by a large scale. If that intermediate
        // overflows, try the other association before rejecting the result.
        let larger = self.left_scale.max(self.right_scale);
        let smaller = self.left_scale.min(self.right_scale);
        let restored = (value * larger) * smaller;
        if restored.is_finite() {
            restored
        } else {
            (value * smaller) * larger
        }
    }
}

#[derive(Default)]
struct CompensatedSum {
    value: f64,
    correction: f64,
}

impl CompensatedSum {
    fn add(&mut self, value: f64) {
        let corrected = value - self.correction;
        let next = self.value + corrected;
        self.correction = (next - self.value) - corrected;
        self.value = next;
    }
}

fn centered_pair_moments(
    left: &RollingWindowState,
    right: &RollingWindowState,
    length: usize,
) -> Option<CenteredPairMoments> {
    if !left.is_ready(length) || !right.is_ready(length) || length == 0 {
        return None;
    }
    if left.is_constant_ready(length) || right.is_constant_ready(length) {
        return Some(CenteredPairMoments {
            left_scale: 0.0,
            right_scale: 0.0,
            left_square_sum: 0.0,
            right_square_sum: 0.0,
            cross_sum: 0.0,
            length,
        });
    }
    let mut left_anchor = left.values.front().copied().flatten()?;
    let mut right_anchor = right.values.front().copied().flatten()?;
    let mut left_scale = 0.0_f64;
    let mut right_scale = 0.0_f64;
    let mut left_magnitude = 0.0_f64;
    let mut right_magnitude = 0.0_f64;
    let mut left_difference_overflow = false;
    let mut right_difference_overflow = false;
    for (left, right) in left.values.iter().zip(&right.values) {
        let (left, right) = ((*left)?, (*right)?);
        if !left.is_finite() || !right.is_finite() {
            return None;
        }
        left_magnitude = left_magnitude.max(left.abs());
        right_magnitude = right_magnitude.max(right.abs());
        let left_difference = left - left_anchor;
        let right_difference = right - right_anchor;
        left_difference_overflow |= !left_difference.is_finite();
        right_difference_overflow |= !right_difference.is_finite();
        if left_difference.is_finite() {
            left_scale = left_scale.max(left_difference.abs());
        }
        if right_difference.is_finite() {
            right_scale = right_scale.max(right_difference.abs());
        }
    }
    // Anchor subtraction preserves small differences beside a large offset.
    // Opposite extreme values may overflow that subtraction; only then use a
    // zero origin and scale the original finite values instead.
    if left_difference_overflow {
        left_anchor = 0.0;
        left_scale = left_magnitude;
    }
    if right_difference_overflow {
        right_anchor = 0.0;
        right_scale = right_magnitude;
    }
    if left_scale == 0.0 || right_scale == 0.0 {
        return None;
    }
    let normalized_pair = |left: f64, right: f64| {
        (
            (left - left_anchor) / left_scale,
            (right - right_anchor) / right_scale,
        )
    };
    let mut left_sum = CompensatedSum::default();
    let mut right_sum = CompensatedSum::default();
    for (left, right) in left.values.iter().zip(&right.values) {
        let (left, right) = normalized_pair((*left)?, (*right)?);
        left_sum.add(left);
        right_sum.add(right);
    }
    let left_mean = left_sum.value / length as f64;
    let right_mean = right_sum.value / length as f64;
    let mut left_square_sum = CompensatedSum::default();
    let mut right_square_sum = CompensatedSum::default();
    let mut cross_sum = CompensatedSum::default();
    for (left, right) in left.values.iter().zip(&right.values) {
        let (left, right) = normalized_pair((*left)?, (*right)?);
        let left = left - left_mean;
        let right = right - right_mean;
        left_square_sum.add(left * left);
        right_square_sum.add(right * right);
        cross_sum.add(left * right);
    }
    Some(CenteredPairMoments {
        left_scale,
        right_scale,
        left_square_sum: left_square_sum.value,
        right_square_sum: right_square_sum.value,
        cross_sum: cross_sum.value,
        length,
    })
}

#[cfg(test)]
#[path = "statistics_recovery_tests.rs"]
mod recovery_tests;
