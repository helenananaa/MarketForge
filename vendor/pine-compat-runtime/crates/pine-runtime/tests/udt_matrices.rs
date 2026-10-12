use pine_runtime::{Bar, BarUpdate, RealtimeRuntime, run_historical};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn bar(i: i64) -> Bar {
    Bar {
        time: i * 60000,
        open: 10.,
        high: 10.,
        low: 10.,
        close: 10.,
        volume: 1.,
    }
}

#[test]
fn complete_native_matrix_control_and_realtime_match() {
    let source = include_str!("../../../tests/fixtures/runtime/udt_matrix_identity.pine");
    for version in [5, 6] {
        let analysis = analyze_source(&SourceFile::new(
            "matrix.pine",
            source.replace("version=6", &format!("version={version}")),
        ));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.unwrap();
        let bars = [bar(0), bar(1), bar(2)];
        let expected = run_historical(&hir, &bars).unwrap();
        for i in 0..3 {
            for (plot, value) in expected
                .plots
                .iter()
                .zip([2., 2., 4., 4., 5., 1., 4., i as f64])
            {
                assert_eq!(plot.values[i].as_f64(), Some(value));
            }
        }
        let mut session = RealtimeRuntime::new(&hir);
        for bar in bars {
            session.update(BarUpdate::forming(bar)).unwrap();
            session.update(BarUpdate::forming(bar)).unwrap();
            session.update(BarUpdate::confirmed(bar)).unwrap();
        }
        assert_eq!(session.result(), expected);
    }
}

#[test]
fn omitted_append_index_and_removed_row_preserve_identity() {
    let source = "//@version=6\nindicator(\"rows\")\ntype Item\n    int value\nvar matrix<Item> values=matrix.new<Item>()\na=Item.new(7)\nvalues.add_row(array_id=array.from(a))\nremoved=values.remove_row(0)\nb=removed.last()\nb.value:=9\nplot(a.value)\nplot(values.rows())\n";
    let analysis = analyze_source(&SourceFile::new("rows.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(&analysis.hir.unwrap(), &[bar(0)]).unwrap();
    assert_eq!(result.plots[0].values[0].as_f64(), Some(9.));
    assert_eq!(result.plots[1].values[0].as_f64(), Some(0.));
}

#[test]
fn incompatible_matrix_elements_and_numeric_operations_are_rejected() {
    for statement in [
        "values.set(0,0,Other.new(2))",
        "values.add_row(array_id=array.from(Other.new(2)))",
        "values.concat(matrix.new<Other>())",
        "matrix<Item> wrong=matrix.new<Other>()",
        "plot(matrix.sum(values))",
    ] {
        let source = format!(
            "//@version=6\nindicator(\"negative\")\ntype Item\n    int value\ntype Other\n    int value\nvalues=matrix.new<Item>(1,1,Item.new(1))\n{statement}\nplot(close)\n"
        );
        let analysis = analyze_source(&SourceFile::new("negative.pine", source));
        assert!(analysis.hir.is_none(), "{statement}");
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|d| matches!(d.code.as_str(), "E_UDT_MATRIX_ARG" | "E_CALL_ARG_TYPE")),
            "{:?}",
            analysis.diagnostics
        );
    }
}
