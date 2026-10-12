use pine_runtime::{
    Bar, PineValue, public_runtime_result_json, run_historical, runtime_result_from_json,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;
fn run(source: &str) -> pine_runtime::RuntimeResult {
    let analysis = analyze_source(&SourceFile::new("display.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    run_historical(
        &analysis.hir.unwrap(),
        &[Bar {
            time: 0,
            open: 10.,
            high: 12.,
            low: 9.,
            close: 11.,
            volume: 1.,
        }],
    )
    .unwrap()
}
#[test]
fn display_algebra_matches_native_relations() {
    let result = run(include_str!(
        "../../../tests/fixtures/runtime/display_relations.pine"
    ));
    for (i, plot) in result.plots.iter().enumerate() {
        assert_eq!(
            plot.values[0],
            PineValue::Int(if i == 3 { 0 } else { 1 }),
            "relation {i}"
        );
    }
}
#[test]
fn display_identity_survives_variables_functions_and_wire_output() {
    let result = run(r#"//@version=6
indicator("display identity")
f() => display.all-display.pane
show=input.bool(true)
a=f()
b=show ? a : display.none
plot(close,display=b)
plot(open,display=display.data_window+display.status_line)
plot(("display.all"+"display.pane")=="display.alldisplay.pane"?1:0)
"#);
    assert_eq!(
        result.plots[0].metadata.display,
        PineValue::String("display.all-display.pane".into())
    );
    assert_eq!(
        result.plots[1].metadata.display,
        PineValue::String("display.status_line+display.data_window".into())
    );
    assert_eq!(result.plots[2].values[0], PineValue::Int(1));
    let json = public_runtime_result_json(&result);
    assert_eq!(
        json,
        public_runtime_result_json(&runtime_result_from_json(&json).unwrap())
    );
}
#[test]
fn display_values_are_not_strings_or_numbers() {
    for body in [
        r#"plot(close,display="display.all")"#,
        r#"x=display.all+"display.pane"
plot(close)"#,
        r#"x=display.all*display.pane
plot(close)"#,
        r#"plot(close,display=close>open?display.all:display.none)"#,
    ] {
        let analysis = analyze_source(&SourceFile::new(
            "invalid.pine",
            format!("//@version=6\nindicator(\"invalid\")\n{body}\n"),
        ));
        assert!(!analysis.diagnostics.is_empty(), "accepted {body}");
    }
}

#[test]
fn all_is_not_the_five_named_locations() {
    let result = run(include_str!(
        "../../../tests/fixtures/runtime/display_five_locations.pine"
    ));
    assert_eq!(result.plots[0].values[0], PineValue::Int(0));
    assert_eq!(result.plots[1].values[0], PineValue::Int(0));
}
#[test]
fn const_display_equality_controls_constant_history_offsets() {
    let result = run(r#"//@version=6
indicator("constant display folding")
offset=(display.pane+display.pane==display.pane)?0:1
plot(close[offset])
"#);
    assert_eq!(result.plots[0].values[0], PineValue::Float(11.0));
}
