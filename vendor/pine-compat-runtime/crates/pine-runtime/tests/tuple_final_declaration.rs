use pine_runtime::{Bar, BarUpdate, RealtimeRuntime, run_historical};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

#[test]
fn final_tuple_declarations_return_all_slots_and_evaluate_once() {
    for version in [5, 6] {
        let source = include_str!("../../../tests/fixtures/runtime/tuple_final_declaration.pine")
            .replace("version=6", &format!("version={version}"));
        let analysis = analyze_source(&SourceFile::new("control.pine", source));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.unwrap();
        let bars = (0..8)
            .map(|i| Bar {
                time: i * 60000,
                open: 10. + i as f64,
                high: 10. + i as f64,
                low: 10. + i as f64,
                close: 10. + i as f64,
                volume: 1.,
            })
            .collect::<Vec<_>>();
        let result = run_historical(&hir, &bars).unwrap();
        for (i, bar) in bars.iter().enumerate() {
            for pair in [0, 2, 4] {
                assert_eq!(result.plots[pair].values[i].as_f64(), Some(bar.close));
                assert_eq!(
                    result.plots[pair + 1].values[i].as_f64(),
                    Some(bar.close + 10.)
                );
            }
            let branch = if i % 2 == 0 { bar.close } else { -bar.close };
            assert_eq!(result.plots[6].values[i].as_f64(), Some(branch));
            assert_eq!(result.plots[7].values[i].as_f64(), Some(branch + 10.));
            assert_eq!(result.plots[8].values[i].as_f64(), Some((i + 1) as f64));
            assert_eq!(
                result.plots[9].values[i].as_f64(),
                Some(((i + 1) * 10) as f64)
            );
        }
        let mut session = RealtimeRuntime::new(&hir);
        for bar in bars {
            session.update(BarUpdate::forming(bar)).unwrap();
            session.update(BarUpdate::forming(bar)).unwrap();
            session.update(BarUpdate::confirmed(bar)).unwrap();
        }
        assert_eq!(session.result(), result);
    }
}

#[test]
fn final_tuple_declarations_still_validate_the_declaration() {
    for declaration in ["[a,b,c]=pair()", "[a,b]=close"] {
        let source = format!(
            "//@version=6\nindicator(\"negative\")\npair()=>[1,2]\nf()=>\n    {declaration}\n[x,y]=f()\nplot(x)\n"
        );
        let analysis = analyze_source(&SourceFile::new("negative.pine", source));
        assert!(analysis.hir.is_none());
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|d| matches!(d.code.as_str(), "E_TUPLE_ARITY" | "E_TUPLE_TYPE")),
            "{:?}",
            analysis.diagnostics
        );
    }
}

#[test]
fn imported_tuple_declarations_preserve_array_results() {
    let root = SourceFile::new(
        "root.pine",
        "//@version=6\nimport example/tuple/1 as lib\nindicator(\"import\")\n[a,b]=lib.values(close)\nplot(array.get(a,0))\nplot(b)\n",
    );
    let library = SourceFile::new(
        "library.pine",
        "//@version=6\nlibrary(\"tuple\")\nexport values(float x)=>\n    [a,b]=[array.from(x),x+1]\n",
    );
    let input = pine_sema::AnalysisInput::with_library_sources(
        root,
        vec![("example/tuple/1".to_owned(), library)],
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
            open: 3.,
            high: 3.,
            low: 3.,
            close: 3.,
            volume: 1.,
        }],
    )
    .unwrap();
    assert_eq!(result.plots[0].values[0].as_f64(), Some(3.));
    assert_eq!(result.plots[1].values[0].as_f64(), Some(4.));
}
