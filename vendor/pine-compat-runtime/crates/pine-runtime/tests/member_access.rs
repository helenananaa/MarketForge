use pine_runtime::{Bar, BarUpdate, RealtimeRuntime, run_historical};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn bar(i: i64) -> Bar {
    Bar {
        time: i * 60000,
        open: 10.0 + i as f64,
        high: 10.0 + i as f64,
        low: 10.0 + i as f64,
        close: 10.0 + i as f64,
        volume: 1.0,
    }
}

#[test]
fn member_fields_nested_calls_history_and_drawing_methods_execute_once() {
    let source = include_str!("../../../tests/fixtures/runtime/member_access.pine");
    let analysis = analyze_source(&SourceFile::new("members.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.unwrap();
    let bars: Vec<_> = (0..4).map(bar).collect();
    let result = run_historical(&hir, &bars).unwrap();
    let expected = [
        vec![Some(10.0), Some(11.0), Some(12.0), Some(13.0)],
        vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0)],
        vec![None, Some(1.0), Some(2.0), Some(3.0)],
        vec![Some(10.0), Some(11.0), Some(12.0), Some(13.0)],
        vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0)],
        vec![None, Some(10.0), Some(11.0), Some(12.0)],
    ];
    for (plot, expected) in result.plots.iter().zip(expected) {
        assert_eq!(
            plot.values.iter().map(|v| v.as_f64()).collect::<Vec<_>>(),
            expected
        );
    }
    assert_eq!(result.plots.len(), 6);
    assert_eq!(
        result.lines.len(),
        4,
        "receiver drawing constructor must execute once per bar"
    );
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime.update(BarUpdate::historical(bars[0])).unwrap();
    for b in &bars[1..] {
        runtime.update(BarUpdate::forming(*b)).unwrap();
        runtime.update(BarUpdate::forming(*b)).unwrap();
        runtime.update(BarUpdate::confirmed(*b)).unwrap();
    }
    assert_eq!(result, runtime.result());
}

#[test]
fn generic_member_field_indexes_follow_each_call_identity() {
    let source = "//@version=6\nindicator(\"generic member\")\ntype A\n    float value\ntype B\n    float other\n    float value\nforward(x)=>x\nread(x)=>forward(x).value\nplot(read(A.new(10)))\nplot(read(B.new(20,30)))\n";
    let analysis = analyze_source(&SourceFile::new("generic.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(&analysis.hir.unwrap(), &[bar(0)]).unwrap();
    assert_eq!(result.plots[0].values[0].as_f64(), Some(10.0));
    assert_eq!(result.plots[1].values[0].as_f64(), Some(30.0));
}

#[test]
fn generic_qualified_field_indexes_follow_each_call_identity() {
    let source = "//@version=6\nindicator(\"generic qualified\")\ntype A\n    float value\ntype B\n    float other\n    float value\nread(x)=>x.value\nplot(read(A.new(10)))\nplot(read(B.new(20,30)))\n";
    let analysis = analyze_source(&SourceFile::new("generic.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(&analysis.hir.unwrap(), &[bar(0)]).unwrap();
    assert_eq!(result.plots[0].values[0].as_f64(), Some(10.0));
    assert_eq!(result.plots[1].values[0].as_f64(), Some(30.0));
}

#[test]
fn generic_loop_return_retains_identity_before_field_analysis() {
    let source = "//@version=6\nindicator(\"loop identity\")\ntype A\n    float value\ntype B\n    float other\n    float value\nlast(values)=>\n    for item in values\n        item\na=last(array.from(A.new(10)))\nb=last(array.from(B.new(20,30)))\nplot(a.value)\nplot(b.value)\nplot(last(array.from(A.new(40))).value)\n";
    let analysis = analyze_source(&SourceFile::new("loop.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(&analysis.hir.unwrap(), &[bar(0)]).unwrap();
    assert_eq!(
        result
            .plots
            .iter()
            .map(|p| p.values[0].as_f64())
            .collect::<Vec<_>>(),
        [Some(10.0), Some(30.0), Some(40.0)]
    );
}

#[test]
fn imported_member_reads_keep_library_type_identity_and_source_scope() {
    let root = SourceFile::new(
        "root.pine",
        "//@version=6\nimport example/fields/1 as lib\nindicator(\"imported members\")\nplot(lib.make(close).value)\nplot(lib.read(close+1))\n",
    );
    let library = SourceFile::new(
        "library.pine",
        "//@version=6\nlibrary(\"fields\")\nexport type Item\n    float value\nexport make(float value)=>Item.new(value)\nexport read(float value)=>Item.new(value).value\n",
    );
    let input = pine_sema::AnalysisInput::with_library_sources(
        root,
        vec![("example/fields/1".to_owned(), library)],
    )
    .unwrap();
    let analysis = pine_sema::analyze_input(&input);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(&analysis.hir.unwrap(), &[bar(0), bar(1)]).unwrap();
    assert_eq!(
        result.plots[0]
            .values
            .iter()
            .map(|v| v.as_f64())
            .collect::<Vec<_>>(),
        [Some(10.0), Some(11.0)]
    );
    assert_eq!(
        result.plots[1]
            .values
            .iter()
            .map(|v| v.as_f64())
            .collect::<Vec<_>>(),
        [Some(11.0), Some(12.0)]
    );
}

#[test]
fn qualified_field_methods_preserve_pure_collection_argument_rules() {
    let source = "//@version=6\nindicator(\"field methods\")\ntype Owner\n    line drawing\nowner=Owner.new(line.new(0,1,3,2))\nsizeOf(array<float> values)=>array.size(values)\nplot(owner.drawing.get_x2())\nplot(sizeOf(array.new_float(2,7).copy()))\nplot((owner).drawing.get_x2())\n";
    let analysis = analyze_source(&SourceFile::new("fields.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(&analysis.hir.unwrap(), &[bar(0)]).unwrap();
    assert_eq!(
        result
            .plots
            .iter()
            .map(|p| p.values[0].as_f64())
            .collect::<Vec<_>>(),
        [Some(3.0), Some(2.0), Some(3.0)]
    );
}

#[test]
fn qualified_field_method_spans_preserve_whitespace_and_unicode_comments() {
    let source = "//@version=6\nindicator(\"spans\")\ntype Owner\n    line drawing\nowner=Owner.new(line.new(0,1,3,2))\nplot(owner . // 中文\n    drawing . get_x2())\n";
    let analysis = analyze_source(&SourceFile::new("unicode.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.unwrap();
    let result = run_historical(&hir, &[bar(0)]).unwrap();
    assert_eq!(result.plots[0].values[0].as_f64(), Some(3.0));
    let mut found = false;
    for call in &hir.call_site_sources {
        let start = call.start;
        let end = call.end;
        assert!(source.is_char_boundary(start) && source.is_char_boundary(end));
        found |= source[start..end] == *"owner . // 中文\n    drawing . get_x2()";
    }
    assert!(
        found,
        "member call must retain its complete physical source range"
    );
}

#[test]
fn undefined_user_object_field_access_errors_but_na_field_values_are_valid() {
    for version in [5, 6] {
        let source = format!(
            "//@version={version}\nindicator(\"undefined\")\ntype Item\n    float value\nobject=Item.new(close)\nplot((object[1]).value)\n"
        );
        let analysis = analyze_source(&SourceFile::new("undefined.pine", source));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        assert!(
            run_historical(&analysis.hir.unwrap(), &[bar(0)])
                .unwrap_err()
                .message
                .contains("E_UDT_NA_FIELD")
        );
        let source = format!(
            "//@version={version}\nindicator(\"guarded\")\ntype Item\n    float value\nobject=Item.new(close)\nplot(bar_index>0?(object[1]).value:na)\nplot(Item.new(na).value)\n"
        );
        let analysis = analyze_source(&SourceFile::new("guarded.pine", source));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let result = run_historical(&analysis.hir.unwrap(), &[bar(0), bar(1)]).unwrap();
        assert_eq!(result.plots[0].values[1].as_f64(), Some(10.0));
        assert!(result.plots[1].values.iter().all(|v| v.is_na()));
    }
}

#[test]
fn original_unguarded_history_fixtures_retain_native_error_expectations() {
    for source in [
        include_str!("../../../tests/fixtures/runtime/user_type_history.pine"),
        include_str!("../../../tests/fixtures/runtime/user_type_array_history.pine"),
    ] {
        let analysis = analyze_source(&SourceFile::new("original.pine", source));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        assert!(
            run_historical(&analysis.hir.unwrap(), &[bar(0)])
                .unwrap_err()
                .message
                .contains("E_UDT_NA_FIELD")
        );
    }
    let root = SourceFile::new(
        "original-import.pine",
        include_str!("../../../tests/fixtures/runtime/import_udt_history.pine"),
    );
    let library = SourceFile::new(
        "library.pine",
        include_str!("../../../tests/fixtures/libraries/import_udt_lib.pine"),
    );
    let input = pine_sema::AnalysisInput::with_library_sources(
        root,
        vec![("user/udt/1".to_owned(), library)],
    )
    .unwrap();
    let analysis = pine_sema::analyze_input(&input);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(
        run_historical(&analysis.hir.unwrap(), &[bar(0)])
            .unwrap_err()
            .message
            .contains("E_UDT_NA_FIELD")
    );
}

#[test]
fn failed_member_dereference_keeps_realtime_state_atomic() {
    let source = "//@version=6\nindicator(\"atomic\")\ntype Item\n    float value\nvar int calls=0\ncalls+=1\nobject=close>0?Item.new(close):na\nplot(object.value)\nplot(calls)\n";
    let analysis = analyze_source(&SourceFile::new("atomic.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.unwrap();
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime.update(BarUpdate::historical(bar(0))).unwrap();
    let empty = Bar {
        time: 60000,
        open: 0.0,
        high: 0.0,
        low: 0.0,
        close: 0.0,
        volume: 1.0,
    };
    let before = runtime.result();
    let revision = runtime.revision();
    assert!(
        runtime
            .update(BarUpdate::forming(empty))
            .unwrap_err()
            .message
            .contains("E_UDT_NA_FIELD")
    );
    assert_eq!(before, runtime.result());
    assert_eq!(revision, runtime.revision());
    let preview = runtime.update(BarUpdate::forming(bar(1))).unwrap();
    let revision = runtime.revision();
    assert!(runtime.update(BarUpdate::confirmed(empty)).is_err());
    assert_eq!(preview, runtime.result());
    assert_eq!(revision, runtime.revision());
    let confirmed = runtime.update(BarUpdate::confirmed(bar(1))).unwrap();
    assert_eq!(confirmed.plots[1].values[1].as_f64(), Some(2.0));
}
