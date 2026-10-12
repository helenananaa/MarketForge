use crate::runtime::historical::CrossCallState;

use super::*;

pub(super) fn push_rolling_window(
    windows: &mut std::collections::HashMap<RollingWindowKey, RollingWindowState>,
    key: RollingWindowKey,
    source: Option<f64>,
    length: usize,
) -> &RollingWindowState {
    let window = windows.entry(key).or_default();
    window.push(source.filter(|value| value.is_finite()), length);
    window
}

impl<'a> HistoricalRuntime<'a> {
    pub(crate) fn eval_cum(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let source = self.eval_flow_source(args)?;
        let Some(source) = source.as_f64() else {
            self.ta_state.call_state.insert(call_site_id, PineValue::Na);
            return Ok(PineValue::Na);
        };

        let value = self
            .ta_state
            .call_state
            .get(&call_site_id)
            .and_then(PineValue::as_f64)
            .unwrap_or(0.0)
            + source;
        let value = PineValue::Float(value);
        self.ta_state.call_state.insert(call_site_id, value.clone());
        Ok(value)
    }

    pub(crate) fn eval_all_time_extreme(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
        mode: WindowExtreme,
    ) -> Result<PineValue, RuntimeError> {
        let source = self.eval_flow_source(args)?;
        let Some(source) = source.as_f64() else {
            return Ok(self
                .ta_state
                .call_state
                .get(&call_site_id)
                .cloned()
                .unwrap_or(PineValue::Na));
        };

        let value = match self
            .ta_state
            .call_state
            .get(&call_site_id)
            .and_then(PineValue::as_f64)
        {
            Some(previous) => match mode {
                WindowExtreme::Highest => previous.max(source),
                WindowExtreme::Lowest => previous.min(source),
            },
            None => source,
        };
        let value = finite_float_or_na(value);
        self.ta_state.call_state.insert(call_site_id, value.clone());
        Ok(value)
    }

    pub(crate) fn eval_cci(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let (source, length) = self.eval_flow_source_length(args)?;
        if length <= 0 {
            return Ok(PineValue::Na);
        }

        let Some(length) = usize::try_from(length).ok() else {
            return Ok(PineValue::Na);
        };
        let Some(current) = source.as_f64() else {
            self.update_rolling_window(call_site_id, source, length);
            return Ok(PineValue::Na);
        };

        let window = self.update_rolling_window(call_site_id, PineValue::Float(current), length);
        if !window.is_ready(length) {
            return Ok(PineValue::Na);
        }

        let deviation = window.mean_absolute_deviation(length);
        if deviation == 0.0 {
            return Ok(PineValue::Na);
        }

        Ok(finite_float_or_na(
            (current - window.mean(length)) / (0.015 * deviation),
        ))
    }

    pub(crate) fn eval_cog(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let (source, length) = self.eval_flow_source_length(args)?;
        if length <= 0 {
            return Ok(PineValue::Na);
        }

        let Some(length) = usize::try_from(length).ok() else {
            return Ok(PineValue::Na);
        };
        let window = self.update_rolling_window(call_site_id, source, length);
        if !window.is_ready(length) || window.sum == 0.0 {
            return Ok(PineValue::Na);
        }

        Ok(finite_float_or_na(window.center_of_gravity(length)))
    }

    pub(crate) fn eval_vwma(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let (source, length) = self.eval_flow_source_length(args)?;
        if length <= 0 {
            return Ok(PineValue::Na);
        }

        let Some(length) = usize::try_from(length).ok() else {
            return Ok(PineValue::Na);
        };
        let Some(source) = source.as_f64() else {
            self.update_rolling_window_key(
                RollingWindowKey::VwmaWeighted(call_site_id),
                None,
                length,
            );
            self.update_rolling_window_key(
                RollingWindowKey::VwmaVolume(call_site_id),
                None,
                length,
            );
            return Ok(PineValue::Na);
        };
        let Some(volume) = self.current_builtin_f64("volume") else {
            self.update_rolling_window_key(
                RollingWindowKey::VwmaWeighted(call_site_id),
                None,
                length,
            );
            self.update_rolling_window_key(
                RollingWindowKey::VwmaVolume(call_site_id),
                None,
                length,
            );
            return Ok(PineValue::Na);
        };

        self.update_rolling_window_key(
            RollingWindowKey::VwmaWeighted(call_site_id),
            Some(source * volume),
            length,
        );
        self.update_rolling_window_key(
            RollingWindowKey::VwmaVolume(call_site_id),
            Some(volume),
            length,
        );

        let weighted = self
            .ta_state
            .rolling_windows
            .get(&RollingWindowKey::VwmaWeighted(call_site_id));
        let volumes = self
            .ta_state
            .rolling_windows
            .get(&RollingWindowKey::VwmaVolume(call_site_id));
        let (Some(weighted), Some(volumes)) = (weighted, volumes) else {
            return Ok(PineValue::Na);
        };
        if !weighted.is_ready(length) || !volumes.is_ready(length) || volumes.sum == 0.0 {
            return Ok(PineValue::Na);
        }

        Ok(finite_float_or_na(weighted.sum / volumes.sum))
    }

    fn eval_flow_source(&mut self, args: RuntimeArgs<'_>) -> Result<PineValue, RuntimeError> {
        ta_arg(args, 0, "source")
            .map(|arg| self.eval_expr(arg))
            .transpose()
            .map(|value| value.unwrap_or(PineValue::Na))
    }

    fn eval_flow_source_length(
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

    pub(crate) fn eval_mfi(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let source_arg = ta_arg(args, 0, "source");
        let source = source_arg
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .unwrap_or(PineValue::Na);
        let length = ta_arg(args, 1, "length")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        if length <= 0 {
            return Ok(PineValue::Na);
        }

        let Some(length) = usize::try_from(length).ok() else {
            return Ok(PineValue::Na);
        };
        let Some(source) = source.as_f64() else {
            self.update_mfi_windows(call_site_id, None, None, length);
            return Ok(PineValue::Na);
        };
        let Some(volume) = self.current_builtin_f64("volume") else {
            self.update_mfi_windows(call_site_id, None, None, length);
            return Ok(PineValue::Na);
        };
        let Some(series_id) = source_arg.and_then(|arg| arg.series_id) else {
            self.update_mfi_windows(call_site_id, None, None, length);
            return Ok(PineValue::Na);
        };

        let (positive_flow, negative_flow) =
            match self.read_declared_series_history(series_id, 1).as_f64() {
                Some(previous) if source > previous => (Some(source * volume), Some(0.0)),
                Some(previous) if source < previous => (Some(0.0), Some(source * volume)),
                Some(_) => (Some(0.0), Some(0.0)),
                None => return Ok(PineValue::Na),
            };
        self.update_mfi_windows(call_site_id, positive_flow, negative_flow, length);

        let positive_window = self
            .ta_state
            .rolling_windows
            .get(&RollingWindowKey::MfiPositive(call_site_id));
        let negative_window = self
            .ta_state
            .rolling_windows
            .get(&RollingWindowKey::MfiNegative(call_site_id));
        let (Some(positive_window), Some(negative_window)) = (positive_window, negative_window)
        else {
            return Ok(PineValue::Na);
        };
        if !positive_window.is_ready(length) || !negative_window.is_ready(length) {
            return Ok(PineValue::Na);
        }

        let positive_sum = positive_window.sum;
        let negative_sum = negative_window.sum;
        if positive_sum == 0.0 && negative_sum == 0.0 {
            return Ok(PineValue::Na);
        }
        if negative_sum == 0.0 {
            return Ok(PineValue::Float(100.0));
        }

        Ok(finite_float_or_na(
            100.0 - 100.0 / (1.0 + positive_sum / negative_sum),
        ))
    }

    pub(crate) fn eval_vwap_source(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let has_bands = vwap_arg(args, 2, "stdev_mult").is_some();
        let source_arg = vwap_arg(args, 0, "source").ok_or_else(|| RuntimeError {
            message: "ta.vwap missing source argument".to_owned(),
        })?;
        let source = self.eval_expr(source_arg)?;
        let (anchor, default_anchor_bucket) = if let Some(arg) = vwap_arg(args, 1, "anchor") {
            (matches!(self.eval_expr(arg)?, PineValue::Bool(true)), None)
        } else {
            (
                false,
                self.current_bar.map(|bar| bar.time.div_euclid(86_400_000)),
            )
        };
        let stdev_mult = if let Some(arg) = vwap_arg(args, 2, "stdev_mult") {
            self.eval_expr(arg)?.as_f64()
        } else {
            None
        };
        let source = source.as_f64();
        let volume = self.current_builtin_f64("volume");

        let state = self
            .ta_state
            .vwap_call_state
            .entry(call_site_id)
            .or_default();
        if let Some(bucket) = default_anchor_bucket {
            if state.default_anchor_bucket() != Some(bucket) {
                state.start_default_anchor_bucket(bucket);
            }
        } else if anchor {
            state.start();
        } else if !state.has_started() {
            return Ok(vwap_result_na(has_bands));
        }

        let (Some(source), Some(volume)) = (source, volume) else {
            if let Some(bucket) = default_anchor_bucket {
                state.start_default_anchor_bucket(bucket);
            } else {
                state.start();
            }
            return Ok(vwap_result_na(has_bands));
        };
        let weighted = source * volume;
        let weighted_square = source * source * volume;
        if !source.is_finite()
            || !volume.is_finite()
            || !weighted.is_finite()
            || !weighted_square.is_finite()
        {
            if let Some(bucket) = default_anchor_bucket {
                state.start_default_anchor_bucket(bucket);
            } else {
                state.start();
            }
            return Ok(vwap_result_na(has_bands));
        }
        state.weighted_sum += weighted;
        state.weighted_square_sum += weighted_square;
        state.volume_sum += volume;
        if state.volume_sum == 0.0
            || !state.weighted_sum.is_finite()
            || !state.weighted_square_sum.is_finite()
            || !state.volume_sum.is_finite()
        {
            return Ok(vwap_result_na(has_bands));
        }

        let vwap = state.weighted_sum / state.volume_sum;
        let value = finite_float_or_na(vwap);
        if !has_bands {
            return Ok(value);
        }
        let Some(mult) = stdev_mult.filter(|mult| mult.is_finite()) else {
            return Ok(vwap_result_na(true));
        };
        let variance = (state.weighted_square_sum / state.volume_sum) - vwap * vwap;
        let deviation = variance.max(0.0).sqrt();
        let band = deviation * mult;
        Ok(PineValue::Tuple(vec![
            value,
            finite_float_or_na(vwap + band),
            finite_float_or_na(vwap - band),
        ]))
    }

    pub(crate) fn eval_stoch(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let source = ta_arg(args, 0, "source")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_f64());
        let high = ta_arg(args, 1, "high")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_f64());
        let low = ta_arg(args, 2, "low")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_f64());
        let length = ta_arg(args, 3, "length")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        if length <= 0 {
            return Ok(PineValue::Na);
        }

        let Some(length) = usize::try_from(length).ok() else {
            return Ok(PineValue::Na);
        };
        self.update_rolling_window_key(RollingWindowKey::StochHigh(call_site_id), high, length);
        self.update_rolling_window_key(RollingWindowKey::StochLow(call_site_id), low, length);

        let high_window = self
            .ta_state
            .rolling_windows
            .get(&RollingWindowKey::StochHigh(call_site_id));
        let low_window = self
            .ta_state
            .rolling_windows
            .get(&RollingWindowKey::StochLow(call_site_id));
        let (Some(source), Some(high_window), Some(low_window)) = (source, high_window, low_window)
        else {
            return Ok(PineValue::Na);
        };
        // Stochastic extrema use the available non-na samples in a full bar
        // window. Requiring every sample to be non-na delays RSI-backed
        // stochastic plots by an extra `length` bars after RSI warms up.
        if high_window.values.len() != length || low_window.values.len() != length {
            return Ok(PineValue::Na);
        }

        let (Some(highest_high), Some(lowest_low)) = (
            high_window.extreme(WindowExtreme::Highest),
            low_window.extreme(WindowExtreme::Lowest),
        ) else {
            return Ok(PineValue::Na);
        };
        let range = highest_high - lowest_low;
        if range == 0.0 {
            return Ok(PineValue::Na);
        }

        Ok(finite_float_or_na(100.0 * (source - lowest_low) / range))
    }

    pub(crate) fn eval_wpr(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let length = ta_arg(args, 0, "length")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        if length <= 0 {
            return Ok(PineValue::Na);
        }

        let Some(length) = usize::try_from(length).ok() else {
            return Ok(PineValue::Na);
        };
        let close = self.current_builtin_f64("close");
        self.update_rolling_window_key(
            RollingWindowKey::WprHigh(call_site_id),
            self.current_builtin_f64("high"),
            length,
        );
        self.update_rolling_window_key(
            RollingWindowKey::WprLow(call_site_id),
            self.current_builtin_f64("low"),
            length,
        );

        let high_window = self
            .ta_state
            .rolling_windows
            .get(&RollingWindowKey::WprHigh(call_site_id));
        let low_window = self
            .ta_state
            .rolling_windows
            .get(&RollingWindowKey::WprLow(call_site_id));
        let (Some(close), Some(high_window), Some(low_window)) = (close, high_window, low_window)
        else {
            return Ok(PineValue::Na);
        };
        if !high_window.is_ready(length) || !low_window.is_ready(length) {
            return Ok(PineValue::Na);
        }

        let (Some(highest_high), Some(lowest_low)) = (
            high_window.extreme(WindowExtreme::Highest),
            low_window.extreme(WindowExtreme::Lowest),
        ) else {
            return Ok(PineValue::Na);
        };
        let range = highest_high - lowest_low;
        if range == 0.0 {
            return Ok(PineValue::Na);
        }

        Ok(finite_float_or_na(-100.0 * (highest_high - close) / range))
    }

    pub(crate) fn eval_ao(&mut self, call_site_id: CallSiteId) -> Result<PineValue, RuntimeError> {
        let source = match (
            self.current_builtin_f64("high"),
            self.current_builtin_f64("low"),
        ) {
            (Some(high), Some(low)) => Some((high + low) / 2.0),
            _ => None,
        };

        self.update_rolling_window_key(RollingWindowKey::AoFast(call_site_id), source, 5);
        self.update_rolling_window_key(RollingWindowKey::AoSlow(call_site_id), source, 34);

        let fast_window = self
            .ta_state
            .rolling_windows
            .get(&RollingWindowKey::AoFast(call_site_id));
        let slow_window = self
            .ta_state
            .rolling_windows
            .get(&RollingWindowKey::AoSlow(call_site_id));
        let (Some(fast_window), Some(slow_window)) = (fast_window, slow_window) else {
            return Ok(PineValue::Na);
        };
        if !fast_window.is_ready(5) || !slow_window.is_ready(34) {
            return Ok(PineValue::Na);
        }

        Ok(finite_float_or_na(
            fast_window.mean(5) - slow_window.mean(34),
        ))
    }

    pub(crate) fn eval_bop(&self) -> Result<PineValue, RuntimeError> {
        let (Some(open), Some(high), Some(low), Some(close)) = (
            self.current_builtin_f64("open"),
            self.current_builtin_f64("high"),
            self.current_builtin_f64("low"),
            self.current_builtin_f64("close"),
        ) else {
            return Ok(PineValue::Na);
        };

        let range = high - low;
        if range == 0.0 {
            return Ok(PineValue::Na);
        }

        Ok(finite_float_or_na((close - open) / range))
    }

    pub(crate) fn eval_rising_falling(
        &mut self,
        _call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
        mode: RisingFallingMode,
    ) -> Result<PineValue, RuntimeError> {
        let Some(source_arg) = ta_arg(args, 0, "source") else {
            return Ok(PineValue::Bool(false));
        };
        let source = self.eval_expr(source_arg)?;
        let length = ta_arg(args, 1, "length")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        if length <= 0 {
            return Ok(PineValue::Bool(false));
        }

        let Some(length) = usize::try_from(length).ok() else {
            return Ok(PineValue::Bool(false));
        };
        let Some(current) = source.as_f64() else {
            return Ok(PineValue::Bool(false));
        };
        let Some(series_id) = source_arg.series_id else {
            return Ok(PineValue::Bool(false));
        };
        for offset in 1..=length {
            let Some(previous) = self.series_store.read(series_id, offset).as_f64() else {
                return Ok(PineValue::Bool(false));
            };
            let trending = match mode {
                RisingFallingMode::Rising => current > previous,
                RisingFallingMode::Falling => current < previous,
            };
            if !trending {
                return Ok(PineValue::Bool(false));
            }
        }

        Ok(PineValue::Bool(true))
    }

    pub(crate) fn eval_cross(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
        mode: CrossMode,
    ) -> Result<PineValue, RuntimeError> {
        let Some(left_arg) = ta_arg(args, 0, "source1") else {
            return Ok(PineValue::Bool(false));
        };
        let Some(right_arg) = ta_arg(args, 1, "source2") else {
            return Ok(PineValue::Bool(false));
        };
        let current_left = self.eval_expr(left_arg)?;
        let current_right = self.eval_expr(right_arg)?;
        let Some(_left_series_id) = left_arg.series_id else {
            return Ok(PineValue::Bool(false));
        };
        let bar_index = self.bars;
        let state = self
            .ta_state
            .cross_state
            .entry(call_site_id)
            .or_insert_with(|| CrossCallState {
                bar_index,
                current_left: PineValue::Na,
                current_right: PineValue::Na,
                previous_left: PineValue::Na,
                previous_right: PineValue::Na,
            });
        if state.bar_index != bar_index {
            state.previous_left = state.current_left.clone();
            state.previous_right = state.current_right.clone();
            state.bar_index = bar_index;
        }
        let previous_left = state.previous_left.clone();
        let previous_right = if right_arg.series_id.is_some() {
            state.previous_right.clone()
        } else {
            current_right.clone()
        };
        state.current_left = current_left.clone();
        state.current_right = current_right.clone();

        let Some(current_left) = current_left.as_f64() else {
            return Ok(PineValue::Bool(false));
        };
        let Some(current_right) = current_right.as_f64() else {
            return Ok(PineValue::Bool(false));
        };
        let Some(previous_left) = previous_left.as_f64() else {
            return Ok(PineValue::Bool(false));
        };
        let Some(previous_right) = previous_right.as_f64() else {
            return Ok(PineValue::Bool(false));
        };

        let crossed_over = current_left > current_right && previous_left <= previous_right;
        let crossed_under = current_left < current_right && previous_left >= previous_right;
        Ok(PineValue::Bool(match mode {
            CrossMode::Any => crossed_over || crossed_under,
            CrossMode::Over => crossed_over,
            CrossMode::Under => crossed_under,
        }))
    }

    pub(crate) fn eval_barssince(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let condition = ta_arg(args, 0, "condition")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .unwrap_or(PineValue::Na);
        let value = if matches!(condition, PineValue::Bool(true)) {
            PineValue::Int(0)
        } else if let Some(previous) = self
            .ta_state
            .call_state
            .get(&call_site_id)
            .and_then(PineValue::as_i64)
        {
            PineValue::Int(previous + 1)
        } else {
            PineValue::Na
        };

        if matches!(value, PineValue::Int(_)) {
            self.ta_state.call_state.insert(call_site_id, value.clone());
        }
        Ok(value)
    }

    pub(crate) fn eval_valuewhen(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let condition = ta_arg(args, 0, "condition")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .unwrap_or(PineValue::Na);
        let source = ta_arg(args, 1, "source")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .unwrap_or(PineValue::Na);
        let occurrence_arg = ta_arg(args, 2, "occurrence");
        let occurrence = occurrence_arg
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_i64())
            .and_then(|value| usize::try_from(value).ok())
            .filter(|&value| value < MAX_SERIES_HISTORY_VALUES);

        if matches!(condition, PineValue::Bool(true)) {
            let retain = if occurrence_arg
                .is_some_and(|arg| arg.pine_type.qualifier == pine_ir::Qualifier::Series)
            {
                MAX_SERIES_HISTORY_VALUES
            } else {
                occurrence.map_or(0, |value| value + 1)
            };

            let entry = self.ta_state.valuewhen_state.entry(call_site_id);
            let previous = match &entry {
                std::collections::hash_map::Entry::Occupied(values) => values.get().len(),
                std::collections::hash_map::Entry::Vacant(_) => 0,
            };
            let replacement = previous.saturating_add(1).min(retain);
            self.valuewhen_budget
                .replace_local_values(previous, replacement)?;
            entry.or_default().push_retained(source, retain);
        }

        let Some(occurrence) = occurrence else {
            return Ok(PineValue::Na);
        };

        Ok(self
            .ta_state
            .valuewhen_state
            .get(&call_site_id)
            .and_then(|values| values.get(occurrence))
            .cloned()
            .unwrap_or(PineValue::Na))
    }

    pub(crate) fn update_rolling_window(
        &mut self,
        call_site_id: CallSiteId,
        source: PineValue,
        length: usize,
    ) -> &RollingWindowState {
        let source = source.as_f64();
        self.update_rolling_window_key(RollingWindowKey::Single(call_site_id), source, length)
    }

    pub(crate) fn update_sum_window(
        &mut self,
        call_site_id: CallSiteId,
        source: PineValue,
        length: usize,
    ) -> &RollingWindowState {
        let window = self
            .ta_state
            .rolling_windows
            .entry(RollingWindowKey::MathSum(call_site_id))
            .or_default();
        if let Some(value) = source.as_f64().filter(|value| value.is_finite()) {
            window.push_for_bar(Some(value), length, self.bars);
        } else {
            window.discard_for_bar(self.bars);
        }
        window
    }

    // SMA/EMA consume one final input per executed bar. Other algorithms keep
    // their existing update path until their own repeated-call contract is qualified.
    pub(crate) fn update_rolling_window_for_bar(
        &mut self,
        call_site_id: CallSiteId,
        source: PineValue,
        length: usize,
    ) -> &RollingWindowState {
        let window = self
            .ta_state
            .rolling_windows
            .entry(RollingWindowKey::Single(call_site_id))
            .or_default();
        window.push_for_bar(
            source.as_f64().filter(|value| value.is_finite()),
            length,
            self.bars,
        );
        window
    }

    pub(crate) fn update_mfi_windows(
        &mut self,
        call_site_id: CallSiteId,
        positive_flow: Option<f64>,
        negative_flow: Option<f64>,
        length: usize,
    ) {
        self.update_rolling_window_key(
            RollingWindowKey::MfiPositive(call_site_id),
            positive_flow,
            length,
        );
        self.update_rolling_window_key(
            RollingWindowKey::MfiNegative(call_site_id),
            negative_flow,
            length,
        );
    }

    pub(crate) fn update_rolling_window_key(
        &mut self,
        key: RollingWindowKey,
        source: Option<f64>,
        length: usize,
    ) -> &RollingWindowState {
        push_rolling_window(&mut self.ta_state.rolling_windows, key, source, length)
    }
}
