use pine_syntax::SourceFile;

use super::*;

fn first_call_site_id(program: &pine_ir::HirProgram, callee: &str) -> u32 {
    fn find_in_stmts(statements: &[pine_ir::HirStmt], callee: &str) -> Option<u32> {
        for statement in statements {
            match &statement.kind {
                pine_ir::HirStmtKind::Expr(expr)
                | pine_ir::HirStmtKind::Decl { value: expr, .. }
                | pine_ir::HirStmtKind::Reassign { value: expr, .. }
                | pine_ir::HirStmtKind::FieldReassign { value: expr, .. }
                | pine_ir::HirStmtKind::TupleDecl { value: expr, .. } => {
                    if let Some(call_site_id) = find_in_expr(expr, callee) {
                        return Some(call_site_id);
                    }
                }
                pine_ir::HirStmtKind::ArrayFieldReassign {
                    array,
                    index,
                    value,
                    ..
                } => {
                    if let Some(call_site_id) = find_in_expr(array, callee)
                        .or_else(|| find_in_expr(index, callee))
                        .or_else(|| find_in_expr(value, callee))
                    {
                        return Some(call_site_id);
                    }
                }
                pine_ir::HirStmtKind::If {
                    condition,
                    then_branch,
                    else_branch,
                } => {
                    if let Some(call_site_id) = find_in_expr(condition, callee)
                        .or_else(|| find_in_stmts(then_branch, callee))
                        .or_else(|| find_in_stmts(else_branch, callee))
                    {
                        return Some(call_site_id);
                    }
                }
                pine_ir::HirStmtKind::Switch { selector, arms } => {
                    if let Some(call_site_id) = selector
                        .as_ref()
                        .and_then(|selector| find_in_expr(selector, callee))
                        .or_else(|| {
                            arms.iter().find_map(|arm| {
                                arm.condition
                                    .as_ref()
                                    .and_then(|condition| find_in_expr(condition, callee))
                                    .or_else(|| find_in_stmts(&arm.body, callee))
                            })
                        })
                    {
                        return Some(call_site_id);
                    }
                }
                pine_ir::HirStmtKind::For {
                    from,
                    to,
                    step,
                    body,
                    ..
                } => {
                    if let Some(call_site_id) = find_in_expr(from, callee)
                        .or_else(|| find_in_expr(to, callee))
                        .or_else(|| step.as_ref().and_then(|step| find_in_expr(step, callee)))
                        .or_else(|| find_in_stmts(body, callee))
                    {
                        return Some(call_site_id);
                    }
                }
                pine_ir::HirStmtKind::While { condition, body } => {
                    if let Some(call_site_id) =
                        find_in_expr(condition, callee).or_else(|| find_in_stmts(body, callee))
                    {
                        return Some(call_site_id);
                    }
                }
                pine_ir::HirStmtKind::ForIn { iterable, body, .. } => {
                    if let Some(call_site_id) =
                        find_in_expr(iterable, callee).or_else(|| find_in_stmts(body, callee))
                    {
                        return Some(call_site_id);
                    }
                }
                pine_ir::HirStmtKind::Break | pine_ir::HirStmtKind::Continue => {}
            }
        }
        None
    }

    fn find_in_expr(expr: &pine_ir::HirExpr, callee: &str) -> Option<u32> {
        match &expr.kind {
            pine_ir::HirExprKind::Call {
                callee: name,
                call_site_id,
                args,
            } => {
                if name == callee {
                    return Some(call_site_id.0);
                }
                args.iter().find_map(|arg| find_in_expr(&arg.value, callee))
            }
            pine_ir::HirExprKind::Unary { expr, .. }
            | pine_ir::HirExprKind::FieldAccess { value: expr, .. }
            | pine_ir::HirExprKind::History { expr, .. } => find_in_expr(expr, callee),
            pine_ir::HirExprKind::Binary { left, right, .. } => {
                find_in_expr(left, callee).or_else(|| find_in_expr(right, callee))
            }
            pine_ir::HirExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => find_in_expr(condition, callee)
                .or_else(|| find_in_expr(then_expr, callee))
                .or_else(|| find_in_expr(else_expr, callee)),
            pine_ir::HirExprKind::Switch { selector, arms } => selector
                .as_deref()
                .and_then(|selector| find_in_expr(selector, callee))
                .or_else(|| {
                    arms.iter().find_map(|arm| {
                        arm.condition
                            .as_ref()
                            .and_then(|condition| find_in_expr(condition, callee))
                            .or_else(|| find_in_expr(&arm.result, callee))
                    })
                }),
            pine_ir::HirExprKind::For {
                from,
                to,
                step,
                statements,
                result,
                ..
            } => find_in_expr(from, callee)
                .or_else(|| find_in_expr(to, callee))
                .or_else(|| step.as_deref().and_then(|step| find_in_expr(step, callee)))
                .or_else(|| find_in_stmts(statements, callee))
                .or_else(|| find_in_expr(result, callee)),
            pine_ir::HirExprKind::ForIn {
                iterable,
                statements,
                result,
                ..
            } => find_in_expr(iterable, callee)
                .or_else(|| find_in_stmts(statements, callee))
                .or_else(|| find_in_expr(result, callee)),
            pine_ir::HirExprKind::While {
                condition,
                statements,
                result,
            } => find_in_expr(condition, callee)
                .or_else(|| find_in_stmts(statements, callee))
                .or_else(|| find_in_expr(result, callee)),
            pine_ir::HirExprKind::Tuple(values)
            | pine_ir::HirExprKind::UserTypeConstruct { fields: values, .. }
            | pine_ir::HirExprKind::UserTypeArrayConstruct {
                elements: values, ..
            } => values.iter().find_map(|value| find_in_expr(value, callee)),
            pine_ir::HirExprKind::Block { statements, result } => {
                find_in_stmts(statements, callee).or_else(|| find_in_expr(result, callee))
            }
            pine_ir::HirExprKind::Literal(_)
            | pine_ir::HirExprKind::Symbol(_)
            | pine_ir::HirExprKind::Builtin(_) => None,
        }
    }

    find_in_stmts(&program.statements, callee).expect("input call should exist")
}

#[test]
fn runs_input_string_condition() {
    let source = SourceFile::new(
        "test.pine",
        r#"indicator("input string")
mode = input.string("Close", "Mode")
plot(mode == "Close" ? close : open)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let bars = vec![bar(1.0), bar(2.0), bar(3.0)];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");

    assert_eq!(result.plots.len(), 1);
    assert_values_close(&result.plots[0].values, &[1.0, 2.0, 3.0]);
}

#[test]
fn runs_input_call_site_overrides() {
    let source = SourceFile::new(
        "test.pine",
        r#"indicator("input override")
length = input.int(2, "Length")
scale = input.float(1.0, "Scale")
plot(ta.sma(close, length) * scale)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let program = analysis.hir.expect("HIR");
    let bars = vec![bar(1.0), bar(2.0), bar(3.0)];

    let default_result = run_historical(&program, &bars).expect("default input result");
    assert_eq!(default_result.plots.len(), 1);
    assert_eq!(default_result.plots[0].values[0], PineValue::Na);
    assert_values_close(&default_result.plots[0].values[1..], &[1.5, 2.5]);

    let length_call_site = first_call_site_id(&program, "input.int");
    let scale_call_site = first_call_site_id(&program, "input.float");
    let overrides = InputOverrides::new()
        .with_value(length_call_site, PineValue::Int(1))
        .with_value(scale_call_site, PineValue::Float(2.0));
    let override_result =
        run_historical_with_input_overrides(&program, &bars, overrides).expect("override result");

    assert_eq!(override_result.plots.len(), 1);
    assert_values_close(&override_result.plots[0].values, &[2.0, 4.0, 6.0]);
}

#[test]
fn runs_additional_input_variants() {
    let source = SourceFile::new(
        "test.pine",
        r#"indicator("more inputs")
threshold = input.price(2.5, "Price")
start = input.time(2, "Start")
symbol = input.symbol("AAPL", "Symbol")
timeframe = input.timeframe("D", "Timeframe")
session = input.session("0930-1600", "Session")
notes = input.text_area("Plan", "Notes")
enabled = time >= start and symbol == "AAPL" and timeframe == "D" and session == "0930-1600" and notes == "Plan"
plot(enabled ? math.max(close, threshold) : 0)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let bars = vec![
        Bar {
            time: 1,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
        },
        Bar {
            time: 2,
            open: 2.0,
            high: 2.0,
            low: 2.0,
            close: 2.0,
            volume: 1.0,
        },
        Bar {
            time: 3,
            open: 3.0,
            high: 3.0,
            low: 3.0,
            close: 3.0,
            volume: 1.0,
        },
    ];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");

    assert_eq!(result.plots.len(), 1);
    assert_values_close(&result.plots[0].values, &[0.0, 2.5, 3.0]);
}

#[test]
fn runs_generic_input_variants() {
    let source = SourceFile::new(
        "test.pine",
        r#"indicator("generic input")
length = input(2, "Length")
scale = input(1.5, "Scale")
enabled = input(true, "Enabled")
mode = input("SMA", "Mode")
shade = input(color.orange, "Shade")
plot(enabled and mode == "SMA" ? ta.sma(close, length) * scale : open, color=color.new(shade, 10))
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let bars = vec![bar(1.0), bar(2.0), bar(3.0)];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");

    assert_eq!(result.plots.len(), 1);
    assert_eq!(result.plots[0].values[0], PineValue::Na);
    assert_values_close(&result.plots[0].values[1..], &[2.25, 3.75]);
}

#[test]
fn input_defaults_follow_named_parameters_instead_of_source_order() {
    let source = SourceFile::new(
        "test.pine",
        r#"indicator("named input defaults")
mode = input.string(title="Mode", options=["SMA", "EMA"], defval="SMA")
plot(mode == "SMA" ? 1 : 0)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let result = run_historical(&analysis.hir.expect("HIR"), &[bar(1.0)]).expect("runtime result");

    assert_eq!(result.plots.len(), 1);
    assert_values_close(&result.plots[0].values, &[1.0]);
}

#[test]
fn runs_input_metadata_parameters() {
    let source = SourceFile::new(
        "test.pine",
        r#"indicator("input metadata")
length = input.int(2, "Length", minval=1, maxval=20, step=1, options=[1, 2, 3], tooltip="Bars", inline="row", group="Settings", confirm=true, display=display.all)
scale = input.float(1.5, "Scale", minval=0.5, maxval=5.0, step=0.25, options=[1.0, 1.5], display=display.none)
enabled = input.bool(true, "Enabled", tooltip="Toggle", inline="row", group="Settings", confirm=false)
mode = input.string("SMA", "Mode", options=["SMA", "EMA"], tooltip="Mode")
shade = input.color(color.orange, "Shade", group="Style")
src = input.source(close, "Source", tooltip="Price", inline="src", group="Settings", confirm=true, display=display.all)
plot(enabled and mode == "SMA" ? math.max(src, length) * scale : close, color=shade)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let bars = vec![bar(1.0), bar(2.0), bar(3.0)];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");

    assert_eq!(result.plots.len(), 1);
    assert_values_close(&result.plots[0].values, &[3.0, 3.0, 4.5]);
}

#[test]
fn source_override_selects_each_bars_builtin_series_and_rejects_external_sources() {
    let source = SourceFile::new(
        "source-override.pine",
        "indicator(\"source override\")\nsrc = input.source(close, \"Source\")\nplot(src)\n",
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let program = analysis.hir.expect("HIR");
    let id = first_call_site_id(&program, "input.source");
    let bars = [bar_ohlc(1.0, 5.0, 2.0, 4.0), bar_ohlc(3.0, 9.0, 6.0, 8.0)];
    for (selector, expected) in [
        ("open", [1.0, 3.0]),
        ("high", [5.0, 9.0]),
        ("low", [2.0, 6.0]),
        ("close", [4.0, 8.0]),
        ("hl2", [3.5, 7.5]),
        ("hlc3", [11.0 / 3.0, 23.0 / 3.0]),
        ("ohlc4", [3.0, 6.5]),
        ("hlcc4", [3.75, 7.75]),
    ] {
        let overrides = InputOverrides::new().with_value(
            id,
            chart_source_input_override(selector).expect("chart source"),
        );
        let result = run_historical_with_input_overrides(&program, &bars, overrides)
            .expect("source override result");
        assert_values_close(&result.plots[0].values, &expected);
    }
    assert!(chart_source_input_override("other indicator plot").is_err());
    let invalid = InputOverrides::new().with_value(id, PineValue::String("other plot".into()));
    assert!(run_historical_with_input_overrides(&program, &bars, invalid).is_err());
}

#[test]
fn runs_generic_input_series_float_source_defval() {
    let source = SourceFile::new(
        "test.pine",
        r#"indicator("generic source")
src = input(close, "Source")
plot(src)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let bars = vec![bar(1.0), bar(2.0), bar(3.0)];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");

    assert_eq!(result.plots.len(), 1);
    assert_values_close(&result.plots[0].values, &[1.0, 2.0, 3.0]);
}

#[test]
fn generic_source_input_override_tracks_chart_series() {
    let source = SourceFile::new(
        "generic-source-override.pine",
        "indicator(\"generic source\")\nsrc = input(close, \"Source\")\nscale = input(2.0, \"Scale\")\nplot(src * scale)\n",
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let program = analysis.hir.expect("HIR");
    let calls = input_calls(&program);
    let source_call = calls
        .iter()
        .find(|call| call.title.as_deref() == Some("Source"))
        .unwrap();
    let scale_call = calls
        .iter()
        .find(|call| call.title.as_deref() == Some("Scale"))
        .unwrap();
    assert!(source_call.is_source);
    assert!(!scale_call.is_source);
    let bars = [bar_ohlc(1.0, 5.0, 2.0, 4.0), bar_ohlc(3.0, 9.0, 6.0, 8.0)];
    let overrides = InputOverrides::new()
        .with_value(
            source_call.call_site_id,
            chart_source_input_override("hl2").unwrap(),
        )
        .with_value(scale_call.call_site_id, PineValue::Float(2.0));
    let result = run_historical_with_input_overrides(&program, &bars, overrides).unwrap();
    assert_values_close(&result.plots[0].values, &[7.0, 15.0]);
    let invalid = InputOverrides::new().with_value(
        source_call.call_site_id,
        PineValue::String("other plot".into()),
    );
    assert!(run_historical_with_input_overrides(&program, &bars, invalid).is_err());
}

#[test]
fn input_metadata_const_aliases_match_literals_and_execution() {
    let alias = SourceFile::new(
        "aliases.pine",
        r#"//@version=6
indicator("const inputs")
const int base = 3
const string caption = "Length"
length = input.int(base + 2, caption, minval=base, maxval=base * 3, step=base - 2, options=[1, base + 2, 7])
plot(length)
"#,
    );
    let literal = SourceFile::new(
        "literals.pine",
        "//@version=6\nindicator(\"const inputs\")\nlength = input.int(5, \"Length\", minval=3, maxval=9, step=1, options=[1, 5, 7])\nplot(length)\n",
    );
    let alias = analyze_source(&alias);
    let literal = analyze_source(&literal);
    assert!(alias.diagnostics.is_empty(), "{:?}", alias.diagnostics);
    assert!(literal.diagnostics.is_empty(), "{:?}", literal.diagnostics);
    let alias = alias.hir.unwrap();
    let literal = literal.hir.unwrap();
    let metadata = input_calls(&alias);
    assert_eq!(metadata, input_calls(&literal));
    assert_eq!(metadata[0].default_value, Some(PineValue::Int(5)));
    assert_eq!(
        metadata[0].options,
        vec![PineValue::Int(1), PineValue::Int(5), PineValue::Int(7)]
    );
    let output = run_historical(&alias, &[bar(1.0)]).unwrap();
    assert_values_close(&output.plots[0].values, &[5.0]);
}

#[test]
fn input_metadata_evaluates_pure_calls_static_constants_and_selected_branches() {
    let source = SourceFile::new(
        "constant-calls.pine",
        r#"//@version=6
indicator("const metadata")
const int base = math.abs(-3)
const string caption = str.upper("长度") + " " + str.tostring(base)
a = input.int(true ? base + 2 : 99, caption, options=[1, math.max(base, 5), 7])
b = input.float(math.pi, "Pi")
c = input.float(math.sqrt(9), "Square root")
plot(a + b + c)
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let program = analysis.hir.unwrap();
    let metadata = input_calls(&program);
    assert_eq!(metadata[0].title.as_deref(), Some("长度 3"));
    assert_eq!(metadata[0].default_value, Some(PineValue::Int(5)));
    assert_eq!(
        metadata[0].options,
        vec![PineValue::Int(1), PineValue::Int(5), PineValue::Int(7)]
    );
    assert_eq!(
        metadata[1].default_value,
        Some(PineValue::Float(std::f64::consts::PI))
    );
    assert_eq!(metadata[2].default_value, Some(PineValue::Float(3.0)));
    let output = run_historical(&program, &[bar(1.0)]).unwrap();
    assert_values_close(&output.plots[0].values, &[8.0 + std::f64::consts::PI]);
}

#[test]
fn input_metadata_unknown_options_are_atomic_and_chart_format_is_unknown() {
    let source = SourceFile::new(
        "options.pine",
        "//@version=6\nindicator(\"metadata\")\nx = input.int(5, \"Length\", options=[1, 5, 7])\nplot(x)\n",
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let mut program = analysis.hir.unwrap();
    let pine_ir::HirStmtKind::Decl { value, .. } = &mut program.statements[1].kind else {
        panic!("input declaration");
    };
    let pine_ir::HirExprKind::Call { args, .. } = &mut value.kind else {
        panic!("input call");
    };
    assert_eq!(
        args.iter()
            .filter(|arg| matches!(&arg.value.kind, pine_ir::HirExprKind::Tuple(_)))
            .count(),
        1,
        "the normalized input call must retain its unique options tuple"
    );
    let options = args
        .iter_mut()
        .find(|arg| matches!(&arg.value.kind, pine_ir::HirExprKind::Tuple(_)))
        .unwrap();
    let pine_ir::HirExprKind::Tuple(values) = &mut options.value.kind else {
        panic!("options tuple");
    };
    values[1].kind = pine_ir::HirExprKind::Builtin("close".to_owned());
    let calls = input_calls(&program);
    assert!(
        calls[0].options.is_empty(),
        "an unknown option must not shorten the list"
    );
    let chart_format = SourceFile::new(
        "format.pine",
        "//@version=6\nindicator(\"metadata\")\ns = input.string(str.tostring(1.234, format.mintick), \"Title\")\nplot(close)\n",
    );
    let analysis = analyze_source(&chart_format);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(input_calls(&analysis.hir.unwrap())[0].default_value, None);
    let chart_pattern = SourceFile::new(
        "pattern.pine",
        "//@version=6\nindicator(\"metadata\")\ns = input.string(str.format(\"{0,number,format.mintick}\", 1.234), \"Title\")\nplot(close)\n",
    );
    let analysis = analyze_source(&chart_pattern);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(input_calls(&analysis.hir.unwrap())[0].default_value, None);
    let huge_empty_repeat = SourceFile::new(
        "repeat.pine",
        "//@version=6\nindicator(\"metadata\")\ns = input.string(str.repeat(\"\", 9223372036854775807), \"Title\")\nplot(close)\n",
    );
    let analysis = analyze_source(&huge_empty_repeat);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(
        input_calls(&analysis.hir.unwrap())[0].default_value,
        Some(PineValue::String(String::new()))
    );
}

#[test]
fn input_metadata_preflights_string_expansion_before_pure_execution() {
    for expression in [
        format!(
            "str.replace_all(\"{}\", \"\", \"{}\")",
            "a".repeat(40_000),
            "b".repeat(40_000)
        ),
        format!(
            "str.format(\"{}\", \"{}\")",
            "{0}".repeat(2000),
            "b".repeat(40_000)
        ),
    ] {
        let source = SourceFile::new(
            "large-format.pine",
            format!(
                "//@version=6\nindicator(\"metadata\")\ns = input.string({expression}, \"Title\")\nplot(close)\n"
            ),
        );
        let analysis = analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        assert_eq!(input_calls(&analysis.hir.unwrap())[0].default_value, None);
    }
    let source = SourceFile::new(
        "small-format.pine",
        "//@version=6\nindicator(\"metadata\")\na = input.string(str.replace_all(\"aaa\", \"a\", \"b\"), \"Replace\")\nb = input.string(str.format(\"Value {0}\", 5), \"Format\")\nplot(close)\n",
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let calls = input_calls(&analysis.hir.unwrap());
    assert_eq!(
        calls[0].default_value,
        Some(PineValue::String("bbb".to_owned()))
    );
    assert_eq!(
        calls[1].default_value,
        Some(PineValue::String("Value 5".to_owned()))
    );
}

#[test]
fn input_metadata_reassigned_alias_does_not_report_an_obsolete_initializer() {
    let source = SourceFile::new(
        "mutable-alias.pine",
        "//@version=6\nindicator(\"mutable alias\")\nx = 3\nx := 4\ny = input.int(x, \"Value\")\nplot(y)\n",
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let program = analysis.hir.unwrap();
    assert_eq!(input_calls(&program)[0].default_value, None);
    let result = run_historical(&program, &[bar(1.0)]).unwrap();
    assert_values_close(&result.plots[0].values, &[4.0]);
}

#[test]
fn input_metadata_depth_failure_does_not_poison_shorter_aliases() {
    let mut text = String::from("//@version=6\nindicator(\"constant cache\")\nconst int s0 = 1\n");
    for index in 1..=80 {
        text.push_str(&format!("const int s{index} = s{}\n", index - 1));
    }
    text.push_str("long = input.int(s80, \"Long alias\")\nshort = input.int(s20, \"Short alias\")\nplot(short)\n");
    let analysis = analyze_source(&SourceFile::new("alias-depth.pine", text));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let program = analysis.hir.unwrap();
    let calls = input_calls(&program);
    assert_eq!(calls[0].default_value, None);
    assert_eq!(calls[1].default_value, Some(PineValue::Int(1)));
}
