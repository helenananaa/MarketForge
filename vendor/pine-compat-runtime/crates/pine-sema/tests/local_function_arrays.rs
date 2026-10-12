use pine_sema::{AnalysisInput, analyze_input};
use pine_syntax::SourceFile;

fn analyze_export(body: &str, params: &str) -> pine_sema::Analysis {
    let input = AnalysisInput::with_library_sources(
        SourceFile::new(
            "root.pine",
            "//@version=6\nimport test/arrays/1 as lib\nindicator(\"test\")\nplot(close)\n",
        ),
        vec![(
            "test/arrays/1".into(),
            SourceFile::new(
                "library.pine",
                format!("//@version=6\nlibrary(\"arrays\")\nexport f({params}) =>\n{body}\n"),
            ),
        )],
    )
    .unwrap();
    analyze_input(&input)
}

#[test]
fn exported_function_can_fill_its_own_array_inside_for_loop() {
    let result = analyze_export(
        "    shades = array.new<color>()\n    for i = 0 to 2\n        array.push(shades, color.rgb(i, 0, 0))\n    shades",
        "",
    );
    assert!(result.hir.is_some(), "{:?}", result.diagnostics);
}

#[test]
fn exported_loop_array_mutation_requires_stable_local_ownership() {
    for (body, params) in [
        (
            "    for i = 0 to 2\n        array.push(values, i)\n    values",
            "array<int> values",
        ),
        (
            "    local = values\n    for i = 0 to 2\n        array.push(local, i)\n    local",
            "array<int> values",
        ),
        (
            "    local = array.new<int>()\n    for i = 0 to 2\n        local := values\n        array.push(local, i)\n    local",
            "array<int> values",
        ),
    ] {
        let result = analyze_export(body, params);
        assert!(result.hir.is_none(), "accepted {body}");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "E_IMPORT_FUNCTION_SIDE_EFFECT"),
            "{body}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn exported_table_helpers_are_admitted_without_allowing_plot_calls() {
    let good = analyze_export(
        "    t = table.new(position.top_right, 1, 1)\n    table.cell(t, 0, 0, \"value\")\n    t",
        "",
    );
    assert!(good.hir.is_some(), "{:?}", good.diagnostics);

    let bad = analyze_export(
        "    t = table.new(position.top_right, 1, 1)\n    table.cell(t, 0, 0, str.tostring(plot(1)))\n    t",
        "",
    );
    assert!(bad.hir.is_none());
    assert!(
        bad.diagnostics
            .iter()
            .any(|d| d.code == "E_IMPORT_FUNCTION_SIDE_EFFECT")
    );
}

#[test]
fn imported_array_admission_retains_effect_and_ownership_rejections() {
    for body in [
        "    array.push(values,1)\n    1",
        "    local = values\n    array.clear(local)\n    1",
        "    local = array.new<float>()\n    local := values\n    array.push(local,1)\n    1",
        "    var local = array.new<float>()\n    array.push(local,1)\n    local := values\n    1",
        "    var local = array.new<float>()\n    array.push(local,1)\n    if true\n        local := values\n    1",
        "    local = array.new<float>()\n    array.push(local,1)\n    plot(1)\n    1",
        "    local = array.new<float>()\n    array.push(local,plot(1))\n    1",
        "    local = array.new<float>()\n    array.push(local,1)\n    switch\n        true =>\n            for i = 0 to 1\n                plot(i)\n            1\n        => 0",
    ] {
        let input = AnalysisInput::with_library_sources(
            SourceFile::new("root.pine", "//@version=6\nimport test/arrays/1 as lib\nindicator(\"test\")\nplot(close)\n"),
            vec![("test/arrays/1".into(),SourceFile::new("library.pine",format!("//@version=6\nlibrary(\"arrays\")\nexport f(array<float> values) =>\n{body}\n")))],
        ).unwrap();
        let analysis = analyze_input(&input);
        assert!(analysis.hir.is_none(), "accepted {body}");
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|d| d.code == "E_IMPORT_FUNCTION_SIDE_EFFECT"),
            "{body}: {:?}",
            analysis.diagnostics
        );
    }
}

#[test]
fn modern_udf_push_admits_global_array_receivers() {
    for receiver in ["global", "alias"] {
        let source = SourceFile::new(
            "root.pine",
            format!(
                "//@version=6\nindicator(\"test\")\nvar global=array.new<float>()\nf() =>\n    alias=global\n    array.push({receiver},1)\n    array.size({receiver})\nplot(f())\n"
            ),
        );
        let analysis = pine_sema::analyze_source(&source);
        assert!(
            analysis.hir.is_some(),
            "{receiver}: {:?}",
            analysis.diagnostics
        );
    }
}
