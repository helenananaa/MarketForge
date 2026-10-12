use pine_syntax::{SourceFile, StmtKind, parse_source};

#[test]
fn explicit_simple_parameters_preserve_qualified_type_and_source_span() {
    for version in [5, 6] {
        let text = format!(
            "//@version={version}\nosc(series float source, simple int shortLength, simple int longLength, other) => other\n"
        );
        let parsed = parse_source(&SourceFile::new("simple.pine", &text));
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let StmtKind::Function { params, .. } = &parsed.program.statements[0].kind else {
            panic!("expected function");
        };
        assert_eq!(params[0].type_name.as_deref(), Some("series float"));
        assert_eq!(params[1].type_name.as_deref(), Some("simple int"));
        assert_eq!(params[2].type_name.as_deref(), Some("simple int"));
        assert_eq!(params[3].type_name, None);
        let span = params[1].span;
        assert_eq!(&text[span.start..span.end], "simple int shortLength");
    }
}

#[test]
fn explicit_simple_requires_a_supported_scalar_type_and_name() {
    for parameter in ["simple float", "simple simple int x", "simple label x"] {
        let parsed = parse_source(&SourceFile::new(
            "invalid.pine",
            format!("//@version=6\nf({parameter}) => 1\n"),
        ));
        assert!(!parsed.diagnostics.is_empty(), "{parameter}");
    }
}
