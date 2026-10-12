use super::*;

impl<'a> HistoricalRuntime<'a> {
    pub(crate) fn eval_tr(&mut self, args: RuntimeArgs<'_>) -> Result<PineValue, RuntimeError> {
        let handle_na = if let Some(arg) = ta_arg(args, 0, "handle_na") {
            matches!(self.eval_expr(arg)?, PineValue::Bool(true))
        } else {
            true
        };

        Ok(self.true_range(handle_na))
    }

    pub(crate) fn eval_atr(
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

        let true_range = self.true_range(true);
        let Some(value) = self.wilder_rma(
            call_site_id,
            0,
            self.ta_state
                .call_state
                .get(&call_site_id)
                .and_then(PineValue::as_f64),
            true_range.as_f64(),
            length,
        ) else {
            return Ok(PineValue::Na);
        };
        let value = PineValue::Float(value);
        self.ta_state.call_state.insert(call_site_id, value.clone());
        Ok(value)
    }

    pub(crate) fn eval_supertrend(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let Some(factor) = ta_arg(args, 0, "factor")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_f64())
        else {
            return Ok(two_na_tuple());
        };
        let atr_period = ta_arg(args, 1, "atrPeriod")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        if atr_period <= 0 {
            return Ok(two_na_tuple());
        }

        let Some(true_range) = self.true_range(true).as_f64() else {
            return Ok(two_na_tuple());
        };
        let (Some(high), Some(low), Some(close)) = (
            self.current_builtin_f64("high"),
            self.current_builtin_f64("low"),
            self.current_builtin_f64("close"),
        ) else {
            return Ok(two_na_tuple());
        };

        let previous = supertrend_state(self.ta_state.call_state.get(&call_site_id));
        let Some(atr) = self.wilder_rma(
            call_site_id,
            0,
            previous.map(|state| state.0),
            Some(true_range),
            atr_period,
        ) else {
            return Ok(two_na_tuple());
        };
        let hl2 = (high + low) / 2.0;
        let basic_upper = hl2 + factor * atr;
        let basic_lower = hl2 - factor * atr;
        let previous_close = self.previous_close();

        let upper = match previous.zip(previous_close) {
            Some(((_, previous_upper, _, _), previous_close))
                if basic_upper >= previous_upper && previous_close <= previous_upper =>
            {
                previous_upper
            }
            _ => basic_upper,
        };
        let lower = match previous.zip(previous_close) {
            Some(((_, _, previous_lower, _), previous_close))
                if basic_lower <= previous_lower && previous_close >= previous_lower =>
            {
                previous_lower
            }
            _ => basic_lower,
        };

        let direction = match previous {
            None => 1.0,
            Some((_, previous_upper, _, previous_supertrend))
                if previous_supertrend == previous_upper && close > upper =>
            {
                -1.0
            }
            Some((_, previous_upper, _, previous_supertrend))
                if previous_supertrend == previous_upper =>
            {
                1.0
            }
            Some(_) if close < lower => 1.0,
            Some(_) => -1.0,
        };
        let supertrend = if direction < 0.0 { lower } else { upper };

        self.ta_state.call_state.insert(
            call_site_id,
            PineValue::Tuple(vec![
                PineValue::Float(atr),
                PineValue::Float(upper),
                PineValue::Float(lower),
                PineValue::Float(supertrend),
            ]),
        );

        Ok(PineValue::Tuple(vec![
            finite_float_or_na(supertrend),
            PineValue::Float(direction),
        ]))
    }

    pub(crate) fn eval_dmi(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let di_length = ta_arg(args, 0, "diLength")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        let adx_smoothing = ta_arg(args, 1, "adxSmoothing")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        if di_length <= 0 || adx_smoothing <= 0 {
            return Ok(three_na_tuple());
        }

        let (Some(high), Some(low)) = (
            self.current_builtin_f64("high"),
            self.current_builtin_f64("low"),
        ) else {
            return Ok(three_na_tuple());
        };
        let Some(true_range) = self.true_range(true).as_f64() else {
            return Ok(three_na_tuple());
        };

        let (Some(previous_high), Some(previous_low)) = (
            self.previous_builtin_f64("high"),
            self.previous_builtin_f64("low"),
        ) else {
            return Ok(three_na_tuple());
        };
        let up_move = high - previous_high;
        let down_move = previous_low - low;
        let plus_dm = if up_move > down_move && up_move > 0.0 {
            up_move
        } else {
            0.0
        };
        let minus_dm = if down_move > up_move && down_move > 0.0 {
            down_move
        } else {
            0.0
        };

        let previous = match self.ta_state.call_state.get(&call_site_id) {
            Some(PineValue::Tuple(values)) if values.len() == 4 => Some((
                values[0].as_f64(),
                values[1].as_f64(),
                values[2].as_f64(),
                values[3].as_f64(),
            )),
            _ => None,
        };
        // Advance TR / +DM / −DM together. Returning after only one RMA is
        // ready desyncs the SMA seed windows and leaves DMI all-na.
        let smoothed_tr = self.wilder_rma(
            call_site_id,
            0,
            previous.and_then(|values| values.0),
            Some(true_range),
            di_length,
        );
        let smoothed_plus_dm = self.wilder_rma(
            call_site_id,
            1,
            previous.and_then(|values| values.1),
            Some(plus_dm),
            di_length,
        );
        let smoothed_minus_dm = self.wilder_rma(
            call_site_id,
            2,
            previous.and_then(|values| values.2),
            Some(minus_dm),
            di_length,
        );
        let (Some(smoothed_tr), Some(smoothed_plus_dm), Some(smoothed_minus_dm)) =
            (smoothed_tr, smoothed_plus_dm, smoothed_minus_dm)
        else {
            return Ok(three_na_tuple());
        };
        let (plus_di, minus_di) = if smoothed_tr.is_finite() && smoothed_tr != 0.0 {
            (
                100.0 * smoothed_plus_dm / smoothed_tr,
                100.0 * smoothed_minus_dm / smoothed_tr,
            )
        } else {
            (0.0, 0.0)
        };
        let di_sum = plus_di + minus_di;
        let dx = if di_sum.is_finite() && di_sum != 0.0 {
            100.0 * (plus_di - minus_di).abs() / di_sum
        } else {
            0.0
        };
        let adx = self.wilder_rma(
            call_site_id,
            3,
            previous.and_then(|values| values.3),
            Some(dx),
            adx_smoothing,
        );
        self.ta_state.call_state.insert(
            call_site_id,
            PineValue::Tuple(vec![
                PineValue::Float(smoothed_tr),
                PineValue::Float(smoothed_plus_dm),
                PineValue::Float(smoothed_minus_dm),
                adx.map_or(PineValue::Na, PineValue::Float),
            ]),
        );
        Ok(PineValue::Tuple(vec![
            finite_float_or_na(plus_di),
            finite_float_or_na(minus_di),
            adx.map_or(PineValue::Na, finite_float_or_na),
        ]))
    }

    pub(crate) fn eval_sar(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let Some(start) = ta_arg(args, 0, "start")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_f64())
        else {
            return Ok(PineValue::Na);
        };
        let Some(increment) = ta_arg(args, 1, "inc")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_f64())
        else {
            return Ok(PineValue::Na);
        };
        let Some(max_acceleration) = ta_arg(args, 2, "max")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_f64())
        else {
            return Ok(PineValue::Na);
        };
        if !start.is_finite() || !increment.is_finite() || !max_acceleration.is_finite() {
            return Ok(PineValue::Na);
        }

        let (Some(high), Some(low), Some(close)) = (
            self.current_builtin_f64("high"),
            self.current_builtin_f64("low"),
            self.current_builtin_f64("close"),
        ) else {
            return Ok(PineValue::Na);
        };

        let mut is_first_trend_bar = false;
        let (mut result, mut max_min, mut acceleration, mut is_below) =
            if let Some(state) = sar_state(self.ta_state.call_state.get(&call_site_id)) {
                state
            } else {
                let (Some(previous_close), Some(previous_high), Some(previous_low)) = (
                    self.previous_builtin_f64("close"),
                    self.previous_builtin_f64("high"),
                    self.previous_builtin_f64("low"),
                ) else {
                    return Ok(PineValue::Na);
                };
                is_first_trend_bar = true;
                if close > previous_close {
                    (previous_low, high, start, true)
                } else {
                    (previous_high, low, start, false)
                }
            };

        result += acceleration * (max_min - result);
        if is_below {
            if result > low {
                is_first_trend_bar = true;
                is_below = false;
                result = high.max(max_min);
                max_min = low;
                acceleration = start;
            }
        } else if result < high {
            is_first_trend_bar = true;
            is_below = true;
            result = low.min(max_min);
            max_min = high;
            acceleration = start;
        }

        if !is_first_trend_bar {
            if is_below {
                if high > max_min {
                    max_min = high;
                    acceleration = (acceleration + increment).min(max_acceleration);
                }
            } else if low < max_min {
                max_min = low;
                acceleration = (acceleration + increment).min(max_acceleration);
            }
        }

        if is_below {
            if let Some(previous_low) = self.previous_builtin_f64("low") {
                result = result.min(previous_low);
            }
            if let Some(previous_previous_low) = self.builtin_f64_at("low", 2) {
                result = result.min(previous_previous_low);
            }
        } else {
            if let Some(previous_high) = self.previous_builtin_f64("high") {
                result = result.max(previous_high);
            }
            if let Some(previous_previous_high) = self.builtin_f64_at("high", 2) {
                result = result.max(previous_previous_high);
            }
        }

        self.ta_state.call_state.insert(
            call_site_id,
            PineValue::Tuple(vec![
                PineValue::Float(result),
                PineValue::Float(max_min),
                PineValue::Float(acceleration),
                PineValue::Bool(is_below),
            ]),
        );

        Ok(finite_float_or_na(result))
    }

    pub(crate) fn true_range(&self, handle_na: bool) -> PineValue {
        let Some(high) = self.current_builtin_f64("high") else {
            return PineValue::Na;
        };
        let Some(low) = self.current_builtin_f64("low") else {
            return PineValue::Na;
        };
        let high_low = high - low;
        let previous_close = self.previous_close();

        let Some(previous_close) = previous_close else {
            return if handle_na {
                PineValue::Float(high_low)
            } else {
                PineValue::Na
            };
        };

        PineValue::Float(
            high_low
                .max((high - previous_close).abs())
                .max((low - previous_close).abs()),
        )
    }
}
