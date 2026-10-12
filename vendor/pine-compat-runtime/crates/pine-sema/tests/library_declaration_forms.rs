use pine_sema::analyze_source;
use pine_syntax::SourceFile;

#[test]
fn parsed_scalar_array_fields_are_supported_in_user_types() {
    let result = analyze_source(&SourceFile::new(
        "fields.pine",
        "//@version=6\nindicator(\"fields\")\ntype Buffer\n    array<float> values\nb=Buffer.new(array.new<float>())\nplot(close)\n",
    ));
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(result.hir.is_some());
}

#[test]
fn series_reference_parameter_uses_existing_identity_and_kind_checks() {
    let prefix = "//@version=6\nindicator(\"reference\")\ntype Point\n    float x\nread(series Point p) => p.x\n";
    let good = analyze_source(&SourceFile::new(
        "good.pine",
        format!("{prefix}plot(read(Point.new(3)))\n"),
    ));
    assert!(good.hir.is_some(), "{:?}", good.diagnostics);
    let bad = analyze_source(&SourceFile::new(
        "bad.pine",
        format!("{prefix}plot(read(3))\n"),
    ));
    assert!(bad.hir.is_none());
    assert!(
        bad.diagnostics
            .iter()
            .any(|d| d.code == "E_FUNCTION_ARG_TYPE")
    );
}

#[test]
fn inline_switch_does_not_allow_function_to_mutate_global_state() {
    let result = analyze_source(&SourceFile::new(
        "global.pine",
        "//@version=6\nindicator(\"global\")\nstate=false\nf() =>\n    switch\n        true => state := true\n    state\nplot(f() ? 1 : 0)\n",
    ));
    assert!(result.hir.is_none());
    assert!(!result.diagnostics.is_empty());
}
