use pine_runtime::{Bar, BarUpdate, RealtimeRuntime, run_historical};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn bar(i: i64) -> Bar {
    Bar {
        time: i * 60000,
        open: 10. + i as f64,
        high: 10. + i as f64,
        low: 10. + i as f64,
        close: 10. + i as f64,
        volume: 1.,
    }
}

#[test]
fn original_native_field_varip_control_runs_without_source_rewrites() {
    for version in [5, 6] {
        let source = include_str!("../../../tests/fixtures/runtime/udt_field_varip.pine")
            .replace("version=6", &format!("version={version}"));
        let analysis = analyze_source(&SourceFile::new("native.pine", source));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.unwrap();
        let mut session = RealtimeRuntime::new(&hir);
        session.update(BarUpdate::historical(bar(0))).unwrap();
        for tick in 1..=3 {
            let result = session.update(BarUpdate::forming(bar(1))).unwrap();
            for (plot, expected) in result.plots.iter().zip([
                2.,
                1. + tick as f64,
                2.,
                1. + tick as f64,
                tick as f64,
                tick as f64,
                1.,
            ]) {
                assert_eq!(plot.values[1].as_f64(), Some(expected));
            }
        }
    }
}

#[test]
fn omitted_named_fields_and_builtin_defaults_evaluate_at_construction() {
    let source = "//@version=6\nindicator(\"defaults\")\ntype Item\n    float price = close\n    int number = -2\n    bool flag\n    float missing\na=Item.new(number=7)\nb=Item.new()\nplot(a.price)\nplot(a.number)\nplot(b.number)\nplot(a.flag?1:0)\nplot(na(a.missing)?1:0)\n";
    let analysis = analyze_source(&SourceFile::new("default.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(&analysis.hir.unwrap(), &[bar(0), bar(1)]).unwrap();
    for (plot, expected) in result.plots.iter().zip([11., 7., -2., 0., 1.]) {
        assert_eq!(plot.values[1].as_f64(), Some(expected));
    }
}

#[test]
fn invalid_field_defaults_are_diagnosed_even_when_constructor_overrides_them() {
    for value in ["close+1", "math.abs(-2)", "\"bad\""] {
        let source = format!(
            "//@version=6\nindicator(\"bad\")\ntype Item\n    float value={value}\nobject=Item.new(1)\nplot(object.value)\n"
        );
        let analysis = analyze_source(&SourceFile::new("bad.pine", source));
        assert!(analysis.hir.is_none());
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|d| d.code == "E_UDT_FIELD_DEFAULT"),
            "{:?}",
            analysis.diagnostics
        );
    }
}

#[test]
fn imported_defaults_and_omitted_nested_objects_are_preserved() {
    let root = SourceFile::new(
        "root.pine",
        "//@version=6\nimport example/defaults/1 as lib\nindicator(\"import\")\nx=lib.Item.new(number=9)\nplot(x.price)\nplot(x.number)\nplot(x.flag?1:0)\nplot(na(x.child)?1:0)\n",
    );
    let library = SourceFile::new(
        "library.pine",
        "//@version=6\nlibrary(\"defaults\")\nexport type Child\n    int value\nexport type Item\n    float price=close\n    int number=4\n    bool flag\n    Child child\n",
    );
    let input = pine_sema::AnalysisInput::with_library_sources(
        root,
        vec![("example/defaults/1".to_owned(), library)],
    )
    .unwrap();
    let analysis = pine_sema::analyze_input(&input);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(&analysis.hir.unwrap(), &[bar(0), bar(1)]).unwrap();
    for (plot, expected) in result.plots.iter().zip([11., 9., 0., 1.]) {
        assert_eq!(plot.values[1].as_f64(), Some(expected));
    }
}

#[test]
fn float_fields_promote_integer_constructor_arguments_before_arithmetic() {
    let analysis = analyze_source(&SourceFile::new(
        "float.pine",
        "//@version=5\nindicator(\"float\")\ntype Item\n    float value=1\nx=Item.new()\nplot(x.value/2)\nx.value:=bar_index+1\nplot(x.value/2)\n",
    ));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(&analysis.hir.unwrap(), &[bar(0)]).unwrap();
    assert_eq!(result.plots[0].values[0].as_f64(), Some(0.5));
    assert_eq!(result.plots[1].values[0].as_f64(), Some(0.5));
}

#[test]
fn builtin_field_default_is_not_captured_by_a_same_named_function_parameter() {
    let analysis = analyze_source(&SourceFile::new(
        "scope.pine",
        "//@version=6\nindicator(\"scope\")\ntype Item\n    float price=close\nf(float close)=>Item.new().price\nplot(f(999))\n",
    ));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(&analysis.hir.unwrap(), &[bar(0)]).unwrap();
    assert_eq!(result.plots[0].values[0].as_f64(), Some(10.));
}
