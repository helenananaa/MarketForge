use std::collections::BTreeSet;

use pine_ir::{CallSiteId, HirCallArg, HirExpr, HirExprKind, HirProgram, HirStmtKind};

use super::{CallFamily, CallPlan};
use crate::builtins::ta::TaOpcode;
use crate::{Bar, BarUpdate, HistoricalRuntime, PineValue, PreparedProgram, RealtimeRuntime};

fn program(text: &str) -> HirProgram {
    let source = pine_syntax::SourceFile::new("ta_dispatch.pine", text);
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

fn bar(index: i64, close: f64) -> Bar {
    Bar {
        time: index * 60_000,
        open: 10.0,
        high: close.max(14.0),
        low: close.min(6.0),
        close,
        volume: 100.0,
    }
}

#[test]
fn opcodes_cover_the_independent_builtin_registry_without_accepting_prefix_matches() {
    let names: BTreeSet<_> = pine_builtins::PHASE_1_BUILTINS
        .iter()
        .map(|signature| signature.name)
        .filter(|name| name.starts_with("ta."))
        .collect();
    assert_eq!(names.len(), 64);
    let selected: BTreeSet<_> = names
        .iter()
        .map(|name| {
            TaOpcode::for_name(name)
                .unwrap_or_else(|| panic!("registered TA call has no opcode: {name}"))
                as u8
        })
        .collect();
    assert_eq!(selected.len(), names.len());
    for name in [
        "ta",
        "ta.unknown_opcode",
        "ta.sma.extra",
        "math.sma",
        "$legacy.rsi_series",
    ] {
        assert_eq!(TaOpcode::for_name(name), None, "{name}");
    }
}

#[test]
fn sparse_binding_callee_guard_and_unrecorded_id_select_the_actual_ta_operation() {
    let mut program = program("//@version=6\nindicator(\"opcode\")\nplot(ta.tr(true))\n");
    let HirExprKind::Call { call_site_id, .. } = &mut plotted_value(&mut program, 0).kind else {
        panic!("expected TA call");
    };
    *call_site_id = CallSiteId(u32::MAX);
    program.next_call_site_id = u32::MAX;
    let plan = CallPlan::from_program(&program);
    assert!(plan.dense.len() <= 6);
    assert!(plan.sparse.contains_key(&CallSiteId(u32::MAX)));
    assert_eq!(
        plan.dispatch(CallSiteId(u32::MAX), "ta.tr", &[]).ta_opcode,
        Some(TaOpcode::Tr)
    );
    for site in [CallSiteId(u32::MAX), CallSiteId(u32::MAX - 1)] {
        let dispatch = plan.dispatch(site, "ta.bop", &[]);
        assert_eq!(dispatch.family, CallFamily::Ta);
        assert_eq!(dispatch.ta_opcode, Some(TaOpcode::Bop));
    }
    let prepared = PreparedProgram::new(program);
    let mut runtime = HistoricalRuntime::from_prepared(&prepared);
    runtime.append_bar(bar(0, 11.0)).unwrap();
    assert_eq!(runtime.result().plots[0].values, [PineValue::Float(8.0)]);
    for site in [CallSiteId(u32::MAX), CallSiteId(u32::MAX - 1)] {
        assert_eq!(
            runtime.eval_call("ta.bop", site, &[]).unwrap(),
            PineValue::Float(0.125)
        );
    }
}

#[test]
fn conflicting_same_family_callees_keep_their_distinct_kernel_results() {
    let mut program =
        program("//@version=6\nindicator(\"opcode\")\nplot(ta.bop())\nplot(ta.tr(true))\n");
    for index in 0..2 {
        let HirExprKind::Call { call_site_id, .. } = &mut plotted_value(&mut program, index).kind
        else {
            panic!("expected TA call");
        };
        *call_site_id = CallSiteId(u32::MAX);
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
    for index in 0..3 {
        runtime.append_bar(bar(index, 11.0 + index as f64)).unwrap();
    }
    let result = runtime.result();
    assert_eq!(
        result.plots[0].values,
        [
            PineValue::Float(0.125),
            PineValue::Float(0.25),
            PineValue::Float(0.375)
        ]
    );
    assert_eq!(result.plots[1].values, vec![PineValue::Float(8.0); 3]);
}

#[test]
fn unknown_guarded_and_unrecorded_calls_do_not_evaluate_error_arguments() {
    let mut program = program("//@version=6\nindicator(\"opcode\")\nplot(ta.bop())\n");
    let HirExprKind::Call { call_site_id, .. } = &plotted_value(&mut program, 0).kind else {
        panic!("expected TA call");
    };
    let bound_site = *call_site_id;
    let errors = self::program(
        "//@version=6\nindicator(\"error argument\")\nruntime.error(\"argument was executed\")\n",
    );
    let HirStmtKind::Expr(error) = &errors.statements[1].kind else {
        panic!("expected error call");
    };
    let args = [HirCallArg {
        name: None,
        value: error.clone(),
    }];
    let mut runtime = HistoricalRuntime::new(&program);
    for site in [bound_site, CallSiteId(u32::MAX)] {
        let error = runtime
            .eval_call("ta.unknown_opcode", site, &args)
            .unwrap_err();
        assert_eq!(
            error.message,
            "unsupported runtime call `ta.unknown_opcode`"
        );
    }
}

#[test]
fn conditional_udf_selects_distinct_opcodes_and_preserves_source_side_effects() {
    let program = program(
        r#"//@version=6
indicator("opcode")
var trace = array.new_int()
mark(float value) =>
    array.push(trace, 7)
    value
choose(float value) =>
    value > open ? ta.cum(mark(value)) : ta.bop()
value = choose(close)
plot(value)
plot(array.size(trace))
plot(array.get(trace, array.size(trace) - 1))
"#,
    );
    let prepared = PreparedProgram::new(program);
    let mut selected = Vec::new();
    crate::runtime::hir_walk::statements(&prepared.statements, &mut |expr| {
        if let HirExprKind::Call {
            callee,
            call_site_id,
            args,
        } = &expr.kind
            && callee.starts_with("ta.")
        {
            selected.push(
                prepared
                    .metadata
                    .calls
                    .dispatch(*call_site_id, callee, args)
                    .ta_opcode
                    .unwrap(),
            );
        }
    });
    assert!(selected.contains(&TaOpcode::Cum));
    assert!(selected.contains(&TaOpcode::Bop));
    let mut runtime = HistoricalRuntime::from_prepared(&prepared);
    for (index, close) in [11.0, 9.0, 13.0, 9.0, 15.0].into_iter().enumerate() {
        runtime.append_bar(bar(index as i64, close)).unwrap();
    }
    let result = runtime.result();
    assert_eq!(
        result.plots[0].values,
        [
            PineValue::Float(11.0),
            PineValue::Float(-0.125),
            PineValue::Float(24.0),
            PineValue::Float(-0.125),
            PineValue::Float(39.0)
        ]
    );
    assert_eq!(
        result.plots[1].values,
        [
            PineValue::Int(1),
            PineValue::Int(1),
            PineValue::Int(2),
            PineValue::Int(2),
            PineValue::Int(3)
        ]
    );
    assert_eq!(result.plots[2].values, vec![PineValue::Int(7); 5]);
}

#[test]
fn chart_and_requested_typed_calls_restore_same_bar_state_before_each_replacement() {
    let program = program(
        r#"//@version=6
indicator("opcode")
plot(ta.cum(close))
plot(request.security(syminfo.tickerid, timeframe.period, ta.cum(close)))
"#,
    );
    let prepared = PreparedProgram::new(program);
    let mut runtime = RealtimeRuntime::from_prepared(&prepared);
    runtime
        .seed_historical(&[bar(0, 10.0), bar(1, 20.0)])
        .unwrap();
    for (close, confirmed, expected) in
        [(30.0, false, 60.0), (35.0, false, 65.0), (40.0, true, 70.0)]
    {
        let update = if confirmed {
            BarUpdate::confirmed(bar(2, close))
        } else {
            BarUpdate::forming(bar(2, close))
        };
        runtime.apply_update(update).unwrap();
        for plot in runtime.result().plots {
            assert_eq!(plot.values.last(), Some(&PineValue::Float(expected)));
        }
    }
}
