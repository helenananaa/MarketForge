use std::collections::BTreeSet;

use pine_ir::{
    CallSiteId, HirCallArg, HirExpr, HirExprKind, HirLiteral, HirProgram, HirStmtKind, PineType,
    Qualifier, ValueKind,
};

use super::{CallFamily, CallPlan};
use crate::builtins::math::MathOpcode;
use crate::{Bar, BarUpdate, HistoricalRuntime, PineValue, PreparedProgram, RealtimeRuntime};

fn program(text: &str) -> HirProgram {
    let source = pine_syntax::SourceFile::new("math_dispatch.pine", text);
    let analysis = pine_sema::analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.unwrap()
}

fn plotted_value(program: &mut HirProgram, index: usize) -> &mut HirExpr {
    program
        .statements
        .iter_mut()
        .filter_map(|statement| {
            let HirStmtKind::Expr(expr) = &mut statement.kind else {
                return None;
            };
            let HirExprKind::Call { callee, args, .. } = &mut expr.kind else {
                return None;
            };
            if callee == "plot" {
                args.first_mut().map(|arg| &mut arg.value)
            } else {
                None
            }
        })
        .nth(index)
        .unwrap()
}

fn set_call_site(expr: &mut HirExpr, site: CallSiteId) {
    let HirExprKind::Call { call_site_id, .. } = &mut expr.kind else {
        panic!("expected call");
    };
    *call_site_id = site;
}

fn int_arg(value: i64) -> HirCallArg {
    HirCallArg {
        name: None,
        value: HirExpr {
            kind: HirExprKind::Literal(HirLiteral::Int(value)),
            pine_type: PineType::new(Qualifier::Const, ValueKind::Int),
            series_id: None,
        },
    }
}

fn error_expr() -> HirExpr {
    let errors = program(
        "//@version=6\nindicator(\"error argument\")\nruntime.error(\"argument was executed\")\n",
    );
    let HirStmtKind::Expr(error) = &errors.statements[1].kind else {
        panic!("expected error call");
    };
    let mut error = error.clone();
    set_call_site(&mut error, CallSiteId(u32::MAX - 10));
    error
}

fn bar(index: i64, close: f64) -> Bar {
    Bar {
        time: index * 60_000,
        open: close,
        high: close,
        low: close,
        close,
        volume: 100.0,
    }
}

#[test]
fn opcodes_cover_the_independent_builtin_registry_without_accepting_prefix_matches() {
    let names: BTreeSet<_> = pine_builtins::PHASE_1_BUILTINS
        .iter()
        .map(|signature| signature.name)
        .filter(|name| name.starts_with("math."))
        .collect();
    assert_eq!(names.len(), 27);
    let selected: BTreeSet<_> = names
        .iter()
        .map(|name| {
            MathOpcode::for_name(name)
                .unwrap_or_else(|| panic!("registered math call has no opcode: {name}"))
                as u8
        })
        .collect();
    assert_eq!(selected.len(), names.len());
    for name in [
        "math",
        "math.unknown_opcode",
        "math.abs.extra",
        "math.pi",
        "math.e",
        "math.phi",
        "math.rphi",
        "ta.sum",
        "abs",
        "sum",
    ] {
        assert_eq!(MathOpcode::for_name(name), None, "{name}");
    }
}

#[test]
fn sparse_binding_callee_guard_and_unrecorded_id_select_the_actual_math_operation() {
    let mut program = program("//@version=6\nindicator(\"opcode\")\nplot(math.pow(3, 2))\n");
    set_call_site(plotted_value(&mut program, 0), CallSiteId(u32::MAX));
    program.next_call_site_id = u32::MAX;
    let plan = CallPlan::from_program(&program);
    assert!(plan.dense.len() <= 6);
    assert!(plan.sparse.contains_key(&CallSiteId(u32::MAX)));
    assert_eq!(
        plan.dispatch(CallSiteId(u32::MAX), "math.pow", &[])
            .math_opcode,
        Some(MathOpcode::Pow)
    );
    let args = [int_arg(-7)];
    for site in [CallSiteId(u32::MAX), CallSiteId(u32::MAX - 1)] {
        let dispatch = plan.dispatch(site, "math.abs", &args);
        assert_eq!(dispatch.family, CallFamily::Math);
        assert_eq!(dispatch.math_opcode, Some(MathOpcode::Abs));
    }
    let prepared = PreparedProgram::new(program);
    let mut runtime = HistoricalRuntime::from_prepared(&prepared);
    runtime.append_bar(bar(0, 1.0)).unwrap();
    assert_eq!(runtime.result().plots[0].values, [PineValue::Float(9.0)]);
    for site in [CallSiteId(u32::MAX), CallSiteId(u32::MAX - 1)] {
        assert_eq!(
            runtime.eval_call("math.abs", site, &args).unwrap(),
            PineValue::Int(7)
        );
    }
}

#[test]
fn conflicting_same_and_other_family_callees_keep_their_distinct_results() {
    let mut program = program(
        "//@version=6\nindicator(\"opcode\")\nplot(math.floor(close))\nplot(math.ceil(close))\nplot(str.length(\"abc\"))\n",
    );
    for index in 0..3 {
        set_call_site(plotted_value(&mut program, index), CallSiteId(u32::MAX));
    }
    let prepared = PreparedProgram::new(program);
    assert!(
        prepared
            .metadata
            .calls
            .binding(CallSiteId(u32::MAX))
            .is_none()
    );
    let mut runtime = HistoricalRuntime::from_prepared(&prepared);
    runtime.append_bars(&[bar(0, 1.75), bar(1, 2.25)]).unwrap();
    let result = runtime.result();
    assert_eq!(
        result.plots[0].values,
        [PineValue::Int(1), PineValue::Int(2)]
    );
    assert_eq!(
        result.plots[1].values,
        [PineValue::Int(2), PineValue::Int(3)]
    );
    assert_eq!(result.plots[2].values, vec![PineValue::Int(3); 2]);
}

#[test]
fn unknown_guarded_and_unrecorded_calls_do_not_evaluate_error_arguments() {
    let mut program = program("//@version=6\nindicator(\"opcode\")\nplot(math.abs(close))\n");
    let HirExprKind::Call { call_site_id, .. } = &plotted_value(&mut program, 0).kind else {
        panic!("expected math call");
    };
    let bound_site = *call_site_id;
    let args = [HirCallArg {
        name: None,
        value: error_expr(),
    }];
    let mut runtime = HistoricalRuntime::new(&program);
    for site in [bound_site, CallSiteId(u32::MAX)] {
        for name in ["math.unknown_opcode", "math.abs.extra", "math.pi", "abs"] {
            let error = runtime.eval_call(name, site, &args).unwrap_err();
            assert_eq!(error.message, format!("unsupported runtime call `{name}`"));
        }
    }
}

#[test]
fn reused_id_with_reordered_named_arguments_keeps_binding_and_side_effect_order() {
    let mut program = program(
        r#"//@version=6
indicator("named math side effects")
var trace = array.new_int()
mark(float value, int tag) =>
    array.push(trace, tag)
    value
plot(math.pow(mark(3, 1), mark(2, 2)))
plot(math.pow(mark(3, 3), mark(2, 4)))
plot(array.size(trace))
plot(array.get(trace, 0) * 1000 + array.get(trace, 1) * 100 + array.get(trace, 2) * 10 + array.get(trace, 3))
"#,
    );
    for index in 0..2 {
        let HirExprKind::Call {
            call_site_id, args, ..
        } = &mut plotted_value(&mut program, index).kind
        else {
            panic!("expected math call");
        };
        *call_site_id = CallSiteId(u32::MAX);
        if index == 1 {
            args[0].name = Some("base".to_owned());
            args[1].name = Some("exponent".to_owned());
            args.reverse();
        }
    }
    let prepared = PreparedProgram::new(program);
    let dispatch = prepared
        .metadata
        .calls
        .binding(CallSiteId(u32::MAX))
        .unwrap()
        .dispatch;
    assert_eq!(dispatch.math_opcode, Some(MathOpcode::Pow));
    assert!(!dispatch.positional_args);
    let mut runtime = HistoricalRuntime::from_prepared(&prepared);
    runtime.append_bar(bar(0, 1.0)).unwrap();
    let result = runtime.result();
    assert_eq!(result.plots[0].values, [PineValue::Float(9.0)]);
    assert_eq!(result.plots[1].values, [PineValue::Float(9.0)]);
    assert_eq!(result.plots[2].values, [PineValue::Int(4)]);
    assert_eq!(result.plots[3].values, [PineValue::Int(1234)]);
}

#[test]
fn omitted_slots_do_not_evaluate_poisoned_expressions_or_override_defaults() {
    let mut program = program(
        "//@version=6\nindicator(\"omitted math\")\nplot(math.random(seed=7))\nplot(math.random(0, 1, 7))\nplot(math.round(close))\n",
    );
    let error = error_expr();
    let HirExprKind::Call { args, .. } = &mut plotted_value(&mut program, 0).kind else {
        panic!("expected random call");
    };
    assert_eq!(args.len(), 3);
    for arg in &mut args[..2] {
        assert_eq!(arg.name.as_deref(), Some(pine_ir::OMITTED_BUILTIN_ARG));
        arg.value = error.clone();
    }
    let HirExprKind::Call { args, .. } = &mut plotted_value(&mut program, 2).kind else {
        panic!("expected round call");
    };
    args.push(HirCallArg {
        name: Some(pine_ir::OMITTED_BUILTIN_ARG.to_owned()),
        value: error,
    });
    let prepared = PreparedProgram::new(program);
    let mut selected = Vec::new();
    crate::runtime::hir_walk::statements(&prepared.statements, &mut |expr| {
        if let HirExprKind::Call {
            callee,
            call_site_id,
            args,
        } = &expr.kind
            && matches!(callee.as_str(), "math.random" | "math.round")
        {
            let dispatch = prepared
                .metadata
                .calls
                .dispatch(*call_site_id, callee, args);
            assert!(dispatch.positional_args);
            selected.push(dispatch.math_opcode.unwrap());
        }
    });
    assert_eq!(
        selected,
        [MathOpcode::Random, MathOpcode::Random, MathOpcode::Round]
    );
    let mut runtime = HistoricalRuntime::from_prepared(&prepared);
    runtime
        .append_bars(&[bar(0, 1.4), bar(1, 2.5), bar(2, 3.6)])
        .unwrap();
    let result = runtime.result();
    assert_eq!(result.plots[0].values, result.plots[1].values);
    assert!(result.plots[0].values.iter().all(|value| {
        value
            .as_f64()
            .is_some_and(|number| (0.0..1.0).contains(&number))
    }));
    assert_eq!(
        result.plots[2].values,
        [PineValue::Int(1), PineValue::Int(3), PineValue::Int(4)]
    );
}

#[test]
fn guarded_and_unrecorded_random_calls_keep_independent_streams_and_invalid_ranges_do_not_advance()
{
    let mut program = program("//@version=6\nindicator(\"random state\")\nplot(math.abs(close))\n");
    let HirExprKind::Call { call_site_id, .. } = &plotted_value(&mut program, 0).kind else {
        panic!("expected math call");
    };
    let bound_site = *call_site_id;
    let prepared = PreparedProgram::new(program);
    let valid = [int_arg(0), int_arg(1), int_arg(7)];
    let invalid = [int_arg(5), int_arg(5), int_arg(7)];
    let mut runtime = HistoricalRuntime::from_prepared(&prepared);
    for site in [bound_site, CallSiteId(u32::MAX)] {
        // Each independent reference has only one stream. A shared opcode-keyed
        // stream in the candidate cannot pass by sharing the same mistake here.
        let mut reference = HistoricalRuntime::from_prepared(&prepared);
        let first = runtime.eval_call("math.random", site, &valid).unwrap();
        assert_eq!(
            first,
            reference.eval_call("math.random", site, &valid).unwrap()
        );
        assert!(
            first
                .as_f64()
                .is_some_and(|value| (0.0..1.0).contains(&value))
        );
        assert_eq!(
            runtime.eval_call("math.random", site, &invalid).unwrap(),
            PineValue::Na
        );
        let second = runtime.eval_call("math.random", site, &valid).unwrap();
        assert_eq!(
            second,
            reference.eval_call("math.random", site, &valid).unwrap()
        );
        assert_ne!(first, second);
    }
    let mut defaults = Vec::new();
    for site in [CallSiteId(u32::MAX - 1), CallSiteId(u32::MAX - 2)] {
        let mut reference = HistoricalRuntime::from_prepared(&prepared);
        let value = runtime.eval_call("math.random", site, &[]).unwrap();
        assert_eq!(
            value,
            reference.eval_call("math.random", site, &[]).unwrap()
        );
        defaults.push(value);
    }
    assert_ne!(defaults[0], defaults[1]);
}

#[test]
fn colliding_sum_and_ta_calls_keep_separate_windows_across_forming_replacements() {
    let mut program = program(
        r#"//@version=6
indicator("math window state")
plot(math.sum(bar_index == 1 ? na : close, bar_index % 2 + 2))
plot(ta.sma(close, 2))
plot(math.sum(close * 10, 2))
"#,
    );
    for index in 0..2 {
        set_call_site(plotted_value(&mut program, index), CallSiteId(u32::MAX));
    }
    set_call_site(plotted_value(&mut program, 2), CallSiteId(u32::MAX - 1));
    program.next_call_site_id = u32::MAX;
    let prepared = PreparedProgram::new(program);
    assert!(
        prepared
            .metadata
            .calls
            .binding(CallSiteId(u32::MAX))
            .is_none()
    );
    assert!(
        prepared
            .metadata
            .calls
            .sparse
            .contains_key(&CallSiteId(u32::MAX - 1))
    );
    let mut historical = HistoricalRuntime::from_prepared(&prepared);
    historical
        .append_bars(&[bar(0, 1.0), bar(1, 2.0), bar(2, 3.0), bar(3, 4.0)])
        .unwrap();
    let result = historical.result();
    assert_eq!(
        result.plots[0].values,
        [
            PineValue::Na,
            PineValue::Na,
            PineValue::Float(4.0),
            PineValue::Float(8.0)
        ]
    );
    assert_eq!(
        result.plots[1].values,
        [
            PineValue::Na,
            PineValue::Float(1.5),
            PineValue::Float(2.5),
            PineValue::Float(3.5)
        ]
    );
    assert_eq!(
        result.plots[2].values,
        [
            PineValue::Na,
            PineValue::Float(30.0),
            PineValue::Float(50.0),
            PineValue::Float(70.0)
        ]
    );
    let mut realtime = RealtimeRuntime::from_prepared(&prepared);
    realtime
        .seed_historical(&[bar(0, 1.0), bar(1, 2.0)])
        .unwrap();
    for (close, confirmed) in [(3.0, false), (4.0, false), (5.0, true)] {
        let update = if confirmed {
            BarUpdate::confirmed(bar(2, close))
        } else {
            BarUpdate::forming(bar(2, close))
        };
        realtime.apply_update(update).unwrap();
        let result = realtime.result();
        for (plot, expected) in
            result
                .plots
                .iter()
                .zip([close + 1.0, (close + 2.0) / 2.0, (close + 2.0) * 10.0])
        {
            assert_eq!(plot.values.last(), Some(&PineValue::Float(expected)));
        }
    }
}
