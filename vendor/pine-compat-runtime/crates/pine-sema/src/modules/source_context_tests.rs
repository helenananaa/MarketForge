use super::*;

fn parsed_decl_value(expression: &str) -> Expr {
    let source = pine_syntax::SourceFile::new("expression.pine", format!("value = {expression}\n"));
    let parsed = parse_source(&source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let statement = parsed
        .program
        .statements
        .first()
        .expect("declaration statement");
    let StmtKind::Decl { value, .. } = &statement.kind else {
        panic!("expected declaration, got {:?}", statement.kind);
    };
    value.clone()
}

#[test]
fn imported_const_expressions_remain_metadata_neutral() {
    for expression in [
        "1",
        "math.pi",
        "-math.pi",
        "1 + 2 * 3",
        "true ? color.red : color.blue",
    ] {
        let value = parsed_decl_value(expression);
        assert!(
            is_const_import_expr(&value),
            "expected metadata-neutral imported const expression: {expression}"
        );
    }

    for (category, expression) in [
        ("identifier", "named"),
        ("user call", "helper(1)"),
        ("builtin call", "math.abs(1)"),
        ("tuple", "[1, 2]"),
        ("history", "close[1]"),
        ("UDT constructor", "Point.new(1)"),
        ("map constructor", "map.new<string, float>()"),
    ] {
        let value = parsed_decl_value(expression);
        assert!(
            !is_const_import_expr(&value),
            "{category} must retain expression provenance: {expression}"
        );
    }
}

#[test]
fn import_plan_assigns_context_per_alias_and_shares_it_across_callables() {
    let input = AnalysisInput::with_library_sources(
        pine_syntax::SourceFile::new(
            "root.pine",
            "import user/context/1 as left\nimport user/context/1 as right\n",
        ),
        vec![(
            "user/context/1".to_owned(),
            pine_syntax::SourceFile::new(
                "context.pine",
                r#"library("context")
export type Point
    float x

helper(float value) => value
export passthrough(float value) => helper(value)
method shift(Point self, float delta) => helper(self.x + delta)
"#,
            ),
        )],
    )
    .expect("valid source graph");

    let validation = validate_modules(&input);

    assert!(
        validation.diagnostics.is_empty(),
        "{:?}",
        validation.diagnostics
    );
    let left_export = validation
        .imported_functions
        .get("@import:left.passthrough")
        .expect("left exported function");
    let left_private = validation
        .imported_functions
        .get("@import:left.helper")
        .expect("left private function");
    let left_method = validation
        .imported_methods
        .get(&("left.Point".to_owned(), "shift".to_owned()))
        .expect("left imported method");
    let right_export = validation
        .imported_functions
        .get("@import:right.passthrough")
        .expect("right exported function");
    let right_private = validation
        .imported_functions
        .get("@import:right.helper")
        .expect("right private function");
    let right_method = validation
        .imported_methods
        .get(&("right.Point".to_owned(), "shift".to_owned()))
        .expect("right imported method");
    assert_eq!(left_export.display_name, "left.passthrough");
    assert_eq!(left_private.display_name, "__import_left_helper");

    assert_eq!(
        left_export.source_context_id,
        left_private.source_context_id
    );
    assert_eq!(left_export.source_context_id, left_method.source_context_id);
    assert_eq!(
        right_export.source_context_id,
        right_private.source_context_id
    );
    assert_eq!(
        right_export.source_context_id,
        right_method.source_context_id
    );
    assert_ne!(
        left_export.source_context_id,
        right_export.source_context_id
    );
    assert_eq!(left_export.source_id, right_export.source_id);
    assert_eq!(left_private.source_id, right_private.source_id);
    assert_eq!(left_method.source_id, right_method.source_id);
}

#[test]
fn library_diagnostics_retain_physical_sources_with_unicode_and_nested_imports() {
    let root = pine_syntax::SourceFile::new(
        "根.pine",
        "//@version=6\nindicator(\"根\")\nimport audit/Outer/1 as outer\nplot(outer.f(close))\n",
    );
    let inner = pine_syntax::SourceFile::new(
        "内部.pine",
        "//@version=6\nlibrary(\"内部\")\n// 中文填充\nexport f(float x) => str.length(\"中文\") + missingName + x\n",
    );
    let offset = inner.text().find("missingName").unwrap();
    let expected = inner.line_col(offset);
    let input = AnalysisInput::with_library_sources(root.clone(), vec![
        ("audit/Inner/1".to_owned(), inner.clone()),
        ("audit/Outer/1".to_owned(), pine_syntax::SourceFile::new("外部.pine", "//@version=6\nlibrary(\"外部\")\nimport audit/Inner/1 as inner\nexport f(float x) => inner.f(x)\n")),
    ]).unwrap();
    let analysis = crate::analyze_input(&input);
    let diagnostic = analysis
        .diagnostics
        .iter()
        .find(|d| d.code == "E_UNKNOWN_SYMBOL" && d.message.contains("missingName"))
        .expect("inner library diagnostic");
    let origin = diagnostic
        .source
        .as_ref()
        .expect("physical library identity");
    assert_eq!(origin.source_id, SourceId::library(0).get());
    assert_eq!(origin.library_key.as_deref(), Some("audit/Inner/1"));
    assert_eq!(origin.source_name, "内部.pine");
    assert_eq!(diagnostic.span.start, offset);
    assert_eq!(diagnostic.line_col(&root), expected);
    assert!(
        diagnostic
            .format(&root)
            .contains("audit/Inner/1 (内部.pine):4:")
    );
}

#[test]
fn library_parser_and_module_validation_diagnostics_retain_origin() {
    for (text, code) in [
        (
            "//@version=6\nlibrary(\"库\")\n// 中文\nexport f(float x) => (\n",
            "E_PARSE_EXPR",
        ),
        (
            "//@version=6\nlibrary(\"库\")\n// 中文\nimport audit/Missing/1 as missing\n",
            "E_IMPORT_MISSING_LIBRARY",
        ),
    ] {
        let root = pine_syntax::SourceFile::new(
            "root.pine",
            "//@version=6\nindicator(\"root\")\nplot(close)\n",
        );
        let library = pine_syntax::SourceFile::new("库.pine", text);
        let input = AnalysisInput::with_library_sources(
            root.clone(),
            vec![("audit/Library/1".to_owned(), library.clone())],
        )
        .unwrap();
        let analysis = crate::analyze_input(&input);
        let diagnostic = analysis
            .diagnostics
            .iter()
            .find(|d| d.code == code)
            .unwrap_or_else(|| panic!("missing {code}: {:?}", analysis.diagnostics));
        let origin = diagnostic
            .source
            .as_ref()
            .expect("library identity even before HIR");
        assert_eq!(origin.library_key.as_deref(), Some("audit/Library/1"));
        assert_eq!(
            diagnostic.line_col(&root),
            library.line_col(diagnostic.span.start)
        );
    }
    let root = pine_syntax::SourceFile::new(
        "root.pine",
        "//@version=6\nindicator(\"root\")\nplot(missingName)\n",
    );
    let analysis = crate::analyze_source(&root);
    assert!(analysis.diagnostics.iter().all(|d| d.source.is_none()));
}

#[test]
fn imported_parameter_type_errors_refer_to_the_library_declaration() {
    let root = pine_syntax::SourceFile::new(
        "root.pine",
        "//@version=6\nindicator(\"root\")\nimport audit/Library/1 as lib\nplot(lib.f(\"bad\"))\n",
    );
    let library = pine_syntax::SourceFile::new(
        "库.pine",
        "//@version=6\nlibrary(\"库\")\n// 中文\nexport f(float value) => value\n",
    );
    let expected = library.line_col(library.text().find("float value").unwrap());
    let input = AnalysisInput::with_library_sources(
        root.clone(),
        vec![("audit/Library/1".to_owned(), library)],
    )
    .unwrap();
    let analysis = crate::analyze_input(&input);
    let diagnostic = analysis
        .diagnostics
        .iter()
        .find(|d| d.code == "E_FUNCTION_ARG_TYPE")
        .expect("parameter type error");
    assert_eq!(
        diagnostic.source.as_ref().unwrap().library_key.as_deref(),
        Some("audit/Library/1")
    );
    assert_eq!(diagnostic.line_col(&root), expected);
}
