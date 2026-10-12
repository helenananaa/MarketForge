use pine_ir::{CallSiteId, HirCallArg, HirExpr};

use crate::builtins::args::RuntimeArgs;
use crate::*;
mod averages;
mod extremes;
mod flow;
mod momentum;
#[path = "ta_opcode.rs"]
mod opcode;
mod pivots;
mod statistics;
mod trend;

pub(crate) use opcode::TaOpcode;

#[cfg(test)]
#[path = "ta_args_tests.rs"]
mod args_tests;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RsiState {
    previous_source: f64,
    average_gain: Option<f64>,
    average_loss: Option<f64>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct MacdState {
    last_bar: Option<usize>,
    base: [Option<f64>; 3],
    fast_ema: Option<f64>,
    slow_ema: Option<f64>,
    signal_ema: Option<f64>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct VwapState {
    weighted_sum: f64,
    weighted_square_sum: f64,
    volume_sum: f64,
    started: bool,
    default_anchor_bucket: Option<i64>,
}

impl VwapState {
    pub(crate) fn start(&mut self) {
        *self = Self {
            started: true,
            ..Self::default()
        };
    }

    pub(crate) fn start_default_anchor_bucket(&mut self, bucket: i64) {
        self.start();
        self.default_anchor_bucket = Some(bucket);
    }

    pub(crate) fn default_anchor_bucket(self) -> Option<i64> {
        self.default_anchor_bucket
    }

    pub(crate) fn has_started(self) -> bool {
        self.started
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PivotPointPeriod {
    open: f64,
    high: f64,
    low: f64,
    close: f64,
}

impl PivotPointPeriod {
    pub(crate) fn new(open: f64, high: f64, low: f64, close: f64) -> Self {
        Self {
            open,
            high,
            low,
            close,
        }
    }

    fn update(&mut self, high: f64, low: f64, close: f64) {
        self.high = self.high.max(high);
        self.low = self.low.min(low);
        self.close = close;
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct PivotPointState {
    current: Option<PivotPointPeriod>,
    active_levels: Option<Vec<PineValue>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CrossMode {
    Any,
    Over,
    Under,
}

pub(crate) fn two_na_tuple() -> PineValue {
    PineValue::Tuple(vec![PineValue::Na, PineValue::Na])
}

pub(crate) fn three_na_tuple() -> PineValue {
    PineValue::Tuple(vec![PineValue::Na, PineValue::Na, PineValue::Na])
}

pub(crate) fn vwap_result_na(has_bands: bool) -> PineValue {
    if has_bands {
        three_na_tuple()
    } else {
        PineValue::Na
    }
}

pub(crate) fn ta_arg<'a>(
    args: RuntimeArgs<'a>,
    positional: usize,
    name: &str,
) -> Option<&'a HirExpr> {
    args.expr(positional, name)
}

pub(crate) fn vwap_arg<'a>(
    args: RuntimeArgs<'a>,
    positional: usize,
    name: &str,
) -> Option<&'a HirExpr> {
    ta_arg(args, positional, name)
}

pub(crate) fn pivot_point_arg<'a>(
    args: RuntimeArgs<'a>,
    positional: usize,
    name: &str,
) -> Option<&'a HirExpr> {
    ta_arg(args, positional, name)
}

pub(crate) fn pivot_na_levels() -> Vec<PineValue> {
    vec![PineValue::Na; 11]
}

pub(crate) fn pivot_level_values(levels: [Option<f64>; 11]) -> Vec<PineValue> {
    levels
        .into_iter()
        .map(|value| value.map_or(PineValue::Na, finite_float_or_na))
        .collect()
}

pub(crate) fn pivot_point_levels(
    type_name: &str,
    period: PivotPointPeriod,
    current_open: f64,
) -> Vec<PineValue> {
    let high = period.high;
    let low = period.low;
    let close = period.close;
    let range = high - low;
    match type_name {
        "Traditional" => {
            let p = (high + low + close) / 3.0;
            pivot_level_values([
                Some(p),
                Some(2.0 * p - low),
                Some(2.0 * p - high),
                Some(p + range),
                Some(p - range),
                Some(2.0 * p + high - 2.0 * low),
                Some(2.0 * p - (2.0 * high - low)),
                Some(3.0 * p + high - 3.0 * low),
                Some(3.0 * p - (3.0 * high - low)),
                Some(4.0 * p + high - 4.0 * low),
                Some(4.0 * p - (4.0 * high - low)),
            ])
        }
        "Fibonacci" => {
            let p = (high + low + close) / 3.0;
            pivot_level_values([
                Some(p),
                Some(p + 0.382 * range),
                Some(p - 0.382 * range),
                Some(p + 0.618 * range),
                Some(p - 0.618 * range),
                Some(p + range),
                Some(p - range),
                None,
                None,
                None,
                None,
            ])
        }
        "Woodie" => {
            let p = (high + low + 2.0 * current_open) / 4.0;
            let r3 = high + 2.0 * (p - low);
            let s3 = low - 2.0 * (high - p);
            pivot_level_values([
                Some(p),
                Some(2.0 * p - low),
                Some(2.0 * p - high),
                Some(p + range),
                Some(p - range),
                Some(r3),
                Some(s3),
                Some(r3 + range),
                Some(s3 - range),
                None,
                None,
            ])
        }
        "Classic" => {
            let p = (high + low + close) / 3.0;
            pivot_level_values([
                Some(p),
                Some(2.0 * p - low),
                Some(2.0 * p - high),
                Some(p + range),
                Some(p - range),
                Some(p + 2.0 * range),
                Some(p - 2.0 * range),
                Some(p + 3.0 * range),
                Some(p - 3.0 * range),
                None,
                None,
            ])
        }
        "DM" => {
            let x = if period.open == close {
                high + low + 2.0 * close
            } else if close > period.open {
                2.0 * high + low + close
            } else {
                2.0 * low + high + close
            };
            pivot_level_values([
                Some(x / 4.0),
                Some(x / 2.0 - low),
                Some(x / 2.0 - high),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ])
        }
        "Camarilla" => {
            let r5 = if low == 0.0 {
                None
            } else {
                Some((high / low) * close)
            };
            let s5 = r5.map(|r5| close - (r5 - close));
            pivot_level_values([
                Some((high + low + close) / 3.0),
                Some(close + 1.1 * range / 12.0),
                Some(close - 1.1 * range / 12.0),
                Some(close + 1.1 * range / 6.0),
                Some(close - 1.1 * range / 6.0),
                Some(close + 1.1 * range / 4.0),
                Some(close - 1.1 * range / 4.0),
                Some(close + 1.1 * range / 2.0),
                Some(close - 1.1 * range / 2.0),
                r5,
                s5,
            ])
        }
        _ => pivot_na_levels(),
    }
}

pub(crate) fn supertrend_state(value: Option<&PineValue>) -> Option<(f64, f64, f64, f64)> {
    let Some(PineValue::Tuple(values)) = value else {
        return None;
    };
    let [atr, upper, lower, supertrend] = values.as_slice() else {
        return None;
    };
    Some((
        atr.as_f64()?,
        upper.as_f64()?,
        lower.as_f64()?,
        supertrend.as_f64()?,
    ))
}

pub(crate) fn kc_state(value: Option<&PineValue>) -> Option<(f64, f64)> {
    let Some(PineValue::Tuple(values)) = value else {
        return None;
    };
    let [basis, range_ema] = values.as_slice() else {
        return None;
    };
    Some((basis.as_f64()?, range_ema.as_f64()?))
}

pub(crate) fn sar_state(value: Option<&PineValue>) -> Option<(f64, f64, f64, bool)> {
    let Some(PineValue::Tuple(values)) = value else {
        return None;
    };
    let [result, max_min, acceleration, is_below] = values.as_slice() else {
        return None;
    };
    let PineValue::Bool(is_below) = is_below else {
        return None;
    };
    Some((
        result.as_f64()?,
        max_min.as_f64()?,
        acceleration.as_f64()?,
        *is_below,
    ))
}

pub(crate) fn tsi_state(value: Option<&PineValue>) -> Option<(f64, f64, f64, f64)> {
    let Some(PineValue::Tuple(values)) = value else {
        return None;
    };
    let [
        short_momentum,
        long_momentum,
        short_abs_momentum,
        long_abs_momentum,
    ] = values.as_slice()
    else {
        return None;
    };
    Some((
        short_momentum.as_f64()?,
        long_momentum.as_f64()?,
        short_abs_momentum.as_f64()?,
        long_abs_momentum.as_f64()?,
    ))
}

pub(crate) fn ema_next(previous: Option<f64>, source: f64, length: i64) -> f64 {
    let alpha = 2.0 / (length as f64 + 1.0);
    match previous {
        Some(previous) => alpha * source + (1.0 - alpha) * previous,
        None => source,
    }
}

pub(crate) fn ema_chain_state(
    value: Option<&PineValue>,
) -> (Option<f64>, Option<f64>, Option<f64>) {
    let Some(PineValue::Tuple(values)) = value else {
        return (None, None, None);
    };
    (
        values.first().and_then(PineValue::as_f64),
        values.get(1).and_then(PineValue::as_f64),
        values.get(2).and_then(PineValue::as_f64),
    )
}

pub(crate) fn rsi_from_averages(average_gain: f64, average_loss: f64) -> f64 {
    if average_loss == 0.0 {
        100.0
    } else if average_gain == 0.0 {
        0.0
    } else {
        100.0 - (100.0 / (1.0 + average_gain / average_loss))
    }
}

impl<'a> HistoricalRuntime<'a> {
    pub(crate) fn wilder_rma(
        &mut self,
        call_site_id: CallSiteId,
        channel: u8,
        previous: Option<f64>,
        source: Option<f64>,
        length: i64,
    ) -> Option<f64> {
        if length <= 0 {
            return None;
        }
        let source = source?;
        let length_us = usize::try_from(length).ok()?;
        let mean = {
            let window = self.update_rolling_window_key(
                RollingWindowKey::Rma {
                    call_site: call_site_id,
                    channel,
                },
                Some(source),
                length_us,
            );
            if !window.is_ready(length_us) {
                return None;
            }
            window.mean(length_us)
        };
        Some(match previous {
            Some(previous) => (previous * (length - 1) as f64 + source) / length as f64,
            None => mean,
        })
    }

    pub(crate) fn eval_ta_call(
        &mut self,
        opcode: Option<TaOpcode>,
        call_site_id: CallSiteId,
        raw_args: &[HirCallArg],
        positional_args: bool,
    ) -> Option<Result<PineValue, RuntimeError>> {
        let opcode = opcode?;
        let args = RuntimeArgs::new(raw_args, positional_args);
        Some(match opcode {
            TaOpcode::Sma => self.eval_sma(call_site_id, args),
            TaOpcode::Ema => self.eval_ema(call_site_id, args),
            TaOpcode::Dema => self.eval_dema(call_site_id, args),
            TaOpcode::Tema => self.eval_tema(call_site_id, args),
            TaOpcode::Rma => self.eval_rma(call_site_id, args),
            TaOpcode::Rsi => self.eval_rsi(call_site_id, args),
            TaOpcode::Rci => self.eval_rci(call_site_id, args),
            TaOpcode::Macd => self.eval_macd(call_site_id, args),
            TaOpcode::Tsi => self.eval_tsi(call_site_id, args),
            TaOpcode::Cmo => self.eval_cmo(call_site_id, args),
            TaOpcode::Cci => self.eval_cci(call_site_id, args),
            TaOpcode::Cog => self.eval_cog(call_site_id, args),
            TaOpcode::Ao => self.eval_ao(call_site_id),
            TaOpcode::Bop => self.eval_bop(),
            TaOpcode::Bb => self.eval_bb(call_site_id, args),
            TaOpcode::Bbw => self.eval_bbw(call_site_id, args),
            TaOpcode::Kc => self.eval_kc(call_site_id, args),
            TaOpcode::Kcw => self.eval_kcw(call_site_id, args),
            TaOpcode::PivotHigh => self.eval_pivot(call_site_id, args, WindowExtreme::Highest),
            TaOpcode::PivotLow => self.eval_pivot(call_site_id, args, WindowExtreme::Lowest),
            TaOpcode::PivotPointLevels => self.eval_pivot_point_levels(call_site_id, args),
            TaOpcode::Cum => self.eval_cum(call_site_id, args),
            TaOpcode::Max => self.eval_all_time_extreme(call_site_id, args, WindowExtreme::Highest),
            TaOpcode::Min => self.eval_all_time_extreme(call_site_id, args, WindowExtreme::Lowest),
            TaOpcode::Stdev => self.eval_stdev(call_site_id, args),
            TaOpcode::Variance => self.eval_variance(call_site_id, args),
            TaOpcode::Range => self.eval_range(call_site_id, args),
            TaOpcode::Dev => self.eval_dev(call_site_id, args),
            TaOpcode::Vwap => self.eval_vwap_source(call_site_id, args),
            TaOpcode::Vwma => self.eval_vwma(call_site_id, args),
            TaOpcode::Mfi => self.eval_mfi(call_site_id, args),
            TaOpcode::Wma => self.eval_wma(call_site_id, args),
            TaOpcode::Hma => self.eval_hma(call_site_id, args),
            TaOpcode::Swma => self.eval_swma(call_site_id, args),
            TaOpcode::Alma => self.eval_alma(call_site_id, args),
            TaOpcode::Linreg => self.eval_linreg(call_site_id, args),
            TaOpcode::Stoch => self.eval_stoch(call_site_id, args),
            TaOpcode::Wpr => self.eval_wpr(call_site_id, args),
            TaOpcode::Correlation => self.eval_correlation(call_site_id, args),
            TaOpcode::Covariance => self.eval_covariance(call_site_id, args),
            TaOpcode::Median => self.eval_median(call_site_id, args),
            TaOpcode::Mode => self.eval_mode(call_site_id, args),
            TaOpcode::PercentileNearestRank => {
                self.eval_percentile(call_site_id, args, ArrayPercentileMode::NearestRank)
            }
            TaOpcode::PercentileLinearInterpolation => {
                self.eval_percentile(call_site_id, args, ArrayPercentileMode::LinearInterpolation)
            }
            TaOpcode::Percentrank => self.eval_percentrank(call_site_id, args),
            TaOpcode::Tr => self.eval_tr(args),
            TaOpcode::Atr => self.eval_atr(call_site_id, args),
            TaOpcode::Supertrend => self.eval_supertrend(call_site_id, args),
            TaOpcode::Dmi => self.eval_dmi(call_site_id, args),
            TaOpcode::Sar => self.eval_sar(call_site_id, args),
            TaOpcode::Change => self.eval_change(args),
            TaOpcode::Mom => self.eval_mom(args),
            TaOpcode::Roc => self.eval_roc(args),
            TaOpcode::Rising => {
                self.eval_rising_falling(call_site_id, args, RisingFallingMode::Rising)
            }
            TaOpcode::Falling => {
                self.eval_rising_falling(call_site_id, args, RisingFallingMode::Falling)
            }
            TaOpcode::BarsSince => self.eval_barssince(call_site_id, args),
            TaOpcode::ValueWhen => self.eval_valuewhen(call_site_id, args),
            TaOpcode::Cross => self.eval_cross(call_site_id, args, CrossMode::Any),
            TaOpcode::Crossover => self.eval_cross(call_site_id, args, CrossMode::Over),
            TaOpcode::Crossunder => self.eval_cross(call_site_id, args, CrossMode::Under),
            TaOpcode::Highest => {
                self.eval_window_extreme(call_site_id, args, WindowExtreme::Highest)
            }
            TaOpcode::Lowest => self.eval_window_extreme(call_site_id, args, WindowExtreme::Lowest),
            TaOpcode::HighestBars => {
                self.eval_window_extreme_offset(call_site_id, args, WindowExtreme::Highest)
            }
            TaOpcode::LowestBars => {
                self.eval_window_extreme_offset(call_site_id, args, WindowExtreme::Lowest)
            }
        })
    }
}

impl<'a> HistoricalRuntime<'a> {}
