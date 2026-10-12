use std::collections::BTreeSet;

use pine_ir::{CallSiteId, HirCallArg, HirExpr, HirExprKind, HirProgram, HirStmtKind};

use super::{CallFamily, CallPlan};
use crate::builtins::arrays::{ArrayElementKind, ArrayOpcode};
use crate::{Bar, BarUpdate, HistoricalRuntime, PineValue, PreparedProgram, RealtimeRuntime};

fn program(text: &str) -> HirProgram {
    let source = pine_syntax::SourceFile::new("array_dispatch.pine", text);
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
fn opcodes_cover_the_independent_registry_and_preserve_complete_generic_names() {
    let names: BTreeSet<_> = pine_builtins::PHASE_1_BUILTINS
        .iter()
        .map(|signature| signature.name)
        .filter(|name| name.starts_with("array."))
        .collect();
    assert_eq!(names.len(), 56);
    let selected: BTreeSet<_> = names
        .iter()
        .map(|name| {
            ArrayOpcode::for_name(name)
                .unwrap_or_else(|| panic!("registered array call has no opcode: {name}"))
                as u8
        })
        .collect();
    assert_eq!(selected.len(), names.len());
    for name in [
        "array",
        "array.unknown_opcode",
        "array.size.extra",
        "math.sum",
        "array.new<Point",
        "array.new<Point>.extra",
    ] {
        assert_eq!(ArrayOpcode::for_name(name), None, "{name}");
    }
    assert_eq!(
        ArrayOpcode::for_name("array.new<chart.point>"),
        Some(ArrayOpcode::NewChartPoint)
    );
    for name in [
        "array.new<Point>",
        "array.new<lib.Point>",
        "array.new<float>",
        "array.new<>",
        "array.new<图>",
        "array.new<Point>extra>",
    ] {
        assert_eq!(
            ArrayOpcode::for_name(name),
            Some(ArrayOpcode::NewUserType),
            "{name}"
        );
    }
}

#[test]
fn sparse_binding_callee_guard_and_unrecorded_id_select_the_actual_array_operation() {
    let mut program =
        program("//@version=6\nindicator(\"opcode\")\nplot(array.sum(array.from(1, 2, 3)))\n");
    let HirExprKind::Call {
        call_site_id, args, ..
    } = &mut plotted_value(&mut program, 0).kind
    else {
        panic!("expected array call");
    };
    *call_site_id = CallSiteId(u32::MAX);
    let args = args.clone();
    program.next_call_site_id = u32::MAX;
    let plan = CallPlan::from_program(&program);
    assert!(plan.dense.len() <= 8);
    assert!(plan.sparse.contains_key(&CallSiteId(u32::MAX)));
    assert_eq!(
        plan.dispatch(CallSiteId(u32::MAX), "array.sum", &args)
            .array_opcode,
        Some(ArrayOpcode::Sum)
    );
    let prepared = PreparedProgram::new(program);
    let mut runtime = HistoricalRuntime::from_prepared(&prepared);
    runtime.append_bar(bar(0, 11.0)).unwrap();
    assert_eq!(runtime.result().plots[0].values, [PineValue::Int(6)]);
    for site in [CallSiteId(u32::MAX), CallSiteId(u32::MAX - 1)] {
        let dispatch = prepared.metadata.calls.dispatch(site, "array.size", &args);
        assert_eq!(dispatch.family, CallFamily::Array);
        assert_eq!(dispatch.array_opcode, Some(ArrayOpcode::Size));
        assert_eq!(
            runtime.eval_call("array.size", site, &args).unwrap(),
            PineValue::Int(3)
        );
    }
}

#[test]
fn conflicting_same_family_callees_keep_their_distinct_array_results() {
    let mut program = program(
        "//@version=6\nindicator(\"opcode\")\nvalues = array.from(1, 2, 3)\nplot(array.sum(values))\nplot(array.size(values))\n",
    );
    for index in 0..2 {
        let HirExprKind::Call { call_site_id, .. } = &mut plotted_value(&mut program, index).kind
        else {
            panic!("expected array call");
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
        runtime.append_bar(bar(index, 11.0)).unwrap();
    }
    let result = runtime.result();
    assert_eq!(result.plots[0].values, vec![PineValue::Int(6); 3]);
    assert_eq!(result.plots[1].values, vec![PineValue::Int(3); 3]);
}

#[test]
fn unknown_guarded_and_unrecorded_array_calls_do_not_evaluate_error_arguments() {
    let mut program =
        program("//@version=6\nindicator(\"opcode\")\nplot(array.size(array.from(1)))\n");
    let HirExprKind::Call { call_site_id, .. } = &plotted_value(&mut program, 0).kind else {
        panic!("expected array call");
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
        for callee in [
            "array.unknown_opcode",
            "array.size.extra",
            "array.new<Point",
        ] {
            let error = runtime.eval_call(callee, site, &args).unwrap_err();
            assert_eq!(
                error.message,
                format!("unsupported runtime call `{callee}`")
            );
        }
    }
}

#[test]
fn generic_constructor_callee_guards_preserve_type_metadata_and_chart_point_priority() {
    let program = program(
        r#"//@version=6
indicator("array constructors")
type Point
    float x
local = array.new<Point>(2, Point.new(close))
chart = array.new<chart.point>(2, chart.point.now(close))
copied = local.copy()
window = local.slice(0, 1)
plot(local.get(0).x)
plot(chart.get(0).price)
plot(copied.get(0).x + window.get(0).x)
"#,
    );
    let prepared = PreparedProgram::new(program);
    let mut bound_site = None;
    crate::runtime::hir_walk::statements(&prepared.statements, &mut |expr| {
        if let HirExprKind::Call {
            callee,
            call_site_id,
            args,
        } = &expr.kind
            && callee == "array.new<Point>"
        {
            bound_site = Some(*call_site_id);
            assert_eq!(
                prepared
                    .metadata
                    .calls
                    .dispatch(*call_site_id, callee, args)
                    .array_opcode,
                Some(ArrayOpcode::NewUserType)
            );
        }
    });
    let bound_site = bound_site.expect("generic constructor is bound");
    let mut runtime = HistoricalRuntime::from_prepared(&prepared);
    runtime.append_bar(bar(0, 11.0)).unwrap();
    for (plot, expected) in runtime.result().plots.iter().zip([11.0, 11.0, 22.0]) {
        assert_eq!(plot.values[0].as_f64(), Some(expected));
    }
    assert!(
        runtime
            .array_user_types
            .values()
            .filter(|name| name.as_str() == "Point")
            .count()
            >= 3
    );
    for site in [bound_site, CallSiteId(u32::MAX)] {
        for type_name in ["Point", "lib.Point", "float", "", "图", "Point>extra"] {
            let callee = format!("array.new<{type_name}>");
            let PineValue::Array(id) = runtime.eval_call(&callee, site, &[]).unwrap() else {
                panic!("expected generic array");
            };
            assert_eq!(
                runtime.array_kinds.get(&id),
                Some(&ArrayElementKind::UserType)
            );
            assert_eq!(runtime.array_user_type_name(id), Some(type_name));
        }
        let PineValue::Array(id) = runtime
            .eval_call("array.new<chart.point>", site, &[])
            .unwrap()
        else {
            panic!("expected chart.point array");
        };
        assert_eq!(
            runtime.array_kinds.get(&id),
            Some(&ArrayElementKind::ChartPoint)
        );
        assert_eq!(runtime.array_user_type_name(id), None);
    }
}

#[test]
fn named_methods_and_call_result_mutations_preserve_aliases_and_source_order() {
    let program = program(
        r#"//@version=6
indicator("array aliases")
values = array.from(1, 2, 3)
alias = values
window = array.slice(values, 1, 3)
array.set(value=4, index=0, id=alias)
window.set(value=5, index=0)
array.copy(values).set(value=99, index=1)
plot(values.get(index=0))
plot(values.get(index=1))
plot(window.get(index=0))
plot(array.copy(values).get(index=1))
plot(array.min(nth=array.shift(values) * 0 + 1, id=array.copy(values)))
plot(values.size())
fresh = array.new_int(initial_value=7, size=2)
plot(fresh.first() + fresh.last())
"#,
    );
    let prepared = PreparedProgram::new(program);
    let mut runtime = HistoricalRuntime::from_prepared(&prepared);
    runtime.append_bar(bar(0, 11.0)).unwrap();
    for (plot, expected) in runtime.result().plots.iter().zip([4, 5, 5, 5, 5, 2, 14]) {
        assert_eq!(plot.values, [PineValue::Int(expected)]);
    }
}

#[test]
fn typed_slice_writes_restore_var_and_preserve_varip_between_replacements() {
    let program = program(
        r#"//@version=6
indicator("array replacements")
var values = array.new_int(2, 0)
varip retained = array.new_int(2, 0)
window = values.slice(0, 1)
live_window = retained.slice(0, 1)
array.set(value=window.get(0) + 1, index=0, id=window)
live_window.set(value=array.get(live_window, 0) + 1, index=0)
plot(values.first())
plot(array.first(retained))
"#,
    );
    let prepared = PreparedProgram::new(program);
    let mut runtime = RealtimeRuntime::from_prepared(&prepared);
    runtime
        .seed_historical_without_output(&[bar(0, 10.0)])
        .unwrap();
    for (update, expected_var, expected_varip) in [
        (BarUpdate::forming(bar(1, 11.0)), 2, 2),
        (BarUpdate::forming(bar(1, 12.0)), 2, 3),
        (BarUpdate::confirmed(bar(1, 13.0)), 2, 4),
        (BarUpdate::forming(bar(2, 14.0)), 3, 5),
    ] {
        runtime.apply_update(update).unwrap();
        let result = runtime.result();
        assert_eq!(
            result.plots[0].values.last(),
            Some(&PineValue::Int(expected_var))
        );
        assert_eq!(
            result.plots[1].values.last(),
            Some(&PineValue::Int(expected_varip))
        );
    }
}
