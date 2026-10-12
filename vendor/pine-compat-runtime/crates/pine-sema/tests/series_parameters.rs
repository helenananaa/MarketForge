use pine_sema::{AnalysisInput, analyze_input, analyze_source};
use pine_syntax::SourceFile;

#[test]
fn explicit_series_cannot_be_weakened_by_constant_or_input_arguments() {
    for version in [5, 6] {
        for argument in ["3", "input.int(3)"] {
            for body in [
                format!("f(series int n) => ta.ema(close, n)\nplot(f({argument}))"),
                format!("f(series int n) => n\nplot(ta.ema(close, f({argument})))"),
            ] {
                let analysis = analyze_source(&SourceFile::new(
                    "series.pine",
                    format!("//@version={version}\nindicator(\"series\")\n{body}\n"),
                ));
                assert!(analysis.hir.is_none());
                assert!(
                    analysis
                        .diagnostics
                        .iter()
                        .any(|d| d.code == "E_CALL_ARG_TYPE"),
                    "{:?}",
                    analysis.diagnostics
                );
            }
        }
    }
}

#[test]
fn imported_series_parameters_preserve_qualifiers_across_aliases() {
    let library = SourceFile::new(
        "library.pine",
        "//@version=6\nlibrary(\"series\")\nexport value(series int n) => n\n",
    );
    let root = SourceFile::new(
        "root.pine",
        "//@version=6\nimport test/series/1 as lib\nindicator(\"series\")\nplot(ta.ema(close, lib.value(3)))\n",
    );
    let input =
        AnalysisInput::with_library_sources(root, vec![("test/series/1".into(), library)]).unwrap();
    let analysis = analyze_input(&input);
    assert!(analysis.hir.is_none());
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|d| d.code == "E_CALL_ARG_TYPE"),
        "{:?}",
        analysis.diagnostics
    );
}

#[test]
fn explicit_series_still_checks_value_kind() {
    for (parameter, argument) in [
        ("int", "\"wrong\""),
        ("float", "true"),
        ("bool", "close"),
        ("string", "3"),
        ("color", "close"),
    ] {
        let analysis = analyze_source(&SourceFile::new(
            "invalid.pine",
            format!(
                "//@version=6\nindicator(\"series\")\nf(series {parameter} x) => x\nf({argument})\nplot(close)\n"
            ),
        ));
        assert!(analysis.hir.is_none());
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|d| d.code == "E_FUNCTION_ARG_TYPE"),
            "{:?}",
            analysis.diagnostics
        );
    }
}
