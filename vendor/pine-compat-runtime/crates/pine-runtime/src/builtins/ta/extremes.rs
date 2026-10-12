use pine_ir::SeriesId;

use super::*;

// Short windows avoid allocating a queue. Deeper windows amortize the persistent
// queue and history cursor costs while checkpoints share their retained leaves.
const CACHED_EXTREME_MIN_LENGTH: usize = 32;

impl HistoricalRuntime<'_> {
    pub(crate) fn eval_window_extreme(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
        mode: WindowExtreme,
    ) -> Result<PineValue, RuntimeError> {
        let (source, series_id, length) = self.eval_extreme_source_length(args, mode)?;
        if length <= 0 {
            self.ta_state.extreme_windows.remove(&call_site_id);
            return Ok(PineValue::Na);
        }

        let Some(length) = usize::try_from(length).ok() else {
            return Ok(PineValue::Na);
        };
        if length < CACHED_EXTREME_MIN_LENGTH {
            self.ta_state.extreme_windows.remove(&call_site_id);
        } else if let Some(series_id) = series_id {
            if self.bars + 1 >= length {
                let best = self.cached_window_extreme(
                    call_site_id,
                    finite_f64(source.clone()),
                    series_id,
                    length,
                    mode,
                );
                // f64::max/min may select either sign on zero ties. Preserve the
                // established scan's exact floating behavior on those windows.
                if best.is_none_or(|(value, _)| value != 0.0) {
                    return Ok(best.map_or(PineValue::Na, |(value, _)| PineValue::Float(value)));
                }
            }
        } else {
            self.ta_state.extreme_windows.remove(&call_site_id);
        }
        self.window_extreme_value(source, series_id, length, mode)
            .map_or(Ok(PineValue::Na), |value| Ok(finite_float_or_na(value)))
    }

    pub(crate) fn eval_window_extreme_offset(
        &mut self,
        call_site_id: CallSiteId,
        args: RuntimeArgs<'_>,
        mode: WindowExtreme,
    ) -> Result<PineValue, RuntimeError> {
        let (source, series_id, length) = self.eval_extreme_source_length(args, mode)?;
        if length <= 0 {
            self.ta_state.extreme_windows.remove(&call_site_id);
            return Ok(PineValue::Na);
        }

        let Some(length) = usize::try_from(length).ok() else {
            return Ok(PineValue::Na);
        };
        if length < CACHED_EXTREME_MIN_LENGTH {
            self.ta_state.extreme_windows.remove(&call_site_id);
        } else if let Some(series_id) = series_id {
            let Some(source) = finite_f64(source) else {
                return Ok(PineValue::Na);
            };
            return Ok(self
                .cached_window_extreme(call_site_id, Some(source), series_id, length, mode)
                .map_or(PineValue::Na, |(_, offset)| {
                    PineValue::Int(-(offset as i64))
                }));
        } else {
            self.ta_state.extreme_windows.remove(&call_site_id);
        }
        Ok(self
            .window_extreme_offset(source, series_id, length, mode)
            .map_or(PineValue::Na, |offset| PineValue::Int(-(offset as i64))))
    }

    fn cached_window_extreme(
        &mut self,
        call_site_id: CallSiteId,
        current: Option<f64>,
        series_id: SeriesId,
        length: usize,
        mode: WindowExtreme,
    ) -> Option<(f64, usize)> {
        let window = self
            .ta_state
            .extreme_windows
            .entry(call_site_id)
            .or_insert_with(|| {
                crate::algorithms::rolling_extreme::RollingExtremeState::new(
                    series_id, length, mode,
                )
            });
        let missing = window.prepare(
            series_id,
            length,
            mode,
            self.bars,
            self.series_store.len(series_id),
        );
        if missing == 1 {
            let previous = self
                .series_store
                .history_window(series_id, 1)
                .next()
                .and_then(PineValue::as_f64);
            window.push(self.bars - 1, previous);
        } else if missing > 1 {
            // Initial and sparse calls rebuild only the available window. The
            // usual consecutive-bar path above never allocates this scratch.
            let history: Vec<_> = self
                .series_store
                .history_window(series_id, missing)
                .enumerate()
                .map(|(index, value)| (self.bars - 1 - index, value.as_f64()))
                .collect();
            for (bar, value) in history.into_iter().rev() {
                window.push(bar, value);
            }
        }
        window.best(current, self.bars)
    }

    pub(crate) fn eval_extreme_source_length(
        &mut self,
        args: RuntimeArgs<'_>,
        mode: WindowExtreme,
    ) -> Result<(PineValue, Option<SeriesId>, i64), RuntimeError> {
        let positional_default_source =
            args.raw().len() == 1 && args.raw().first().is_some_and(|arg| arg.name.is_none());
        let has_explicit_source = !positional_default_source && ta_arg(args, 0, "source").is_some();

        if has_explicit_source {
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
            return Ok((source, source_arg.and_then(|arg| arg.series_id), length));
        }

        let length = ta_arg(args, 1, "length")
            .or_else(|| ta_arg(args, 0, "length"))
            .map(|arg| self.eval_expr(arg))
            .transpose()?
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        let source_name = match mode {
            WindowExtreme::Highest => "high",
            WindowExtreme::Lowest => "low",
        };
        let source = self
            .current_builtin_f64(source_name)
            .map_or(PineValue::Na, PineValue::Float);
        Ok((source, self.builtin_series_id(source_name), length))
    }

    fn window_extreme_value(
        &self,
        source: PineValue,
        series_id: Option<SeriesId>,
        length: usize,
        mode: WindowExtreme,
    ) -> Option<f64> {
        if self.bars + 1 < length {
            return None;
        }
        let mut extreme = finite_f64(source);
        let series_id = series_id?;
        for previous in self.series_store.history_window(series_id, length - 1) {
            let Some(previous) = previous.as_f64().filter(|value| value.is_finite()) else {
                // Missing source samples do not extend or invalidate the bar
                // window, including while an upstream average warms up.
                continue;
            };
            extreme = Some(match (mode, extreme) {
                (_, None) => previous,
                (WindowExtreme::Highest, Some(current)) => current.max(previous),
                (WindowExtreme::Lowest, Some(current)) => current.min(previous),
            });
        }
        extreme
    }

    fn window_extreme_offset(
        &self,
        source: PineValue,
        series_id: Option<SeriesId>,
        length: usize,
        mode: WindowExtreme,
    ) -> Option<usize> {
        let mut extreme = finite_f64(source)?;
        let mut best_offset = 0usize;
        let series_id = series_id?;
        for (index, previous) in self
            .series_store
            .history_window(series_id, (length - 1).min(self.bars))
            .enumerate()
        {
            let offset = index + 1;
            // TradingView evaluates highestbars/lowestbars over the bars already
            // available at the beginning of a series. A missing prehistory bar
            // ends the window; it does not make the current offset undefined.
            let Some(previous) = previous.as_f64().filter(|value| value.is_finite()) else {
                continue;
            };
            let better = match mode {
                WindowExtreme::Highest => previous > extreme,
                WindowExtreme::Lowest => previous < extreme,
            };
            if better {
                extreme = previous;
                best_offset = offset;
            }
        }
        Some(best_offset)
    }

    fn builtin_series_id(&self, name: &str) -> Option<SeriesId> {
        self.program
            .symbols
            .iter()
            .find(|symbol| symbol.name == name)
            .and_then(|symbol| symbol.series_id)
    }
}

fn finite_f64(value: PineValue) -> Option<f64> {
    value.as_f64().filter(|value| value.is_finite())
}
