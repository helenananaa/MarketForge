//! Bind call namespaces and argument layouts once, retaining manual-HIR compatibility.
use std::collections::HashMap;

use pine_ir::{CallSiteId, HirCallArg, HirExprKind, HirProgram};

use crate::builtins::arrays::ArrayOpcode;
use crate::builtins::math::MathOpcode;
use crate::builtins::ta::TaOpcode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CallFamily {
    Legacy,
    Variable,
    RuntimeError,
    Alert,
    Output,
    Drawing,
    ChartPoint,
    Request,
    Strategy,
    Color,
    String,
    Syminfo,
    Ticker,
    Time,
    Cast,
    Math,
    Ta,
    Array,
    Map,
    Matrix,
    Unsupported,
}

impl CallFamily {
    pub(crate) fn for_name(callee: &str) -> Self {
        match callee {
            "$legacy.iff" | "$legacy.rsi_series" => Self::Legacy,
            "$legacy.security.gaps_off.lookahead_off"
            | "$legacy.security.gaps_on.lookahead_off"
            | "$legacy.security.gaps_off.lookahead_on"
            | "$legacy.security.gaps_on.lookahead_on" => Self::Request,
            "indicator" | "strategy" | "max_bars_back" | "input" | "na" | "nz" | "fixnan" => {
                Self::Variable
            }
            "runtime.error" => Self::RuntimeError,
            "alert" | "alertcondition" => Self::Alert,
            "plot" | "plotchar" | "plotshape" | "plotarrow" | "plotbar" | "plotcandle"
            | "bgcolor" | "barcolor" | "hline" | "fill" => Self::Output,
            "year" | "month" | "weekofyear" | "dayofmonth" | "dayofweek" | "hour" | "minute"
            | "second" | "timestamp" | "time" | "time_close" => Self::Time,
            "int" | "float" | "bool" | "string" | "box" | "color" | "label" | "line"
            | "linefill" | "polyline" | "table" => Self::Cast,
            _ if callee.starts_with("chart.point.") => Self::ChartPoint,
            _ => match callee.split_once('.').map(|(namespace, _)| namespace) {
                Some("input") => Self::Variable,
                Some("label" | "line" | "linefill" | "polyline" | "box" | "table") => Self::Drawing,
                Some("request") => Self::Request,
                Some("strategy") => Self::Strategy,
                Some("color") => Self::Color,
                Some("str") => Self::String,
                Some("syminfo") => Self::Syminfo,
                Some("ticker") => Self::Ticker,
                Some("timeframe") => Self::Time,
                Some("math") => Self::Math,
                Some("ta") => Self::Ta,
                Some("array") => Self::Array,
                Some("map") => Self::Map,
                Some("matrix") => Self::Matrix,
                _ => Self::Unsupported,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CallDispatch {
    pub(crate) family: CallFamily,
    pub(crate) state_site: CallSiteId,
    pub(crate) positional_args: bool,
    pub(crate) ta_opcode: Option<TaOpcode>,
    pub(crate) math_opcode: Option<MathOpcode>,
    pub(crate) array_opcode: Option<ArrayOpcode>,
}

impl CallDispatch {
    fn for_call(site: CallSiteId, callee: &str, args: &[HirCallArg]) -> Self {
        let family = CallFamily::for_name(callee);
        Self {
            family,
            state_site: site,
            positional_args: positional_layout(args),
            ta_opcode: if family == CallFamily::Ta {
                TaOpcode::for_name(callee)
            } else {
                None
            },
            math_opcode: if family == CallFamily::Math {
                MathOpcode::for_name(callee)
            } else {
                None
            },
            array_opcode: if family == CallFamily::Array {
                ArrayOpcode::for_name(callee)
            } else {
                None
            },
        }
    }
}

#[derive(Debug)]
struct CallBinding {
    callee: String,
    dispatch: CallDispatch,
}

#[derive(Debug, Default)]
pub(crate) struct CallPlan {
    dense: Vec<Option<CallBinding>>,
    sparse: HashMap<CallSiteId, Option<CallBinding>>,
    ta_state_sites: HashMap<(CallSiteId, TaOpcode), CallSiteId>,
}

impl CallPlan {
    pub(crate) fn from_program(program: &HirProgram) -> Self {
        let mut bindings: HashMap<CallSiteId, Option<CallBinding>> = HashMap::new();
        super::hir_walk::statements(&program.statements, &mut |expr| {
            if let HirExprKind::Call {
                callee,
                call_site_id,
                args,
            } = &expr.kind
            {
                let dispatch = CallDispatch::for_call(*call_site_id, callee, args);
                bindings
                    .entry(*call_site_id)
                    .and_modify(|binding| {
                        if let Some(bound) = binding {
                            if bound.callee != *callee {
                                *binding = None;
                            } else {
                                // A manual HIR may reuse an ID for the same callee
                                // with different argument names. Keep every such
                                // call on the general named-argument lookup path.
                                bound.dispatch.positional_args &= dispatch.positional_args;
                            }
                        }
                    })
                    .or_insert_with(|| {
                        Some(CallBinding {
                            callee: callee.clone(),
                            dispatch,
                        })
                    });
            }
        });

        // Frontend IDs normally form a dense prefix, with possible holes after
        // lowering. Bound the prefix by actual call count rather than trusting
        // next_call_site_id or a possibly huge ID in manually constructed HIR.
        let dense_len = bindings
            .keys()
            .map(|site| site.0 as usize)
            .max()
            .map_or(0, |last| {
                last.saturating_add(1).min(bindings.len().saturating_mul(2))
            });
        let mut plan = Self {
            dense: std::iter::repeat_with(|| None).take(dense_len).collect(),
            sparse: HashMap::new(),
            ta_state_sites: Self::ta_state_sites(program, &bindings),
        };
        for (site, binding) in bindings {
            if let Some(slot) = plan.dense.get_mut(site.0 as usize) {
                *slot = binding;
            } else {
                plan.sparse.insert(site, binding);
            }
        }
        plan
    }

    fn ta_state_sites(
        program: &HirProgram,
        bindings: &HashMap<CallSiteId, Option<CallBinding>>,
    ) -> HashMap<(CallSiteId, TaOpcode), CallSiteId> {
        let mut aliases = HashMap::new();
        if !bindings.values().any(Option::is_none) {
            return aliases;
        }
        let mut conflicts = Vec::new();
        super::hir_walk::statements(&program.statements, &mut |expr| {
            if let HirExprKind::Call {
                callee,
                call_site_id,
                ..
            } = &expr.kind
                && bindings.get(call_site_id).is_some_and(Option::is_none)
                && let Some(opcode) = TaOpcode::for_name(callee)
            {
                conflicts.push((*call_site_id, opcode));
            }
        });
        conflicts.sort_unstable_by_key(|(site, opcode)| (site.0, *opcode as usize));
        conflicts.dedup();
        // Use unused holes, excluding every HIR call ID, including non-TA
        // calls. Aliasing every TA callee also isolates shared call_state
        // storage from a conflicting non-TA call such as fixnan.
        let mut next = 0_u64;
        for conflict in conflicts {
            while next <= u64::from(u32::MAX) && bindings.contains_key(&CallSiteId(next as u32)) {
                next += 1;
            }
            let alias =
                CallSiteId(u32::try_from(next).expect("call-site state ID space exhausted"));
            aliases.insert(conflict, alias);
            next += 1;
        }
        aliases
    }

    fn binding(&self, site: CallSiteId) -> Option<&CallBinding> {
        if let Some(binding) = self.dense.get(site.0 as usize) {
            binding.as_ref()
        } else {
            self.sparse.get(&site).and_then(Option::as_ref)
        }
    }

    pub(crate) fn dispatch(
        &self,
        site: CallSiteId,
        callee: &str,
        args: &[HirCallArg],
    ) -> CallDispatch {
        self.binding(site)
            .filter(|binding| binding.callee == callee)
            .map_or_else(
                || {
                    let mut dispatch = CallDispatch::for_call(site, callee, args);
                    if let Some(opcode) = dispatch.ta_opcode
                        && let Some(alias) = self.ta_state_sites.get(&(site, opcode))
                    {
                        dispatch.state_site = *alias;
                    }
                    dispatch
                },
                |binding| binding.dispatch,
            )
    }
}

fn positional_layout(args: &[HirCallArg]) -> bool {
    args.iter().all(|arg| {
        arg.name
            .as_deref()
            .is_none_or(|name| name == pine_ir::OMITTED_BUILTIN_ARG)
    })
}

#[cfg(test)]
#[path = "ta_dispatch_tests.rs"]
mod ta_dispatch_tests;

#[cfg(test)]
#[path = "ta_state_plan_tests.rs"]
mod ta_state_plan_tests;

#[cfg(test)]
#[path = "math_dispatch_tests.rs"]
mod math_dispatch_tests;

#[cfg(test)]
#[path = "array_dispatch_tests.rs"]
mod array_dispatch_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn program(text: &str) -> HirProgram {
        let source = pine_syntax::SourceFile::new("calls.pine", text);
        let analysis = pine_sema::analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        analysis.hir.unwrap()
    }

    #[test]
    fn conflicting_manual_call_site_ids_do_not_dispatch_to_the_wrong_namespace() {
        let source = pine_syntax::SourceFile::new(
            "calls.pine",
            "//@version=6\nindicator(\"calls\")\nplot(math.abs(close))\n",
        );
        let mut program = pine_sema::analyze_source(&source).hir.unwrap();
        for statement in &mut program.statements {
            if let pine_ir::HirStmtKind::Expr(expr) = &mut statement.kind
                && let HirExprKind::Call { call_site_id, .. } = &mut expr.kind
            {
                *call_site_id = CallSiteId(0);
            }
        }
        let plan = CallPlan::from_program(&program);
        assert!(plan.binding(CallSiteId(0)).is_none());
        assert_eq!(
            plan.dispatch(CallSiteId(u32::MAX), "ta.sma", &[]).family,
            CallFamily::Ta
        );
        assert_eq!(
            plan.dispatch(CallSiteId(0), "str.length", &[]).family,
            CallFamily::String
        );
        assert_eq!(CallFamily::for_name("strategy"), CallFamily::Variable);
        for gaps in ["off", "on"] {
            for lookahead in ["off", "on"] {
                assert_eq!(
                    CallFamily::for_name(&format!(
                        "$legacy.security.gaps_{gaps}.lookahead_{lookahead}"
                    )),
                    CallFamily::Request
                );
            }
        }
        assert_eq!(CallFamily::for_name("strategy.entry"), CallFamily::Strategy);
        assert_eq!(
            CallFamily::for_name("chart.point.new"),
            CallFamily::ChartPoint
        );
    }

    #[test]
    fn dense_prefix_is_bounded_with_holes_and_sparse_manual_ids() {
        let mut program = program(
            "//@version=6\nindicator(\"calls\")\nplot(math.abs(close))\nplot(math.pow(close, 2))\n",
        );
        let mut next = 0_u32;
        for statement in &mut program.statements {
            if let pine_ir::HirStmtKind::Expr(expr) = &mut statement.kind
                && let HirExprKind::Call { call_site_id, .. } = &mut expr.kind
            {
                *call_site_id = if next == 4 {
                    CallSiteId(u32::MAX)
                } else {
                    CallSiteId(next)
                };
                next += 2;
            }
        }
        program.next_call_site_id = u32::MAX;
        let plan = CallPlan::from_program(&program);
        assert!(plan.dense.len() <= 2 * 5);
        assert!(plan.sparse.contains_key(&CallSiteId(u32::MAX)));
        super::super::hir_walk::statements(&program.statements, &mut |expr| {
            if let HirExprKind::Call {
                callee,
                call_site_id,
                args,
            } = &expr.kind
            {
                assert_eq!(
                    plan.dispatch(*call_site_id, callee, args).family,
                    CallFamily::for_name(callee)
                );
            }
        });
        assert_eq!(
            plan.dispatch(CallSiteId(u32::MAX - 1), "not_a_builtin", &[])
                .family,
            CallFamily::Unsupported
        );
        assert!(plan.binding(CallSiteId(9)).is_none());
    }

    #[test]
    fn reused_callee_id_with_different_layouts_preserves_named_binding() {
        let mut program = program(
            "//@version=6\nindicator(\"calls\")\nplot(math.pow(3, 2))\nplot(math.pow(3, 2))\n",
        );
        let mut changed = false;
        for statement in &mut program.statements {
            if let pine_ir::HirStmtKind::Expr(expr) = &mut statement.kind
                && let HirExprKind::Call { args, .. } = &mut expr.kind
                && let Some(arg) = args.first_mut()
                && let HirExprKind::Call {
                    callee,
                    call_site_id,
                    args,
                } = &mut arg.value.kind
                && callee == "math.pow"
            {
                *call_site_id = CallSiteId(u32::MAX);
                if changed {
                    args[0].name = Some("base".to_owned());
                    args[1].name = Some("exponent".to_owned());
                    args.reverse();
                }
                changed = true;
            }
        }
        let prepared = crate::PreparedProgram::new(program);
        assert!(
            !prepared
                .metadata
                .calls
                .binding(CallSiteId(u32::MAX))
                .unwrap()
                .dispatch
                .positional_args
        );
        let mut runtime = crate::HistoricalRuntime::from_prepared(&prepared);
        runtime
            .append_bar(crate::Bar {
                time: 0,
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume: 1.0,
            })
            .unwrap();
        for plot in runtime.result().plots {
            assert_eq!(plot.values, vec![crate::PineValue::Float(9.0)]);
        }
    }

    #[test]
    fn omitted_builtin_slots_use_prepared_positions_and_keep_defaults() {
        let program = program(
            "//@version=6\nindicator(\"calls\")\nplot(math.random(seed=7))\nplot(math.random(0, 1, 7))\n",
        );
        let prepared = crate::PreparedProgram::new(program);
        super::super::hir_walk::statements(&prepared.statements, &mut |expr| {
            if let HirExprKind::Call {
                callee,
                call_site_id,
                args,
            } = &expr.kind
                && callee == "math.random"
            {
                assert!(
                    prepared
                        .metadata
                        .calls
                        .dispatch(*call_site_id, callee, args)
                        .positional_args
                );
            }
        });
        let mut runtime = crate::HistoricalRuntime::from_prepared(&prepared);
        for time in 0..3 {
            runtime
                .append_bar(crate::Bar {
                    time: time * 60_000,
                    open: 1.0,
                    high: 1.0,
                    low: 1.0,
                    close: 1.0,
                    volume: 1.0,
                })
                .unwrap();
        }
        let result = runtime.result();
        assert_eq!(result.plots[0].values, result.plots[1].values);
        assert!(
            result.plots[0]
                .values
                .iter()
                .all(|value| value.as_f64().is_some())
        );
    }
}
