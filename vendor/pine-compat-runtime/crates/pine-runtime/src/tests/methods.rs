use pine_syntax::SourceFile;

use super::*;

#[test]
fn user_methods_run_as_receiver_functions() {
    let source = SourceFile::new(
        "test.pine",
        r#"indicator("methods")
type Point
    float x
method shift(Point p, float delta) => p.x + delta
p = Point.new(close)
plot(p.shift(open))
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let bars = vec![bar_ohlc(1.0, 1.0, 1.0, 2.0), bar_ohlc(3.0, 3.0, 3.0, 4.0)];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");

    assert_eq!(result.plots.len(), 1);
    assert_values_close(&result.plots[0].values, &[3.0, 7.0]);
}

#[test]
fn untyped_method_arguments_color_receivers_and_tuple_udts_execute() {
    let source = SourceFile::new(
        "test.pine",
        r#"//@version=5
indicator("method inference")
type Point
    float x
pair() =>
    var Point first = Point.new(na)
    var Point second = Point.new(na)
    first := Point.new(close)
    second := Point.new(open)
    [first, second]
method shifted(Point p, delta) => p.x + delta
method red(color c) => color.r(c)
[a, b] = pair()
plot(a.shifted(b.x))
shade = color.rgb(18, 52, 86)
plot(shade.red())
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let bars = vec![bar_ohlc(1.0, 1.0, 1.0, 2.0), bar_ohlc(3.0, 3.0, 3.0, 4.0)];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");
    assert_eq!(result.plots.len(), 2);
    assert_values_close(&result.plots[0].values, &[3.0, 7.0]);
    assert_values_close(&result.plots[1].values, &[18.0, 18.0]);
}

#[test]
fn user_methods_in_branches_do_not_share_receiver_state() {
    let source = SourceFile::new(
        "test.pine",
        r#"indicator("method branch")
type Point
    float x
method value(Point p) => p.x
if close > open
    p = Point.new(close)
    plot(p.value())
"#,
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );

    let bars = vec![bar_ohlc(1.0, 1.0, 1.0, 3.0), bar_ohlc(4.0, 4.0, 4.0, 2.0)];
    let result = run_historical(&analysis.hir.expect("HIR"), &bars).expect("runtime result");

    assert_eq!(result.plots.len(), 1);
    assert_eq!(
        result.plots[0].values,
        vec![PineValue::Float(3.0), PineValue::Na]
    );
}
