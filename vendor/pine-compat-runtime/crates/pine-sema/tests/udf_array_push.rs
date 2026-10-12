use pine_sema::analyze_source;
use pine_syntax::SourceFile;

#[test]
fn push_admission_preserves_other_side_effect_and_type_boundaries() {
    for (version, body) in [
        (4, "    array.push(values, 1)\n    array.size(values)"),
        (5, "    array.reverse(values)\n    array.size(values)"),
        (6, "    array.push(values, true)\n    array.size(values)"),
        (6, "    plot(1)\n    array.size(values)"),
        (6, "    outer := 2\n    array.size(values)"),
    ] {
        let declaration = if version == 4 { "study" } else { "indicator" };
        let source = format!(
            "//@version={version}\n{declaration}(\"negative\")\nouter = 1\nf(values) =>\n{body}\nvalues = array.new_float()\nplot(f(values))\n"
        );
        let analysis = analyze_source(&SourceFile::new("negative.pine", source));
        assert!(
            analysis.hir.is_none(),
            "unexpected admission: v{version} {body}"
        );
        assert!(!analysis.diagnostics.is_empty());
    }
}
