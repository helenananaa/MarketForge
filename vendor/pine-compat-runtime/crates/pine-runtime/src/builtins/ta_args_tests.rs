use pine_ir::{CallSiteId, HirCallArg, HirExprKind, HirLiteral, HirProgram, HirStmtKind};

use crate::{Bar, HistoricalRuntime, PineValue, PreparedProgram};

fn program(text: &str) -> HirProgram {
    let source = pine_syntax::SourceFile::new("ta_args.pine", text);
    let analysis = pine_sema::analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis.hir.unwrap()
}

fn bar(index: i64, open: f64) -> Bar {
    Bar {
        time: index * 60_000,
        open,
        high: open + 2.0,
        low: open - 2.0,
        close: open + 1.0,
        volume: 10.0,
    }
}

#[test]
fn sparse_reused_ta_id_preserves_named_binding_and_first_duplicate() {
    let mut program = program(
        "//@version=6\nindicator(\"binding\")\nplot(ta.change(close, 2))\nplot(ta.change(close, 2))\n",
    );
    let mut changed = 0;
    for statement in &mut program.statements {
        if let HirStmtKind::Expr(expr) = &mut statement.kind
            && let HirExprKind::Call { args, .. } = &mut expr.kind
            && let Some(arg) = args.first_mut()
            && let HirExprKind::Call {
                callee,
                call_site_id,
                args,
            } = &mut arg.value.kind
            && callee == "ta.change"
        {
            *call_site_id = CallSiteId(u32::MAX);
            if changed == 1 {
                args[0].name = Some("source".to_owned());
                args[1].name = Some("length".to_owned());
                let mut duplicate = args[1].clone();
                duplicate.value.kind = HirExprKind::Literal(HirLiteral::Int(99));
                args.reverse();
                args.push(duplicate);
            }
            changed += 1;
        }
    }
    assert_eq!(changed, 2);
    program.next_call_site_id = u32::MAX;
    let prepared = PreparedProgram::new(program);
    let mut checked = 0;
    crate::runtime::hir_walk::statements(&prepared.statements, &mut |expr| {
        if let HirExprKind::Call {
            callee,
            call_site_id,
            args,
        } = &expr.kind
            && callee == "ta.change"
        {
            assert_eq!(*call_site_id, CallSiteId(u32::MAX));
            let dispatch = prepared
                .metadata
                .calls
                .dispatch(*call_site_id, callee, args);
            assert!(!dispatch.positional_args);
            checked += 1;
        }
    });
    assert_eq!(checked, 2);
    let mut runtime = HistoricalRuntime::from_prepared(&prepared);
    for index in 0..5 {
        runtime.append_bar(bar(index, 10.0 + index as f64)).unwrap();
    }
    let expected = vec![
        PineValue::Na,
        PineValue::Na,
        PineValue::Float(2.0),
        PineValue::Float(2.0),
        PineValue::Float(2.0),
    ];
    let result = runtime.result();
    assert_eq!(result.plots.len(), 2);
    for plot in result.plots {
        assert_eq!(plot.values, expected);
    }
}

#[test]
fn conflicting_sparse_id_dispatches_actual_ta_and_math_calls() {
    let mut program = program(
        "//@version=6\nindicator(\"binding\")\nplot(ta.change(close, 2))\nplot(math.abs(-3))\n",
    );
    let mut changed = 0;
    for statement in &mut program.statements {
        if let HirStmtKind::Expr(expr) = &mut statement.kind
            && let HirExprKind::Call { args, .. } = &mut expr.kind
            && let Some(arg) = args.first_mut()
            && let HirExprKind::Call {
                callee,
                call_site_id,
                ..
            } = &mut arg.value.kind
            && matches!(callee.as_str(), "ta.change" | "math.abs")
        {
            *call_site_id = CallSiteId(u32::MAX);
            changed += 1;
        }
    }
    assert_eq!(changed, 2);
    let prepared = PreparedProgram::new(program);
    let mut checked = 0;
    crate::runtime::hir_walk::statements(&prepared.statements, &mut |expr| {
        if let HirExprKind::Call {
            callee,
            call_site_id,
            args,
        } = &expr.kind
            && *call_site_id == CallSiteId(u32::MAX)
        {
            let dispatch = prepared
                .metadata
                .calls
                .dispatch(*call_site_id, callee, args);
            assert!(matches!(callee.as_str(), "ta.change" | "math.abs"));
            assert!(dispatch.positional_args);
            checked += 1;
        }
    });
    assert_eq!(checked, 2);
    let mut runtime = HistoricalRuntime::from_prepared(&prepared);
    for index in 0..3 {
        runtime.append_bar(bar(index, 10.0 + index as f64)).unwrap();
    }
    let result = runtime.result();
    assert_eq!(
        result.plots[0].values,
        [PineValue::Na, PineValue::Na, PineValue::Float(2.0)]
    );
    assert_eq!(result.plots[1].values, vec![PineValue::Int(3); 3]);
}

#[test]
fn default_source_overloads_and_middle_omitted_slot_keep_defaults() {
    let mut program = program(
        r#"//@version=6
indicator("binding")
plot(ta.highest(2))
plot(ta.highest(length=2))
plot(ta.highest(source=high, length=2))
plot(ta.pivothigh(1, 1))
plot(ta.pivothigh(rightbars=1, leftbars=1))
plot(ta.pivothigh(source=high, leftbars=1, rightbars=1))
[basis, upper, lower] = ta.vwap(source=close, stdev_mult=2)
plot(basis)
plot(ta.vwap(close))
plot(upper)
plot(lower)
"#,
    );
    let mut omitted = 0;
    for statement in &mut program.statements {
        if let HirStmtKind::TupleDecl { value, .. } = &mut statement.kind
            && let HirExprKind::Call { callee, args, .. } = &mut value.kind
            && callee == "ta.vwap"
        {
            let anchor = &mut args[1];
            assert_eq!(anchor.name.as_deref(), Some(pine_ir::OMITTED_BUILTIN_ARG));
            let mut message = anchor.value.clone();
            message.kind =
                HirExprKind::Literal(HirLiteral::String("omitted anchor evaluated".to_owned()));
            message.pine_type =
                pine_ir::PineType::new(pine_ir::Qualifier::Const, pine_ir::ValueKind::String);
            // A placeholder must stay absent even if manual HIR gives it an
            // expression that would fail when evaluated.
            anchor.value.kind = HirExprKind::Call {
                callee: "runtime.error".to_owned(),
                call_site_id: CallSiteId(u32::MAX),
                args: vec![HirCallArg {
                    name: None,
                    value: message,
                }],
            };
            omitted += 1;
        }
    }
    assert_eq!(omitted, 1);
    let prepared = PreparedProgram::new(program);
    crate::runtime::hir_walk::statements(&prepared.statements, &mut |expr| {
        if let HirExprKind::Call {
            callee,
            call_site_id,
            args,
        } = &expr.kind
            && callee.starts_with("ta.")
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
    let mut runtime = HistoricalRuntime::from_prepared(&prepared);
    for (index, open) in [10.0, 14.0, 11.0, 16.0, 12.0].into_iter().enumerate() {
        let mut current = bar(index as i64, open);
        if index >= 3 {
            current.time = 86_400_000 + (index as i64 - 3) * 60_000;
        }
        runtime.append_bar(current).unwrap();
    }
    let result = runtime.result();
    assert_eq!(result.plots.len(), 10);
    for index in [1, 2] {
        assert_eq!(result.plots[0].values, result.plots[index].values);
    }
    for index in [4, 5] {
        assert_eq!(result.plots[3].values, result.plots[index].values);
    }
    assert_eq!(result.plots[0].values[2], PineValue::Float(16.0));
    assert_eq!(result.plots[3].values[2], PineValue::Float(16.0));
    assert_eq!(result.plots[3].values[4], PineValue::Float(18.0));
    assert_eq!(result.plots[6].values, result.plots[7].values);
    assert_eq!(result.plots[6].values[3], PineValue::Float(17.0));
    assert_eq!(result.plots[6].values[4], PineValue::Float(15.0));
    assert_eq!(result.plots[8].values[4], PineValue::Float(19.0));
    assert_eq!(result.plots[9].values[4], PineValue::Float(11.0));
}

#[test]
fn named_ta_arguments_preserve_side_effect_order_in_conditional_udf() {
    let mut program = program(
        r#"//@version=6
indicator("binding")
var trace = array.new_int()
mark(int id, float value) =>
    array.push(trace, id)
    value
check(float a, float b) =>
    ta.cross(source2=mark(4, b), source1=mark(3, a))
crossed = ta.cross(source2=mark(2, open), source1=mark(1, close))
plot(array.get(trace, array.size(trace) - 2) * 10 + array.get(trace, array.size(trace) - 1))
conditional = bar_index % 2 == 0 ? check(close, open) : false
plot(array.get(trace, array.size(trace) - 2) * 10 + array.get(trace, array.size(trace) - 1))
plot(array.size(trace))
plot(crossed ? 1 : 0)
plot(conditional ? 1 : 0)
"#,
    );
    let mut named = 0;
    for statement in &mut program.statements {
        if let HirStmtKind::Decl { value, .. } = &mut statement.kind
            && let HirExprKind::Call { callee, args, .. } = &mut value.kind
            && callee == "ta.cross"
        {
            // Keep one actual named layout, while the lowered conditional UDF
            // exercises the prepared positional path.
            args[0].name = Some("source1".to_owned());
            args[1].name = Some("source2".to_owned());
            args.reverse();
            named += 1;
        }
    }
    assert_eq!(named, 1);
    let prepared = PreparedProgram::new(program);
    let mut layouts = [0; 2];
    crate::runtime::hir_walk::statements(&prepared.statements, &mut |expr| {
        if let HirExprKind::Call {
            callee,
            call_site_id,
            args,
        } = &expr.kind
            && callee == "ta.cross"
        {
            let dispatch = prepared
                .metadata
                .calls
                .dispatch(*call_site_id, callee, args);
            layouts[usize::from(dispatch.positional_args)] += 1;
        }
    });
    assert_eq!(layouts, [1, 1]);
    let mut runtime = HistoricalRuntime::from_prepared(&prepared);
    for index in 0..5 {
        runtime.append_bar(bar(index, 10.0 + index as f64)).unwrap();
    }
    let result = runtime.result();
    assert_eq!(result.plots[0].values, vec![PineValue::Int(12); 5]);
    assert_eq!(
        result.plots[1].values,
        [
            PineValue::Int(34),
            PineValue::Int(12),
            PineValue::Int(34),
            PineValue::Int(12),
            PineValue::Int(34)
        ]
    );
    assert_eq!(
        result.plots[2].values,
        [
            PineValue::Int(4),
            PineValue::Int(6),
            PineValue::Int(10),
            PineValue::Int(12),
            PineValue::Int(16)
        ]
    );
    for index in [3, 4] {
        assert_eq!(result.plots[index].values, vec![PineValue::Int(0); 5]);
    }
}
