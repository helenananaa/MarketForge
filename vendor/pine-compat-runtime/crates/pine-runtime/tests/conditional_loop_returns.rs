use pine_runtime::{Bar, run_historical};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

#[test]
fn function_loop_final_condition_executes_void_drawing_calls() {
    let source = "//@version=6\nindicator(\"conditional\")\ntype Graphic\n    line handle\naffix(array<Graphic> values,bool active,int end)=>\n    for graphic in values\n        if active\n            graphic.handle.set_x2(end)\nitem=Graphic.new(line.new(0,10,1,10))\nvalues=array.from(item)\naffix(values,false,5)\nplot(item.handle.get_x2())\naffix(values,true,9)\nplot(item.handle.get_x2())\n";
    for version in [5, 6] {
        let analysis = analyze_source(&SourceFile::new(
            "void.pine",
            source.replace("version=6", &format!("version={version}")),
        ));
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let result =
            run_historical(&analysis.hir.expect("lowered conditional loop"), &[bar()]).unwrap();
        assert_eq!(result.plots[0].values[0].as_f64(), Some(1.));
        assert_eq!(result.plots[1].values[0].as_f64(), Some(9.));
    }
}

#[test]
fn scalar_conditional_loop_results_preserve_selected_values() {
    let source = "//@version=6\nindicator(\"scalar\")\nf(bool active)=>\n    for i=0 to 2\n        if active\n            i+10\n        else\n            i+20\nplot(f(true))\nplot(f(false))\n";
    let analysis = analyze_source(&SourceFile::new("scalar.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(&analysis.hir.expect("lowered scalar loop"), &[bar()]).unwrap();
    assert_eq!(result.plots[0].values[0].as_f64(), Some(12.));
    assert_eq!(result.plots[1].values[0].as_f64(), Some(22.));
}

#[test]
fn while_prefix_runs_once_and_skipped_final_branch_returns_na() {
    let source = "//@version=6\nindicator(\"while\")\nf(bool active)=>\n    int count=0\n    while count<3\n        count+=1\n        if active\n            count\nplot(f(true))\nplot(na(f(false))?1:0)\n";
    let analysis = analyze_source(&SourceFile::new("while.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(&analysis.hir.expect("lowered while"), &[bar()]).unwrap();
    assert_eq!(result.plots[0].values[0].as_f64(), Some(3.));
    assert_eq!(result.plots[1].values[0].as_f64(), Some(1.));
}

fn bar() -> Bar {
    Bar {
        time: 0,
        open: 10.,
        high: 10.,
        low: 10.,
        close: 10.,
        volume: 1.,
    }
}
