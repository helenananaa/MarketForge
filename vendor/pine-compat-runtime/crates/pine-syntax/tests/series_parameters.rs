use pine_syntax::{SourceFile, StmtKind, parse_source};

#[test]
fn explicit_series_parameters_preserve_qualified_type_and_source_span() {
    for version in [5, 6] {
        let text = format!(
            "//@version={version}\nmeasure(series float price, series bool enabled, other) => enabled ? price : other\n"
        );
        let parsed = parse_source(&SourceFile::new("series.pine", &text));
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let StmtKind::Function { params, .. } = &parsed.program.statements[0].kind else {
            panic!("expected function");
        };
        assert_eq!(params[0].type_name.as_deref(), Some("series float"));
        assert_eq!(params[1].type_name.as_deref(), Some("series bool"));
        assert_eq!(params[2].type_name, None);
        let span = params[0].span;
        assert_eq!(&text[span.start..span.end], "series float price");
    }
}

#[test]
fn explicit_series_requires_a_supported_scalar_type_and_name() {
    for parameter in ["series float", "series series float x"] {
        let parsed = parse_source(&SourceFile::new(
            "invalid.pine",
            format!("//@version=6\nf({parameter}) => 1\n"),
        ));
        assert!(!parsed.diagnostics.is_empty(), "{parameter}");
    }
}
