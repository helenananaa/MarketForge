use pine_sema::{AnalysisInput, analyze_input, analyze_source};
use pine_syntax::SourceFile;

fn analyze(version: u16, body: &str) -> pine_sema::Analysis {
    analyze_source(&SourceFile::new(
        "simple.pine",
        format!("//@version={version}\nindicator(\"simple\")\n{body}\n"),
    ))
}

#[test]
fn explicit_simple_accepts_const_input_and_simple_and_rejects_series() {
    for version in [5, 6] {
        for body in [
            "f(simple int n) => ta.ema(close, n)\nplot(f(3))",
            "f(simple int n) => n\nplot(ta.ema(close, f(input.int(3))))",
            "f(simple float x) => x\nplot(f(3))",
            "osc(series float source=hl2, simple int shortLength=5, simple int longLength=34) => ta.sma(source, shortLength) - ta.sma(source, longLength)\nplot(osc())\nplot(osc(close, 5, 34))\nplot(osc(shortLength=6))",
            "full(simple int periodK, simple int smoothK, simple int periodD) => ta.sma(ta.stoch(close, high, low, periodK), smoothK)\nplot(full(14, 3, 3))",
            "srsi(simple int lengthRsi, simple int periodK, simple int smoothK, simple int periodD, series float source=close) => ta.rsi(source, lengthRsi)\nplot(srsi(14, 14, 3, 3))\nplot(srsi(14, 14, 3, 3, close))",
            "ult(simple int fastLen, simple int midLen, simple int slowLen) => ta.sma(close, fastLen)\nplot(ult(7, 14, 28))",
        ] {
            let analysis = analyze(version, body);
            assert!(
                analysis.diagnostics.is_empty() && analysis.hir.is_some(),
                "{version} {body}: {:?}",
                analysis.diagnostics
            );
        }

        for (body, code) in [
            (
                "f(simple float n) => n\nplot(f(close))",
                "E_FUNCTION_ARG_TYPE",
            ),
            (
                "f(simple float n=close) => n\nplot(f(1))",
                "E_FUNCTION_DEFAULT_TYPE",
            ),
            (
                "f(simple int n) => n\nplot(ta.ema(close, f(close)))",
                "E_FUNCTION_ARG_TYPE",
            ),
            (
                "f(simple int n) => n\nplot(ta.ema(close, f(bar_index)))",
                "E_FUNCTION_ARG_TYPE",
            ),
            (
                "as_series(series int n) => n\ninto_simple(simple int n) => n\nplot(ta.ema(close, into_simple(as_series(3))))",
                "E_FUNCTION_ARG_TYPE",
            ),
            (
                "title_of(simple string name) => name\nplot(close, title=title_of(\"x\"))",
                "E_CALL_ARG_TYPE",
            ),
            (
                "f(simple int n=close) => n\nplot(f())",
                "E_FUNCTION_DEFAULT_TYPE",
            ),
        ] {
            let analysis = analyze(version, body);
            assert!(analysis.hir.is_none(), "unexpected acceptance: {body}");
            assert!(
                analysis.diagnostics.iter().any(|d| d.code == code),
                "{body}: {:?}",
                analysis.diagnostics
            );
        }
    }
}

#[test]
fn direct_global_input_keeps_inner_effects_and_function_bodies_rejected() {
    for body in [
        "f(simple int n) => n\ng() => f(input.int(3))\nplot(g())",
        "f(simple int n) => n\nplot(f(input.int(3, title=str.tostring(plot(close)))))",
        "f(float n) => n\nplot(f(plot(close)))",
    ] {
        let analysis = analyze(6, body);
        assert!(analysis.hir.is_none(), "unexpected acceptance: {body}");
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|d| d.severity == pine_syntax::Severity::Error)
        );
    }
}

#[test]
fn imported_simple_parameters_and_method_qualifiers_agree_with_local_rules() {
    let library = SourceFile::new(
        "library.pine",
        "//@version=6\nlibrary(\"simple\")\nhelper(simple int n) => n\nexport value(simple int n) => helper(n)\nexport title_of(simple string name) => name\n",
    );
    let root = SourceFile::new(
        "root.pine",
        "//@version=6\nimport test/simple/1 as lib\nindicator(\"simple\")\nplot(ta.ema(close, lib.value(3)))\nplot(ta.ema(close, lib.value(input.int(3))))\n",
    );
    let input =
        AnalysisInput::with_library_sources(root, vec![("test/simple/1".into(), library)]).unwrap();
    let analysis = analyze_input(&input);
    assert!(
        analysis.diagnostics.is_empty() && analysis.hir.is_some(),
        "{:?}",
        analysis.diagnostics
    );

    let library = SourceFile::new(
        "library.pine",
        "//@version=6\nlibrary(\"simple\")\nexport value(simple int n) => n\n",
    );
    let root = SourceFile::new(
        "root.pine",
        "//@version=6\nimport test/simple/1 as lib\nindicator(\"simple\")\nplot(ta.ema(close, lib.value(close)))\n",
    );
    let input =
        AnalysisInput::with_library_sources(root, vec![("test/simple/1".into(), library)]).unwrap();
    let analysis = analyze_input(&input);
    assert!(analysis.hir.is_none());
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|d| d.code == "E_FUNCTION_ARG_TYPE"),
        "{:?}",
        analysis.diagnostics
    );

    let analysis = analyze_source(&SourceFile::new(
        "method.pine",
        "//@version=6\nindicator(\"simple\")\ntype Point\n    float x\nmethod shift(Point self, simple int n) => self.x + n\nplot(close)\n",
    ));
    assert!(analysis.hir.is_none());
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|d| d.code == "E_METHOD_PARAM"),
        "{:?}",
        analysis.diagnostics
    );
}
