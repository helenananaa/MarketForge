use pine_sema::analyze_source;
use pine_syntax::{SourceFile, parse_source};

#[test]
fn arbitrary_chained_field_and_method_access_parses_structurally() {
    let parsed = parse_source(&SourceFile::new(
        "chain.pine",
        "//@version=6\nindicator(\"chain\")\nif collection.row(0).last().line.get_x2() <= time[1]\n    result = 1\n",
    ));
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
}

#[test]
fn member_unknown_fields_and_receivers_fail_closed() {
    for expression in [
        "chart.point.from_index(0,1).missing",
        "(close+1).value",
        "chart.point.from_index(0,1).price.get_x2()",
    ] {
        let source = format!("//@version=6\nindicator(\"negative\")\nplot({expression})\n");
        let analysis = analyze_source(&SourceFile::new("negative.pine", source));
        assert!(!analysis.diagnostics.is_empty(), "{expression}");
        assert!(analysis.hir.is_none());
    }
}

#[test]
fn member_access_does_not_hide_imported_collection_side_effects() {
    let root = SourceFile::new(
        "root.pine",
        "//@version=6\nimport example/fields/1 as lib\nindicator(\"negative\")\nplot(close)\n",
    );
    let library = SourceFile::new(
        "library.pine",
        "//@version=6\nlibrary(\"fields\")\ntype Owner\n    array<float> values\nexport bad(float v)=>\n    Owner.new(array.new_float(1,v)).values.clear()\n    v\n",
    );
    let input = pine_sema::AnalysisInput::with_library_sources(
        root,
        vec![("example/fields/1".to_owned(), library)],
    )
    .unwrap();
    let analysis = pine_sema::analyze_input(&input);
    assert!(analysis.hir.is_none());
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|d| d.code == "E_IMPORT_FUNCTION_SIDE_EFFECT"),
        "{:?}",
        analysis.diagnostics
    );
}

#[test]
fn member_drawing_copy_cannot_bypass_udf_argument_effect_checks() {
    for arg in ["owner.drawing.copy()", "(owner).drawing.copy()"] {
        let source = format!(
            "//@version=6\nindicator(\"negative\")\ntype Owner\n    line drawing\nowner=Owner.new(line.new(0,1,2,3))\nread(line value)=>line.get_x2(value)\nplot(read({arg}))\n"
        );
        let analysis = analyze_source(&SourceFile::new("negative.pine", source));
        assert!(analysis.hir.is_none());
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|d| d.message.contains("function_side_effect")),
            "{:?}",
            analysis.diagnostics
        );
    }
}

#[test]
fn v6_udt_field_history_rejects_but_object_history_remains_legal() {
    for expr in ["object.value[1]", "Item.new(close).value[1]"] {
        let source = format!(
            "//@version=6\nindicator(\"field history\")\ntype Item\n    float value\nobject=Item.new(close)\nplot({expr})\n"
        );
        let analysis = analyze_source(&SourceFile::new("history.pine", source));
        assert!(analysis.hir.is_none());
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|d| d.code == "E_UDT_FIELD_HISTORY"),
            "{:?}",
            analysis.diagnostics
        );
    }
    let source = "//@version=6\nindicator(\"object history\")\ntype Item\n    float value\nobject=Item.new(close)\nplot((object[1]).value)\n";
    let analysis = analyze_source(&SourceFile::new("history.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(analysis.hir.is_some());
}
