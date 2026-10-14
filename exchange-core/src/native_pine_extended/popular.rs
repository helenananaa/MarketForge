//! Incremental formula adapters for five popular indicator families.
//!
//! Formulas and parameters are frozen in `docs/native-pine-popular-catalog.json`.
//! This module is intentionally not an implementation of the original community strategies.

use super::{BarInput, Signals, pine_gt, pine_lt};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

const ATR_LEN: usize = 10;
const SSL_LEN: usize = 10;
const WT_FAST: usize = 10;
const WT_SLOW: usize = 21;
const WT_SIGNAL: usize = 4;
const SQUEEZE_LEN: usize = 20;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum Kind {
    Supertrend,
    UtAtr,
    Ssl,
    WaveTrend,
    Squeeze,
}

/// A rolling series window. `sum` follows the Pine runtime's update order
/// (subtract the evicted value, then add the new value), which matters near
/// strict comparison boundaries such as the squeeze test.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Window {
    values: VecDeque<Option<f64>>,
    sum: f64,
    /// Conservative cumulative bound for rounding introduced by incremental
    /// additions/subtractions. It is validation metadata only; Pine-compatible
    /// `sum` operation order remains unchanged.
    #[serde(default)]
    roundoff_budget: f64,
}

impl Window {
    fn push(&mut self, value: Option<f64>, length: usize) -> Option<f64> {
        self.push_inner(value, length, false)
    }

    /// Pine's `ta.sma` feeds its rolling sum through `push_for_bar`, including
    /// the exact nearby-value difference optimization. The other consumers
    /// (notably `ta.stdev`) use the ordinary push path above.
    fn push_sma(&mut self, value: Option<f64>, length: usize) -> Option<f64> {
        self.push_inner(value, length, true)
    }

    fn push_inner(
        &mut self,
        value: Option<f64>,
        length: usize,
        sma_difference: bool,
    ) -> Option<f64> {
        if length == 0 {
            return None;
        }
        // ta.update_rolling_window_key uses the runtime's ordinary rolling
        // push path. Filter nonfinite source values and retain its subtract,
        // append, then targeted aggregate-recovery ordering.
        let value = value.filter(|sample| sample.is_finite());
        let mut recover_tail = false;
        let original_sum = self.sum;
        let original_budget = self.roundoff_budget;
        let mut evicted = None;
        while self.values.len() >= length {
            let old = self.values.pop_front().flatten();
            if let Some(old) = old {
                evicted = Some(old);
                self.accumulate(old, true);
                recover_tail |= self.lost_tail_needs_recovery(old);
            }
            self.reset_zero_window_aggregate();
        }
        let mut tail_recovered = false;
        if recover_tail && let Some(sum) = self.reconstructed_sum() {
            self.sum = sum;
            self.roundoff_budget = 0.0;
            tail_recovered = true;
        }
        self.values.push_back(value);
        if let Some(value) = value {
            self.accumulate(value, false);
        }
        if sma_difference
            && !tail_recovered
            && let (Some(incoming), Some(outgoing)) = (value, evicted)
            && incoming.is_sign_positive() == outgoing.is_sign_positive()
            && incoming.abs() >= outgoing.abs() * 0.5
            && outgoing.abs() >= incoming.abs() * 0.5
        {
            // Sterbenz's lemma makes this same-sign nearby subtraction exact.
            // Preserve the runtime's `prev_sum + (incoming - outgoing)` order.
            let difference = incoming - outgoing;
            let next = original_sum + difference;
            self.sum = next;
            let bound = f64::EPSILON * (original_sum.abs() + difference.abs() + next.abs());
            self.roundoff_budget = (original_budget + bound).min(f64::MAX);
        }
        if !self.sum.is_finite()
            && let Some(sum) = self.reconstructed_sum()
        {
            self.sum = sum;
            self.roundoff_budget = 0.0;
        }
        self.reset_zero_window_aggregate();
        self.mean(length)
    }

    fn reset_zero_window_aggregate(&mut self) {
        if self
            .values
            .iter()
            .all(|value| value.is_some_and(|value| value == 0.0))
        {
            // Pine clears accumulated residue when every retained sample is
            // an exact signed zero. This matters after a long nonzero-to-zero
            // transition because it changes subsequent means and crosses.
            self.sum = 0.0;
            self.roundoff_budget = 0.0;
        }
    }

    fn accumulate(&mut self, operand: f64, subtract: bool) {
        let previous = self.sum;
        let next = if subtract {
            previous - operand
        } else {
            previous + operand
        };
        self.sum = next;
        let step_bound = f64::EPSILON * (previous.abs() + operand.abs() + next.abs());
        self.roundoff_budget = if step_bound.is_finite() {
            (self.roundoff_budget + step_bound).min(f64::MAX)
        } else {
            f64::MAX
        };
    }

    fn lost_tail_needs_recovery(&self, outgoing: f64) -> bool {
        if self.sum != 0.0 || !outgoing.is_finite() {
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

    /// Rebuild a lost or overflowing finite sum only when every source value
    /// survives power-of-two normalization exactly, matching Pine's qualified
    /// recovery path for rolling sums.
    fn reconstructed_sum(&self) -> Option<f64> {
        let mut maximum = 0.0_f64;
        for value in self.values.iter().flatten() {
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
        for value in self.values.iter().flatten() {
            let scaled = *value / scale;
            if (scaled * scale).to_bits() != value.to_bits() {
                return None;
            }
            let (next, error) = two_sum(total, scaled);
            let (next_correction, correction_error) = two_sum(correction, error);
            if !next.is_finite() || !next_correction.is_finite() || correction_error != 0.0 {
                return None;
            }
            total = next;
            correction = next_correction;
        }
        let (rounded, error) = two_sum(total, correction);
        if error != 0.0 {
            return None;
        }
        let sum = rounded * scale;
        (sum.is_finite() && (sum / scale).to_bits() == rounded.to_bits()).then_some(sum)
    }

    fn ready(&self, length: usize) -> bool {
        self.values.len() == length && self.values.iter().all(Option::is_some)
    }

    fn mean(&self, length: usize) -> Option<f64> {
        self.ready(length).then(|| self.sum / length as f64)
    }

    fn variance_biased(&self, length: usize) -> Option<f64> {
        let mean = self.mean(length)?;
        let squared_diff_sum = self
            .values
            .iter()
            .flatten()
            .map(|value| {
                let diff = *value - mean;
                diff * diff
            })
            .sum::<f64>();
        Some((squared_diff_sum / length as f64).max(0.0))
    }

    fn highest(&self, length: usize) -> Option<f64> {
        self.ready(length).then(|| {
            self.values
                .iter()
                .flatten()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max)
        })
    }

    fn lowest(&self, length: usize) -> Option<f64> {
        self.ready(length).then(|| {
            self.values
                .iter()
                .flatten()
                .copied()
                .fold(f64::INFINITY, f64::min)
        })
    }

    fn valid(&self, length: usize, bars: u64) -> bool {
        let reconstructed = self
            .values
            .iter()
            .flatten()
            .copied()
            .fold(0.0, |sum, value| sum + value);
        let absolute_sum = self
            .values
            .iter()
            .flatten()
            .map(|value| value.abs())
            .fold(0.0, |sum, value| (sum + value).min(f64::MAX));
        // Old serialized windows predate the cumulative budget. Bound their
        // possible accumulated update error by the number of historical bars.
        let operations = if self.roundoff_budget == 0.0 {
            bars.max(length as u64) as f64
        } else {
            length as f64
        };
        let fold_bound = (f64::EPSILON * absolute_sum * operations).min(f64::MAX);
        let tolerance = (self.roundoff_budget + fold_bound).min(f64::MAX);
        self.values.len() == (bars as usize).min(length)
            && self.values.iter().flatten().all(|value| value.is_finite())
            && self.sum.is_finite()
            && self.roundoff_budget.is_finite()
            && self.roundoff_budget >= 0.0
            && (self.sum - reconstructed).abs() <= tolerance
    }
}

/// EMA with Pine's SMA seed. Before the seed is ready the output is `na`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Ema {
    seed: Window,
    value: Option<f64>,
}

impl Ema {
    fn push(&mut self, source: Option<f64>, length: usize) -> Option<f64> {
        match (source, self.value) {
            (Some(source), Some(previous)) => {
                let alpha = 2.0 / (length as f64 + 1.0);
                let value = alpha * source + (1.0 - alpha) * previous;
                self.value = Some(value);
                Some(value)
            }
            (Some(source), None) => {
                let value = self.seed.push_sma(Some(source), length);
                if let Some(value) = value {
                    self.value = Some(value);
                }
                value
            }
            (None, _) => {
                // Pine's rolling window records a missing sample; it does not
                // backfill the seed with later values.
                self.seed.push(None, length);
                None
            }
        }
    }

    fn valid(&self, length: usize, bars: u64) -> bool {
        self.seed.valid(length, bars)
            && self.value.is_some() == self.seed.ready(length)
            && self.value.is_none_or(f64::is_finite)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(crate) enum State {
    Supertrend {
        bars: u64,
        tr: Window,
        atr_seed: Window,
        atr: Option<f64>,
        previous_close: Option<f64>,
        upper: Option<f64>,
        lower: Option<f64>,
        line: Option<f64>,
        direction: Option<f64>,
    },
    UtAtr {
        bars: u64,
        tr: Window,
        atr_seed: Window,
        atr: Option<f64>,
        previous_close: Option<f64>,
        trail: Option<f64>,
        previous_trail: Option<f64>,
    },
    Ssl {
        bars: u64,
        highs: Window,
        lows: Window,
        previous_close: Option<f64>,
        side: i8,
    },
    WaveTrend {
        bars: u64,
        esa: Ema,
        deviation: Ema,
        wt: Ema,
        signal: Window,
        previous_wt: Option<f64>,
        previous_signal: Option<f64>,
    },
    Squeeze {
        bars: u64,
        closes: Window,
        stdev_closes: Window,
        highs: Window,
        lows: Window,
        true_ranges: Window,
        centers: Window,
        previous_momentum: Option<f64>,
        squeezed: Option<bool>,
        previous_squeezed: Option<bool>,
    },
}

pub(crate) fn new(kind: Kind) -> State {
    match kind {
        Kind::Supertrend => State::Supertrend {
            bars: 0,
            tr: Window::default(),
            atr_seed: Window::default(),
            atr: None,
            previous_close: None,
            upper: None,
            lower: None,
            line: None,
            direction: None,
        },
        Kind::UtAtr => State::UtAtr {
            bars: 0,
            tr: Window::default(),
            atr_seed: Window::default(),
            atr: None,
            previous_close: None,
            trail: None,
            previous_trail: None,
        },
        Kind::Ssl => State::Ssl {
            bars: 0,
            highs: Window::default(),
            lows: Window::default(),
            previous_close: None,
            side: 0,
        },
        Kind::WaveTrend => State::WaveTrend {
            bars: 0,
            esa: Ema::default(),
            deviation: Ema::default(),
            wt: Ema::default(),
            signal: Window::default(),
            previous_wt: None,
            previous_signal: None,
        },
        Kind::Squeeze => State::Squeeze {
            bars: 0,
            closes: Window::default(),
            stdev_closes: Window::default(),
            highs: Window::default(),
            lows: Window::default(),
            true_ranges: Window::default(),
            centers: Window::default(),
            previous_momentum: None,
            squeezed: None,
            previous_squeezed: None,
        },
    }
}

pub(crate) fn valid(state: &State, kind: Kind, bars: u64) -> bool {
    match (state, kind) {
        (
            State::Supertrend {
                bars: count,
                tr,
                atr_seed,
                atr,
                previous_close,
                upper,
                lower,
                line,
                direction,
            },
            Kind::Supertrend,
        ) => {
            *count == bars
                && valid_atr(tr, atr_seed, *atr, bars)
                && option_finite(*previous_close)
                && previous_close.is_some() == (bars > 0)
                && [upper, lower, line]
                    .iter()
                    .all(|value| option_finite(**value))
                && direction.is_some_and(|value| value == -1.0 || value == 1.0)
                    == (bars >= ATR_LEN as u64)
                && upper.is_some() == (bars >= ATR_LEN as u64)
                && lower.is_some() == (bars >= ATR_LEN as u64)
                && line.is_some() == (bars >= ATR_LEN as u64)
        }
        (
            State::UtAtr {
                bars: count,
                tr,
                atr_seed,
                atr,
                previous_close,
                trail,
                previous_trail,
            },
            Kind::UtAtr,
        ) => {
            *count == bars
                && valid_atr(tr, atr_seed, *atr, bars)
                && option_finite(*previous_close)
                && previous_close.is_some() == (bars > 0)
                && option_finite(*trail)
                && option_finite(*previous_trail)
                && trail.is_some() == (bars >= ATR_LEN as u64)
                && previous_trail.is_some() == (bars > ATR_LEN as u64)
        }
        (
            State::Ssl {
                bars: count,
                highs,
                lows,
                previous_close,
                side,
            },
            Kind::Ssl,
        ) => {
            *count == bars
                && highs.valid(SSL_LEN, bars)
                && lows.valid(SSL_LEN, bars)
                && option_finite(*previous_close)
                && previous_close.is_some() == (bars > 0)
                && (-1..=1).contains(side)
        }
        (
            State::WaveTrend {
                bars: count,
                esa,
                deviation,
                wt,
                signal,
                previous_wt,
                previous_signal,
            },
            Kind::WaveTrend,
        ) => {
            *count == bars
                && esa.valid(WT_FAST, bars)
                && deviation.valid(WT_FAST, bars)
                && wt.valid(WT_SLOW, bars)
                && signal.valid(WT_SIGNAL, bars)
                && option_finite(*previous_wt)
                && option_finite(*previous_signal)
                && previous_wt.is_some() == (bars >= WT_SLOW as u64)
                && previous_signal.is_some() == (bars >= (WT_SLOW + WT_SIGNAL - 1) as u64)
        }
        (
            State::Squeeze {
                bars: count,
                closes,
                stdev_closes,
                highs,
                lows,
                true_ranges,
                centers,
                previous_momentum,
                squeezed,
                previous_squeezed,
            },
            Kind::Squeeze,
        ) => {
            *count == bars
                && closes.valid(SQUEEZE_LEN, bars)
                && stdev_closes.valid(SQUEEZE_LEN, bars)
                && highs.valid(SQUEEZE_LEN, bars)
                && lows.valid(SQUEEZE_LEN, bars)
                && true_ranges.valid(SQUEEZE_LEN, bars)
                && centers.valid(SQUEEZE_LEN, bars)
                && option_finite(*previous_momentum)
                && previous_momentum.is_some() == (bars >= (SQUEEZE_LEN * 2 - 1) as u64)
                && squeezed.is_some() == (bars >= SQUEEZE_LEN as u64)
                && previous_squeezed.is_some() == (bars > SQUEEZE_LEN as u64)
        }
        _ => false,
    }
}

pub(crate) fn update(state: &mut State, kind: Kind, bar: BarInput) -> Signals {
    let mut out = Signals::default();
    if !valid_bar(bar) {
        return out;
    }
    let squeeze_previous_close = match state {
        State::Squeeze { closes, .. } => closes.values.back().copied().flatten(),
        _ => None,
    };
    match (state, kind) {
        (
            State::Supertrend {
                bars,
                tr,
                atr_seed,
                atr,
                previous_close,
                upper,
                lower,
                line,
                direction,
            },
            Kind::Supertrend,
        ) => {
            let range = true_range(bar, *previous_close);
            tr.push(Some(range), ATR_LEN);
            *atr = rma_step(atr_seed, *atr, range, ATR_LEN);
            if let Some(atr_value) = *atr {
                let hl2 = (bar.high + bar.low) / 2.0;
                let basic_upper = hl2 + 3.0 * atr_value;
                let basic_lower = hl2 - 3.0 * atr_value;
                let prev_close = previous_close.unwrap_or(f64::NAN);
                let next_upper = match (*upper, *line) {
                    (Some(prev_upper), Some(_))
                        if basic_upper >= prev_upper && prev_close <= prev_upper =>
                    {
                        prev_upper
                    }
                    _ => basic_upper,
                };
                let next_lower = match (*lower, *line) {
                    (Some(prev_lower), Some(_))
                        if basic_lower <= prev_lower && prev_close >= prev_lower =>
                    {
                        prev_lower
                    }
                    _ => basic_lower,
                };
                let next_direction = match (*upper, *line, *direction) {
                    (Some(prev_upper), Some(prev_line), Some(_))
                        if prev_line == prev_upper && bar.close > next_upper =>
                    {
                        -1.0
                    }
                    (Some(prev_upper), Some(prev_line), Some(_)) if prev_line == prev_upper => 1.0,
                    (Some(_), Some(_), Some(_)) if bar.close < next_lower => 1.0,
                    (Some(_), Some(_), Some(_)) => -1.0,
                    _ => 1.0,
                };
                let next_line = if next_direction < 0.0 {
                    next_lower
                } else {
                    next_upper
                };
                *upper = Some(next_upper);
                *lower = Some(next_lower);
                *line = Some(next_line);
                *direction = Some(next_direction);
                out.buy = next_direction < 0.0;
                out.sell = next_direction > 0.0;
            }
            plot(&mut out, "supertrend", *line);
            plot(&mut out, "direction", *direction);
            *previous_close = Some(bar.close);
            *bars += 1;
        }
        (
            State::UtAtr {
                bars,
                tr,
                atr_seed,
                atr,
                previous_close,
                trail,
                previous_trail,
            },
            Kind::UtAtr,
        ) => {
            let range = true_range(bar, *previous_close);
            tr.push(Some(range), ATR_LEN);
            *atr = rma_step(atr_seed, *atr, range, ATR_LEN);
            let old_trail = *trail;
            if let Some(loss) = *atr {
                let prev = old_trail.unwrap_or(0.0);
                let previous_close_value = previous_close.unwrap_or(f64::NAN);
                let next_trail = if pine_gt(bar.close, prev) && pine_gt(previous_close_value, prev)
                {
                    prev.max(bar.close - loss)
                } else if pine_lt(bar.close, prev) && pine_lt(previous_close_value, prev) {
                    prev.min(bar.close + loss)
                } else if pine_gt(bar.close, prev) {
                    bar.close - loss
                } else {
                    bar.close + loss
                };
                let crossed_up = previous_close.is_some_and(|pclose| {
                    old_trail.is_some_and(|ptrail| bar.close > next_trail && pclose <= ptrail)
                });
                let crossed_down = previous_close.is_some_and(|pclose| {
                    old_trail.is_some_and(|ptrail| bar.close < next_trail && pclose >= ptrail)
                });
                out.buy = crossed_up;
                out.sell = crossed_down;
                *trail = Some(next_trail);
            } else {
                *trail = None;
            }
            plot(&mut out, "atr", *atr);
            plot(&mut out, "trail", *trail);
            *previous_trail = old_trail;
            *previous_close = Some(bar.close);
            *bars += 1;
        }
        (
            State::Ssl {
                bars,
                highs,
                lows,
                previous_close,
                side,
            },
            Kind::Ssl,
        ) => {
            let high_ma = highs.push_sma(Some(bar.high), SSL_LEN);
            let low_ma = lows.push_sma(Some(bar.low), SSL_LEN);
            if high_ma.is_some_and(|value| pine_gt(bar.close, value)) {
                *side = 1;
            } else if low_ma.is_some_and(|value| pine_lt(bar.close, value)) {
                *side = -1;
            }
            out.buy = *side == 1;
            out.sell = *side == -1;
            plot(&mut out, "high_ma", high_ma);
            plot(&mut out, "low_ma", low_ma);
            plot(&mut out, "side", Some(*side as f64));
            *previous_close = Some(bar.close);
            *bars += 1;
        }
        (
            State::WaveTrend {
                bars,
                esa,
                deviation,
                wt,
                signal,
                previous_wt,
                previous_signal,
            },
            Kind::WaveTrend,
        ) => {
            let ap = (bar.high + bar.low + bar.close) / 3.0;
            let esa_value = esa.push(Some(ap), WT_FAST);
            let deviation_input = esa_value.map(|value| (ap - value).abs());
            let deviation_value = deviation.push(deviation_input, WT_FAST);
            // Pine's conditional selects the false branch when d is `na`.
            let ci = match deviation_value {
                Some(d) if pine_gt(d, 0.0) => esa_value.map(|value| (ap - value) / (0.015 * d)),
                _ => Some(0.0),
            };
            let wt_value = wt.push(ci, WT_SLOW);
            let signal_value = signal.push_sma(wt_value, WT_SIGNAL);
            out.buy = wt_enter(
                cross_up(*previous_wt, *previous_signal, wt_value, signal_value),
                wt_value,
            );
            out.sell = wt_leave(
                cross_down(*previous_wt, *previous_signal, wt_value, signal_value),
                wt_value,
            );
            plot(&mut out, "wt", wt_value);
            plot(&mut out, "signal", signal_value);
            *previous_wt = wt_value;
            *previous_signal = signal_value;
            *bars += 1;
        }
        (
            State::Squeeze {
                bars,
                closes,
                stdev_closes,
                highs,
                lows,
                true_ranges,
                centers,
                previous_momentum,
                squeezed,
                previous_squeezed,
            },
            Kind::Squeeze,
        ) => {
            let range = true_range(bar, squeeze_previous_close);
            // The current range uses the preceding close, including the high-low fallback at bar zero.
            let close_ma = closes.push_sma(Some(bar.close), SQUEEZE_LEN);
            let _ = stdev_closes.push(Some(bar.close), SQUEEZE_LEN);
            let _ = highs.push(Some(bar.high), SQUEEZE_LEN);
            let _ = lows.push(Some(bar.low), SQUEEZE_LEN);
            let range_ma = true_ranges.push_sma(Some(range), SQUEEZE_LEN);
            let squeeze_value = is_squeezed(
                stdev_closes
                    .mean(SQUEEZE_LEN)
                    .zip(stdev_closes.variance_biased(SQUEEZE_LEN))
                    .map(|(_, variance)| 2.0 * variance.sqrt()),
                range_ma.map(|value| 1.5 * value),
            );
            let center = highs
                .highest(SQUEEZE_LEN)
                .zip(lows.lowest(SQUEEZE_LEN))
                .zip(close_ma)
                .map(|((highest_high, lowest_low), mid)| {
                    ((highest_high + lowest_low) / 2.0 + mid) / 2.0
                });
            let mom_source = center.map(|value| bar.close - value);
            let _ = centers.push(mom_source, SQUEEZE_LEN);
            let mom = centers.linreg(SQUEEZE_LEN);
            let enter = *squeezed == Some(true)
                && squeeze_value == Some(false)
                && mom.is_some_and(|value| pine_gt(value, 0.0));
            let leave = mom
                .zip(*previous_momentum)
                .is_some_and(|(current, previous)| pine_lt(current, previous));
            out.buy = enter;
            out.sell = leave;
            plot(&mut out, "mid", close_ma);
            plot(
                &mut out,
                "deviation",
                stdev_closes
                    .variance_biased(SQUEEZE_LEN)
                    .map(|value| 2.0 * value.sqrt()),
            );
            plot(&mut out, "k_range", range_ma.map(|value| 1.5 * value));
            plot(&mut out, "momentum", mom);
            // Match the formula's `squeezed ? 1 : 0`: Pine treats an `na`
            // condition as false, so this diagnostic plot is zero during warmup.
            plot(
                &mut out,
                "squeezed",
                Some(if squeeze_value == Some(true) {
                    1.0
                } else {
                    0.0
                }),
            );
            *previous_momentum = mom;
            *previous_squeezed = *squeezed;
            *squeezed = squeeze_value;
            *bars += 1;
        }
        _ => {}
    }
    out
}

impl Window {
    fn linreg(&self, length: usize) -> Option<f64> {
        if !self.ready(length) {
            return None;
        }
        if length == 1 {
            return self.values[0];
        }
        let n = length as f64;
        let mut sum_x = 0.0;
        let mut sum_y = 0.0;
        let mut sum_x_squared = 0.0;
        let mut sum_xy = 0.0;
        for (index, value) in self.values.iter().flatten().copied().enumerate() {
            let x = index as f64;
            sum_x += x;
            sum_y += value;
            sum_x_squared += x * x;
            sum_xy += x * value;
        }
        let denominator = n * sum_x_squared - sum_x * sum_x;
        (denominator != 0.0).then(|| {
            let slope = (n * sum_xy - sum_x * sum_y) / denominator;
            let intercept = (sum_y - slope * sum_x) / n;
            intercept + slope * (length as f64 - 1.0)
        })
    }
}

fn valid_atr(tr: &Window, seed: &Window, atr: Option<f64>, bars: u64) -> bool {
    tr.valid(ATR_LEN, bars)
        && seed.valid(ATR_LEN, bars)
        && option_finite(atr)
        && atr.is_some() == (bars >= ATR_LEN as u64)
}

fn rma_step(seed: &mut Window, previous: Option<f64>, source: f64, length: usize) -> Option<f64> {
    let initial = seed.push(Some(source), length);
    match previous {
        Some(previous) => Some((previous * (length - 1) as f64 + source) / length as f64),
        None => initial,
    }
}

fn true_range(bar: BarInput, previous_close: Option<f64>) -> f64 {
    let high_low = bar.high - bar.low;
    previous_close.map_or(high_low, |previous| {
        high_low
            .max((bar.high - previous).abs())
            .max((bar.low - previous).abs())
    })
}

fn cross_up(
    previous_left: Option<f64>,
    previous_right: Option<f64>,
    left: Option<f64>,
    right: Option<f64>,
) -> bool {
    matches!((previous_left, previous_right, left, right), (Some(pl), Some(pr), Some(l), Some(r)) if l > r && pl <= pr)
}

fn cross_down(
    previous_left: Option<f64>,
    previous_right: Option<f64>,
    left: Option<f64>,
    right: Option<f64>,
) -> bool {
    matches!((previous_left, previous_right, left, right), (Some(pl), Some(pr), Some(l), Some(r)) if l < r && pl >= pr)
}

fn is_squeezed(deviation: Option<f64>, k_range: Option<f64>) -> Option<bool> {
    deviation
        .zip(k_range)
        .map(|(deviation, k_range)| pine_lt(deviation, k_range))
}

fn wt_enter(crossed: bool, wt: Option<f64>) -> bool {
    crossed && wt.is_some_and(|value| pine_lt(value, -53.0))
}

fn wt_leave(crossed: bool, wt: Option<f64>) -> bool {
    crossed && wt.is_some_and(|value| pine_gt(value, 53.0))
}

fn option_finite(value: Option<f64>) -> bool {
    value.is_none_or(f64::is_finite)
}

fn valid_bar(bar: BarInput) -> bool {
    [bar.high, bar.low, bar.close]
        .iter()
        .all(|value| value.is_finite())
        && bar.high >= bar.low
}

fn plot(out: &mut Signals, name: &str, value: Option<f64>) {
    out.plots
        .insert(name.to_owned(), value.filter(|value| value.is_finite()));
}

fn two_sum(left: f64, right: f64) -> (f64, f64) {
    let sum = left + right;
    let virtual_right = sum - left;
    let error = (left - (sum - virtual_right)) + (right - virtual_right);
    (sum, error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(kind: Kind, bars: &[BarInput]) -> Vec<Signals> {
        let mut state = new(kind);
        bars.iter()
            .map(|bar| update(&mut state, kind, *bar))
            .collect()
    }

    fn bar(close: f64, width: f64) -> BarInput {
        BarInput {
            high: close + width,
            low: close - width,
            close,
        }
    }

    fn near(actual: Option<f64>, expected: Option<f64>, epsilon: f64) {
        assert_eq!(actual.is_some(), expected.is_some());
        if let (Some(actual), Some(expected)) = (actual, expected) {
            assert!(
                (actual - expected).abs() <= epsilon,
                "actual={actual:?}, expected={expected:?}"
            );
        }
    }

    #[test]
    fn supertrend_and_utatr_match_pine_compat_031_warmup_and_flat_oracle() {
        // Captured from pine_compat 0.3.1 run_script on the pinned formulas and
        // flat OHLC bars. Supertrend's first non-na is bar 9; ATR uses Wilder's
        // 10-observation SMA seed and the first TR is high-low.
        let bars = vec![bar(100.0, 1.0); 24];
        let supertrend = run(Kind::Supertrend, &bars);
        let ut = run(Kind::UtAtr, &bars);
        for i in 0..9 {
            assert_eq!(supertrend[i].plots["supertrend"], None);
            assert_eq!(supertrend[i].plots["direction"], None);
            assert_eq!(ut[i].plots["atr"], None);
        }
        for i in 9..bars.len() {
            near(supertrend[i].plots["supertrend"], Some(106.0), 1e-12);
            near(supertrend[i].plots["direction"], Some(1.0), 1e-12);
            near(ut[i].plots["atr"], Some(2.0), 1e-12);
            near(ut[i].plots["trail"], Some(98.0), 1e-12);
        }
        assert!(supertrend[..9].iter().all(|signal| !signal.sell));
        assert!(supertrend[9..].iter().all(|signal| signal.sell));
        assert!(ut.iter().all(|signal| !signal.buy && !signal.sell));
    }

    #[test]
    fn ssl_and_wavetrend_match_pine_compat_031_flat_and_cross_equality_contract() {
        // Runtime capture: SSL 10-bar high/low averages on a constant series;
        // WaveTrend has an EMA(10) SMA seed, then deviation EMA(10), WT EMA(21),
        // and signal SMA(4). The comparisons are strict as written in Pine.
        let bars = vec![bar(100.0, 1.0); 40];
        let ssl = run(Kind::Ssl, &bars);
        let wt = run(Kind::WaveTrend, &bars);
        for signal in &ssl[..9] {
            assert_eq!(signal.plots["high_ma"], None);
            assert_eq!(signal.plots["low_ma"], None);
            assert_eq!(signal.plots["side"], Some(0.0));
        }
        for signal in &ssl {
            assert!(!signal.buy && !signal.sell);
        }
        for signal in &wt[..20] {
            assert_eq!(signal.plots["wt"], None);
        }
        for signal in &wt[20..23] {
            assert_eq!(signal.plots["wt"], Some(0.0));
            assert_eq!(signal.plots["signal"], None);
        }
        for signal in &wt[..20] {
            assert_eq!(signal.plots["signal"], None);
        }
        for signal in &wt[20..] {
            assert_eq!(signal.plots["wt"], Some(0.0));
            assert!(!signal.buy && !signal.sell);
        }
        for signal in &wt[23..] {
            assert_eq!(signal.plots["signal"], Some(0.0));
        }
        assert!(cross_up(Some(1.0), Some(1.0), Some(1.0), Some(0.0)));
        assert!(cross_up(Some(1.0), Some(1.0), Some(1.0001), Some(1.0)));
        assert!(cross_down(Some(1.0), Some(1.0), Some(1.0), Some(2.0)));
        assert!(cross_down(Some(1.0), Some(1.0), Some(0.9999), Some(1.0)));
    }

    #[test]
    fn squeeze_regression_warmup_and_strict_release_edges() {
        // Flat source oracle: population stdev and true-range SMA both warm at
        // bar 19; the derived center-relative source then needs 20 bars of its
        // own before linreg(20) is defined (bar 38).
        let bars = vec![bar(100.0, 1.0); 45];
        let result = run(Kind::Squeeze, &bars);
        for signal in &result[..19] {
            assert_eq!(signal.plots["mid"], None);
            assert_eq!(signal.plots["squeezed"], Some(0.0));
        }
        near(result[19].plots["mid"], Some(100.0), 1e-12);
        near(result[19].plots["deviation"], Some(0.0), 1e-12);
        near(result[19].plots["k_range"], Some(3.0), 1e-12);
        for signal in &result[..38] {
            assert_eq!(signal.plots["momentum"], None);
        }
        for signal in &result[38..] {
            near(signal.plots["momentum"], Some(0.0), 1e-12);
            assert!(!signal.buy && !signal.sell);
        }
        // The script comparison treats sub-1e-10 differences as equality.
        assert_eq!(is_squeezed(Some(1.5), Some(1.5)), Some(false));
        assert_eq!(
            is_squeezed(Some(1.4999999999999998), Some(1.5)),
            Some(false)
        );
        assert_eq!(is_squeezed(Some(1.4999999998), Some(1.5)), Some(true));
        assert_eq!(is_squeezed(None, Some(1.5)), None);
        assert!(!wt_enter(true, Some(-53.0)));
        assert!(!wt_enter(true, Some(-53.00000000000001)));
        assert!(wt_enter(true, Some(-53.0000000002)));
        assert!(!wt_leave(true, Some(53.0)));
        assert!(!wt_leave(true, Some(53.00000000000001)));
        assert!(wt_leave(true, Some(53.0000000002)));
    }

    #[test]
    fn squeeze_jump_release_matches_pine_compat_031_and_uses_window_extrema() {
        // Captured with pine_compat 0.3.1 from the pinned squeeze formula.
        // Thirty-nine quiet bars establish a squeeze and warm linreg; a jump
        // at bar 39 releases it for exactly one entry bar.
        let closes: Vec<_> = std::iter::repeat_n(100.0, 39)
            .chain(std::iter::once(104.0))
            .chain((1..=11).map(|offset| 104.0 + offset as f64 * 0.2))
            .collect();
        let bars: Vec<_> = closes.iter().map(|close| bar(*close, 0.3)).collect();
        let result = run(Kind::Squeeze, &bars);

        for signal in &result[..19] {
            assert_eq!(signal.plots["mid"], None);
            assert_eq!(signal.plots["deviation"], None);
            assert_eq!(signal.plots["k_range"], None);
            assert_eq!(signal.plots["squeezed"], Some(0.0));
        }
        near(result[19].plots["mid"], Some(100.0), 1e-12);
        near(result[19].plots["deviation"], Some(0.0), 1e-12);
        near(result[19].plots["k_range"], Some(0.8999999999999915), 1e-12);
        near(result[19].plots["squeezed"], Some(1.0), 0.0);
        for signal in &result[19..39] {
            assert_eq!(signal.plots["squeezed"], Some(1.0));
        }
        // High/low rolling extrema make center=101.1 at the jump. Using SMA
        // of high/low instead centers at 100.2 and changes the momentum.
        near(result[39].plots["mid"], Some(100.2), 1e-12);
        near(
            result[39].plots["deviation"],
            Some(1.7435595774162693),
            1e-12,
        );
        near(result[39].plots["k_range"], Some(1.1774999999999918), 1e-12);
        near(
            result[39].plots["momentum"],
            Some(0.5385714285714297),
            1e-12,
        );
        assert_eq!(result[39].plots["squeezed"], Some(0.0));
        assert!(result[39].buy);
        for (index, signal) in result.iter().enumerate() {
            assert_eq!(signal.buy, index == 39, "entry predicate at bar {index}");
        }
        near(result[40].plots["mid"], Some(100.41), 1e-12);
        near(
            result[40].plots["deviation"],
            Some(2.4608128738284845),
            1e-12,
        );
        near(result[40].plots["momentum"], Some(1.044071428571431), 1e-12);

        let mut state = new(Kind::Squeeze);
        let mut release_source = None;
        for (index, input) in bars.iter().copied().enumerate() {
            update(&mut state, Kind::Squeeze, input);
            if index == 39 {
                let State::Squeeze { centers, .. } = &state else {
                    unreachable!();
                };
                release_source = centers.values.back().copied().flatten();
            }
            assert!(valid(&state, Kind::Squeeze, index as u64 + 1));
            let restored: State = serde_json::from_slice(
                &serde_json::to_vec(&state).expect("serialize squeeze state"),
            )
            .expect("restore squeeze state");
            assert!(valid(&restored, Kind::Squeeze, index as u64 + 1));
            assert_eq!(restored, state);
            state = restored;
        }
        let State::Squeeze { centers, .. } = state else {
            unreachable!();
        };
        near(centers.values.back().copied().flatten(), Some(3.12), 1e-12);
        near(release_source, Some(2.9), 1e-12);
    }

    #[test]
    fn rolling_sum_recovers_a_small_tail_after_a_large_sample_rolls_off() {
        let mut window = Window::default();
        let large = 2.0_f64.powi(54);
        assert_eq!(window.push(Some(large), 3), None);
        assert_eq!(window.push(Some(1.0), 3), None);
        assert_eq!(window.push(Some(0.0), 3), Some(large / 3.0));
        // The legacy accumulator rounds `large + 1` to `large`; removing the
        // dominant sample then loses the remaining one unless Pine's qualified
        // rolling-tail recovery runs before appending the incoming zero.
        near(window.push(Some(0.0), 3), Some(1.0 / 3.0), 1e-15);
    }

    #[test]
    fn rolling_sum_resets_residue_when_window_becomes_exact_zero() {
        let mut window = Window::default();
        for value in [0.1, 0.2, 0.3, 0.0, 0.0, 0.0] {
            window.push(Some(value), 3);
        }
        assert_eq!(
            window.values.iter().copied().collect::<Vec<_>>(),
            vec![Some(0.0); 3]
        );
        assert_eq!(window.sum.to_bits(), 0.0_f64.to_bits());
        assert_eq!(window.roundoff_budget, 0.0);
        assert!(window.valid(3, 6));
    }

    #[test]
    fn wavetrend_bar_77_restore_accepts_captured_rolling_roundoff() {
        // Captured from the 13 x 6 x 128 differential failure at bar 77.
        // The previous state predates the roundoff-budget field, which defaults
        // to zero on restore; the summand-derived bound must still qualify it.
        let encoded = r#"{
            "values":[10.744069101815757,2.141433814350883,-1.7288477917216263,-9.32360609538121],
            "sum":1.8330490290636998
        }"#;
        let window: Window = serde_json::from_str(encoded).unwrap();
        assert!(window.valid(WT_SIGNAL, 77));
        let restored: Window =
            serde_json::from_slice(&serde_json::to_vec(&window).unwrap()).unwrap();
        assert!(restored.valid(WT_SIGNAL, 77));
        assert_eq!(restored, window);
    }

    #[test]
    fn rolling_budget_survives_long_magnitude_switches_and_json_restores() {
        let large = 2.0_f64.powi(54);
        let mut window = Window::default();
        for index in 0..4_096_u64 {
            let value = match (index / 512) % 4 {
                0 => large + (index % 7) as f64,
                1 => 1.0 + (index % 5) as f64 * 0.125,
                2 => -large + (index % 11) as f64,
                _ => -1.0 - (index % 3) as f64 * 0.25,
            };
            window.push(Some(value), 8);
            assert!(
                window.valid(8, index + 1),
                "window invalid at bar {}",
                index + 1
            );
            if index % 512 == 511 {
                let encoded = serde_json::to_vec(&window).unwrap();
                let restored: Window = serde_json::from_slice(&encoded).unwrap();
                assert!(
                    restored.valid(8, index + 1),
                    "restored window invalid at bar {}",
                    index + 1
                );
                assert_eq!(restored, window);
                window = restored;
            }
        }
        let mut malformed = serde_json::to_value(&window).unwrap();
        malformed["roundoff_budget"] = serde_json::json!(-1.0);
        let malformed: Window = serde_json::from_value(malformed).unwrap();
        assert!(!malformed.valid(8, 4_096));
    }

    #[test]
    fn deterministic_ramp_matches_pine_compat_031_golden_plots() {
        let bars: Vec<_> = (0..45)
            .map(|index| bar(100.0 + index as f64, 1.0))
            .collect();
        let supertrend = run(Kind::Supertrend, &bars);
        let ut = run(Kind::UtAtr, &bars);
        let ssl = run(Kind::Ssl, &bars);
        let wt = run(Kind::WaveTrend, &bars);
        let squeeze = run(Kind::Squeeze, &bars);
        // Values sampled from the pinned 0.3.1 runtime over this exact ramp.
        for (index, line, direction) in [(9, 115.0, 1.0), (18, 112.0, -1.0), (38, 132.0, -1.0)] {
            near(supertrend[index].plots["supertrend"], Some(line), 1e-11);
            near(supertrend[index].plots["direction"], Some(direction), 1e-11);
        }
        for (index, trail) in [(9, 107.0), (18, 116.0), (38, 136.0)] {
            near(ut[index].plots["atr"], Some(2.0), 1e-12);
            near(ut[index].plots["trail"], Some(trail), 1e-11);
        }
        near(ssl[9].plots["high_ma"], Some(105.5), 1e-12);
        near(ssl[9].plots["low_ma"], Some(103.5), 1e-12);
        assert_eq!(ssl[9].plots["side"], Some(1.0));
        near(wt[20].plots["wt"], Some(9.523809523809545), 1e-11);
        near(wt[23].plots["signal"], Some(16.854495366892092), 1e-11);
        near(wt[38].plots["wt"], Some(56.38902152899731), 1e-10);
        near(
            squeeze[19].plots["deviation"],
            Some(11.532562594670797),
            1e-11,
        );
        near(squeeze[38].plots["momentum"], Some(9.5), 1e-11);
    }

    #[test]
    fn each_state_round_trips_and_corrupt_or_wrong_kind_restoration_fails_closed() {
        for kind in [
            Kind::Supertrend,
            Kind::UtAtr,
            Kind::Ssl,
            Kind::WaveTrend,
            Kind::Squeeze,
        ] {
            let mut state = new(kind);
            for i in 0..50 {
                update(
                    &mut state,
                    kind,
                    bar(100.0 + (i as f64 * 0.71).sin() * 4.0, 1.0),
                );
                assert!(
                    valid(&state, kind, i + 1),
                    "invalid {kind:?} state at bar {}",
                    i + 1
                );
                let encoded = serde_json::to_vec(&state).unwrap();
                let restored: State = serde_json::from_slice(&encoded).unwrap();
                assert_eq!(restored, state);
                assert!(
                    valid(&restored, kind, i + 1),
                    "restored {kind:?} state invalid at bar {}",
                    i + 1
                );
                assert!(!valid(&restored, other_kind(kind), i + 1));
            }
        }
        let mut damaged = serde_json::to_value(new(Kind::Ssl)).unwrap();
        damaged["side"] = json!(7);
        let restored: State = serde_json::from_value(damaged).unwrap();
        assert!(!valid(&restored, Kind::Ssl, 0));
        let mut unknown = serde_json::to_value(new(Kind::Ssl)).unwrap();
        unknown["unexpected"] = json!(true);
        assert!(serde_json::from_value::<State>(unknown).is_err());
    }

    fn other_kind(kind: Kind) -> Kind {
        match kind {
            Kind::Supertrend => Kind::UtAtr,
            Kind::UtAtr => Kind::Ssl,
            Kind::Ssl => Kind::WaveTrend,
            Kind::WaveTrend => Kind::Squeeze,
            Kind::Squeeze => Kind::Supertrend,
        }
    }
}
