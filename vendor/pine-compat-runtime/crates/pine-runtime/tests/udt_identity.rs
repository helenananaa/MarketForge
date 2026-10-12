use pine_runtime::{Bar, BarUpdate, RealtimeRuntime, run_historical};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn bar(i: i64, close: f64) -> Bar {
    Bar {
        time: i * 60000,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.,
    }
}

#[test]
fn udf_parameter_field_mutation_preserves_caller_identity_and_realtime_rollback() {
    let source = "//@version=6\nindicator(\"UDT parameter mutation\")\ntype Counter\n    int value\nbump(Counter object) =>\n    object.value := object.value + 1\n    object\nvar Counter original = Counter.new(0)\nalias = original\nreturned = bump(alias)\nplot(original.value)\nplot(returned.value)\n";
    let analysis = analyze_source(&SourceFile::new("udf_udt_param.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.expect("HIR");
    let bars = [bar(0, 10.), bar(1, 20.), bar(2, 30.)];
    let expected = run_historical(&hir, &bars).expect("historical result");
    for plot in &expected.plots {
        assert_eq!(
            plot.values.iter().map(|v| v.as_f64()).collect::<Vec<_>>(),
            [Some(1.), Some(2.), Some(3.)],
            "plots: {:?}",
            expected.plots
        );
    }
    let mut session = RealtimeRuntime::new(&hir);
    session.update(BarUpdate::historical(bars[0])).unwrap();
    session.update(BarUpdate::historical(bars[1])).unwrap();
    session.update(BarUpdate::forming(bar(2, 40.))).unwrap();
    session.update(BarUpdate::forming(bar(2, 35.))).unwrap();
    session.update(BarUpdate::confirmed(bars[2])).unwrap();
    assert_eq!(session.result(), expected);
}

#[test]
fn udt_array_retains_nested_drawing_arrays_across_bars_and_rollback() {
    let source = "//@version=6\nindicator(\"UDT drawing arrays\")\ntype Visual\n    line[] paths\n    box[] zones\nvar Visual[] history = array.new<Visual>(0)\nif bar_index == 0\n    array.push(history, Visual.new(array.new_line(), array.new_box()))\nVisual current = array.get(history, 0)\narray.push(current.paths, line.new(bar_index, close, bar_index + 1, close))\narray.push(current.zones, box.new(bar_index, high, bar_index + 1, low))\nplot(array.size(current.paths))\nplot(array.size(current.zones))\n";
    let analysis = analyze_source(&SourceFile::new("udt_drawing_arrays.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.expect("HIR");
    let bars = [bar(0, 10.), bar(1, 20.), bar(2, 30.)];
    let expected = run_historical(&hir, &bars).expect("historical result");
    for plot in &expected.plots {
        assert_eq!(
            plot.values.iter().map(|v| v.as_f64()).collect::<Vec<_>>(),
            [Some(1.), Some(2.), Some(3.)]
        );
    }
    let mut session = RealtimeRuntime::new(&hir);
    session.update(BarUpdate::historical(bars[0])).unwrap();
    session.update(BarUpdate::historical(bars[1])).unwrap();
    session.update(BarUpdate::forming(bar(2, 40.))).unwrap();
    session.update(BarUpdate::forming(bar(2, 35.))).unwrap();
    session.update(BarUpdate::confirmed(bars[2])).unwrap();
    assert_eq!(session.result(), expected);
}

#[test]
fn user_type_array_fields_hold_and_mutate_array_values() {
    let source = "//@version=6\nindicator(\"arrays\")\ntype Features\n    array<float> values\n    array<int> labels\nfeatures=Features.new(array.new_float(),array.new_int())\narray.push(features.values,close)\narray.push(features.labels,bar_index)\nplot(array.get(features.values,0))\nplot(array.get(features.labels,0))\n";
    let analysis = analyze_source(&SourceFile::new("arrays.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(&analysis.hir.unwrap(), &[bar(0, 10.), bar(1, 20.)]).unwrap();
    assert_eq!(
        result.plots[0]
            .values
            .iter()
            .map(|v| v.as_f64())
            .collect::<Vec<_>>(),
        vec![Some(10.), Some(20.)]
    );
    assert_eq!(
        result.plots[1]
            .values
            .iter()
            .map(|v| v.as_f64())
            .collect::<Vec<_>>(),
        vec![Some(0.), Some(1.)]
    );
}

#[test]
fn assignment_and_array_aliases_mutate_the_same_object() {
    let source = "//@version=6\nindicator(\"alias\")\ntype Item\n    float value\na=Item.new(close)\nb=a\nb.value:=close+1\nplot(a.value)\nitems=array.from(a)\nx=array.get(items,0)\nx.value:=close+4\nplot(a.value)\n";
    for version in [5, 6] {
        let analysis = analyze_source(&SourceFile::new(
            "alias.pine",
            source.replace("version=6", &format!("version={version}")),
        ));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.unwrap();
        let result = run_historical(&hir, &[bar(0, 10.), bar(1, 20.)]).unwrap();
        assert_eq!(
            result.plots[0]
                .values
                .iter()
                .map(|v| v.as_f64())
                .collect::<Vec<_>>(),
            vec![Some(11.), Some(21.)]
        );
        assert_eq!(
            result.plots[1]
                .values
                .iter()
                .map(|v| v.as_f64())
                .collect::<Vec<_>>(),
            vec![Some(14.), Some(24.)]
        );
        let mut session = RealtimeRuntime::new(&hir);
        session.update(BarUpdate::historical(bar(0, 10.))).unwrap();
        session.update(BarUpdate::forming(bar(1, 30.))).unwrap();
        session.update(BarUpdate::forming(bar(1, 25.))).unwrap();
        session.update(BarUpdate::confirmed(bar(1, 20.))).unwrap();
        assert_eq!(session.result(), result);
    }
}

#[test]
fn persistent_history_retains_identity_but_rollback_restores_fields() {
    let source = "//@version=6\nindicator(\"history\")\ntype Item\n    int value\nvar object=Item.new(0)\nobject.value:=object.value+1\nprior=object[1]\nplot(bar_index>0?prior.value:na)\nplot(object.value)\n";
    let analysis = analyze_source(&SourceFile::new("history.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.unwrap();
    let expected = run_historical(&hir, &[bar(0, 10.), bar(1, 20.)]).unwrap();
    assert_eq!(expected.plots[0].values[1].as_f64(), Some(2.));
    let mut session = RealtimeRuntime::new(&hir);
    session.update(BarUpdate::historical(bar(0, 10.))).unwrap();
    for _ in 0..3 {
        session.update(BarUpdate::forming(bar(1, 20.))).unwrap();
    }
    session.update(BarUpdate::confirmed(bar(1, 20.))).unwrap();
    assert_eq!(session.result(), expected);
}

#[test]
fn field_varip_and_transient_reference_follow_distinct_rollback_rules() {
    // Explicit constructor values isolate persistence from the still-open
    // field-default parser/constructor contract.
    let source = "//@version=6\nindicator(\"varip fields\")\ntype Counter\n    int regular\n    varip int ticks\nvar normal=Counter.new(0,0)\nvarip persistent=Counter.new(0,0)\nnormal.regular+=1\nnormal.ticks+=1\npersistent.regular+=1\npersistent.ticks+=1\nvarip Counter transient=na\nif barstate.isrealtime and na(transient)\n    transient:=Counter.new(0,0)\nif barstate.isrealtime\n    transient.regular+=1\n    transient.ticks+=1\nplot(normal.regular)\nplot(normal.ticks)\nplot(persistent.regular)\nplot(persistent.ticks)\nplot(na(transient)?na:transient.regular)\nplot(na(transient)?na:transient.ticks)\n";
    for version in [5, 6] {
        let analysis = analyze_source(&SourceFile::new(
            "varip.pine",
            source.replace("version=6", &format!("version={version}")),
        ));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let hir = analysis.hir.unwrap();
        let mut session = RealtimeRuntime::new(&hir);
        session.update(BarUpdate::historical(bar(0, 10.))).unwrap();
        for tick in 1..=3 {
            let output = session.update(BarUpdate::forming(bar(1, 20.))).unwrap();
            let expected = [
                2.,
                1. + tick as f64,
                2.,
                1. + tick as f64,
                tick as f64,
                tick as f64,
            ];
            for (plot, value) in output.plots.iter().zip(expected) {
                assert_eq!(
                    plot.values[1].as_f64(),
                    Some(value),
                    "version {version}, tick {tick}"
                );
            }
        }
    }
}

#[test]
fn imported_varip_field_metadata_survives_lowering() {
    let root = SourceFile::new(
        "root.pine",
        "//@version=6\nimport example/counter/1 as lib\nindicator(\"import\")\nvar x=lib.Counter.new(0,0)\nx.regular+=1\nx.ticks+=1\nplot(x.regular)\nplot(x.ticks)\n",
    );
    let library = SourceFile::new(
        "library.pine",
        "//@version=6\nlibrary(\"counter\")\nexport type Counter\n    int regular\n    varip int ticks\n",
    );
    let input = pine_sema::AnalysisInput::with_library_sources(
        root,
        vec![("example/counter/1".to_owned(), library)],
    )
    .unwrap();
    let analysis = pine_sema::analyze_input(&input);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.unwrap();
    let mut session = RealtimeRuntime::new(&hir);
    session.update(BarUpdate::historical(bar(0, 10.))).unwrap();
    session.update(BarUpdate::forming(bar(1, 20.))).unwrap();
    let output = session.update(BarUpdate::forming(bar(1, 20.))).unwrap();
    assert_eq!(output.plots[0].values[1].as_f64(), Some(2.));
    assert_eq!(output.plots[1].values[1].as_f64(), Some(3.));
}
