//! Incremental implementations for four formula-level Pine reference scripts.
//!
//! The warm-up behavior follows the pinned pine-compat 0.3.1 implementation:
//! `ta.ema` seeds with an SMA after `length` samples, MACD's signal EMA begins
//! only after both component EMAs exist, and `ta.bb` uses population variance.

use super::{BarInput, Signals, pine_gt, pine_lt};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

const EMA_FAST: usize = 12;
const EMA_SLOW: usize = 26;
const MACD_SIGNAL: usize = 9;
const BB_LENGTH: usize = 20;
const ROC_LENGTH: usize = 20;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum Kind {
    EmaCross,
    Macd,
    BollingerReversion,
    Roc,
}

/// The serde tag makes snapshots self-identifying; `valid` additionally
/// requires the caller's strategy kind and bar count to agree with the state.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", content = "state", deny_unknown_fields)]
pub(crate) enum State {
    EmaCross(EmaState),
    Macd(MacdState),
    BollingerReversion(BollingerState),
    Roc(RocState),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EmaState {
    samples: u64,
    fast_seed_samples: u8,
    fast_seed_sum: f64,
    fast: Option<f64>,
    slow_seed_samples: u8,
    slow_seed_sum: f64,
    slow: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MacdState {
    samples: u64,
    fast_seed_samples: u8,
    fast_seed_sum: f64,
    fast: Option<f64>,
    slow_seed_samples: u8,
    slow_seed_sum: f64,
    slow: Option<f64>,
    signal_seed_samples: u8,
    signal_seed_sum: f64,
    signal: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BollingerState {
    samples: u64,
    closes: VecDeque<f64>,
    sum: f64,
    nonzero_count: u8,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RocState {
    samples: u64,
    closes: VecDeque<f64>,
}

pub(crate) fn new(kind: Kind) -> State {
    match kind {
        Kind::EmaCross => State::EmaCross(EmaState {
            samples: 0,
            fast_seed_samples: 0,
            fast_seed_sum: 0.0,
            fast: None,
            slow_seed_samples: 0,
            slow_seed_sum: 0.0,
            slow: None,
        }),
        Kind::Macd => State::Macd(MacdState {
            samples: 0,
            fast_seed_samples: 0,
            fast_seed_sum: 0.0,
            fast: None,
            slow_seed_samples: 0,
            slow_seed_sum: 0.0,
            slow: None,
            signal_seed_samples: 0,
            signal_seed_sum: 0.0,
            signal: None,
        }),
        Kind::BollingerReversion => State::BollingerReversion(BollingerState {
            samples: 0,
            closes: VecDeque::with_capacity(BB_LENGTH),
            sum: 0.0,
            nonzero_count: 0,
        }),
        Kind::Roc => State::Roc(RocState {
            samples: 0,
            closes: VecDeque::with_capacity(ROC_LENGTH + 1),
        }),
    }
}

pub(crate) fn valid(state: &State, kind: Kind, bars: u64) -> bool {
    match (state, kind) {
        (State::EmaCross(state), Kind::EmaCross) => valid_ema(state, bars),
        (State::Macd(state), Kind::Macd) => valid_macd(state, bars),
        (State::BollingerReversion(state), Kind::BollingerReversion) => {
            state.samples == bars
                && state.closes.len() == bars.min(BB_LENGTH as u64) as usize
                && state.closes.iter().all(|value| value.is_finite())
                && state.sum.is_finite()
                && state.nonzero_count as usize
                    == state.closes.iter().filter(|value| **value != 0.0).count()
        }
        (State::Roc(state), Kind::Roc) => {
            state.samples == bars
                && state.closes.len() == bars.min((ROC_LENGTH + 1) as u64) as usize
                && state.closes.iter().all(|value| value.is_finite())
        }
        _ => false,
    }
}

fn valid_ema(state: &EmaState, bars: u64) -> bool {
    state.samples == bars
        && state.fast_seed_samples as u64 == bars.min(EMA_FAST as u64)
        && state.slow_seed_samples as u64 == bars.min(EMA_SLOW as u64)
        && state.fast.is_some() == (bars >= EMA_FAST as u64)
        && state.slow.is_some() == (bars >= EMA_SLOW as u64)
        && [state.fast_seed_sum, state.slow_seed_sum]
            .into_iter()
            .all(f64::is_finite)
        && [state.fast, state.slow]
            .into_iter()
            .flatten()
            .all(f64::is_finite)
}

fn valid_macd(state: &MacdState, bars: u64) -> bool {
    let macd_samples = bars.saturating_sub((EMA_SLOW - 1) as u64);
    state.samples == bars
        && state.fast_seed_samples as u64 == bars.min(EMA_FAST as u64)
        && state.slow_seed_samples as u64 == bars.min(EMA_SLOW as u64)
        && state.fast.is_some() == (bars >= EMA_FAST as u64)
        && state.slow.is_some() == (bars >= EMA_SLOW as u64)
        && state.signal_seed_samples as u64 == macd_samples.min(MACD_SIGNAL as u64)
        && state.signal.is_some() == (macd_samples >= MACD_SIGNAL as u64)
        && [
            state.fast_seed_sum,
            state.slow_seed_sum,
            state.signal_seed_sum,
        ]
        .into_iter()
        .all(f64::is_finite)
        && [state.fast, state.slow, state.signal]
            .into_iter()
            .flatten()
            .all(f64::is_finite)
}

pub(crate) fn update(state: &mut State, kind: Kind, bar: BarInput) -> Signals {
    match (state, kind) {
        (State::EmaCross(state), Kind::EmaCross) => update_ema(state, bar.close),
        (State::Macd(state), Kind::Macd) => update_macd(state, bar.close),
        (State::BollingerReversion(state), Kind::BollingerReversion) => {
            update_bollinger(state, bar.close)
        }
        (State::Roc(state), Kind::Roc) => update_roc(state, bar.close),
        _ => Signals::default(),
    }
}

fn update_ema(state: &mut EmaState, close: f64) -> Signals {
    state.samples = state.samples.saturating_add(1);
    state.fast = ema_step(
        close,
        EMA_FAST,
        &mut state.fast_seed_samples,
        &mut state.fast_seed_sum,
        state.fast,
    );
    state.slow = ema_step(
        close,
        EMA_SLOW,
        &mut state.slow_seed_samples,
        &mut state.slow_seed_sum,
        state.slow,
    );
    let (buy, sell) = level_pair_signals(state.fast, state.slow);
    plots(buy, sell, [("fast", state.fast), ("slow", state.slow)])
}

/// This is pine-compat 0.3.1's `ta.ema`: its first defined value is the SMA of
/// the full seed window; following samples use alpha*src + (1-alpha)*previous.
fn ema_step(
    source: f64,
    length: usize,
    seed_samples: &mut u8,
    seed_sum: &mut f64,
    previous: Option<f64>,
) -> Option<f64> {
    if let Some(previous) = previous {
        let alpha = 2.0 / (length as f64 + 1.0);
        Some(alpha * source + (1.0 - alpha) * previous)
    } else {
        *seed_samples += 1;
        *seed_sum += source;
        (*seed_samples as usize == length).then(|| *seed_sum / length as f64)
    }
}

fn update_macd(state: &mut MacdState, close: f64) -> Signals {
    state.samples = state.samples.saturating_add(1);
    state.fast = ema_step(
        close,
        EMA_FAST,
        &mut state.fast_seed_samples,
        &mut state.fast_seed_sum,
        state.fast,
    );
    state.slow = ema_step(
        close,
        EMA_SLOW,
        &mut state.slow_seed_samples,
        &mut state.slow_seed_sum,
        state.slow,
    );
    let macd = state.fast.zip(state.slow).map(|(fast, slow)| fast - slow);
    state.signal = macd.and_then(|value| {
        ema_step(
            value,
            MACD_SIGNAL,
            &mut state.signal_seed_samples,
            &mut state.signal_seed_sum,
            state.signal,
        )
    });
    let histogram = macd.zip(state.signal).map(|(macd, signal)| macd - signal);
    let (buy, sell) = level_pair_signals(macd, state.signal);
    plots(
        buy,
        sell,
        [
            ("macd", macd),
            ("signal", state.signal),
            ("histogram", histogram),
        ],
    )
}

fn update_bollinger(state: &mut BollingerState, close: f64) -> Signals {
    state.samples = state.samples.saturating_add(1);
    if state.closes.len() == BB_LENGTH {
        let outgoing = state.closes.pop_front().expect("full BB window");
        state.nonzero_count -= u8::from(outgoing != 0.0);
        state.sum -= outgoing;
        let recover_tail =
            lost_tail_needs_recovery(&state.closes, state.nonzero_count, state.sum, outgoing);
        // `ta.bb` uses the ordinary rolling-window push path. It repairs a
        // qualified tail immediately after eviction, before appending this
        // bar's close; it does not use the `push_for_bar` same-sign shortcut.
        if recover_tail && let Some(sum) = reconstructed_sum(&state.closes, true) {
            state.sum = sum;
        }
        state.closes.push_back(close);
        state.nonzero_count += u8::from(close != 0.0);
        state.sum += close;
    } else {
        state.closes.push_back(close);
        state.nonzero_count += u8::from(close != 0.0);
        state.sum += close;
    }
    if state.nonzero_count == 0 {
        state.sum = 0.0;
    }
    if !state.sum.is_finite()
        && let Some(sum) = reconstructed_sum(&state.closes, false)
    {
        state.sum = sum;
    }

    let (mid, upper, lower) = if state.closes.len() == BB_LENGTH {
        let mid = state.sum / BB_LENGTH as f64;
        let squared_diff_sum = state
            .closes
            .iter()
            .map(|value| {
                let diff = *value - mid;
                diff * diff
            })
            .sum::<f64>();
        let variance = (squared_diff_sum / BB_LENGTH as f64).max(0.0);
        let deviation = 2.0 * variance.sqrt();
        (Some(mid), Some(mid + deviation), Some(mid - deviation))
    } else {
        (None, None, None)
    };
    let buy = matches!(lower, Some(lower) if pine_lt(close, lower));
    let sell = matches!(mid, Some(mid) if pine_gt(close, mid));
    plots(
        buy,
        sell,
        [("mid", mid), ("upper", upper), ("lower", lower)],
    )
}

/// Keep in step with the pinned rolling window's O(1) eligibility check before
/// its more careful scaled reconstruction. Scanning happens only on this rare
/// cancellation path, so the steady-state BB update remains incremental.
fn lost_tail_needs_recovery(
    values: &VecDeque<f64>,
    nonzero_count: u8,
    sum: f64,
    outgoing: f64,
) -> bool {
    if sum != 0.0 || nonzero_count == 0 || !outgoing.is_finite() {
        return false;
    }
    let small_limit = outgoing.abs() * (1.0 / 9_007_199_254_740_992.0);
    let is_small = |sample: Option<&f64>| {
        sample.is_some_and(|value| value.is_finite() && *value != 0.0 && value.abs() <= small_limit)
    };
    is_small(values.front()) || is_small(values.back())
}

/// Reconstruct a bounded rolling sum using the same power-of-two scaling and
/// two-component accumulation as pine-compat 0.3.1. `require_exact` is true
/// for lost-tail repair and false when recovering a non-finite running sum.
fn reconstructed_sum(values: &VecDeque<f64>, require_exact: bool) -> Option<f64> {
    if let Some(value) = values
        .front()
        .copied()
        .filter(|value| values.iter().all(|sample| *sample == *value))
        && constant_sum_overflows(value, values.len())
    {
        return None;
    }

    let mut maximum = 0.0_f64;
    for &value in values {
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
    for &value in values {
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
    let (rounded, error) = two_sum(total, correction);
    if require_exact && error != 0.0 {
        return None;
    }
    let sum = rounded * scale;
    (sum.is_finite() && (sum / scale).to_bits() == rounded.to_bits()).then_some(sum)
}

fn constant_sum_overflows(value: f64, count: usize) -> bool {
    if count == 0 || !value.is_finite() {
        return false;
    }
    let lower_count = 1_usize << count.ilog2();
    value.abs() > f64::MAX / lower_count as f64
}

fn two_sum(left: f64, right: f64) -> (f64, f64) {
    let sum = left + right;
    let virtual_right = sum - left;
    let error = (left - (sum - virtual_right)) + (right - virtual_right);
    (sum, error)
}

fn update_roc(state: &mut RocState, close: f64) -> Signals {
    state.samples = state.samples.saturating_add(1);
    if state.closes.len() == ROC_LENGTH + 1 {
        state.closes.pop_front();
    }
    state.closes.push_back(close);
    let roc = if state.closes.len() == ROC_LENGTH + 1 {
        let previous = state.closes.front().copied().expect("ROC lag sample");
        (previous != 0.0).then(|| 100.0 * (close - previous) / previous)
    } else {
        None
    };
    let roc = roc.filter(|value| value.is_finite());
    let buy = matches!(roc, Some(roc) if pine_gt(roc, 2.0));
    let sell = matches!(roc, Some(roc) if pine_lt(roc, 0.0));
    plots(buy, sell, [("roc", roc)])
}

fn plots<const N: usize>(
    buy: bool,
    sell: bool,
    values: [(&'static str, Option<f64>); N],
) -> Signals {
    Signals {
        buy,
        sell,
        plots: values
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value.filter(|value| value.is_finite())))
            .collect::<BTreeMap<_, _>>(),
    }
}

fn level_pair_signals(left: Option<f64>, right: Option<f64>) -> (bool, bool) {
    match (left, right) {
        (Some(left), Some(right)) => (pine_gt(left, right), pine_lt(left, right)),
        _ => (false, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bar(close: f64) -> BarInput {
        BarInput {
            high: close,
            low: close,
            close,
        }
    }

    fn run(kind: Kind, closes: &[f64]) -> Vec<Signals> {
        let mut state = new(kind);
        closes
            .iter()
            .map(|close| update(&mut state, kind, bar(*close)))
            .collect()
    }

    #[test]
    fn ema_matches_pinned_sma_seed_and_recursive_values() {
        let closes: Vec<_> = (1..=27).map(f64::from).collect();
        let outputs = run(Kind::EmaCross, &closes);
        assert_eq!(outputs[10].plots["fast"], None);
        assert_eq!(outputs[11].plots["fast"], Some(6.5));
        assert_eq!(outputs[24].plots["slow"], None);
        assert_eq!(outputs[25].plots["slow"], Some(13.5));
        let fast_26 = outputs[25].plots["fast"].unwrap();
        let slow_26 = outputs[25].plots["slow"].unwrap();
        assert!((fast_26 - 20.5).abs() < 1e-12);
        assert!((slow_26 - 13.5).abs() < 1e-12);
        assert!(outputs[25].buy);
        assert!(!outputs[25].sell);
    }

    #[test]
    fn macd_waits_for_all_three_pine_ema_warmups() {
        let outputs = run(Kind::Macd, &(1..=35).map(f64::from).collect::<Vec<_>>());
        assert_eq!(outputs[24].plots["macd"], None);
        assert_eq!(outputs[25].plots["macd"], Some(7.0));
        assert_eq!(outputs[32].plots["signal"], None);
        assert_eq!(outputs[33].plots["signal"], Some(7.0));
        assert_eq!(outputs[33].plots["histogram"], Some(0.0));
        assert!(!outputs[33].buy && !outputs[33].sell);
    }

    #[test]
    fn macd_exit_uses_pines_numeric_equality_boundary() {
        // Captured from the 512-bar large-price Pine oracle: the raw f64 values
        // are ordered, but pine-ir treats their sub-1e-10 difference as equal.
        let macd = -0.000_122_070_312_5;
        let signal = -0.000_122_070_312_499_999_97;
        assert!(macd < signal);
        assert_eq!(level_pair_signals(Some(macd), Some(signal)), (false, false));
        assert_eq!(
            level_pair_signals(Some(2.0 + 1e-11), Some(2.0)),
            (false, false)
        );
        assert_eq!(level_pair_signals(Some(-1e-11), Some(0.0)), (false, false));
    }

    #[test]
    fn bollinger_uses_a_twenty_sample_population_window() {
        let outputs = run(
            Kind::BollingerReversion,
            &(1..=21).map(f64::from).collect::<Vec<_>>(),
        );
        assert_eq!(outputs[18].plots["mid"], None);
        assert_eq!(outputs[19].plots["mid"], Some(10.5));
        assert!(
            (outputs[19].plots["upper"].unwrap() - (10.5 + 2.0 * 33.25_f64.sqrt())).abs() < 1e-12
        );
        assert!(
            (outputs[19].plots["lower"].unwrap() - (10.5 - 2.0 * 33.25_f64.sqrt())).abs() < 1e-12
        );
        assert_eq!(outputs[20].plots["mid"], Some(11.5));
        // The frozen exit predicate is close > mid even with no held entry.
        assert!(!outputs[19].buy && outputs[19].sell);
    }

    #[test]
    fn bollinger_recovers_small_tail_after_evicting_a_dominant_sample() {
        // At 2^41, the ULP is 2^-11. Each 2^-13 sample is smaller than half
        // that spacing, so nineteen adds disappear from the running sum.
        // Repair reconstructs those nineteen tail samples after eviction, then
        // the ordinary push adds the twentieth sample.
        let tiny = 1.0 / 8_192.0;
        let mut closes = vec![2_199_023_255_552.0];
        closes.extend(std::iter::repeat_n(tiny, BB_LENGTH));
        let outputs = run(Kind::BollingerReversion, &closes);
        assert_eq!(
            outputs[BB_LENGTH - 1].plots["mid"],
            Some(2_199_023_255_552.0 / BB_LENGTH as f64)
        );
        assert_eq!(outputs[BB_LENGTH].plots["mid"], Some(tiny));
        assert_eq!(outputs[BB_LENGTH].plots["upper"], Some(tiny));
        assert_eq!(outputs[BB_LENGTH].plots["lower"], Some(tiny));
    }

    #[test]
    fn roc_is_percent_change_from_twenty_bars_ago_with_strict_levels() {
        let mut closes = vec![100.0; 20];
        closes.push(102.0);
        let outputs = run(Kind::Roc, &closes);
        assert_eq!(outputs[19].plots["roc"], None);
        assert_eq!(outputs[20].plots["roc"], Some(2.0));
        assert!(!outputs[20].buy); // Pine reference uses > 2, not >= 2.

        closes[20] = 103.0;
        assert!(run(Kind::Roc, &closes)[20].buy);
    }

    #[test]
    fn snapshots_round_trip_and_validation_rejects_wrong_kind_counts_and_corruption() {
        let kind = Kind::BollingerReversion;
        let mut state = new(kind);
        for close in [1.25, 2.5, 3.75, 4.125] {
            update(&mut state, kind, bar(close));
        }
        let encoded = serde_json::to_vec(&state).unwrap();
        let restored: State = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(restored, state);
        assert!(valid(&restored, kind, 4));
        assert!(!valid(&restored, kind, 3));
        assert!(!valid(&restored, Kind::Roc, 4));

        let mut malformed = serde_json::to_value(&state).unwrap();
        malformed["state"]["closes"] = json!([1.0, 2.0]);
        let malformed: State = serde_json::from_value(malformed).unwrap();
        assert!(!valid(&malformed, kind, 4));

        let wrong_kind: State = serde_json::from_value(json!({
            "kind": "Roc",
            "state": {"samples": 4, "closes": [1.25, 2.5, 3.75, 4.125]}
        }))
        .unwrap();
        assert!(!valid(&wrong_kind, kind, 4));
    }
}
