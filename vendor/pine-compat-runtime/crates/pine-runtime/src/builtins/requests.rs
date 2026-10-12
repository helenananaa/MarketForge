use super::request_values::RequestedValue;
use std::collections::{HashMap, HashSet};

use pine_ir::{
    CallSiteId, HirCallArg, HirExpr, HirExprKind, HirHistoryOffset, HirStmt, HirStmtKind,
    Qualifier, SymbolId, ValueKind,
};

use crate::builtins::args::call_arg_expr;
use crate::builtins::time::timeframe_change_bucket;
use crate::runtime::append_history::AppendHistory;
use crate::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestGaps {
    Off,
    On,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestLookahead {
    Off,
    On,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RequestMergePolicy {
    gaps: RequestGaps,
    lookahead: RequestLookahead,
}

impl RequestMergePolicy {
    const MODERN: Self = Self {
        gaps: RequestGaps::Off,
        lookahead: RequestLookahead::Off,
    };
}

impl<'a> HistoricalRuntime<'a> {
    pub(crate) fn eval_request_call(
        &mut self,
        callee: &str,
        call_site_id: CallSiteId,
        args: &[HirCallArg],
    ) -> Option<Result<PineValue, RuntimeError>> {
        if callee == "request.security_lower_tf" {
            return Some(self.eval_request_security_lower_tf(call_site_id, args));
        }
        let (merge, legacy) = match callee {
            "request.security" => {
                let mut policy = RequestMergePolicy::MODERN;
                for (index, name) in [(3, "gaps"), (4, "lookahead")] {
                    if let Some(expr) = call_arg_expr(args, index, name) {
                        let value = match self.eval_expr(expr) {
                            Ok(value) => value,
                            Err(error) => return Some(Err(error)),
                        };
                        match (name, value) {
                            ("gaps", PineValue::String(value)) if value == "barmerge.gaps_off" => {
                                policy.gaps = RequestGaps::Off
                            }
                            ("gaps", PineValue::String(value)) if value == "barmerge.gaps_on" => {
                                policy.gaps = RequestGaps::On
                            }
                            ("lookahead", PineValue::String(value))
                                if value == "barmerge.lookahead_off" =>
                            {
                                policy.lookahead = RequestLookahead::Off
                            }
                            ("lookahead", PineValue::String(value))
                                if value == "barmerge.lookahead_on" =>
                            {
                                policy.lookahead = RequestLookahead::On
                            }
                            _ => {
                                return Some(Err(RuntimeError {
                                    message: format!("invalid request.security {name} policy"),
                                }));
                            }
                        }
                    }
                }
                (policy, None)
            }
            "$legacy.security.gaps_off.lookahead_off" => (
                RequestMergePolicy {
                    gaps: RequestGaps::Off,
                    lookahead: RequestLookahead::Off,
                },
                legacy_source_span(args),
            ),
            "$legacy.security.gaps_on.lookahead_off" => (
                RequestMergePolicy {
                    gaps: RequestGaps::On,
                    lookahead: RequestLookahead::Off,
                },
                legacy_source_span(args),
            ),
            "$legacy.security.gaps_off.lookahead_on" => (
                RequestMergePolicy {
                    gaps: RequestGaps::Off,
                    lookahead: RequestLookahead::On,
                },
                legacy_source_span(args),
            ),
            "$legacy.security.gaps_on.lookahead_on" => (
                RequestMergePolicy {
                    gaps: RequestGaps::On,
                    lookahead: RequestLookahead::On,
                },
                legacy_source_span(args),
            ),
            _ => return None,
        };
        if callee != "request.security" && merge.lookahead == RequestLookahead::On {
            self.legacy_security_repaint_warnings
                .entry(call_site_id)
                .or_insert(legacy.unwrap_or((0, 0)));
        }
        let result =
            self.eval_request_security(call_site_id, args, merge, callee == "request.security");
        Some(match legacy {
            Some((start, end)) => result.map_err(|error| RuntimeError {
                message: format!(
                    "legacy security at source span {start}..{end}: {}",
                    error.message
                ),
            }),
            None => result,
        })
    }

    fn eval_request_security_lower_tf(
        &mut self,
        call_site_id: CallSiteId,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let (Some(symbol_arg), Some(timeframe_arg), Some(expression_arg)) = (
            call_arg_expr(args, 0, "symbol"),
            call_arg_expr(args, 1, "timeframe"),
            call_arg_expr(args, 2, "expression"),
        ) else {
            return Err(RuntimeError {
                message: "request.security_lower_tf expects symbol, timeframe, and expression"
                    .to_owned(),
            });
        };
        let calc_bars_count = if let Some(expr) = call_arg_expr(args, 6, "calc_bars_count") {
            parse_request_calc_bars_count(self.eval_expr(expr)?, "request.security_lower_tf")?
        } else {
            None
        };
        let PineValue::String(symbol) = self.eval_expr(symbol_arg)? else {
            return Err(RuntimeError {
                message: "request.security_lower_tf symbol must evaluate to string".to_owned(),
            });
        };
        let PineValue::String(timeframe) = self.eval_expr(timeframe_arg)? else {
            return Err(RuntimeError {
                message: "request.security_lower_tf timeframe must evaluate to string".to_owned(),
            });
        };
        let scalar_kind = lower_tf_array_kind(expression_arg.pine_type.kind);
        let tuple_kinds = self
            .program
            .lower_tf_tuple_types
            .iter()
            .find(|(id, _)| *id == call_site_id)
            .and_then(|(_, kinds)| {
                kinds
                    .iter()
                    .copied()
                    .map(lower_tf_array_kind)
                    .collect::<Option<Vec<_>>>()
            });
        if scalar_kind.is_none() && tuple_kinds.is_none() {
            return Err(RuntimeError {
                message:
                    "request.security_lower_tf expression must return a scalar or scalar tuple"
                        .to_owned(),
            });
        }
        let chart = self.request_environment.chart().clone();
        let symbol = if symbol.trim().is_empty() {
            chart.symbol().to_owned()
        } else {
            symbol
        };
        let chart_timeframe = chart.timeframe().clone();
        let requested_timeframe = if timeframe.trim().is_empty() {
            chart_timeframe.clone()
        } else {
            RequestTimeframe::parse(&timeframe).map_err(|error| RuntimeError {
                message: error.to_string(),
            })?
        };
        if requested_timeframe.seconds() > chart_timeframe.seconds() {
            return Err(RuntimeError {
                message: format!(
                    "request.security_lower_tf timeframe `{}` exceeds chart timeframe `{}`",
                    requested_timeframe.value(),
                    chart_timeframe.value()
                ),
            });
        }
        self.check_dynamic_request_context(
            "request.security_lower_tf",
            call_site_id,
            symbol_arg,
            timeframe_arg,
            &symbol,
            &requested_timeframe,
        )?;
        if symbol == chart.symbol() && requested_timeframe == chart_timeframe {
            if let Some(count) = calc_bars_count {
                let end = self.historical_end.ok_or_else(|| RuntimeError {
                    message: "request.security_lower_tf bounded equal-timeframe history requires a known historical dataset end"
                        .to_owned(),
                })?;
                if self.bars + count < end {
                    return self.lower_tf_arrays_from_samples(
                        scalar_kind,
                        tuple_kinds.as_deref(),
                        vec![],
                    );
                }
            }
            let value = self.eval_expr(expression_arg)?;
            let sample = self.freeze_requested_value(&value)?;
            return self.lower_tf_arrays_from_samples(
                scalar_kind,
                tuple_kinds.as_deref(),
                vec![sample],
            );
        }
        let current_time = self
            .current_bar
            .map(|bar| bar.time)
            .ok_or_else(|| RuntimeError {
                message: "request.security_lower_tf has no current chart bar".to_owned(),
            })?;
        let chart_close = request_bar_nominal_close(current_time, &chart_timeframe);
        let key = RequestKey::new(&symbol, requested_timeframe.clone());
        let cache_key = RequestCacheKey::new(call_site_id, key.symbol(), key.timeframe().value());
        if self.current_bar_update_kind != BarUpdateKind::Historical {
            let observed_time = self.request_feed.last_update_time(&key);
            if observed_time.is_some_and(|time| time >= chart_close) {
                return Err(RuntimeError {
                    message:
                        "request.security_lower_tf received an intrabar from a future chart period"
                            .to_owned(),
                });
            }
            let Some(observed_time) = observed_time.filter(|time| *time >= current_time) else {
                return self.lower_tf_arrays_from_samples(
                    scalar_kind,
                    tuple_kinds.as_deref(),
                    vec![],
                );
            };
            let requested_chart = if key.symbol() == chart.symbol() {
                chart.clone().with_timeframe(requested_timeframe)
            } else {
                ChartContext::new(&symbol, requested_timeframe)
            };
            let environment = self.request_environment.for_chart(requested_chart);
            let include_forming = self.request_feed.has_forming(&key);
            let values = if let Some(count) = calc_bars_count {
                let bars = self.resolved_request_bars_from(&key, 0)?;
                let available = bars.partition_point(|bar| bar.time <= observed_time);
                let start = available.saturating_sub(count);
                AppendHistory::from_values(self.evaluate_requested_values(
                    &bars[start..available],
                    expression_arg,
                    environment,
                )?)
            } else {
                self.evaluate_request_incremental(
                    &key,
                    &cache_key,
                    expression_arg,
                    environment,
                    include_forming,
                )?
            };
            let start = values.partition_point(|(time, _)| *time < current_time);
            let end = values.partition_point(|(time, _)| *time < chart_close);
            let samples = (start..end).map(|index| values[index].1.clone()).collect();
            return self.lower_tf_arrays_from_samples(scalar_kind, tuple_kinds.as_deref(), samples);
        }
        if !self.request_cache.contains_key(&cache_key) {
            let requested_chart = if key.symbol() == chart.symbol() {
                chart.clone().with_timeframe(requested_timeframe)
            } else {
                ChartContext::new(&symbol, requested_timeframe)
            };
            let environment = self.request_environment.for_chart(requested_chart);
            let values = if let Some(count) = calc_bars_count {
                let bars = self.resolved_request_bars_from(&key, 0)?;
                let start = bars.len().saturating_sub(count);
                AppendHistory::from_values(self.evaluate_requested_values(
                    &bars[start..],
                    expression_arg,
                    environment,
                )?)
            } else {
                self.evaluate_request_incremental(
                    &key,
                    &cache_key,
                    expression_arg,
                    environment,
                    false,
                )?
            };
            self.request_cache.insert(cache_key.clone(), values);
        }
        let samples = self
            .request_cache
            .get(&cache_key)
            .expect("request cache populated");
        let start = samples.partition_point(|(time, _)| *time < current_time);
        let end = samples.partition_point(|(time, _)| *time < chart_close);
        let values = (start..end).map(|index| samples[index].1.clone()).collect();
        self.lower_tf_arrays_from_samples(scalar_kind, tuple_kinds.as_deref(), values)
    }

    fn lower_tf_arrays_from_samples(
        &mut self,
        scalar_kind: Option<ArrayElementKind>,
        tuple_kinds: Option<&[ArrayElementKind]>,
        samples: Vec<RequestedValue>,
    ) -> Result<PineValue, RuntimeError> {
        if let Some(kinds) = tuple_kinds {
            let mut fields = vec![Vec::with_capacity(samples.len()); kinds.len()];
            for sample in samples {
                let RequestedValue::Tuple(values) = sample else {
                    return Err(RuntimeError {
                        message: "request.security_lower_tf expected a tuple expression".to_owned(),
                    });
                };
                if values.len() != kinds.len() {
                    return Err(RuntimeError {
                        message: "request.security_lower_tf tuple width changed".to_owned(),
                    });
                }
                for (field, value) in fields.iter_mut().zip(values) {
                    let RequestedValue::Scalar(value) = value else {
                        return Err(RuntimeError {
                            message: "request.security_lower_tf tuple contains a reference value"
                                .to_owned(),
                        });
                    };
                    field.push(value);
                }
            }
            return Ok(PineValue::Tuple(
                kinds
                    .iter()
                    .copied()
                    .zip(fields)
                    .map(|(kind, values)| self.new_array_from_values(kind, values))
                    .collect(),
            ));
        }
        let kind = scalar_kind.expect("validated scalar request kind");
        let mut values = Vec::with_capacity(samples.len());
        for sample in samples {
            let RequestedValue::Scalar(value) = sample else {
                return Err(RuntimeError {
                    message: "request.security_lower_tf expression returned a reference value"
                        .to_owned(),
                });
            };
            values.push(value);
        }
        Ok(self.new_array_from_values(kind, values))
    }

    fn eval_request_security(
        &mut self,
        call_site_id: CallSiteId,
        args: &[HirCallArg],
        merge: RequestMergePolicy,
        modern: bool,
    ) -> Result<PineValue, RuntimeError> {
        if !(3..=if modern { 8 } else { 5 }).contains(&args.len()) {
            return Err(RuntimeError {
                message: format!(
                    "request.security received an unsupported argument count of {}",
                    args.len()
                ),
            });
        }

        let calc_bars_count = if modern {
            match call_arg_expr(args, 7, "calc_bars_count") {
                Some(expr) => {
                    parse_request_calc_bars_count(self.eval_expr(expr)?, "request.security")?
                }
                None => None,
            }
        } else {
            None
        };

        let Some(symbol_expr) = call_arg_expr(args, 0, "symbol") else {
            return Err(RuntimeError {
                message: "request.security missing symbol argument".to_owned(),
            });
        };
        let PineValue::String(symbol) = self.eval_expr(symbol_expr)? else {
            return Err(RuntimeError {
                message: "request.security symbol must evaluate to string".to_owned(),
            });
        };
        let Some(timeframe_expr) = call_arg_expr(args, 1, "timeframe") else {
            return Err(RuntimeError {
                message: "request.security missing timeframe argument".to_owned(),
            });
        };
        let PineValue::String(timeframe) = self.eval_expr(timeframe_expr)? else {
            return Err(RuntimeError {
                message: "request.security timeframe must evaluate to string".to_owned(),
            });
        };
        let Some(expression) = call_arg_expr(args, 2, "expression") else {
            return Err(RuntimeError {
                message: "request.security missing expression argument".to_owned(),
            });
        };

        let chart = self.request_environment.chart();
        let chart_symbol = chart.symbol().to_owned();
        let symbol = if symbol.trim().is_empty() {
            chart_symbol.clone()
        } else {
            symbol
        };
        let chart_timeframe = chart.timeframe().clone();
        let requested_timeframe = if timeframe.trim().is_empty() {
            chart_timeframe.clone()
        } else {
            RequestTimeframe::parse(&timeframe).map_err(|err| RuntimeError {
                message: err.to_string(),
            })?
        };

        if modern {
            self.check_dynamic_request_context(
                "request.security",
                call_site_id,
                symbol_expr,
                timeframe_expr,
                &symbol,
                &requested_timeframe,
            )?;
        }

        if symbol == chart_symbol && requested_timeframe == chart_timeframe {
            if let Some(count) = calc_bars_count {
                let end = self.historical_end.ok_or_else(|| RuntimeError {
                    message: "request.security bounded equal-timeframe history requires a known historical dataset end"
                        .to_owned(),
                })?;
                let start = end.saturating_sub(count);
                if self.bars < start {
                    return Ok(PineValue::Na);
                }
                let key =
                    RequestCacheKey::new(call_site_id, &chart_symbol, chart_timeframe.value());
                let program = self.program.clone();
                let initializers = request_dependency_initializers(&program);
                let tuple_dependencies = request_tuple_dependency_statements(&program);
                let captures = request_capture_values(
                    &self.program,
                    expression,
                    &self.current_symbols,
                    &initializers,
                    &tuple_dependencies,
                );
                for value in captures.values() {
                    self.reject_request_object_graph(value)?;
                }
                let saved = self.bounded_same_context_evaluations.remove(&key);
                let replaced_values = saved
                    .as_ref()
                    .map_or(0, |runtime| runtime.valuewhen_retained_values());
                self.replace_requested_valuewhen_values(replaced_values, 0)?;
                let mut runtime = saved.unwrap_or_else(|| {
                    Box::new(self.fork_with_request_environment(self.request_environment.clone()))
                });
                runtime.inherit_execution_budget(self);
                runtime.inherit_valuewhen_budget(self, 0)?;
                if runtime.bars != self.bars - start {
                    return Err(RuntimeError {
                        message: "request.security bounded equal-timeframe expression must execute on every retained chart bar"
                            .to_owned(),
                    });
                }
                runtime.historical_end = Some(end - start);
                let bar = self.current_bar.ok_or_else(|| RuntimeError {
                    message: "request.security has no current chart bar".to_owned(),
                })?;
                let result = runtime.eval_requested_bar_expression(
                    bar,
                    expression,
                    &captures,
                    &initializers,
                    &tuple_dependencies,
                );
                self.accept_execution_budget(&runtime);
                let value = result?;
                self.replace_requested_valuewhen_values(0, runtime.valuewhen_retained_values())?;
                self.bounded_same_context_evaluations.insert(key, runtime);
                self.bounded_same_context_captures.insert(
                    RequestCacheKey::new(call_site_id, &chart_symbol, chart_timeframe.value()),
                    captures,
                );
                return Ok(self.import_requested_value(&value));
            }
            let value = self.eval_expr(expression)?;
            let snapshot = self.freeze_requested_value(&value)?;
            return Ok(self.import_requested_value(&snapshot));
        }

        self.eval_provider_security(
            call_site_id,
            &symbol,
            requested_timeframe,
            &chart_timeframe,
            expression,
            merge,
            calc_bars_count,
        )
    }

    fn check_dynamic_request_context(
        &mut self,
        callee: &str,
        call_site_id: CallSiteId,
        symbol_expr: &HirExpr,
        timeframe_expr: &HirExpr,
        symbol: &str,
        timeframe: &RequestTimeframe,
    ) -> Result<(), RuntimeError> {
        if symbol_expr.pine_type.qualifier != Qualifier::Series
            && timeframe_expr.pine_type.qualifier != Qualifier::Series
        {
            return Ok(());
        }
        let key = RequestCacheKey::new(call_site_id, symbol, timeframe.value());
        if self.current_bar_update_kind == BarUpdateKind::Historical {
            self.historical_dynamic_request_contexts.insert(key);
            return Ok(());
        }
        if self.historical_dynamic_request_contexts.contains(&key) {
            Ok(())
        } else {
            Err(RuntimeError {
                message: format!(
                    "{callee} cannot access a new dynamic context `{symbol}` / `{}` on a realtime bar",
                    timeframe.value()
                ),
            })
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "request context and merge settings are independent inputs"
    )]
    fn eval_provider_security(
        &mut self,
        call_site_id: CallSiteId,
        symbol: &str,
        requested_timeframe: RequestTimeframe,
        chart_timeframe: &RequestTimeframe,
        expression: &HirExpr,
        merge: RequestMergePolicy,
        calc_bars_count: Option<usize>,
    ) -> Result<PineValue, RuntimeError> {
        validate_provider_timeframe(symbol, &requested_timeframe, chart_timeframe)?;
        let key = RequestKey::new(symbol, requested_timeframe.clone());
        let current_time = self
            .current_bar
            .map(|bar| bar.time)
            .ok_or_else(|| RuntimeError {
                message: "request.security has no current chart bar".to_owned(),
            })?;

        if requested_timeframe.seconds() < chart_timeframe.seconds()
            && self.current_bar_update_kind != BarUpdateKind::Historical
        {
            return self.eval_realtime_lower_security(
                call_site_id,
                &key,
                current_time,
                chart_timeframe,
                expression,
                merge,
                calc_bars_count,
            );
        }

        let cache_key = RequestCacheKey::new(call_site_id, key.symbol(), key.timeframe().value());
        let include_forming = self.current_bar_update_kind != BarUpdateKind::Historical
            && self.request_feed.has_forming(&key);
        if include_forming || !self.request_cache.contains_key(&cache_key) {
            let requested_chart = if key.symbol() == self.request_environment.chart().symbol() {
                self.request_environment
                    .chart()
                    .clone()
                    .with_timeframe(requested_timeframe.clone())
            } else {
                ChartContext::new(key.symbol(), requested_timeframe.clone())
            };
            let requested_environment = self.request_environment.for_chart(requested_chart);
            let requested_values = if let Some(count) = calc_bars_count {
                let bars = self.resolved_request_bars_from(&key, 0)?;
                let start = bars.len().saturating_sub(count);
                AppendHistory::from_values(self.evaluate_requested_values(
                    &bars[start..],
                    expression,
                    requested_environment,
                )?)
            } else {
                self.evaluate_request_incremental(
                    &key,
                    &cache_key,
                    expression,
                    requested_environment,
                    include_forming,
                )?
            };
            let aligned = align_requested_value(
                &requested_values,
                current_time,
                &requested_timeframe,
                chart_timeframe,
                merge,
                self.current_bar_update_kind,
                include_forming,
            );
            if !include_forming {
                self.request_cache
                    .insert(cache_key.clone(), requested_values);
            }
            return Ok(self.import_requested_value(&aligned));
        }

        let requested_values = self
            .request_cache
            .get(&cache_key)
            .ok_or_else(|| RuntimeError {
                message: "request.security requested context cache was not populated".to_owned(),
            })?;
        let aligned = align_requested_value(
            requested_values,
            current_time,
            &requested_timeframe,
            chart_timeframe,
            merge,
            self.current_bar_update_kind,
            false,
        );
        Ok(self.import_requested_value(&aligned))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "realtime request replay needs explicit context and merge inputs"
    )]
    fn eval_realtime_lower_security(
        &mut self,
        call_site_id: CallSiteId,
        key: &RequestKey,
        current_time: i64,
        chart_timeframe: &RequestTimeframe,
        expression: &HirExpr,
        merge: RequestMergePolicy,
        calc_bars_count: Option<usize>,
    ) -> Result<PineValue, RuntimeError> {
        let chart_close = request_bar_nominal_close(current_time, chart_timeframe);
        let Some(observed_time) = self
            .request_feed
            .last_update_time(key)
            .filter(|time| *time >= current_time && *time < chart_close)
        else {
            return Err(RuntimeError {
                message: "request.security lower-timeframe forming updates require an ordered intrabar feed for the current chart bar"
                    .to_owned(),
            });
        };
        let requested_chart = if key.symbol() == self.request_environment.chart().symbol() {
            self.request_environment
                .chart()
                .clone()
                .with_timeframe(key.timeframe().clone())
        } else {
            ChartContext::new(key.symbol(), key.timeframe().clone())
        };
        let requested_environment = self.request_environment.for_chart(requested_chart);
        let include_forming = self.request_feed.has_forming(key);
        let values = if let Some(count) = calc_bars_count {
            let bars = self.resolved_request_bars_from(key, 0)?;
            let available = bars.partition_point(|bar| bar.time <= observed_time);
            let start = available.saturating_sub(count);
            AppendHistory::from_values(self.evaluate_requested_values(
                &bars[start..available],
                expression,
                requested_environment,
            )?)
        } else {
            let cache_key =
                RequestCacheKey::new(call_site_id, key.symbol(), key.timeframe().value());
            self.evaluate_request_incremental(
                key,
                &cache_key,
                expression,
                requested_environment,
                include_forming,
            )?
        };
        let aligned = align_requested_value(
            &values,
            current_time,
            key.timeframe(),
            chart_timeframe,
            merge,
            self.current_bar_update_kind,
            include_forming,
        );
        Ok(self.import_requested_value(&aligned))
    }

    pub(crate) fn resolved_request_bars_from(
        &self,
        key: &RequestKey,
        start: usize,
    ) -> Result<Vec<Bar>, RuntimeError> {
        let provider_bars = match self.request_environment.provider().bars(key) {
            Ok(bars) => bars,
            Err(RequestDataError::MissingData { symbol, timeframe }) => {
                if !self.request_feed.contains(key) {
                    return Err(RuntimeError {
                        message: RequestDataError::MissingData { symbol, timeframe }.to_string(),
                    });
                }
                &[]
            }
            Err(error) => {
                return Err(RuntimeError {
                    message: error.to_string(),
                });
            }
        };
        let include_forming = self.current_bar_update_kind != BarUpdateKind::Historical;
        let bars = self
            .request_feed
            .resolved_from(key, provider_bars, include_forming, start);
        // External execution can receive a known stream with no closed bars yet.
        // Missing keys still fail above; empty visible history aligns to na.
        if bars.is_empty() && self.external_execution.is_none() {
            return Err(RuntimeError {
                message: RequestDataError::MissingData {
                    symbol: key.symbol().to_owned(),
                    timeframe: key.timeframe().value().to_owned(),
                }
                .to_string(),
            });
        }
        Ok(bars)
    }

    pub(crate) fn evaluate_requested_values(
        &mut self,
        requested_bars: &[Bar],
        expression: &HirExpr,
        requested_environment: RequestEnvironment,
    ) -> Result<Vec<(i64, RequestedValue)>, RuntimeError> {
        let program = self.program.clone();
        let dependency_initializers = request_dependency_initializers(&program);
        let tuple_dependencies = request_tuple_dependency_statements(&program);
        let captures = request_capture_values(
            &self.program,
            expression,
            &self.current_symbols,
            &dependency_initializers,
            &tuple_dependencies,
        );
        for value in captures.values() {
            self.reject_request_object_graph(value)?;
        }
        let mut runtime = self.fork_with_request_environment(requested_environment);
        runtime.inherit_valuewhen_budget(self, 0)?;
        runtime.historical_end = Some(requested_bars.len());
        let mut values = Vec::with_capacity(requested_bars.len());
        for bar in requested_bars {
            let result = runtime.eval_requested_bar_expression(
                *bar,
                expression,
                &captures,
                &dependency_initializers,
                &tuple_dependencies,
            );
            // Requested history is part of this chart execution's work. Do
            // not grant each requested bar a fresh chart-sized allowance.
            self.accept_execution_budget(&runtime);
            values.push((bar.time, result?));
        }
        self.legacy_security_repaint_warnings
            .extend(runtime.legacy_security_repaint_warnings);
        Ok(values)
    }

    pub(crate) fn eval_requested_bar_expression(
        &mut self,
        bar: Bar,
        expression: &HirExpr,
        captures: &HashMap<SymbolId, PineValue>,
        dependency_initializers: &HashMap<SymbolId, &HirExpr>,
        tuple_dependencies: &HashMap<SymbolId, &HirStmt>,
    ) -> Result<RequestedValue, RuntimeError> {
        let bar_index = self.bars;
        self.current_bar_update_kind = BarUpdateKind::Historical;
        self.current_bar_is_new = true;
        self.current_bar = Some(bar);
        self.series_store.set_current_bar(bar_index);
        self.current_symbols.clear();
        self.current_series.clear();
        self.set_builtin_symbols(&bar, bar_index)?;
        self.current_symbols.extend(
            captures
                .iter()
                .map(|(symbol, value)| (*symbol, value.clone())),
        );
        self.eval_requested_expression_dependencies(
            expression,
            &mut HashSet::new(),
            dependency_initializers,
            tuple_dependencies,
        )?;

        let value = self.eval_expr(expression)?;
        let value = self.freeze_requested_value(&value)?;
        self.commit_current_series()?;
        self.previous_bar_time = Some(bar.time);
        self.bars += 1;
        self.collect_temporary_collections();
        self.current_bar_update_kind = BarUpdateKind::Historical;
        self.current_bar_is_new = true;
        self.current_bar = None;
        Ok(value)
    }

    fn eval_requested_expression_dependencies(
        &mut self,
        expression: &HirExpr,
        visiting: &mut HashSet<SymbolId>,
        dependency_initializers: &HashMap<SymbolId, &HirExpr>,
        tuple_dependencies: &HashMap<SymbolId, &HirStmt>,
    ) -> Result<(), RuntimeError> {
        let mut symbols = Vec::new();
        collect_hir_expr_symbols(expression, &mut symbols);
        for symbol in symbols {
            self.eval_requested_symbol_dependency(
                symbol,
                visiting,
                dependency_initializers,
                tuple_dependencies,
            )?;
        }
        Ok(())
    }

    fn eval_requested_symbol_dependency(
        &mut self,
        symbol: SymbolId,
        visiting: &mut HashSet<SymbolId>,
        dependency_initializers: &HashMap<SymbolId, &HirExpr>,
        tuple_dependencies: &HashMap<SymbolId, &HirStmt>,
    ) -> Result<(), RuntimeError> {
        if self.current_symbols.contains_key(&symbol) {
            return Ok(());
        }
        let pine_type = self
            .program
            .symbols
            .iter()
            .find(|candidate| candidate.id == symbol)
            .map(|candidate| candidate.pine_type)
            .ok_or_else(|| RuntimeError {
                message: format!(
                    "request.security expression references unknown symbol {}",
                    symbol.0
                ),
            })?;
        if pine_type.kind == ValueKind::Na {
            return Ok(());
        }
        if pine_type.qualifier != Qualifier::Series {
            return Err(RuntimeError {
                message: format!(
                    "request.security expression capture {} was not initialized before the request",
                    symbol.0
                ),
            });
        }
        if !visiting.insert(symbol) {
            // Analysis admits a recursive provider dependency only through a
            // positive history reference. Its current value will be computed
            // after the historical read, from this requested context's series.
            return Ok(());
        }
        if let Some(initializer) = dependency_initializers.get(&symbol).copied() {
            self.eval_requested_expression_dependencies(
                initializer,
                visiting,
                dependency_initializers,
                tuple_dependencies,
            )?;
            let value = self.eval_expr(initializer)?;
            self.set_symbol_value(symbol, value);
        } else if let Some(statement) = tuple_dependencies.get(&symbol).copied() {
            let HirStmtKind::TupleDecl { value, .. } = &statement.kind else {
                unreachable!("tuple dependency index contains only tuple declarations")
            };
            self.eval_requested_expression_dependencies(
                value,
                visiting,
                dependency_initializers,
                tuple_dependencies,
            )?;
            self.eval_stmt(statement)?;
        } else {
            return Err(RuntimeError {
                message: format!(
                    "request.security expression symbol {} is not an immutable admitted dependency",
                    symbol.0
                ),
            });
        }
        visiting.remove(&symbol);
        Ok(())
    }
}

pub(crate) fn request_capture_values(
    program: &pine_ir::HirProgram,
    expression: &HirExpr,
    current_symbols: &HashMap<SymbolId, PineValue>,
    dependency_initializers: &HashMap<SymbolId, &HirExpr>,
    tuple_dependencies: &HashMap<SymbolId, &HirStmt>,
) -> HashMap<SymbolId, PineValue> {
    let mut candidates = HashSet::new();
    collect_request_capture_symbols(
        program,
        expression,
        &mut HashSet::new(),
        &mut candidates,
        dependency_initializers,
        tuple_dependencies,
    );
    candidates
        .into_iter()
        .filter_map(|symbol| {
            current_symbols
                .get(&symbol)
                .cloned()
                .map(|value| (symbol, value))
        })
        .collect()
}

fn collect_request_capture_symbols(
    program: &pine_ir::HirProgram,
    expression: &HirExpr,
    visited: &mut HashSet<SymbolId>,
    captures: &mut HashSet<SymbolId>,
    dependency_initializers: &HashMap<SymbolId, &HirExpr>,
    tuple_dependencies: &HashMap<SymbolId, &HirStmt>,
) {
    let mut symbols = Vec::new();
    collect_hir_expr_symbols(expression, &mut symbols);
    for symbol in symbols {
        if !visited.insert(symbol) {
            continue;
        }
        let Some(pine_type) = program
            .symbols
            .iter()
            .find(|candidate| candidate.id == symbol)
            .map(|candidate| candidate.pine_type)
        else {
            continue;
        };
        let initializer = dependency_initializers.get(&symbol).copied().or_else(|| {
            tuple_dependencies.get(&symbol).and_then(|stmt| {
                if let HirStmtKind::TupleDecl { value, .. } = &stmt.kind {
                    Some(value)
                } else {
                    None
                }
            })
        });
        let Some(initializer) = initializer else {
            continue;
        };
        if pine_type.qualifier == Qualifier::Series {
            collect_request_capture_symbols(
                program,
                initializer,
                visited,
                captures,
                dependency_initializers,
                tuple_dependencies,
            );
        } else {
            captures.insert(symbol);
        }
    }
}

pub(crate) fn request_dependency_initializers(
    program: &pine_ir::HirProgram,
) -> HashMap<SymbolId, &HirExpr> {
    let mut initializers = HashMap::new();
    collect_request_stmt_initializers(&program.statements, &mut initializers);
    initializers
}

pub(crate) fn request_tuple_dependency_statements(
    program: &pine_ir::HirProgram,
) -> HashMap<SymbolId, &HirStmt> {
    let mut tuples = HashMap::new();
    collect_request_tuple_stmt_dependencies(&program.statements, &mut tuples);
    tuples
}

fn collect_request_tuple_stmt_dependencies<'a>(
    statements: &'a [HirStmt],
    tuples: &mut HashMap<SymbolId, &'a HirStmt>,
) {
    for statement in statements {
        if let HirStmtKind::TupleDecl { symbols, .. } = &statement.kind {
            for symbol in symbols {
                tuples.insert(*symbol, statement);
            }
        }
        match &statement.kind {
            HirStmtKind::Expr(expr)
            | HirStmtKind::Decl { value: expr, .. }
            | HirStmtKind::Reassign { value: expr, .. }
            | HirStmtKind::FieldReassign { value: expr, .. }
            | HirStmtKind::TupleDecl { value: expr, .. } => {
                collect_request_tuple_expr_dependencies(expr, tuples);
            }
            HirStmtKind::ArrayFieldReassign {
                array,
                index,
                value,
                ..
            } => {
                for expr in [array, index, value] {
                    collect_request_tuple_expr_dependencies(expr, tuples);
                }
            }
            HirStmtKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                collect_request_tuple_expr_dependencies(condition, tuples);
                collect_request_tuple_stmt_dependencies(then_branch, tuples);
                collect_request_tuple_stmt_dependencies(else_branch, tuples);
            }
            HirStmtKind::Switch { selector, arms } => {
                if let Some(selector) = selector {
                    collect_request_tuple_expr_dependencies(selector, tuples);
                }
                for arm in arms {
                    if let Some(condition) = &arm.condition {
                        collect_request_tuple_expr_dependencies(condition, tuples);
                    }
                    collect_request_tuple_stmt_dependencies(&arm.body, tuples);
                }
            }
            HirStmtKind::For {
                from,
                to,
                step,
                body,
                ..
            } => {
                collect_request_tuple_expr_dependencies(from, tuples);
                collect_request_tuple_expr_dependencies(to, tuples);
                if let Some(step) = step {
                    collect_request_tuple_expr_dependencies(step, tuples);
                }
                collect_request_tuple_stmt_dependencies(body, tuples);
            }
            HirStmtKind::ForIn { iterable, body, .. } => {
                collect_request_tuple_expr_dependencies(iterable, tuples);
                collect_request_tuple_stmt_dependencies(body, tuples);
            }
            HirStmtKind::While { condition, body } => {
                collect_request_tuple_expr_dependencies(condition, tuples);
                collect_request_tuple_stmt_dependencies(body, tuples);
            }
            HirStmtKind::Break | HirStmtKind::Continue => {}
        }
    }
}

fn collect_request_tuple_expr_dependencies<'a>(
    expr: &'a HirExpr,
    tuples: &mut HashMap<SymbolId, &'a HirStmt>,
) {
    match &expr.kind {
        HirExprKind::Literal(_) | HirExprKind::Symbol(_) | HirExprKind::Builtin(_) => {}
        HirExprKind::Unary { expr, .. } | HirExprKind::FieldAccess { value: expr, .. } => {
            collect_request_tuple_expr_dependencies(expr, tuples)
        }
        HirExprKind::History { expr, offset } => {
            collect_request_tuple_expr_dependencies(expr, tuples);
            if let HirHistoryOffset::Dynamic(offset) = offset {
                collect_request_tuple_expr_dependencies(offset, tuples);
            }
        }
        HirExprKind::Binary { left, right, .. } => {
            collect_request_tuple_expr_dependencies(left, tuples);
            collect_request_tuple_expr_dependencies(right, tuples);
        }
        HirExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            for value in [condition, then_expr, else_expr] {
                collect_request_tuple_expr_dependencies(value, tuples);
            }
        }
        HirExprKind::Switch { selector, arms } => {
            if let Some(selector) = selector {
                collect_request_tuple_expr_dependencies(selector, tuples);
            }
            for arm in arms {
                if let Some(condition) = &arm.condition {
                    collect_request_tuple_expr_dependencies(condition, tuples);
                }
                collect_request_tuple_expr_dependencies(&arm.result, tuples);
            }
        }
        HirExprKind::For {
            from,
            to,
            step,
            statements,
            result,
            ..
        } => {
            collect_request_tuple_expr_dependencies(from, tuples);
            collect_request_tuple_expr_dependencies(to, tuples);
            if let Some(step) = step {
                collect_request_tuple_expr_dependencies(step, tuples);
            }
            collect_request_tuple_stmt_dependencies(statements, tuples);
            collect_request_tuple_expr_dependencies(result, tuples);
        }
        HirExprKind::ForIn {
            iterable,
            statements,
            result,
            ..
        } => {
            collect_request_tuple_expr_dependencies(iterable, tuples);
            collect_request_tuple_stmt_dependencies(statements, tuples);
            collect_request_tuple_expr_dependencies(result, tuples);
        }
        HirExprKind::While {
            condition,
            statements,
            result,
        } => {
            collect_request_tuple_expr_dependencies(condition, tuples);
            collect_request_tuple_stmt_dependencies(statements, tuples);
            collect_request_tuple_expr_dependencies(result, tuples);
        }
        HirExprKind::Tuple(values)
        | HirExprKind::UserTypeConstruct { fields: values, .. }
        | HirExprKind::UserTypeArrayConstruct {
            elements: values, ..
        } => {
            for value in values {
                collect_request_tuple_expr_dependencies(value, tuples);
            }
        }
        HirExprKind::Block { statements, result } => {
            collect_request_tuple_stmt_dependencies(statements, tuples);
            collect_request_tuple_expr_dependencies(result, tuples);
        }
        HirExprKind::Call { args, .. } => {
            for arg in args {
                collect_request_tuple_expr_dependencies(&arg.value, tuples);
            }
        }
    }
}

fn collect_request_stmt_initializers<'a>(
    statements: &'a [HirStmt],
    initializers: &mut HashMap<SymbolId, &'a HirExpr>,
) {
    for statement in statements {
        match &statement.kind {
            HirStmtKind::Expr(expr) => collect_request_expr_initializers(expr, initializers),
            HirStmtKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                collect_request_expr_initializers(condition, initializers);
                collect_request_stmt_initializers(then_branch, initializers);
                collect_request_stmt_initializers(else_branch, initializers);
            }
            HirStmtKind::Switch { selector, arms } => {
                if let Some(selector) = selector {
                    collect_request_expr_initializers(selector, initializers);
                }
                for arm in arms {
                    if let Some(condition) = &arm.condition {
                        collect_request_expr_initializers(condition, initializers);
                    }
                    collect_request_stmt_initializers(&arm.body, initializers);
                }
            }
            HirStmtKind::For {
                from,
                to,
                step,
                body,
                ..
            } => {
                collect_request_expr_initializers(from, initializers);
                collect_request_expr_initializers(to, initializers);
                if let Some(step) = step {
                    collect_request_expr_initializers(step, initializers);
                }
                collect_request_stmt_initializers(body, initializers);
            }
            HirStmtKind::ForIn { iterable, body, .. } => {
                collect_request_expr_initializers(iterable, initializers);
                collect_request_stmt_initializers(body, initializers);
            }
            HirStmtKind::While { condition, body } => {
                collect_request_expr_initializers(condition, initializers);
                collect_request_stmt_initializers(body, initializers);
            }
            HirStmtKind::Decl { symbol, value } => {
                initializers.insert(*symbol, value);
                collect_request_expr_initializers(value, initializers);
            }
            HirStmtKind::Reassign { value, .. } | HirStmtKind::FieldReassign { value, .. } => {
                collect_request_expr_initializers(value, initializers);
            }
            HirStmtKind::ArrayFieldReassign {
                array,
                index,
                value,
                ..
            } => {
                collect_request_expr_initializers(array, initializers);
                collect_request_expr_initializers(index, initializers);
                collect_request_expr_initializers(value, initializers);
            }
            HirStmtKind::TupleDecl { symbols, value } => {
                if let HirExprKind::Tuple(items) = &value.kind {
                    for (symbol, item) in symbols.iter().zip(items) {
                        initializers.insert(*symbol, item);
                    }
                }
                collect_request_expr_initializers(value, initializers);
            }
            HirStmtKind::Break | HirStmtKind::Continue => {}
        }
    }
}

fn collect_request_expr_initializers<'a>(
    expression: &'a HirExpr,
    initializers: &mut HashMap<SymbolId, &'a HirExpr>,
) {
    match &expression.kind {
        HirExprKind::Literal(_) | HirExprKind::Builtin(_) | HirExprKind::Symbol(_) => {}
        HirExprKind::Unary { expr, .. } => collect_request_expr_initializers(expr, initializers),
        HirExprKind::Binary { left, right, .. } => {
            collect_request_expr_initializers(left, initializers);
            collect_request_expr_initializers(right, initializers);
        }
        HirExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_request_expr_initializers(condition, initializers);
            collect_request_expr_initializers(then_expr, initializers);
            collect_request_expr_initializers(else_expr, initializers);
        }
        HirExprKind::Switch { selector, arms } => {
            if let Some(selector) = selector {
                collect_request_expr_initializers(selector, initializers);
            }
            for arm in arms {
                if let Some(condition) = &arm.condition {
                    collect_request_expr_initializers(condition, initializers);
                }
                collect_request_expr_initializers(&arm.result, initializers);
            }
        }
        HirExprKind::For {
            from,
            to,
            step,
            statements,
            result,
            ..
        } => {
            collect_request_expr_initializers(from, initializers);
            collect_request_expr_initializers(to, initializers);
            if let Some(step) = step {
                collect_request_expr_initializers(step, initializers);
            }
            collect_request_stmt_initializers(statements, initializers);
            collect_request_expr_initializers(result, initializers);
        }
        HirExprKind::ForIn {
            iterable,
            statements,
            result,
            ..
        } => {
            collect_request_expr_initializers(iterable, initializers);
            collect_request_stmt_initializers(statements, initializers);
            collect_request_expr_initializers(result, initializers);
        }
        HirExprKind::While {
            condition,
            statements,
            result,
        } => {
            collect_request_expr_initializers(condition, initializers);
            collect_request_stmt_initializers(statements, initializers);
            collect_request_expr_initializers(result, initializers);
        }
        HirExprKind::Tuple(items) => {
            for item in items {
                collect_request_expr_initializers(item, initializers);
            }
        }
        HirExprKind::UserTypeConstruct { fields, .. } => {
            for field in fields {
                collect_request_expr_initializers(field, initializers);
            }
        }
        HirExprKind::UserTypeArrayConstruct { elements, .. } => {
            for element in elements {
                collect_request_expr_initializers(element, initializers);
            }
        }
        HirExprKind::FieldAccess { value, .. } => {
            collect_request_expr_initializers(value, initializers);
        }
        HirExprKind::Block { statements, result } => {
            collect_request_stmt_initializers(statements, initializers);
            collect_request_expr_initializers(result, initializers);
        }
        HirExprKind::Call { args, .. } => {
            for arg in args {
                collect_request_expr_initializers(&arg.value, initializers);
            }
        }
        HirExprKind::History { expr, offset } => {
            collect_request_expr_initializers(expr, initializers);
            if let HirHistoryOffset::Dynamic(offset) = offset {
                collect_request_expr_initializers(offset, initializers);
            }
        }
    }
}

fn collect_hir_expr_symbols(expression: &HirExpr, symbols: &mut Vec<SymbolId>) {
    collect_hir_expr_symbols_inner(expression, symbols, &mut HashSet::new());
}

fn collect_hir_expr_symbols_inner(
    expression: &HirExpr,
    symbols: &mut Vec<SymbolId>,
    bound: &mut HashSet<SymbolId>,
) {
    match &expression.kind {
        HirExprKind::Literal(_) | HirExprKind::Builtin(_) => {}
        HirExprKind::Symbol(symbol) => {
            if !bound.contains(symbol) {
                symbols.push(*symbol);
            }
        }
        HirExprKind::Unary { expr, .. } => collect_hir_expr_symbols_inner(expr, symbols, bound),
        HirExprKind::Binary { left, right, .. } => {
            collect_hir_expr_symbols_inner(left, symbols, bound);
            collect_hir_expr_symbols_inner(right, symbols, bound);
        }
        HirExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_hir_expr_symbols_inner(condition, symbols, bound);
            collect_hir_expr_symbols_inner(then_expr, symbols, bound);
            collect_hir_expr_symbols_inner(else_expr, symbols, bound);
        }
        HirExprKind::Switch { selector, arms } => {
            if let Some(selector) = selector {
                collect_hir_expr_symbols_inner(selector, symbols, bound);
            }
            for arm in arms {
                if let Some(condition) = &arm.condition {
                    collect_hir_expr_symbols_inner(condition, symbols, bound);
                }
                collect_hir_expr_symbols_inner(&arm.result, symbols, bound);
            }
        }
        HirExprKind::For {
            counter,
            from,
            to,
            step,
            statements,
            result,
            ..
        } => {
            collect_hir_expr_symbols_inner(from, symbols, bound);
            collect_hir_expr_symbols_inner(to, symbols, bound);
            if let Some(step) = step {
                collect_hir_expr_symbols_inner(step, symbols, bound);
            }
            bound.insert(*counter);
            collect_hir_stmt_symbols(statements, symbols, bound);
            collect_hir_expr_symbols_inner(result, symbols, bound);
        }
        HirExprKind::ForIn {
            index,
            value,
            iterable,
            statements,
            result,
            ..
        } => {
            collect_hir_expr_symbols_inner(iterable, symbols, bound);
            if let Some(index) = index {
                bound.insert(*index);
            }
            bound.insert(*value);
            collect_hir_stmt_symbols(statements, symbols, bound);
            collect_hir_expr_symbols_inner(result, symbols, bound);
        }
        HirExprKind::While {
            condition,
            statements,
            result,
        } => {
            collect_hir_expr_symbols_inner(condition, symbols, bound);
            collect_hir_stmt_symbols(statements, symbols, bound);
            collect_hir_expr_symbols_inner(result, symbols, bound);
        }
        HirExprKind::Tuple(items) => {
            for item in items {
                collect_hir_expr_symbols_inner(item, symbols, bound);
            }
        }
        HirExprKind::UserTypeConstruct { fields, .. } => {
            for field in fields {
                collect_hir_expr_symbols_inner(field, symbols, bound);
            }
        }
        HirExprKind::UserTypeArrayConstruct { elements, .. } => {
            for element in elements {
                collect_hir_expr_symbols_inner(element, symbols, bound);
            }
        }
        HirExprKind::FieldAccess { value, .. } => {
            collect_hir_expr_symbols_inner(value, symbols, bound)
        }
        HirExprKind::Block { statements, result } => {
            collect_hir_stmt_symbols(statements, symbols, bound);
            collect_hir_expr_symbols_inner(result, symbols, bound);
        }
        HirExprKind::Call { args, .. } => {
            for arg in args {
                collect_hir_expr_symbols_inner(&arg.value, symbols, bound);
            }
        }
        HirExprKind::History { expr, offset } => {
            collect_hir_expr_symbols_inner(expr, symbols, bound);
            if let HirHistoryOffset::Dynamic(offset) = offset {
                collect_hir_expr_symbols_inner(offset, symbols, bound);
            }
        }
    }
}

fn collect_hir_stmt_symbols(
    statements: &[HirStmt],
    symbols: &mut Vec<SymbolId>,
    bound: &mut HashSet<SymbolId>,
) {
    for statement in statements {
        match &statement.kind {
            HirStmtKind::Expr(expr) => collect_hir_expr_symbols_inner(expr, symbols, bound),
            HirStmtKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                collect_hir_expr_symbols_inner(condition, symbols, bound);
                collect_hir_stmt_symbols(then_branch, symbols, bound);
                collect_hir_stmt_symbols(else_branch, symbols, bound);
            }
            HirStmtKind::Switch { selector, arms } => {
                if let Some(selector) = selector {
                    collect_hir_expr_symbols_inner(selector, symbols, bound);
                }
                for arm in arms {
                    if let Some(condition) = &arm.condition {
                        collect_hir_expr_symbols_inner(condition, symbols, bound);
                    }
                    collect_hir_stmt_symbols(&arm.body, symbols, bound);
                }
            }
            HirStmtKind::For {
                counter,
                from,
                to,
                step,
                body,
                ..
            } => {
                collect_hir_expr_symbols_inner(from, symbols, bound);
                collect_hir_expr_symbols_inner(to, symbols, bound);
                if let Some(step) = step {
                    collect_hir_expr_symbols_inner(step, symbols, bound);
                }
                bound.insert(*counter);
                collect_hir_stmt_symbols(body, symbols, bound);
            }
            HirStmtKind::ForIn {
                index,
                value,
                iterable,
                body,
            } => {
                collect_hir_expr_symbols_inner(iterable, symbols, bound);
                if let Some(index) = index {
                    bound.insert(*index);
                }
                bound.insert(*value);
                collect_hir_stmt_symbols(body, symbols, bound);
            }
            HirStmtKind::While { condition, body } => {
                collect_hir_expr_symbols_inner(condition, symbols, bound);
                collect_hir_stmt_symbols(body, symbols, bound);
            }
            HirStmtKind::Decl { symbol, value } => {
                collect_hir_expr_symbols_inner(value, symbols, bound);
                bound.insert(*symbol);
            }
            HirStmtKind::Reassign { value, .. } | HirStmtKind::FieldReassign { value, .. } => {
                collect_hir_expr_symbols_inner(value, symbols, bound);
            }
            HirStmtKind::ArrayFieldReassign {
                array,
                index,
                value,
                ..
            } => {
                collect_hir_expr_symbols_inner(array, symbols, bound);
                collect_hir_expr_symbols_inner(index, symbols, bound);
                collect_hir_expr_symbols_inner(value, symbols, bound);
            }
            HirStmtKind::TupleDecl {
                symbols: declared,
                value,
            } => {
                collect_hir_expr_symbols_inner(value, symbols, bound);
                bound.extend(declared.iter().copied());
            }
            HirStmtKind::Break | HirStmtKind::Continue => {}
        }
    }
}

fn validate_provider_timeframe(
    _symbol: &str,
    requested_timeframe: &RequestTimeframe,
    chart_timeframe: &RequestTimeframe,
) -> Result<(), RuntimeError> {
    // Calendar months do not have a fixed number of seconds. Their merge
    // boundaries come from calendar opens/closes, not nominal-duration ratios.
    if !chart_timeframe.supports_security_timeframe(requested_timeframe) {
        return Err(RuntimeError {
            message: format!(
                "request.security requested timeframe `{}` must be an integer multiple of chart timeframe `{}`",
                requested_timeframe.value(),
                chart_timeframe.value()
            ),
        });
    }
    Ok(())
}

fn align_requested_value(
    requested_values: &AppendHistory<(i64, RequestedValue)>,
    current_time: i64,
    requested_timeframe: &RequestTimeframe,
    chart_timeframe: &RequestTimeframe,
    merge: RequestMergePolicy,
    update_kind: BarUpdateKind,
    includes_forming: bool,
) -> RequestedValue {
    if requested_timeframe.seconds() < chart_timeframe.seconds() {
        return align_lower_timeframe_value(
            requested_values,
            current_time,
            chart_timeframe,
            merge,
            update_kind,
        );
    }
    let mut available = requested_values.len();
    if includes_forming && available > 0 {
        let (open, value) = &requested_values[available - 1];
        if merge.gaps == RequestGaps::Off && *open <= current_time {
            return value.clone();
        }
        // Gapped output requires confirmation. A future-open forming bar is
        // unavailable even when the caller supplied it ahead of chart time.
        available -= 1;
    }
    let by_open = requested_timeframe == chart_timeframe
        || (merge.lookahead == RequestLookahead::On && update_kind == BarUpdateKind::Historical);
    let target = if by_open {
        current_time
    } else {
        request_bar_nominal_close(current_time, chart_timeframe)
    };
    let stamp = |index| {
        if by_open {
            requested_values[index].0
        } else {
            requested_bar_close(requested_values, index, requested_timeframe)
        }
    };
    let (mut lo, mut hi) = (0, available);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let before = if merge.gaps == RequestGaps::On {
            stamp(mid) < target
        } else {
            stamp(mid) <= target
        };
        if before {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    let index = if merge.gaps == RequestGaps::On {
        if lo == available || stamp(lo) != target {
            return RequestedValue::Scalar(PineValue::Na);
        }
        lo
    } else {
        let Some(index) = lo.checked_sub(1) else {
            return RequestedValue::Scalar(PineValue::Na);
        };
        index
    };
    requested_values[index].1.clone()
}

fn align_lower_timeframe_value(
    requested_values: &AppendHistory<(i64, RequestedValue)>,
    current_time: i64,
    chart_timeframe: &RequestTimeframe,
    merge: RequestMergePolicy,
    update_kind: BarUpdateKind,
) -> RequestedValue {
    let start = requested_values.partition_point(|(time, _)| *time < current_time);
    let chart_close = request_bar_nominal_close(current_time, chart_timeframe);
    let end = requested_values.partition_point(|(time, _)| *time < chart_close);
    let index = if start < end {
        if merge.lookahead == RequestLookahead::On && update_kind == BarUpdateKind::Historical {
            Some(start)
        } else {
            Some(end - 1)
        }
    } else if merge.gaps == RequestGaps::Off {
        start.checked_sub(1).map(|last| {
            if merge.lookahead == RequestLookahead::On && update_kind == BarUpdateKind::Historical {
                let bucket = timeframe_change_bucket(
                    requested_values[last].0,
                    chart_timeframe.value(),
                    chart_timeframe.seconds(),
                );
                let mut first = last;
                while first > 0
                    && timeframe_change_bucket(
                        requested_values[first - 1].0,
                        chart_timeframe.value(),
                        chart_timeframe.seconds(),
                    ) == bucket
                {
                    first -= 1;
                }
                first
            } else {
                last
            }
        })
    } else {
        None
    };
    index.map_or(RequestedValue::Scalar(PineValue::Na), |index| {
        requested_values[index].1.clone()
    })
}

fn request_bar_nominal_close(open_time: i64, timeframe: &RequestTimeframe) -> i64 {
    timeframe.nominal_close(open_time)
}

fn requested_bar_close(
    requested_values: &AppendHistory<(i64, RequestedValue)>,
    index: usize,
    timeframe: &RequestTimeframe,
) -> i64 {
    let open_time = requested_values[index].0;
    let nominal_close = request_bar_nominal_close(open_time, timeframe);
    requested_values
        .get(index + 1)
        .map(|(next_open, _)| *next_open)
        .filter(|next_open| *next_open > open_time)
        .map_or(nominal_close, |next_open| nominal_close.min(next_open))
}

fn legacy_source_span(args: &[HirCallArg]) -> Option<(i64, i64)> {
    let literal = |name: &str| {
        args.iter()
            .find(|arg| arg.name.as_deref() == Some(name))
            .and_then(|arg| match arg.value.kind {
                pine_ir::HirExprKind::Literal(pine_ir::HirLiteral::Int(value)) => Some(value),
                _ => None,
            })
    };
    Some((literal("$legacy_span_start")?, literal("$legacy_span_end")?))
}

fn lower_tf_array_kind(kind: ValueKind) -> Option<ArrayElementKind> {
    match kind {
        ValueKind::Int => Some(ArrayElementKind::Int),
        ValueKind::Float => Some(ArrayElementKind::Float),
        ValueKind::Bool => Some(ArrayElementKind::Bool),
        ValueKind::String => Some(ArrayElementKind::String),
        ValueKind::Color => Some(ArrayElementKind::Color),
        _ => None,
    }
}

fn parse_request_calc_bars_count(
    value: PineValue,
    callee: &str,
) -> Result<Option<usize>, RuntimeError> {
    let PineValue::Int(value) = value else {
        return Err(RuntimeError {
            message: format!("{callee} calc_bars_count must evaluate to a nonnegative int"),
        });
    };
    usize::try_from(value)
        .map(|count| (count != 0).then_some(count))
        .map_err(|_| RuntimeError {
            message: format!("{callee} calc_bars_count must evaluate to a nonnegative int"),
        })
}
