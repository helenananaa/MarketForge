use pine_sema::analyze_source;
use pine_syntax::SourceFile;

#[test]
fn polyline_accepts_series_chart_point_array_from_udt_field() {
    let source = SourceFile::new(
        "series_points.pine",
        "//@version=6\nindicator(\"points\")\ntype Drawing\n    array<chart.point> points\nvar Drawing drawing = Drawing.new(array.new<chart.point>())\ndrawing.points.push(chart.point.from_index(bar_index, close))\npolyline.new(drawing.points, false, false)\n",
    );
    let analysis = analyze_source(&source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(analysis.hir.is_some());
}
