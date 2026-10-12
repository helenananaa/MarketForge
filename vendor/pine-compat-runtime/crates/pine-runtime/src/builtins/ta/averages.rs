use super::*;

impl<'a> HistoricalRuntime<'a> {
    pub(crate) fn eval_sma(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let source = ta_arg(args, 0, "source")
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
        let window = self.update_rolling_window_for_bar(call_site_id, source, length);
        if !window.is_ready(length) {
            return Ok(PineValue::Na);
        }

        Ok(PineValue::Float(window.mean(length)))
    }

    pub(crate) fn eval_bb(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let (source, length, mult) = self.eval_average_source_length_mult(args)?;
        if length <= 0 {
            return Ok(PineValue::Tuple(vec![
                PineValue::Na,
                PineValue::Na,
                PineValue::Na,
            ]));
        }

        let Some(length) = usize::try_from(length).ok() else {
            return Ok(three_na_tuple());
        };
        let window = self.update_rolling_window(call_site_id, source, length);
        if !window.is_ready(length) {
            return Ok(PineValue::Tuple(vec![
                PineValue::Na,
                PineValue::Na,
                PineValue::Na,
            ]));
        }
        let Some(mult) = mult else {
            return Ok(PineValue::Tuple(vec![
                PineValue::Na,
                PineValue::Na,
                PineValue::Na,
            ]));
        };

        let basis = window.mean(length);
        let variance = window.variance(length, true);
        let dev = mult * variance.sqrt();

        Ok(PineValue::Tuple(vec![
            PineValue::Float(basis),
            PineValue::Float(basis + dev),
            PineValue::Float(basis - dev),
        ]))
    }

    pub(crate) fn eval_bbw(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let (source, length, mult) = self.eval_average_source_length_mult(args)?;
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
        let Some(mult) = mult else {
            return Ok(PineValue::Na);
        };

        let basis = window.mean(length);
        if basis == 0.0 {
            return Ok(PineValue::Na);
        }
        let dev = mult * window.variance(length, true).sqrt();

        Ok(finite_float_or_na((2.0 * dev) / basis))
    }

    pub(crate) fn eval_kc(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let Some((basis, range_ema, mult)) = self.eval_kc_components(call_site_id, args)? else {
            return Ok(three_na_tuple());
        };

        Ok(PineValue::Tuple(vec![
            finite_float_or_na(basis),
            finite_float_or_na(basis + range_ema * mult),
            finite_float_or_na(basis - range_ema * mult),
        ]))
    }

    pub(crate) fn eval_kcw(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let Some((basis, range_ema, mult)) = self.eval_kc_components(call_site_id, args)? else {
            return Ok(PineValue::Na);
        };
        if basis == 0.0 {
            return Ok(PineValue::Na);
        }

        Ok(finite_float_or_na((2.0 * range_ema * mult) / basis))
    }

    pub(crate) fn eval_kc_components(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<Option<(f64, f64, f64)>, RuntimeError> {
        let (source, length) = self.eval_average_source_length(args)?;
        let Some(source) = source.as_f64() else {
            return Ok(None);
        };
        let Some(mult) = ta_arg(args, 2, "mult")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_f64())
        else {
            return Ok(None);
        };
        let use_true_range = if let Some(arg) = ta_arg(args, 3, "useTrueRange") {
            match self.eval_expr(arg)? {
                PineValue::Bool(value) => value,
                PineValue::Na => true,
                _ => false,
            }
        } else {
            true
        };
        if length <= 0 {
            return Ok(None);
        }

        let span = if use_true_range {
            self.true_range(true).as_f64()
        } else {
            match (
                self.current_builtin_f64("high"),
                self.current_builtin_f64("low"),
            ) {
                (Some(high), Some(low)) => Some(high - low),
                _ => None,
            }
        };
        let Some(span) = span else {
            return Ok(None);
        };

        let previous = kc_state(self.ta_state.call_state.get(&call_site_id));
        let basis = ema_next(previous.map(|state| state.0), source, length);
        let range_ema = ema_next(previous.map(|state| state.1), span, length);
        self.ta_state.call_state.insert(
            call_site_id,
            PineValue::Tuple(vec![PineValue::Float(basis), PineValue::Float(range_ema)]),
        );

        Ok(Some((basis, range_ema, mult)))
    }

    pub(crate) fn eval_wma(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let (source, length) = self.eval_average_source_length(args)?;
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

        Ok(finite_float_or_na(window.weighted_mean(length)))
    }

    pub(crate) fn eval_hma(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let (source, length) = self.eval_average_source_length(args)?;
        if length <= 0 {
            return Ok(PineValue::Na);
        }

        let Some(length) = usize::try_from(length).ok() else {
            return Ok(PineValue::Na);
        };
        let half_length = (length / 2).max(1);
        let smooth_length = (length as f64).sqrt().round().max(1.0) as usize;
        let source = source.as_f64();

        self.update_rolling_window_key(RollingWindowKey::HmaFull(call_site_id), source, length);

        let full = self
            .ta_state
            .rolling_windows
            .get(&RollingWindowKey::HmaFull(call_site_id));
        let diff = match full {
            Some(full) if full.is_ready(length) => {
                // The half window is always this suffix. Once the full window
                // is ready, its half suffix is ready too, even after length changes.
                let (full_mean, half_mean) = full.weighted_mean_with_tail(length, half_length);
                let diff = 2.0 * half_mean - full_mean;
                Some(
                    if !diff.is_finite() && half_mean.is_finite() && full_mean.is_finite() {
                        // Doubling can overflow although the final HMA difference
                        // fits, including a constant extreme finite source.
                        (half_mean - full_mean) + half_mean
                    } else {
                        diff
                    },
                )
            }
            _ => None,
        };

        let smooth = self.update_rolling_window_key(
            RollingWindowKey::HmaSmooth(call_site_id),
            diff,
            smooth_length,
        );
        if !smooth.is_ready(smooth_length) {
            return Ok(PineValue::Na);
        }

        Ok(finite_float_or_na(smooth.weighted_mean(smooth_length)))
    }

    fn eval_average_source_length(
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
            .and_then(|value| value.as_trunc_i64())
            .unwrap_or(0);
        Ok((source, length))
    }

    fn eval_average_source_length_mult(
        &mut self,
        args: RuntimeArgs<'_>,
    ) -> Result<(PineValue, i64, Option<f64>), RuntimeError> {
        let (source, length) = self.eval_average_source_length(args)?;
        let mult = ta_arg(args, 2, "mult")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_f64());
        Ok((source, length, mult))
    }

    pub(crate) fn eval_swma(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let source = ta_arg(args, 0, "source")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .unwrap_or(PineValue::Na);
        let length = 4_usize;
        let window = self.update_rolling_window(call_site_id, source, length);
        if !window.is_ready(length) {
            return Ok(PineValue::Na);
        }

        let values = [
            window.values[0].expect("ready SWMA window"),
            window.values[1].expect("ready SWMA window"),
            window.values[2].expect("ready SWMA window"),
            window.values[3].expect("ready SWMA window"),
        ];
        let value = (values[0] + 2.0 * values[1] + 2.0 * values[2] + values[3]) / 6.0;
        Ok(finite_float_or_na(value))
    }

    pub(crate) fn eval_alma(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let source = ta_arg(args, 0, "series")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .unwrap_or(PineValue::Na);
        let length = ta_arg(args, 1, "length")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        let offset = ta_arg(args, 2, "offset")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_f64());
        let sigma = ta_arg(args, 3, "sigma")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_f64());
        let floor_center = ta_arg(args, 4, "floor")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .is_some_and(|value| matches!(value, PineValue::Bool(true)));
        if length <= 0 {
            return Ok(PineValue::Na);
        }
        let (Some(offset), Some(sigma)) = (offset, sigma) else {
            return Ok(PineValue::Na);
        };
        if sigma <= 0.0 || !offset.is_finite() || !sigma.is_finite() {
            return Ok(PineValue::Na);
        }

        let Some(length) = usize::try_from(length).ok() else {
            return Ok(PineValue::Na);
        };
        let weight_key = crate::algorithms::alma_weights::AlmaWeightKey::new(
            length,
            offset,
            sigma,
            floor_center,
        );
        let window = super::flow::push_rolling_window(
            &mut self.ta_state.rolling_windows,
            RollingWindowKey::Single(call_site_id),
            source.as_f64(),
            length,
        );
        if !window.is_ready(length) {
            self.alma_weights
                .with_weights(call_site_id, weight_key, false, |_| ());
            return Ok(PineValue::Na);
        }

        let scale = length as f64 / sigma;
        if scale == 0.0 || !scale.is_finite() {
            self.alma_weights
                .with_weights(call_site_id, weight_key, false, |_| ());
            return Ok(PineValue::Na);
        }

        self.alma_weights
            .with_weights(call_site_id, weight_key, true, |weights| {
                let (weighted_sum, weight_sum) = if let Some(weights) = weights {
                    let mut weighted_sum = 0.0;
                    for (value, weight) in window.values.iter().flatten().zip(weights.values()) {
                        weighted_sum += *value * *weight;
                    }
                    (weighted_sum, weights.weight_sum())
                } else {
                    let mut center = offset * (length as f64 - 1.0);
                    if floor_center {
                        center = center.floor();
                    }
                    let mut weighted_sum = 0.0;
                    let mut weight_sum = 0.0;
                    for (index, value) in window.values.iter().flatten().copied().enumerate() {
                        let distance = index as f64 - center;
                        let weight = (-(distance * distance) / (2.0 * scale * scale)).exp();
                        weighted_sum += value * weight;
                        weight_sum += weight;
                    }
                    (weighted_sum, weight_sum)
                };
                if weight_sum == 0.0 || !weight_sum.is_finite() {
                    return Ok(PineValue::Na);
                }

                Ok(finite_float_or_na(weighted_sum / weight_sum))
            })
    }

    pub(crate) fn eval_linreg(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let (source, length) = self.eval_average_source_length(args)?;
        let offset = ta_arg(args, 2, "offset")
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
        let window = self.update_rolling_window(call_site_id, source, length);
        if !window.is_ready(length) {
            return Ok(PineValue::Na);
        }

        if length == 1 {
            return Ok(finite_float_or_na(
                window.values[0].expect("ready LinReg window"),
            ));
        }

        let n = length as f64;
        let mut sum_x = 0.0;
        let mut sum_y = 0.0;
        let mut sum_x_squared = 0.0;
        let mut sum_xy = 0.0;
        window
            .values
            .iter()
            .flatten()
            .copied()
            .enumerate()
            .for_each(|(index, value)| {
                let x = index as f64;
                sum_x += x;
                sum_y += value;
                sum_x_squared += x * x;
                sum_xy += x * value;
            });

        let denominator = n * sum_x_squared - sum_x * sum_x;
        if denominator == 0.0 {
            return Ok(PineValue::Na);
        }
        let slope = (n * sum_xy - sum_x * sum_y) / denominator;
        let intercept = (sum_y - slope * sum_x) / n;
        let value = intercept + slope * (length as f64 - 1.0 - offset as f64);
        Ok(finite_float_or_na(value))
    }

    pub(crate) fn eval_ema(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let (source, length) = self.eval_average_source_length(args)?;
        if length <= 0 {
            return Ok(PineValue::Na);
        }
        let bar = self.bars as i64;
        // State is [last evaluation bar, committed base, current candidate].
        // Repeated calls on one bar share the base; the final candidate becomes
        // the base only when execution advances to another bar.
        let previous = match self.ta_state.call_state.get(&call_site_id) {
            Some(PineValue::Tuple(state)) if state.len() == 3 => {
                let index = if state[0].as_i64() == Some(bar) { 1 } else { 2 };
                state[index].as_f64()
            }
            Some(state) => state.as_f64(),
            None => None,
        };
        let source = source.as_f64();
        let alpha = 2.0 / (length as f64 + 1.0);
        let value = match (source, previous) {
            (Some(source), Some(previous)) => {
                PineValue::Float(alpha * source + (1.0 - alpha) * previous)
            }
            (Some(source), None) => {
                let Some(length) = usize::try_from(length).ok() else {
                    return Ok(PineValue::Na);
                };
                let window = self.update_rolling_window_for_bar(
                    call_site_id,
                    PineValue::Float(source),
                    length,
                );
                if window.is_ready(length) {
                    PineValue::Float(window.mean(length))
                } else {
                    PineValue::Na
                }
            }
            (None, _) => {
                if let Some(window) = self
                    .ta_state
                    .rolling_windows
                    .get_mut(&RollingWindowKey::Single(call_site_id))
                {
                    window.discard_for_bar(self.bars);
                }
                PineValue::Na
            }
        };
        let base = previous.map_or(PineValue::Na, PineValue::Float);
        let pending = if source.is_some() {
            value.clone()
        } else {
            base.clone()
        };
        self.ta_state.call_state.insert(
            call_site_id,
            PineValue::Tuple(vec![PineValue::Int(bar), base, pending]),
        );
        Ok(value)
    }

    pub(crate) fn eval_dema(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let Some((source, length)) = self.eval_ema_source_and_length(args)? else {
            return Ok(PineValue::Na);
        };
        let (previous_ema1, previous_ema2, _) =
            ema_chain_state(self.ta_state.call_state.get(&call_site_id));
        let ema1 = ema_next(previous_ema1, source, length);
        let ema2 = ema_next(previous_ema2, ema1, length);
        self.ta_state.call_state.insert(
            call_site_id,
            PineValue::Tuple(vec![PineValue::Float(ema1), PineValue::Float(ema2)]),
        );
        Ok(finite_float_or_na(2.0 * ema1 - ema2))
    }

    pub(crate) fn eval_tema(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let Some((source, length)) = self.eval_ema_source_and_length(args)? else {
            return Ok(PineValue::Na);
        };
        let (previous_ema1, previous_ema2, previous_ema3) =
            ema_chain_state(self.ta_state.call_state.get(&call_site_id));
        let ema1 = ema_next(previous_ema1, source, length);
        let ema2 = ema_next(previous_ema2, ema1, length);
        let ema3 = ema_next(previous_ema3, ema2, length);
        self.ta_state.call_state.insert(
            call_site_id,
            PineValue::Tuple(vec![
                PineValue::Float(ema1),
                PineValue::Float(ema2),
                PineValue::Float(ema3),
            ]),
        );
        Ok(finite_float_or_na(3.0 * ema1 - 3.0 * ema2 + ema3))
    }

    pub(crate) fn eval_ema_source_and_length(
        &mut self,
        args: RuntimeArgs<'_>,
    ) -> Result<Option<(f64, i64)>, RuntimeError> {
        let (source, length) = self.eval_average_source_length(args)?;
        let Some(source) = source.as_f64() else {
            return Ok(None);
        };
        if length <= 0 {
            return Ok(None);
        }
        Ok(Some((source, length)))
    }

    pub(crate) fn eval_rma(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let (source, length) = self.eval_average_source_length(args)?;
        let Some(source) = source.as_f64() else {
            return Ok(PineValue::Na);
        };
        if length <= 0 {
            return Ok(PineValue::Na);
        }

        let Some(value) = self.wilder_rma(
            call_site_id,
            0,
            self.ta_state
                .call_state
                .get(&call_site_id)
                .and_then(PineValue::as_f64),
            Some(source),
            length,
        ) else {
            return Ok(PineValue::Na);
        };
        let value = PineValue::Float(value);
        self.ta_state.call_state.insert(call_site_id, value.clone());
        Ok(value)
    }

    pub(crate) fn eval_rsi(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let (source, length) = self.eval_average_source_length(args)?;
        let Some(source) = source.as_f64() else {
            return Ok(PineValue::Na);
        };
        if length <= 0 {
            return Ok(PineValue::Na);
        }

        let Some(mut state) = self.ta_state.rsi_state.get(&call_site_id).copied() else {
            self.ta_state.rsi_state.insert(
                call_site_id,
                RsiState {
                    previous_source: source,
                    average_gain: None,
                    average_loss: None,
                },
            );
            return Ok(PineValue::Na);
        };

        let change = source - state.previous_source;
        let gain = change.max(0.0);
        let loss = (-change).max(0.0);
        let average_gain = self.wilder_rma(call_site_id, 0, state.average_gain, Some(gain), length);
        let average_loss = self.wilder_rma(call_site_id, 1, state.average_loss, Some(loss), length);
        state.previous_source = source;
        state.average_gain = average_gain;
        state.average_loss = average_loss;
        self.ta_state.rsi_state.insert(call_site_id, state);
        let (Some(average_gain), Some(average_loss)) = (average_gain, average_loss) else {
            return Ok(PineValue::Na);
        };

        Ok(PineValue::Float(rsi_from_averages(
            average_gain,
            average_loss,
        )))
    }

    pub(crate) fn eval_macd(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
    ) -> Result<PineValue, RuntimeError> {
        let source = ta_arg(args, 0, "source")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .unwrap_or(PineValue::Na);
        let fast_length = ta_arg(args, 1, "fastlen")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        let slow_length = ta_arg(args, 2, "slowlen")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        let signal_length = ta_arg(args, 3, "siglen")
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        if fast_length <= 0 || slow_length <= 0 || signal_length <= 0 {
            return Ok(PineValue::Tuple(vec![
                PineValue::Na,
                PineValue::Na,
                PineValue::Na,
            ]));
        }

        let mut state = self
            .ta_state
            .macd_state
            .get(&call_site_id)
            .copied()
            .unwrap_or_default();
        if state.last_bar != Some(self.bars) {
            state.base = [state.fast_ema, state.slow_ema, state.signal_ema];
            state.last_bar = Some(self.bars);
        }
        let source = source.as_f64().filter(|value| value.is_finite());
        let fast = self.macd_ema_sample(call_site_id, 0, state.base[0], source, fast_length);
        let slow = self.macd_ema_sample(call_site_id, 1, state.base[1], source, slow_length);
        let macd = fast.zip(slow).map(|(fast, slow)| fast - slow);
        let signal = self.macd_ema_sample(call_site_id, 2, state.base[2], macd, signal_length);
        let hist = macd.zip(signal).map(|(macd, signal)| macd - signal);
        state.fast_ema = if source.is_some() {
            fast
        } else {
            state.base[0]
        };
        state.slow_ema = if source.is_some() {
            slow
        } else {
            state.base[1]
        };
        state.signal_ema = if macd.is_some() {
            signal
        } else {
            state.base[2]
        };
        self.ta_state.macd_state.insert(call_site_id, state);

        Ok(PineValue::Tuple(vec![
            macd.map_or(PineValue::Na, PineValue::Float),
            signal.map_or(PineValue::Na, PineValue::Float),
            hist.map_or(PineValue::Na, PineValue::Float),
        ]))
    }

    fn macd_ema_sample(
        &mut self,
        call_site: CallSiteId,
        channel: u8,
        previous: Option<f64>,
        source: Option<f64>,
        length: i64,
    ) -> Option<f64> {
        let key = RollingWindowKey::Macd { call_site, channel };
        let Some(source) = source else {
            if let Some(window) = self.ta_state.rolling_windows.get_mut(&key) {
                window.discard_for_bar(self.bars);
            }
            return None;
        };
        if let Some(previous) = previous {
            return Some(ema_next(Some(previous), source, length));
        }
        let length = usize::try_from(length).ok()?;
        let window = self.ta_state.rolling_windows.entry(key).or_default();
        window.push_for_bar(Some(source), length, self.bars);
        window.is_ready(length).then(|| window.mean(length))
    }
}
