use pine_syntax::{ExprKind, SourceFile, StmtKind, SwitchArmResult, parse_source};

#[test]
fn complete_library_declarations_keep_collection_fields_and_reference_qualifiers() {
    let source = "//@version=6\nlibrary(\"buffers\")\ntype Buffer\n    array<float> data\n    array<int> times\n    int start\nmethod append(array<Buffer> buffers, simple int limit, series Buffer value) =>\n    buffers.push(value)\n";
    let parsed = parse_source(&SourceFile::new("buffers.pine", source));
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let StmtKind::UserType(decl) = &parsed.program.statements[1].kind else {
        panic!("type missing")
    };
    assert_eq!(
        decl.fields
            .iter()
            .map(|f| f.type_name.as_str())
            .collect::<Vec<_>>(),
        ["array<float>", "array<int>", "int"]
    );
    let StmtKind::Method(method) = &parsed.program.statements[2].kind else {
        panic!("method missing")
    };
    assert_eq!(method.params[2].type_name, "series Buffer");
}

#[test]
fn switch_inline_reassignment_is_a_statement_block_and_expressions_stay_expressions() {
    let source = "//@version=6\nindicator(\"switch\")\nx = false\nswitch\n    close > open => x := true\n    => x := false\ny = switch\n    close > open => 1\n    => 2\n";
    let parsed = parse_source(&SourceFile::new("switch.pine", source));
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let StmtKind::Expr(expr) = &parsed.program.statements[2].kind else {
        panic!("switch statement missing")
    };
    let ExprKind::Switch { arms, .. } = &expr.kind else {
        panic!("switch missing")
    };
    for arm in arms {
        let SwitchArmResult::Block(block) = &arm.result else {
            panic!("assignment must remain a statement")
        };
        assert!(matches!(block[0].kind, StmtKind::Reassign { .. }));
    }
    let StmtKind::Decl { value, .. } = &parsed.program.statements[3].kind else {
        panic!("declaration missing")
    };
    let ExprKind::Switch { arms, .. } = &value.kind else {
        panic!("switch missing")
    };
    assert!(
        arms.iter()
            .all(|a| matches!(a.result, SwitchArmResult::Expr(_)))
    );
}

#[test]
fn malformed_collection_fields_and_qualifiers_remain_errors() {
    for body in [
        "type Buffer\n    array<> data\n",
        "type Buffer\n    array<float data\n",
        "type Buffer\n    array<float>\n",
        "f(series series int x) => x\n",
        "f(simple array<float> x) => x\n",
        "x=false\nswitch\n    true => x := true false => 0\n",
    ] {
        let parsed = parse_source(&SourceFile::new(
            "bad.pine",
            format!("//@version=6\n{body}"),
        ));
        assert!(
            !parsed.diagnostics.is_empty(),
            "unexpected acceptance: {body}"
        );
    }
}
