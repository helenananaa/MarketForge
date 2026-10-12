use pine_sema::{AnalysisInput, analyze_input, analyze_source};
use pine_syntax::SourceFile;

fn rejects(source: &str, code: &str) {
    let analysis = analyze_source(&SourceFile::new(
        "defaults.pine",
        format!("//@version=6\nindicator(\"defaults\")\n{source}\nplot(close)\n"),
    ));
    assert!(analysis.hir.is_none(), "unexpected acceptance: {source}");
    assert!(
        analysis.diagnostics.iter().any(|d| d.code == code),
        "{source}: {:?}",
        analysis.diagnostics
    );
}

#[test]
fn defaults_do_not_hide_invalid_argument_binding() {
    for (call, code) in [
        ("f()", "E_FUNCTION_ARITY"),
        ("f(y=3)", "E_FUNCTION_ARITY"),
        ("f(1,2,3)", "E_FUNCTION_ARITY"),
        ("f(1,x=2)", "E_FUNCTION_ARG_DUPLICATE"),
        ("f(1,other=2)", "E_FUNCTION_ARG_NAME"),
        ("f(x=1,2)", "E_FUNCTION_ARG_ORDER"),
    ] {
        rejects(&format!("f(x,y=2) => x+y\n{call}"), code);
    }
    rejects("f(x=2,y) => x+y\nf(3)", "E_FUNCTION_ARITY");
}

#[test]
fn invalid_defaults_fail_at_declaration_even_when_overridden_or_unused() {
    for value in [
        "close[1]",
        "1+2",
        "input.float(1)",
        "math.abs(-1)",
        "x",
        "FACTOR",
    ] {
        rejects(
            &format!("FACTOR=1\nf(x={value}) => x\nf(2)"),
            "E_FUNCTION_DEFAULT",
        );
        rejects(&format!("f(x={value}) => x"), "E_FUNCTION_DEFAULT");
    }
    for declaration in [
        "float x=true",
        "int x=close",
        "string x=1",
        "x=na",
        "bool x=na",
        "array<float> x=na",
    ] {
        rejects(&format!("f({declaration}) => x"), "E_FUNCTION_DEFAULT_TYPE");
    }
}

#[test]
fn omitted_series_defaults_keep_qualifier_and_recursion_guards() {
    rejects(
        "f(series int x=3) => x\nplot(ta.ema(close,f()))",
        "E_CALL_ARG_TYPE",
    );
    rejects("f(x=3) => f()\nf()", "E_RECURSIVE_FUNCTION");
}

#[test]
fn default_builtin_must_not_already_be_shadowed_at_definition() {
    rejects("close=42.0\nf(x=close) => x\nf(1)", "E_FUNCTION_DEFAULT");
}

#[test]
fn imported_unused_bad_defaults_are_validated() {
    let root = SourceFile::new(
        "root.pine",
        "//@version=6\nimport test/defaults/1 as lib\nindicator(\"bad default\")\nplot(close)\n",
    );
    let library = SourceFile::new(
        "lib.pine",
        "//@version=6\nlibrary(\"bad default\")\nexport value(float x=true) => x\n",
    );
    let input =
        AnalysisInput::with_library_sources(root, vec![("test/defaults/1".into(), library)])
            .unwrap();
    let analysis = analyze_input(&input);
    assert!(analysis.hir.is_none());
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|d| d.code == "E_FUNCTION_DEFAULT_TYPE"),
        "{:?}",
        analysis.diagnostics
    );
}
