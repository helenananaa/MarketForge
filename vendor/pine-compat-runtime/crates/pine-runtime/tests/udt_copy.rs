use pine_runtime::{Bar, run_historical};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

#[test]
fn copy_allocates_an_outer_object_and_retains_nested_identity() {
    let source = "//@version=6\nindicator(\"copy\")\ntype Leaf\n    float value\ntype Parent\n    Leaf child\na=Leaf.new(close)\nb=a.copy()\nb.value:=close+1\nplot(a.value)\nplot(b.value)\np=Parent.new(a)\nq=Parent.copy(p)\nx=q.child\nx.value:=close+3\nplot(p.child.value)\n";
    for version in [5, 6] {
        let analysis = analyze_source(&SourceFile::new(
            "copy.pine",
            source.replace("version=6", &format!("version={version}")),
        ));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let result = run_historical(
            &analysis.hir.expect("lowered copy"),
            &[Bar {
                time: 0,
                open: 10.,
                high: 10.,
                low: 10.,
                close: 10.,
                volume: 1.,
            }],
        )
        .unwrap();
        for (plot, expected) in result.plots.iter().zip([10., 11., 13.]) {
            assert_eq!(plot.values[0].as_f64(), Some(expected));
        }
    }
}

#[test]
fn evaluated_copy_receiver_runs_once_and_generic_calls_keep_type_identity() {
    let source = "//@version=6\nindicator(\"once\")\ntype Item\n    int value\nmake()=>\n    var int count=0\n    count+=1\n    Item.new(count)\nclone(value)=>value.copy()\nplot(make().copy().value)\nx=clone(Item.new(7))\nplot(x.value)\n";
    let analysis = analyze_source(&SourceFile::new("once.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.expect("lowered copy"),
        &[Bar {
            time: 0,
            open: 10.,
            high: 10.,
            low: 10.,
            close: 10.,
            volume: 1.,
        }],
    )
    .unwrap();
    assert_eq!(result.plots[0].values[0].as_f64(), Some(1.));
    assert_eq!(result.plots[1].values[0].as_f64(), Some(7.));
}

#[test]
fn complete_native_identity_control_runs_unchanged() {
    let source = include_str!("../../../tests/fixtures/runtime/udt_reference_identity.pine");
    for version in [5, 6] {
        let analysis = analyze_source(&SourceFile::new(
            "native.pine",
            source.replace("version=6", &format!("version={version}")),
        ));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let bars = (0..4)
            .map(|i| Bar {
                time: i * 60000,
                open: 10. + i as f64,
                high: 10. + i as f64,
                low: 10. + i as f64,
                close: 10. + i as f64,
                volume: 1.,
            })
            .collect::<Vec<_>>();
        let result = run_historical(&analysis.hir.unwrap(), &bars).unwrap();
        for (i, bar) in bars.iter().enumerate() {
            for (plot, expected) in result.plots.iter().zip([
                Some(bar.close + 1.),
                Some(bar.close + 1.),
                Some(bar.close + 2.),
                Some(bar.close + 3.),
                Some(bar.close + 4.),
                (i > 0).then_some(i as f64),
                (i > 0).then_some(i as f64 - 1.),
                Some(i as f64),
            ]) {
                assert_eq!(plot.values[i].as_f64(), expected);
            }
        }
    }
}

#[test]
fn named_copy_argument_works_and_undefined_object_copy_errors() {
    for initial in ["Item.new(3)", "na"] {
        let source = format!(
            "//@version=6\nindicator(\"copy\")\ntype Item\n    int value\nItem a={initial}\nb=Item.copy(object=a)\nplot(b.value)\n"
        );
        let analysis = analyze_source(&SourceFile::new("named.pine", source));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let result = run_historical(
            &analysis.hir.unwrap(),
            &[Bar {
                time: 0,
                open: 1.,
                high: 1.,
                low: 1.,
                close: 1.,
                volume: 1.,
            }],
        );
        if initial == "na" {
            assert!(result.unwrap_err().message.contains("E_UDT_NA_FIELD"));
        } else {
            assert_eq!(result.unwrap().plots[0].values[0].as_f64(), Some(3.));
        }
    }
}
