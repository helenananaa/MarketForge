//! Reuse a checkpoint preceding the terminal requested bar. The old terminal
//! bar is always replayed when a context grows, preserving endpoint semantics.
use super::request_values::RequestedValue;
use super::requests::{
    request_capture_values, request_dependency_initializers, request_tuple_dependency_statements,
};
use crate::request::RequestCacheKey;
use crate::runtime::append_history::AppendHistory;
use crate::{
    HistoricalRuntime, PineValue, RequestDataError, RequestEnvironment, RequestKey, RuntimeError,
};
use pine_ir::{HirExpr, HirExprKind, HirHistoryOffset, HirStmt, HirStmtKind, SymbolId};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

#[derive(Clone)]
pub(crate) struct RequestEvaluation<'a> {
    prefix: AppendHistory<(i64, RequestedValue)>,
    before_last: Arc<HistoricalRuntime<'a>>,
    captures: HashMap<SymbolId, PineValue>,
    provider_len: usize,
    #[cfg(test)]
    replayed_bars: usize,
}

impl RequestEvaluation<'_> {
    pub(crate) fn capture_values(&self) -> impl Iterator<Item = &PineValue> {
        self.captures.values()
    }

    pub(crate) fn valuewhen_retained_values(&self) -> usize {
        self.before_last.valuewhen_retained_values()
    }

    #[cfg(test)]
    pub(crate) fn valuewhen_checkpoint(&self) -> &HistoricalRuntime<'_> {
        &self.before_last
    }
}

impl<'a> HistoricalRuntime<'a> {
    pub(crate) fn evaluate_request_incremental(
        &mut self,
        key: &RequestKey,
        cache_key: &RequestCacheKey,
        expression: &HirExpr,
        environment: RequestEnvironment,
        include_forming: bool,
    ) -> Result<AppendHistory<(i64, RequestedValue)>, RuntimeError> {
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
        if !incremental_expression(
            expression,
            &initializers,
            &captures,
            &self.program.symbols,
            0,
        ) {
            let bars = self.resolved_request_bars_from(key, 0)?;
            return self
                .evaluate_requested_values(&bars, expression, environment)
                .map(AppendHistory::from_values);
        }
        let provider_len = match environment.provider().bars(key) {
            Ok(bars) => bars.len(),
            Err(RequestDataError::MissingData { .. }) => 0,
            Err(error) => {
                return Err(RuntimeError {
                    message: error.to_string(),
                });
            }
        };
        let count = self
            .request_feed
            .resolved_len(key, provider_len, include_forming);
        let previous = self
            .request_evaluations
            .get(cache_key)
            .filter(|saved| {
                saved.provider_len == provider_len
                    && saved.captures == captures
                    && saved.prefix.len() < count
            })
            .cloned();
        let replaced_values = self
            .request_evaluations
            .get(cache_key)
            .map_or(0, |saved| saved.valuewhen_retained_values());
        let mut prefix = previous
            .as_ref()
            .map_or_else(AppendHistory::default, |saved| saved.prefix.clone());
        let mut runtime = match previous {
            Some(saved) => (*saved.before_last).clone(),
            None => self.fork_with_request_environment(environment),
        };
        // Checkpoints retain Pine state, not an allowance from an older chart
        // execution. All newly evaluated requested bars spend today's budget.
        runtime.inherit_execution_budget(self);
        runtime.inherit_valuewhen_budget(self, replaced_values)?;
        let bars = self.resolved_request_bars_from(key, prefix.len())?;
        if bars.is_empty() {
            self.request_evaluations.remove(cache_key);
            return Ok(prefix);
        }
        runtime.historical_end = Some(count);
        let mut before_last = None;
        let mut last = None;
        for (index, bar) in bars.iter().enumerate() {
            let terminal = index + 1 == bars.len();
            if terminal {
                before_last = Some(Arc::new(runtime.clone()));
            }
            let result = runtime.eval_requested_bar_expression(
                *bar,
                expression,
                &captures,
                &initializers,
                &tuple_dependencies,
            );
            self.accept_execution_budget(&runtime);
            let value = result?;
            if terminal {
                last = Some((bar.time, value));
            } else {
                prefix.push((bar.time, value));
            }
        }
        let mut values = prefix.clone();
        values.push(last.expect("resolved bars are nonempty"));
        let before_last = before_last.expect("terminal checkpoint");
        self.replace_requested_valuewhen_values(
            replaced_values,
            before_last.valuewhen_retained_values(),
        )?;
        self.request_evaluations.insert(
            cache_key.clone(),
            Arc::new(RequestEvaluation {
                prefix,
                before_last,
                captures,
                provider_len,
                #[cfg(test)]
                replayed_bars: bars.len(),
            }),
        );
        Ok(values)
    }
}

// Replay only expressions whose effects live in the requested checkpoint and
// whose inputs do not depend on the dataset endpoint. Other expressions retain
// the complete evaluator.
fn incremental_expression(
    expr: &HirExpr,
    initializers: &HashMap<SymbolId, &HirExpr>,
    captures: &HashMap<SymbolId, PineValue>,
    symbols: &[pine_ir::HirSymbol],
    depth: usize,
) -> bool {
    incremental_scoped_expression(
        expr,
        initializers,
        captures,
        symbols,
        &HashSet::new(),
        depth,
    )
}

fn incremental_scoped_expression(
    expr: &HirExpr,
    initializers: &HashMap<SymbolId, &HirExpr>,
    captures: &HashMap<SymbolId, PineValue>,
    symbols: &[pine_ir::HirSymbol],
    locals: &HashSet<SymbolId>,
    depth: usize,
) -> bool {
    if depth > 64 {
        return false;
    }
    let visit = |expr| {
        incremental_scoped_expression(expr, initializers, captures, symbols, locals, depth + 1)
    };
    match &expr.kind {
        HirExprKind::Literal(_) => true,
        HirExprKind::Builtin(name) => stable_builtin(name),
        HirExprKind::Symbol(symbol) => {
            locals.contains(symbol)
                || captures.contains_key(symbol)
                || initializers.get(symbol).map_or_else(
                    || {
                        symbols
                            .iter()
                            .find(|item| item.id == *symbol)
                            .is_some_and(|item| stable_builtin(&item.name))
                    },
                    |value| visit(value),
                )
        }
        HirExprKind::Unary { expr, .. } => visit(expr),
        HirExprKind::Binary { left, right, .. } => visit(left) && visit(right),
        HirExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => visit(condition) && visit(then_expr) && visit(else_expr),
        HirExprKind::Tuple(items) => items.iter().all(visit),
        HirExprKind::History { expr, offset } => {
            visit(expr)
                && match offset {
                    HirHistoryOffset::Constant(_) => true,
                    HirHistoryOffset::Dynamic(value) => visit(value),
                }
        }
        HirExprKind::Call { callee, args, .. } => {
            (callee.starts_with("ta.")
                || (callee.starts_with("math.") && callee != "math.random")
                || pine_builtins::is_pure_scalar_string_builtin(callee)
                || matches!(callee.as_str(), "timeframe.change" | "year"))
                && args.iter().all(|arg| visit(&arg.value))
        }
        HirExprKind::Block { statements, result } => {
            let mut bound = locals.clone();
            statements.iter().all(|statement| {
                incremental_statement(
                    statement,
                    initializers,
                    captures,
                    symbols,
                    &mut bound,
                    depth + 1,
                )
            }) && incremental_scoped_expression(
                result,
                initializers,
                captures,
                symbols,
                &bound,
                depth + 1,
            )
        }
        _ => false,
    }
}

// Lowered scalar UDFs keep their local state in the checkpoint. Admit only
// declarations, local assignments and conditional branches with stable inputs.
// Mutations of external state, loops, nested requests and endpoint reads retain
// the complete evaluator.
fn incremental_statement(
    statement: &HirStmt,
    initializers: &HashMap<SymbolId, &HirExpr>,
    captures: &HashMap<SymbolId, PineValue>,
    symbols: &[pine_ir::HirSymbol],
    locals: &mut HashSet<SymbolId>,
    depth: usize,
) -> bool {
    if depth > 64 {
        return false;
    }
    let visit = |expr, bound: &HashSet<SymbolId>| {
        incremental_scoped_expression(expr, initializers, captures, symbols, bound, depth + 1)
    };
    match &statement.kind {
        HirStmtKind::Decl { symbol, value } => {
            if !visit(value, locals) {
                return false;
            }
            locals.insert(*symbol);
            true
        }
        HirStmtKind::Reassign { symbol, value } => locals.contains(symbol) && visit(value, locals),
        HirStmtKind::Expr(value) => visit(value, locals),
        HirStmtKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            visit(condition, locals)
                && [then_branch, else_branch].into_iter().all(|branch| {
                    let mut bound = locals.clone();
                    branch.iter().all(|statement| {
                        incremental_statement(
                            statement,
                            initializers,
                            captures,
                            symbols,
                            &mut bound,
                            depth + 1,
                        )
                    })
                })
        }
        _ => false,
    }
}

fn stable_builtin(name: &str) -> bool {
    matches!(
        name,
        "open"
            | "high"
            | "low"
            | "close"
            | "volume"
            | "time"
            | "time_tradingday"
            | "bar_index"
            | "hl2"
            | "hlc3"
            | "ohlc4"
            | "hlcc4"
            | "barstate.isfirst"
    ) || name.starts_with("syminfo.")
        || name.starts_with("timeframe.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Bar, BarUpdate, BarUpdateKind, ChartContext, InMemoryRequestDataProvider, RequestTimeframe,
    };
    use pine_sema::analyze_source;
    use pine_syntax::SourceFile;

    fn bar(time: i64, close: f64) -> Bar {
        Bar {
            time,
            open: close,
            high: close,
            low: close,
            close,
            volume: 1.0,
        }
    }

    #[test]
    fn live_ta_checkpoint_matches_forced_full_evaluation_with_bounded_replay() {
        for expression in [
            "ta.sma(close, 3)",
            "ta.ema(close, 5)",
            "ta.rsi(close, 7)",
            "ta.cum(close)",
            "str.tonumber(str.tostring(close))",
            "str.length(str.upper(str.format(\"value={0}\", close)))",
            r#"str.length(str.match(str.tostring(close), close % 2 == 0 ? "[0-9]+" : "[0-9]+\\.[0-9]+"))"#,
            "str.tonumber(str.replace_all(str.tostring(close), \".\", \"\"))",
        ] {
            let source = format!(
                "//@version=6\nindicator(\"cache\")\nplot(request.security(\"B\", \"5\", {expression}))"
            );
            let analysis = analyze_source(&SourceFile::new("cache.pine", source));
            assert!(
                analysis.diagnostics.is_empty(),
                "{expression}: {:?}",
                analysis.diagnostics
            );
            let hir = analysis.hir.expect("supported request expression");
            let key = RequestKey::new("B", RequestTimeframe::parse("5").unwrap());
            let mut provider = InMemoryRequestDataProvider::new();
            provider
                .insert(
                    key.clone(),
                    (0..128)
                        .map(|i| bar(i * 300_000, 10.0 + i as f64))
                        .collect(),
                )
                .unwrap();
            let environment = RequestEnvironment::new(
                ChartContext::new("A", RequestTimeframe::parse("1").unwrap()),
                Arc::new(provider),
            );
            let mut fast = HistoricalRuntime::with_request_environment(&hir, environment.clone());
            let mut full = HistoricalRuntime::with_request_environment(&hir, environment);
            fast.append_bar(bar(0, 1.0)).unwrap();
            full.append_bar(bar(0, 1.0)).unwrap();
            for index in 128..144 {
                for bump in [0.0, 0.5, 1.0] {
                    let requested = bar(index * 300_000, index as f64 + bump);
                    fast.apply_request_update(key.clone(), BarUpdate::forming(requested))
                        .unwrap();
                    full.apply_request_update(key.clone(), BarUpdate::forming(requested))
                        .unwrap();
                    full.clear_request_evaluations();
                    let chart = bar(index * 300_000 + 240_000, 1.0);
                    fast.append_bar_with_kind(chart, BarUpdateKind::Forming)
                        .unwrap();
                    full.append_bar_with_kind(chart, BarUpdateKind::Forming)
                        .unwrap();
                    assert_eq!(
                        fast.result().plots,
                        full.result().plots,
                        "{expression}/{index}/{bump}"
                    );
                    assert!(
                        !fast.request_evaluations.is_empty(),
                        "{:#?}",
                        hir.statements
                    );
                    assert!(
                        fast.request_evaluations
                            .values()
                            .all(|state| state.replayed_bars <= 2)
                    );
                }
                let requested = bar(index * 300_000, index as f64 + 1.0);
                fast.apply_request_update(key.clone(), BarUpdate::confirmed(requested))
                    .unwrap();
                full.apply_request_update(key.clone(), BarUpdate::confirmed(requested))
                    .unwrap();
                full.clear_request_evaluations();
                let chart = bar(index * 300_000 + 240_000, 1.0);
                fast.append_bar_with_kind(chart, BarUpdateKind::Confirmed)
                    .unwrap();
                full.append_bar_with_kind(chart, BarUpdateKind::Confirmed)
                    .unwrap();
                assert_eq!(
                    fast.result().plots,
                    full.result().plots,
                    "{expression}/{index}/confirmed"
                );
                assert!(
                    fast.request_evaluations
                        .values()
                        .all(|state| state.replayed_bars <= 2)
                );
            }
        }
    }

    #[test]
    fn dataset_endpoint_expression_uses_complete_evaluator() {
        let hir = analyze_source(&SourceFile::new("endpoint.pine",
            "//@version=6\nindicator(\"end\")\nplot(request.security(\"B\", \"5\", barstate.islast ? close : close[1]))")).hir.unwrap();
        let key = RequestKey::new("B", RequestTimeframe::parse("5").unwrap());
        let mut provider = InMemoryRequestDataProvider::new();
        provider
            .insert(key, vec![bar(0, 1.), bar(300_000, 2.)])
            .unwrap();
        let environment = RequestEnvironment::new(
            ChartContext::new("A", RequestTimeframe::parse("1").unwrap()),
            Arc::new(provider),
        );
        let mut runtime = HistoricalRuntime::with_request_environment(&hir, environment);
        runtime.append_bar(bar(240_000, 1.)).unwrap();
        assert!(runtime.request_evaluations.is_empty());
    }

    #[test]
    fn live_udf_tuple_checkpoint_matches_full_replay_with_local_counter() {
        let source = SourceFile::new(
            "udf.pine",
            "//@version=6\nindicator(\"udf\")\ncount(bool anchor) =>\n    var int n = 0\n    if anchor\n        n += 1\n    n\nvalues() =>\n    bool anchor = timeframe.change(\"5\") and year(time_tradingday, syminfo.timezone) % 1 == 0\n    [ta.pivot_point_levels(\"Traditional\", anchor), ta.pivot_point_levels(\"Traditional\", anchor, developing=true), count(anchor)]\n[a,b,n] = request.security(\"B\", \"5\", values(), lookahead=barmerge.lookahead_on)\nplot(array.get(a, 0))\nplot(array.get(b, 0))\nplot(n)\n",
        );
        let hir = analyze_source(&source).hir.unwrap();
        let key = RequestKey::new("B", RequestTimeframe::parse("5").unwrap());
        let provider = InMemoryRequestDataProvider::from_streams(vec![(
            key.clone(),
            (0..128)
                .map(|i| bar(i * 300_000, 10.0 + i as f64))
                .collect(),
        )])
        .unwrap();
        let environment = RequestEnvironment::new(
            ChartContext::new("A", RequestTimeframe::parse("1").unwrap()),
            Arc::new(provider),
        );
        let mut fast = HistoricalRuntime::with_request_environment(&hir, environment.clone());
        let mut full = HistoricalRuntime::with_request_environment(&hir, environment);
        fast.append_bar(bar(0, 1.0)).unwrap();
        full.append_bar(bar(0, 1.0)).unwrap();
        for index in 128..144 {
            for bump in [0.0, 0.5, 1.0] {
                let requested = bar(index * 300_000, index as f64 + bump);
                fast.apply_request_update(key.clone(), BarUpdate::forming(requested))
                    .unwrap();
                full.apply_request_update(key.clone(), BarUpdate::forming(requested))
                    .unwrap();
                full.clear_request_evaluations();
                let chart = bar(index * 300_000 + 240_000, 1.0);
                fast.append_bar_with_kind(chart, BarUpdateKind::Forming)
                    .unwrap();
                full.append_bar_with_kind(chart, BarUpdateKind::Forming)
                    .unwrap();
                assert_eq!(fast.result().plots, full.result().plots, "{index}/{bump}");
                assert!(!fast.request_evaluations.is_empty());
                assert!(
                    fast.request_evaluations
                        .values()
                        .all(|state| state.replayed_bars <= 2)
                );
            }
            let requested = bar(index * 300_000, index as f64 + 1.0);
            fast.apply_request_update(key.clone(), BarUpdate::confirmed(requested))
                .unwrap();
            full.apply_request_update(key.clone(), BarUpdate::confirmed(requested))
                .unwrap();
        }
    }

    #[test]
    fn endpoint_read_inside_udf_branch_keeps_complete_evaluator() {
        let source = SourceFile::new(
            "endpoint-udf.pine",
            "//@version=6\nindicator(\"end\")\nf() =>\n    var float n = 0\n    if barstate.islast\n        n += close\n    n\nplot(request.security(\"B\", \"5\", f(), lookahead=barmerge.lookahead_on))\n",
        );
        let hir = analyze_source(&source).hir.unwrap();
        let key = RequestKey::new("B", RequestTimeframe::parse("5").unwrap());
        let provider = InMemoryRequestDataProvider::from_streams(vec![(
            key,
            vec![bar(0, 1.0), bar(300_000, 2.0)],
        )])
        .unwrap();
        let environment = RequestEnvironment::new(
            ChartContext::new("A", RequestTimeframe::parse("1").unwrap()),
            Arc::new(provider),
        );
        let mut runtime = HistoricalRuntime::with_request_environment(&hir, environment);
        runtime.append_bar(bar(240_000, 1.0)).unwrap();
        assert!(runtime.request_evaluations.is_empty());
    }

    #[test]
    fn string_endpoint_reads_inside_arguments_and_udf_branches_keep_complete_evaluator() {
        for (expression, expected_values) in [
            (
                "str.tonumber(str.tostring(barstate.islast ? close : close[1]))",
                [4.0, 6.0, 5.0],
            ),
            (
                "str.length(str.match(str.tostring(close), barstate.islast ? \"[0-9]+\" : \".\"))",
                [1.0, 1.0, 1.0],
            ),
            ("f()", [2.0, 2.0, 2.0]),
        ] {
            let source = SourceFile::new(
                "string-endpoint.pine",
                format!(
                    "//@version=6\nindicator(\"string endpoint\")\nf() =>\n    string result = str.tostring(close)\n    if barstate.islast\n        result := str.tostring(close[1])\n    str.tonumber(result)\nplot(request.security(\"B\", \"5\", {expression}, lookahead=barmerge.lookahead_on))\n"
                ),
            );
            let analysis = analyze_source(&source);
            assert!(
                analysis.diagnostics.is_empty(),
                "{expression}: {:?}",
                analysis.diagnostics
            );
            let hir = analysis.hir.unwrap();
            let key = RequestKey::new("B", RequestTimeframe::parse("5").unwrap());
            let provider = InMemoryRequestDataProvider::from_streams(vec![(
                key.clone(),
                vec![bar(0, 1.0), bar(300_000, 2.0)],
            )])
            .unwrap();
            let environment = RequestEnvironment::new(
                ChartContext::new("A", RequestTimeframe::parse("1").unwrap()),
                Arc::new(provider),
            );
            let mut runtime = HistoricalRuntime::with_request_environment(&hir, environment);
            runtime.append_bar(bar(240_000, 1.0)).unwrap();
            for (close, expected) in [4.0, 6.0, 5.0].into_iter().zip(expected_values) {
                runtime
                    .apply_request_update(key.clone(), BarUpdate::forming(bar(600_000, close)))
                    .unwrap();
                runtime
                    .append_bar_with_kind(bar(840_000, 1.0), BarUpdateKind::Forming)
                    .unwrap();
                assert!(runtime.request_evaluations.is_empty(), "{expression}");
                assert_eq!(
                    runtime.result().plots[0]
                        .values
                        .last()
                        .and_then(PineValue::as_f64),
                    Some(expected),
                    "{expression}/{close}"
                );
            }
            runtime
                .apply_request_update(key.clone(), BarUpdate::confirmed(bar(600_000, 5.0)))
                .unwrap();
            runtime
                .append_bar_with_kind(bar(840_000, 1.0), BarUpdateKind::Confirmed)
                .unwrap();
            assert!(
                runtime.request_evaluations.is_empty(),
                "{expression}/confirmed"
            );
            assert_eq!(
                runtime.result().plots[0]
                    .values
                    .last()
                    .and_then(PineValue::as_f64),
                Some(expected_values[2]),
                "{expression}/confirmed"
            );
        }
    }
}
