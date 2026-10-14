//! Incremental ports of four frozen, formula-level Pine references.
//!
//! The retained state is deliberately bounded: each series keeps only its
//! rolling warm-up window or the seed needed by Pine's Wilder RMA. The caller
//! owns fills, account state, and order recovery.

use super::{BarInput, Signals, pine_gt, pine_lt};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

const STOCH_LEN: usize = 14;
const STOCH_SMOOTH: usize = 3;
const CCI_LEN: usize = 20;
const WPR_LEN: usize = 14;
const DMI_LEN: usize = 14;
const ADX_LEN: usize = 14;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum Kind {
    Stochastic,
    Cci,
    Williams,
    Adx,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) enum State {
    Stochastic(StochasticState),
    Cci(CciState),
    Williams(WilliamsState),
    Adx(AdxState),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StochasticState {
    bars: u64,
    highs: VecDeque<f64>,
    lows: VecDeque<f64>,
    raw_k: VecDeque<Option<f64>>,
    raw_k_sum: f64,
    raw_k_error_budget: f64,
    raw_k_max_abs_seen: f64,
    k_values: VecDeque<Option<f64>>,
    k_sum: f64,
    k_error_budget: f64,
    k_max_abs_seen: f64,
    previous_k: Option<f64>,
    previous_d: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CciState {
    bars: u64,
    source: VecDeque<f64>,
    sum: f64,
    sum_error_budget: f64,
    max_abs_seen: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WilliamsState {
    bars: u64,
    highs: VecDeque<f64>,
    lows: VecDeque<f64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdxState {
    bars: u64,
    previous_high: Option<f64>,
    previous_low: Option<f64>,
    previous_close: Option<f64>,
    tr_seed: VecDeque<f64>,
    plus_dm_seed: VecDeque<f64>,
    minus_dm_seed: VecDeque<f64>,
    smoothed_tr: Option<f64>,
    smoothed_plus_dm: Option<f64>,
    smoothed_minus_dm: Option<f64>,
    adx_seed: VecDeque<f64>,
    adx: Option<f64>,
    previous_plus: Option<f64>,
    previous_minus: Option<f64>,
    previous_adx: Option<f64>,
}

pub(crate) fn new(kind: Kind) -> State {
    match kind {
        Kind::Stochastic => State::Stochastic(StochasticState {
            bars: 0,
            highs: VecDeque::new(),
            lows: VecDeque::new(),
            raw_k: VecDeque::new(),
            raw_k_sum: 0.0,
            raw_k_error_budget: 0.0,
            raw_k_max_abs_seen: 0.0,
            k_values: VecDeque::new(),
            k_sum: 0.0,
            k_error_budget: 0.0,
            k_max_abs_seen: 0.0,
            previous_k: None,
            previous_d: None,
        }),
        Kind::Cci => State::Cci(CciState {
            bars: 0,
            source: VecDeque::new(),
            sum: 0.0,
            sum_error_budget: 0.0,
            max_abs_seen: 0.0,
        }),
        Kind::Williams => State::Williams(WilliamsState {
            bars: 0,
            highs: VecDeque::new(),
            lows: VecDeque::new(),
        }),
        Kind::Adx => State::Adx(AdxState {
            bars: 0,
            previous_high: None,
            previous_low: None,
            previous_close: None,
            tr_seed: VecDeque::new(),
            plus_dm_seed: VecDeque::new(),
            minus_dm_seed: VecDeque::new(),
            smoothed_tr: None,
            smoothed_plus_dm: None,
            smoothed_minus_dm: None,
            adx_seed: VecDeque::new(),
            adx: None,
            previous_plus: None,
            previous_minus: None,
            previous_adx: None,
        }),
    }
}

pub(crate) fn valid(state: &State, kind: Kind, bars: u64) -> bool {
    match (state, kind) {
        (State::Stochastic(s), Kind::Stochastic) => {
            s.bars == bars
                && exact_len(s.highs.len(), bars, STOCH_LEN)
                && exact_len(s.lows.len(), bars, STOCH_LEN)
                && exact_len(s.raw_k.len(), bars, STOCH_SMOOTH)
                && exact_len(s.k_values.len(), bars, STOCH_SMOOTH)
                && s.highs.iter().chain(s.lows.iter()).all(|v| v.is_finite())
                && s.raw_k.iter().chain(s.k_values.iter()).all(finite_option)
                && rolling_sum_valid(
                    &s.raw_k,
                    s.raw_k_sum,
                    s.raw_k_error_budget,
                    s.raw_k_max_abs_seen,
                    bars,
                )
                && rolling_sum_valid(
                    &s.k_values,
                    s.k_sum,
                    s.k_error_budget,
                    s.k_max_abs_seen,
                    bars,
                )
                && finite_option(&s.previous_k)
                && finite_option(&s.previous_d)
        }
        (State::Cci(s), Kind::Cci) => {
            s.bars == bars
                && exact_len(s.source.len(), bars, CCI_LEN)
                && s.source.iter().all(|v| v.is_finite())
                && rolling_value_sum_valid(
                    &s.source,
                    s.sum,
                    s.sum_error_budget,
                    s.max_abs_seen,
                    bars,
                )
        }
        (State::Williams(s), Kind::Williams) => {
            s.bars == bars
                && exact_len(s.highs.len(), bars, WPR_LEN)
                && exact_len(s.lows.len(), bars, WPR_LEN)
                && s.highs.iter().chain(s.lows.iter()).all(|v| v.is_finite())
        }
        (State::Adx(s), Kind::Adx) => valid_adx(s, bars),
        _ => false,
    }
}

pub(crate) fn update(state: &mut State, kind: Kind, bar: BarInput) -> Signals {
    let mut signals = Signals::default();
    if !bar.high.is_finite() || !bar.low.is_finite() || !bar.close.is_finite() {
        return signals;
    }
    match (state, kind) {
        (State::Stochastic(s), Kind::Stochastic) if s.bars < u64::MAX => {
            signals = update_stochastic(s, bar);
        }
        (State::Cci(s), Kind::Cci) if s.bars < u64::MAX => signals = update_cci(s, bar),
        (State::Williams(s), Kind::Williams) if s.bars < u64::MAX => {
            signals = update_williams(s, bar);
        }
        (State::Adx(s), Kind::Adx) if s.bars < u64::MAX => signals = update_adx(s, bar),
        _ => {}
    }
    signals
}

fn update_stochastic(s: &mut StochasticState, bar: BarInput) -> Signals {
    push_bounded(&mut s.highs, bar.high, STOCH_LEN);
    push_bounded(&mut s.lows, bar.low, STOCH_LEN);
    let raw = if s.highs.len() == STOCH_LEN && s.lows.len() == STOCH_LEN {
        let highest = s.highs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let lowest = s.lows.iter().copied().fold(f64::INFINITY, f64::min);
        let range = highest - lowest;
        (range != 0.0)
            .then(|| 100.0 * (bar.close - lowest) / range)
            .filter(|v| v.is_finite())
    } else {
        None
    };
    push_rolling_option(
        &mut s.raw_k,
        raw,
        STOCH_SMOOTH,
        &mut s.raw_k_sum,
        &mut s.raw_k_error_budget,
        &mut s.raw_k_max_abs_seen,
    );
    let k = full_window_mean(&s.raw_k, s.raw_k_sum, STOCH_SMOOTH);
    push_rolling_option(
        &mut s.k_values,
        k,
        STOCH_SMOOTH,
        &mut s.k_sum,
        &mut s.k_error_budget,
        &mut s.k_max_abs_seen,
    );
    let d = full_window_mean(&s.k_values, s.k_sum, STOCH_SMOOTH);

    let buy = matches!((s.previous_k, s.previous_d, k, d),
        (Some(pk), Some(pd), Some(k), Some(d)) if pk <= pd && k > d && pine_lt(k, 25.0));
    let sell = matches!((s.previous_k, s.previous_d, k, d),
        (Some(pk), Some(pd), Some(k), Some(d)) if pk >= pd && k < d && pine_gt(k, 75.0));
    s.previous_k = k;
    s.previous_d = d;
    s.bars += 1;

    let mut out = Signals {
        buy,
        sell,
        ..Signals::default()
    };
    insert(&mut out, "k", k);
    insert(&mut out, "d", d);
    out
}

fn update_cci(s: &mut CciState, bar: BarInput) -> Signals {
    let source = (bar.high + bar.low + bar.close) / 3.0;
    if !source.is_finite() {
        return Signals::default();
    }
    push_rolling_value(
        &mut s.source,
        source,
        CCI_LEN,
        &mut s.sum,
        &mut s.sum_error_budget,
        &mut s.max_abs_seen,
    );
    let cci = if s.source.len() == CCI_LEN {
        let mean = s.sum / CCI_LEN as f64;
        let deviation = s.source.iter().map(|v| (*v - mean).abs()).sum::<f64>() / CCI_LEN as f64;
        (deviation != 0.0).then(|| (source - mean) / (0.015 * deviation))
    } else {
        None
    };
    s.bars += 1;
    let mut out = Signals {
        buy: cci.is_some_and(|v| pine_lt(v, -100.0)),
        sell: cci.is_some_and(|v| pine_gt(v, 50.0)),
        ..Signals::default()
    };
    insert(&mut out, "cci", cci);
    out
}

fn update_williams(s: &mut WilliamsState, bar: BarInput) -> Signals {
    push_bounded(&mut s.highs, bar.high, WPR_LEN);
    push_bounded(&mut s.lows, bar.low, WPR_LEN);
    let w = if s.highs.len() == WPR_LEN && s.lows.len() == WPR_LEN {
        let highest = s.highs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let lowest = s.lows.iter().copied().fold(f64::INFINITY, f64::min);
        let range = highest - lowest;
        (range != 0.0).then(|| -100.0 * (highest - bar.close) / range)
    } else {
        None
    };
    s.bars += 1;
    let mut out = Signals {
        buy: w.is_some_and(|v| pine_lt(v, -80.0)),
        sell: w.is_some_and(|v| pine_gt(v, -20.0)),
        ..Signals::default()
    };
    insert(&mut out, "williams", w);
    out
}

fn update_adx(s: &mut AdxState, bar: BarInput) -> Signals {
    let mut plus = None;
    let mut minus = None;
    let mut adx = None;
    if let (Some(previous_high), Some(previous_low), Some(previous_close)) =
        (s.previous_high, s.previous_low, s.previous_close)
    {
        let tr = (bar.high - bar.low)
            .max((bar.high - previous_close).abs())
            .max((bar.low - previous_close).abs());
        let up = bar.high - previous_high;
        let down = previous_low - bar.low;
        let plus_dm = if up > down && up > 0.0 { up } else { 0.0 };
        let minus_dm = if down > up && down > 0.0 { down } else { 0.0 };
        if !tr.is_finite() || !plus_dm.is_finite() || !minus_dm.is_finite() {
            return Signals::default();
        }
        s.smoothed_tr = rma_step(&mut s.tr_seed, s.smoothed_tr, tr, DMI_LEN);
        s.smoothed_plus_dm = rma_step(&mut s.plus_dm_seed, s.smoothed_plus_dm, plus_dm, DMI_LEN);
        s.smoothed_minus_dm =
            rma_step(&mut s.minus_dm_seed, s.smoothed_minus_dm, minus_dm, DMI_LEN);
        if let (Some(tr), Some(plus_dm), Some(minus_dm)) =
            (s.smoothed_tr, s.smoothed_plus_dm, s.smoothed_minus_dm)
        {
            let (plus_di, minus_di) = if tr != 0.0 {
                (100.0 * plus_dm / tr, 100.0 * minus_dm / tr)
            } else {
                (0.0, 0.0)
            };
            let sum = plus_di + minus_di;
            let dx = if sum != 0.0 {
                100.0 * (plus_di - minus_di).abs() / sum
            } else {
                0.0
            };
            s.adx = rma_step(&mut s.adx_seed, s.adx, dx, ADX_LEN);
            plus = Some(plus_di);
            minus = Some(minus_di);
            adx = s.adx;
        }
    }
    let buy = matches!((plus, minus, adx), (Some(p), Some(m), Some(a)) if pine_gt(p, m) && pine_gt(a, 25.0));
    let sell = matches!((plus, minus), (Some(p), Some(m)) if pine_lt(p, m));
    s.previous_high = Some(bar.high);
    s.previous_low = Some(bar.low);
    s.previous_close = Some(bar.close);
    s.previous_plus = plus;
    s.previous_minus = minus;
    s.previous_adx = adx;
    s.bars += 1;
    let mut out = Signals {
        buy,
        sell,
        ..Signals::default()
    };
    insert(&mut out, "plus", plus);
    insert(&mut out, "minus", minus);
    insert(&mut out, "adx", adx);
    out
}

fn rma_step(
    seed: &mut VecDeque<f64>,
    previous: Option<f64>,
    source: f64,
    length: usize,
) -> Option<f64> {
    if let Some(previous) = previous {
        return Some((previous * (length - 1) as f64 + source) / length as f64);
    }
    seed.push_back(source);
    if seed.len() == length {
        let mean = seed.iter().copied().sum::<f64>() / length as f64;
        seed.clear();
        Some(mean)
    } else {
        None
    }
}

fn valid_adx(s: &AdxState, bars: u64) -> bool {
    let dmi_samples = bars.saturating_sub(1).min(usize::MAX as u64) as usize;
    let dx_samples = dmi_samples.saturating_sub(DMI_LEN - 1);
    let expected_dmi_seed = if s.smoothed_tr.is_some() {
        0
    } else {
        dmi_samples.min(DMI_LEN)
    };
    let expected_adx_seed = if s.adx.is_some() {
        0
    } else {
        dx_samples.min(ADX_LEN)
    };
    s.bars == bars
        && [s.previous_high, s.previous_low, s.previous_close]
            .iter()
            .all(finite_option)
        && (bars == 0) == s.previous_close.is_none()
        && s.previous_high.is_some() == s.previous_close.is_some()
        && s.previous_low.is_some() == s.previous_close.is_some()
        && s.tr_seed.len() == expected_dmi_seed
        && s.plus_dm_seed.len() == expected_dmi_seed
        && s.minus_dm_seed.len() == expected_dmi_seed
        && s.adx_seed.len() == expected_adx_seed
        && (s.smoothed_tr.is_some() == s.smoothed_plus_dm.is_some())
        && (s.smoothed_tr.is_some() == s.smoothed_minus_dm.is_some())
        && (s.smoothed_tr.is_some() == (dmi_samples >= DMI_LEN))
        && (s.adx.is_some() == (dx_samples >= ADX_LEN))
        && s.previous_plus.is_some() == (dmi_samples >= DMI_LEN)
        && s.previous_minus.is_some() == (dmi_samples >= DMI_LEN)
        && s.previous_adx.is_some() == (dx_samples >= ADX_LEN)
        && s.tr_seed
            .iter()
            .chain(s.plus_dm_seed.iter())
            .chain(s.minus_dm_seed.iter())
            .chain(s.adx_seed.iter())
            .all(|v| v.is_finite())
        && [
            s.smoothed_tr,
            s.smoothed_plus_dm,
            s.smoothed_minus_dm,
            s.adx,
            s.previous_plus,
            s.previous_minus,
            s.previous_adx,
        ]
        .iter()
        .all(finite_option)
}

fn push_bounded<T>(queue: &mut VecDeque<T>, value: T, max: usize) {
    if queue.len() == max {
        queue.pop_front();
    }
    queue.push_back(value);
}

fn push_rolling_option(
    queue: &mut VecDeque<Option<f64>>,
    value: Option<f64>,
    length: usize,
    sum: &mut f64,
    error_budget: &mut f64,
    max_abs_seen: &mut f64,
) {
    let previous_sum = *sum;
    let outgoing = if queue.len() == length {
        queue.pop_front().flatten()
    } else {
        None
    };
    let tail_recovered = rolling_sum_remove(
        sum,
        error_budget,
        outgoing,
        queue.iter().copied().flatten(),
        queue.front().copied().flatten(),
        queue.back().copied().flatten(),
    );
    if queue
        .iter()
        .all(|value| value.is_some_and(|value| value == 0.0))
    {
        *sum = 0.0;
    }
    queue.push_back(value);
    rolling_sum_append(sum, error_budget, value);
    // The ta.sma bar-update path uses one exact input delta for nearby
    // same-sign replacements after appending the incoming value.
    if !tail_recovered
        && let (Some(incoming), Some(outgoing)) = (value, outgoing)
        && incoming.is_sign_positive() == outgoing.is_sign_positive()
        && incoming.abs() >= outgoing.abs() * 0.5
        && outgoing.abs() >= incoming.abs() * 0.5
    {
        let delta = incoming - outgoing;
        *sum = previous_sum + delta;
        *error_budget += rounding_allowance(previous_sum.abs() + delta.abs());
    }
    if let Some(value) = value {
        *max_abs_seen = (*max_abs_seen).max(value.abs());
    }
    if queue
        .iter()
        .all(|value| value.is_some_and(|value| value == 0.0))
    {
        *sum = 0.0;
        *error_budget = 0.0;
    }
}

/// The pinned runtime's rolling sum update: both call sites subtract the
/// outgoing sample and add the incoming sample. The SMA caller may then apply
/// its additional nearby same-sign delta shortcut; CCI's generic flow caller
/// does not.
fn rolling_sum_remove(
    sum: &mut f64,
    error_budget: &mut f64,
    outgoing: Option<f64>,
    retained: impl Iterator<Item = f64> + Clone,
    front: Option<f64>,
    back: Option<f64>,
) -> bool {
    let mut tail_recovered = false;
    if let Some(outgoing) = outgoing {
        let before = *sum;
        *sum -= outgoing;
        *error_budget += rounding_allowance(before.abs() + outgoing.abs());
        if *sum == 0.0
            && outgoing.is_finite()
            && (front.is_some_and(|v| v != 0.0) || back.is_some_and(|v| v != 0.0))
            && (front.into_iter().chain(back).any(|v| {
                v.is_finite()
                    && v != 0.0
                    && v.abs() <= outgoing.abs() * (1.0 / 9_007_199_254_740_992.0)
            }))
            && let Some(rebuilt) = reconstruct_sum(retained.clone())
            && rebuilt != *sum
        {
            *sum = rebuilt;
            tail_recovered = true;
        }
    }
    tail_recovered
}

fn rolling_sum_append(sum: &mut f64, error_budget: &mut f64, incoming: Option<f64>) {
    if let Some(incoming) = incoming {
        let before = *sum;
        *sum += incoming;
        *error_budget += rounding_allowance(before.abs() + incoming.abs());
    }
}

fn push_rolling_value(
    queue: &mut VecDeque<f64>,
    value: f64,
    length: usize,
    sum: &mut f64,
    error_budget: &mut f64,
    max_abs_seen: &mut f64,
) {
    let outgoing = (queue.len() == length).then(|| queue.pop_front().unwrap());
    let _tail_recovered = rolling_sum_remove(
        sum,
        error_budget,
        outgoing,
        queue.iter().copied(),
        queue.front().copied(),
        queue.back().copied(),
    );
    if queue.iter().all(|value| *value == 0.0) {
        *sum = 0.0;
    }
    queue.push_back(value);
    rolling_sum_append(sum, error_budget, Some(value));
    // CCI calls the generic update_rolling_window path, which is ordinary
    // subtract-then-add; only ta.sma's update_rolling_window_for_bar gets the
    // targeted same-sign delta optimization.
    *max_abs_seen = (*max_abs_seen).max(value.abs());
    if queue.iter().all(|value| *value == 0.0) {
        *sum = 0.0;
        *error_budget = 0.0;
    }
}

fn two_sum(left: f64, right: f64) -> (f64, f64) {
    let sum = left + right;
    let right_virtual = sum - left;
    let left_virtual = sum - right_virtual;
    let right_roundoff = right - right_virtual;
    let left_roundoff = left - left_virtual;
    (sum, left_roundoff + right_roundoff)
}

/// Exact, scale-normalized tail reconstruction used only when a dominant
/// evicted value appears to have erased a tiny retained residual.
fn reconstruct_sum(values: impl Iterator<Item = f64>) -> Option<f64> {
    let values: Vec<_> = values.collect();
    let maximum = values.iter().map(|v| v.abs()).fold(0.0_f64, f64::max);
    if maximum == 0.0 {
        return Some(0.0);
    }
    if !maximum.is_finite() {
        return None;
    }
    let exponent = maximum.to_bits() & 0x7ff0_0000_0000_0000;
    let scale = if exponent == 0 {
        f64::from_bits(1_u64 << maximum.to_bits().ilog2())
    } else {
        f64::from_bits(exponent)
    };
    let mut total = 0.0;
    let mut correction = 0.0;
    for value in values {
        let scaled = value / scale;
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
    let (rounded, _) = two_sum(total, correction);
    let sum = rounded * scale;
    (sum.is_finite() && (sum / scale).to_bits() == rounded.to_bits()).then_some(sum)
}

fn rounding_allowance(magnitude: f64) -> f64 {
    magnitude * f64::EPSILON * 2.0
}

fn full_window_mean(queue: &VecDeque<Option<f64>>, sum: f64, length: usize) -> Option<f64> {
    (queue.len() == length && queue.iter().all(Option::is_some)).then_some(sum / length as f64)
}

fn rolling_sum_valid(
    queue: &VecDeque<Option<f64>>,
    sum: f64,
    error_budget: f64,
    max_abs_seen: f64,
    bars: u64,
) -> bool {
    rolling_sum_valid_parts(
        queue.iter().copied().flatten(),
        queue.len(),
        sum,
        error_budget,
        max_abs_seen,
        bars,
        true,
    )
}

fn rolling_value_sum_valid(
    queue: &VecDeque<f64>,
    sum: f64,
    error_budget: f64,
    max_abs_seen: f64,
    bars: u64,
) -> bool {
    rolling_sum_valid_parts(
        queue.iter().copied(),
        queue.len(),
        sum,
        error_budget,
        max_abs_seen,
        bars,
        false,
    )
}

fn rolling_sum_valid_parts(
    values: impl Iterator<Item = f64> + Clone,
    length: usize,
    sum: f64,
    error_budget: f64,
    max_abs_seen: f64,
    bars: u64,
    sma_delta_shortcut: bool,
) -> bool {
    if !sum.is_finite()
        || !error_budget.is_finite()
        || error_budget < 0.0
        || !max_abs_seen.is_finite()
        || max_abs_seen < 0.0
    {
        return false;
    }
    let mut canonical = 0.0;
    let mut absolute_mass = 0.0;
    let mut maximum = 0.0_f64;
    for value in values.clone() {
        canonical += value;
        absolute_mass += value.abs();
        maximum = maximum.max(value.abs());
    }
    // `rounding_allowance` contributes 2*EPSILON times each operation's
    // operand magnitude. A rolling sum's magnitude is bounded by
    // window_len*max_sample plus its accumulated rounding budget. One normal
    // replacement has two operations; the SMA delta path records one more.
    // Thus B[n+1] <= (1+q)*B[n] + q*(window_len+2)*max_sample, where q is
    // 4*EPSILON for CCI and 6*EPSILON for SMA. Solve that recurrence rather
    // than using an unrelated constant or dropping the window-length factor.
    let allowance_coefficient = if sma_delta_shortcut { 6.0 } else { 4.0 };
    let q = allowance_coefficient * f64::EPSILON;
    let cumulative_growth = (q.ln_1p() * bars as f64).exp_m1();
    let history_limit = max_abs_seen * (length as f64 + 2.0) * cumulative_growth;
    if maximum > max_abs_seen || history_limit.is_nan() || error_budget > history_limit {
        return false;
    }
    if maximum == 0.0 {
        return sum.abs() <= error_budget;
    }
    if canonical.is_finite() {
        let tolerance = error_budget + absolute_mass * f64::EPSILON * (length as f64 + 1.0) * 4.0;
        (sum - canonical).abs() <= tolerance
    } else {
        // The runtime can retain a finite compensated aggregate where a cold
        // iterator's intermediate sum overflows. Keep the finite state and its
        // bounded history-derived error budget in that case.
        true
    }
}

fn exact_len(actual: usize, bars: u64, max: usize) -> bool {
    actual == bars.min(max as u64) as usize
}

fn finite_option(value: &Option<f64>) -> bool {
    value.is_none_or(f64::is_finite)
}

fn insert(signals: &mut Signals, name: &str, value: Option<f64>) {
    signals
        .plots
        .insert(name.to_owned(), value.filter(|v| v.is_finite()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(high: f64, low: f64, close: f64) -> BarInput {
        BarInput { high, low, close }
    }

    fn step(state: &mut State, kind: Kind, bars: &[BarInput]) -> Vec<Signals> {
        bars.iter().map(|bar| update(state, kind, *bar)).collect()
    }

    fn close(actual: Option<f64>, expected: f64) {
        assert!(
            (actual.unwrap() - expected).abs() <= 1e-12,
            "{actual:?} != {expected}"
        );
    }

    #[test]
    fn frozen_reference_oracles_cover_warmup_and_values() {
        // Every oracle below came from pine_compat 0.3.1 Program.run over the
        // same 45 deterministic OHLC bars; null positions are asserted too.
        let bars: Vec<_> = (0..45)
            .map(|i| {
                let close = 100.0 + (i % 9) as f64 * 2.0 + (i / 9) as f64;
                bar(
                    close + 2.0 + (i % 3) as f64,
                    close - 1.0 - (i % 2) as f64,
                    close,
                )
            })
            .collect();
        let mut stochastic = new(Kind::Stochastic);
        let stoch = step(&mut stochastic, Kind::Stochastic, &bars);
        assert!(stoch[..15].iter().all(|s| s.plots["k"].is_none()));
        close(stoch[15].plots["k"], 57.142857142857146);
        close(stoch[17].plots["d"], 66.23376623376623);
        close(stoch[44].plots["k"], 73.65079365079366);
        let mut cci = new(Kind::Cci);
        let cci_values = step(&mut cci, Kind::Cci, &bars);
        assert!(cci_values[..19].iter().all(|s| s.plots["cci"].is_none()));
        close(cci_values[19].plots["cci"], -58.99280575539599);
        close(cci_values[44].plots["cci"], 121.08159846853302);
        let mut flat = new(Kind::Cci);
        let flat_values = step(&mut flat, Kind::Cci, &vec![bar(5.0, 5.0, 5.0); CCI_LEN]);
        assert!(flat_values.iter().all(|s| s.plots["cci"].is_none()));

        let mut wpr = new(Kind::Williams);
        let wpr_values = step(&mut wpr, Kind::Williams, &bars);
        assert!(
            wpr_values[..13]
                .iter()
                .all(|s| s.plots["williams"].is_none())
        );
        close(wpr_values[13].plots["williams"], -52.38095238095238);
        close(wpr_values[44].plots["williams"], -19.047619047619047);

        let mut adx = new(Kind::Adx);
        let adx_values = step(&mut adx, Kind::Adx, &bars);
        assert!(adx_values[..14].iter().all(|s| s.plots["plus"].is_none()));
        close(adx_values[14].plots["plus"], 35.714285714285715);
        close(adx_values[14].plots["minus"], 19.047619047619047);
        assert!(adx_values[..27].iter().all(|s| s.plots["adx"].is_none()));
        close(adx_values[27].plots["adx"], 22.340626749933993);
        close(adx_values[44].plots["adx"], 17.591885909909696);
    }

    #[test]
    fn pinned_runtime_oracle_numbers_for_short_formula_periods() {
        // The 3-bar forms are the same ta.* kernels with test-sized periods in
        // the pinned runtime's own builtins_ta_flow fixtures.
        let seq = [
            bar(11.0, 9.0, 10.0),
            bar(12.0, 10.0, 11.0),
            bar(13.0, 11.0, 12.0),
            bar(16.0, 12.0, 15.0),
            bar(17.0, 14.0, 16.0),
            bar(16.0, 8.0, 9.0),
        ];
        let expected_stoch = [
            None,
            None,
            Some(75.0),
            Some(83.33333333333333),
            Some(83.33333333333333),
            Some(11.11111111111111),
        ];
        let raw: Vec<Option<f64>> = seq
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if i < 2 {
                    None
                } else {
                    let hi = seq[i.saturating_sub(2)..=i]
                        .iter()
                        .map(|x| x.high)
                        .fold(f64::NEG_INFINITY, f64::max);
                    let lo = seq[i.saturating_sub(2)..=i]
                        .iter()
                        .map(|x| x.low)
                        .fold(f64::INFINITY, f64::min);
                    Some(100.0 * (b.close - lo) / (hi - lo))
                }
            })
            .collect();
        assert_eq!(raw, expected_stoch);

        let mut cci = new(Kind::Cci);
        let bars: Vec<_> = (0..CCI_LEN)
            .map(|i| bar((i + 1) as f64, (i + 1) as f64, (i + 1) as f64))
            .collect();
        let signals = step(&mut cci, Kind::Cci, &bars);
        assert!((signals.last().unwrap().plots["cci"].unwrap() - 126.66666666666666).abs() < 1e-12);
    }

    #[test]
    fn json_roundtrip_rejects_wrong_kind_overflow_and_corruption() {
        for kind in [Kind::Stochastic, Kind::Cci, Kind::Williams, Kind::Adx] {
            let mut state = new(kind);
            assert!(valid(&state, kind, 0));
            let wrong_kind = match kind {
                Kind::Stochastic => Kind::Cci,
                _ => Kind::Stochastic,
            };
            assert!(!valid(&state, wrong_kind, 0));
            let bars = [bar(12.0, 9.0, 10.0); 35];
            step(&mut state, kind, &bars);
            assert!(valid(&state, kind, 35));
            let restored: State =
                serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
            assert_eq!(restored, state);
            assert!(valid(&restored, kind, 35));
            assert!(!valid(&restored, kind, 34));
        }
        let bad = State::Cci(CciState {
            bars: 1,
            source: VecDeque::from([f64::NAN]),
            sum: 0.0,
            sum_error_budget: 0.0,
            max_abs_seen: 0.0,
        });
        assert!(!valid(&bad, Kind::Cci, 1));
        let overflow = State::Cci(CciState {
            bars: 1,
            source: VecDeque::from(vec![0.0; CCI_LEN + 1]),
            sum: 0.0,
            sum_error_budget: 0.0,
            max_abs_seen: 0.0,
        });
        assert!(!valid(&overflow, Kind::Cci, 1));
        let mut mismatch = new(Kind::Cci);
        let output = update(&mut mismatch, Kind::Williams, bar(2.0, 1.0, 1.5));
        assert!(!output.buy && !output.sell);
        assert!(valid(&mismatch, Kind::Cci, 0));
    }

    #[test]
    fn wpr_warmup_flat_range_and_dmi_oracle_seed() {
        let mut wpr = new(Kind::Williams);
        let values = step(&mut wpr, Kind::Williams, &vec![bar(1.0, 1.0, 1.0); WPR_LEN]);
        assert!(values.iter().all(|s| s.plots["williams"].is_none()));
        let bars = [
            bar(11.0, 9.0, 10.0),
            bar(12.0, 10.0, 11.0),
            bar(13.0, 11.0, 12.0),
            bar(16.0, 12.0, 15.0),
            bar(17.0, 14.0, 16.0),
            bar(16.0, 8.0, 9.0),
        ];
        let mut state = new(Kind::Williams);
        let values = step(&mut state, Kind::Williams, &bars);
        assert!(values.iter().all(|s| s.plots["williams"].is_none()));

        let mut adx = new(Kind::Adx);
        let bars: Vec<_> = (0..40)
            .map(|i| {
                let close = 100.0 + i as f64;
                bar(close + 1.0, close - 1.0, close)
            })
            .collect();
        let values = step(&mut adx, Kind::Adx, &bars);
        assert!(
            values[..14]
                .iter()
                .all(|s| s.plots["plus"].is_none() && s.plots["adx"].is_none())
        );
        assert_eq!(values[14].plots["plus"], Some(50.0));
        assert!(values[14..26].iter().all(|s| s.plots["adx"].is_none()));
        assert_eq!(values[27].plots["adx"], Some(100.0));
    }

    #[test]
    fn reference_thresholds_and_stochastic_equality_crossings() {
        let mut wpr = new(Kind::Williams);
        let mut values = step(
            &mut wpr,
            Kind::Williams,
            &vec![bar(100.0, 0.0, 50.0); WPR_LEN],
        );
        values.push(update(&mut wpr, Kind::Williams, bar(100.0, 0.0, 20.0)));
        assert!(
            !values.last().unwrap().buy,
            "-80 is not below the entry threshold"
        );
        values.push(update(&mut wpr, Kind::Williams, bar(100.0, 0.0, 10.0)));
        assert!(values.last().unwrap().buy);
        values.push(update(&mut wpr, Kind::Williams, bar(100.0, 0.0, 80.0)));
        assert!(
            !values.last().unwrap().sell,
            "-20 is not above the exit threshold"
        );
        values.push(update(&mut wpr, Kind::Williams, bar(100.0, 0.0, 90.0)));
        assert!(values.last().unwrap().sell);

        let mut rising = new(Kind::Stochastic);
        let mut rising_bars = vec![bar(100.0, 0.0, 5.0); 18];
        rising_bars.push(bar(100.0, 0.0, 6.0));
        let entry = step(&mut rising, Kind::Stochastic, &rising_bars);
        assert!(
            entry.last().unwrap().buy,
            "equal previous K/D crosses up below 25"
        );

        let mut falling = new(Kind::Stochastic);
        let mut falling_bars = vec![bar(100.0, 0.0, 95.0); 18];
        falling_bars.push(bar(100.0, 0.0, 94.0));
        let exit = step(&mut falling, Kind::Stochastic, &falling_bars);
        assert!(
            exit.last().unwrap().sell,
            "equal previous K/D crosses down above 75"
        );

        let mut adx = new(Kind::Adx);
        let trend: Vec<_> = (0..28)
            .map(|i| {
                let close = 100.0 + i as f64;
                bar(close + 1.0, close - 1.0, close)
            })
            .collect();
        let signals = step(&mut adx, Kind::Adx, &trend);
        assert!(!signals[26].buy);
        assert!(
            signals[27].buy,
            "+DI leads and seeded ADX is strictly above 25"
        );
    }

    #[test]
    fn rolling_sma_matches_pinned_order_on_long_sideways_and_volatile_to_flat() {
        // Captured independently from pine_compat 0.3.1 Program.run with the
        // same formulas. These long windows distinguish the runtime's rolling
        // aggregate order from a fresh `iter().sum()` on each bar.
        let mut sideways = Vec::with_capacity(1024);
        for i in 0..1024 {
            let delta = (((i * 17) % 13) - 6) as f64 * 0.0001220703125;
            let close = 1.0e12 + delta;
            sideways.push(bar(close + 0.00048828125, close - 0.00048828125, close));
        }
        let mut stoch = new(Kind::Stochastic);
        let stoch_values = step(&mut stoch, Kind::Stochastic, &sideways);
        close(stoch_values[511].plots["k"], 58.333333333333336);
        close(stoch_values[511].plots["d"], 52.77777777777777);
        close(stoch_values[1023].plots["k"], 50.0);
        close(stoch_values[1023].plots["d"], 51.66666666666666);
        assert!(valid(&stoch, Kind::Stochastic, 1024));
        let restored: State =
            serde_json::from_value(serde_json::to_value(&stoch).unwrap()).unwrap();
        assert_eq!(restored, stoch);
        assert!(valid(&restored, Kind::Stochastic, 1024));

        let mut transition = Vec::with_capacity(1024);
        for i in 0..128 {
            let close = 100.0 + ((i * 37) % 101) as f64 * 1.0e10;
            transition.push(bar(close + 4.0e9, close - 3.0e9, close));
        }
        transition.extend((128..1024).map(|_| bar(1.0e12, 1.0e12, 1.0e12)));

        let mut cci = new(Kind::Cci);
        let first = step(&mut cci, Kind::Cci, &transition[..128]);
        close(first[127].plots["cci"], 8.38315124029413);
        assert!(valid(&cci, Kind::Cci, 128));
        let snapshot = serde_json::to_value(&cci).unwrap();
        let mut resumed: State = serde_json::from_value(snapshot).unwrap();
        assert!(valid(&resumed, Kind::Cci, 128));
        let flat = step(&mut resumed, Kind::Cci, &transition[128..]);
        close(flat[18].plots["cci"], 35.08771929824613);
        close(flat[19].plots["cci"], 66.66666666666667);
        close(flat[895].plots["cci"], 66.66666666666667);
        assert!(valid(&resumed, Kind::Cci, 1024));

        // The source is completely flat after its old volatile observations
        // leave the 20-sample window. The pinned aggregate order retains this
        // small nonzero mean residual, so the CCI diagnostic remains finite.
        assert!(
            flat[19..]
                .iter()
                .all(|signal| signal.plots["cci"].is_some())
        );
        let restored_flat: State =
            serde_json::from_value(serde_json::to_value(&resumed).unwrap()).unwrap();
        assert_eq!(restored_flat, resumed);
        assert!(valid(&restored_flat, Kind::Cci, 1024));
        let mut corrupted = match resumed.clone() {
            State::Cci(state) => state,
            _ => unreachable!(),
        };
        corrupted.sum += corrupted.sum_error_budget * 2.0 + 1.0;
        assert!(!valid(&State::Cci(corrupted), Kind::Cci, 1024));
    }
}
