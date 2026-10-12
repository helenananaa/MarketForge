use pine_runtime::{Bar, BarUpdate, RealtimeRuntime, run_historical};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

#[test]
fn drawing_bearing_udt_arrays_preserve_object_and_drawing_aliases() {
    for version in [5, 6] {
        let source = include_str!("../../../tests/fixtures/runtime/udt_drawing_array.pine")
            .replace("version=6", &format!("version={version}"));
        let analysis = analyze_source(&SourceFile::new("drawing.pine", source));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.unwrap();
        let bars = (0..4)
            .map(|i| Bar {
                time: i * 60000,
                open: 10.,
                high: 10.,
                low: 10.,
                close: 10.,
                volume: 1.,
            })
            .collect::<Vec<_>>();
        let expected = run_historical(&hir, &bars).unwrap();
        for (i, _) in bars.iter().enumerate() {
            for (plot, value) in expected
                .plots
                .iter()
                .zip([i as f64 + 10., 1., i as f64 + 20., 1.])
            {
                assert_eq!(plot.values[i].as_f64(), Some(value));
            }
        }
        let mut runtime = RealtimeRuntime::new(&hir);
        for bar in bars {
            runtime.update(BarUpdate::forming(bar)).unwrap();
            runtime.update(BarUpdate::forming(bar)).unwrap();
            runtime.update(BarUpdate::confirmed(bar)).unwrap();
        }
        assert_eq!(runtime.result(), expected);
    }
}

#[test]
fn drawing_bearing_arrays_do_not_inherit_scalar_varip_admission() {
    let source = "//@version=6\nindicator(\"negative\")\ntype Graphic\n    line handle\nvarip array<Graphic> values=array.new<Graphic>()\nplot(close)\n";
    let analysis = analyze_source(&SourceFile::new("negative.pine", source));
    assert!(analysis.hir.is_none());
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|d| d.message.contains("varip")),
        "{:?}",
        analysis.diagnostics
    );
}

#[test]
fn imported_nested_drawing_arrays_keep_field_identity_through_udf_parameters() {
    let root = SourceFile::new(
        "root.pine",
        "//@version=6\nimport example/graphics/1 as lib\nindicator(\"nested\")\nread(array<lib.Wrapper> values)=>array.first(values).child.handle.get_x2()\nitem=lib.Graphic.new(line.new(0,10,1,10))\nwrapped=lib.Wrapper.new(item)\narray<lib.Wrapper> values=array.from(wrapped)\nother=array.first(values)\nother.child.handle.set_x2(9)\nplot(read(values))\nplot(item.handle.get_x2())\n",
    );
    let library = SourceFile::new(
        "library.pine",
        "//@version=6\nlibrary(\"graphics\")\nexport type Graphic\n    line handle\nexport type Wrapper\n    Graphic child\n",
    );
    let input = pine_sema::AnalysisInput::with_library_sources(
        root,
        vec![("example/graphics/1".to_owned(), library)],
    )
    .unwrap();
    let analysis = pine_sema::analyze_input(&input);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.unwrap(),
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
    for plot in result.plots {
        assert_eq!(plot.values[0].as_f64(), Some(9.));
    }
}

#[test]
fn local_methods_create_update_and_delete_drawing_references() {
    let source = "//@version=6\nindicator(\"methods\")\ntype Graphic\n    line handle\ncreate()=>Graphic.new(line.new(0,10,1,10))\nmethod move(Graphic value)=>value.handle.set_x2(9)\nmethod erase(Graphic value)=>value.handle.delete()\nvalue=create()\nvalue.move()\nplot(value.handle.get_x2())\nvalue.erase()\nplot(array.size(line.all))\n";
    let analysis = analyze_source(&SourceFile::new("methods.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(
        &analysis.hir.unwrap(),
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
    assert_eq!(result.plots[0].values[0].as_f64(), Some(9.));
    assert_eq!(result.plots[1].values[0].as_f64(), Some(0.));
}
