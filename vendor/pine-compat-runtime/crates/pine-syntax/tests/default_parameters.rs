use pine_syntax::{ExprKind, FunctionBody, Literal, SourceFile, StmtKind, parse_source};

#[test]
fn parses_required_and_optional_parameters_with_original_spans() {
    for version in [5, 6] {
        let text = format!(
            "//@version={version}\nf(x, series float strong = 0.5, weak = -0.1) => x + strong + weak\n"
        );
        let parsed = parse_source(&SourceFile::new("defaults.pine", &text));
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let StmtKind::Function { params, body, .. } = &parsed.program.statements[0].kind else {
            panic!("function expected")
        };
        assert!(matches!(body, FunctionBody::Expr(_)));
        assert!(params[0].default_value.is_none());
        let value = params[1].default_value.as_ref().unwrap();
        assert!(matches!(value.kind, ExprKind::Literal(Literal::Float(0.5))));
        assert_eq!(&text[value.span.start..value.span.end], "0.5");
        assert_eq!(
            &text[params[1].span.start..params[1].span.end],
            "series float strong = 0.5"
        );
        assert!(params[2].default_value.is_some());
    }
}

#[test]
fn malformed_defaults_and_unimplemented_method_defaults_are_rejected() {
    for source in [
        "//@version=6\nf(x=) => x\n",
        "//@version=6\nf(x=1,, y=2) => x\n",
        "//@version=6\nmethod f(float x, float y=1) => x+y\n",
        "//@version=4\nf(x=1) => x\n",
    ] {
        assert!(
            !parse_source(&SourceFile::new("invalid.pine", source))
                .diagnostics
                .is_empty(),
            "{source}"
        );
    }
}
