use pine_runtime::{
    Bar, BarUpdate, DrawingAction, DrawingFamily, OutputRetention, RealtimeRuntime,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

fn runtime(source: &str) -> RealtimeRuntime<'static> {
    let analysis = analyze_source(&SourceFile::new("persistent drawings.pine", source));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    RealtimeRuntime::from_program(analysis.hir.unwrap())
}

fn bar(index: i64, close: f64) -> Bar {
    Bar {
        time: index * 60_000,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.,
    }
}

#[test]
fn divergent_previews_retract_objects_and_replace_equal_length_tails_and_table_properties() {
    let mut runtime = runtime(
        r#"//@version=6
indicator("persistent drawing branches")
var tag = label.new(0, 10)
var edge = line.new(0, 10, 1, 10)
var other = line.new(0, 11, 1, 11)
var fill = linefill.new(edge, other, color.blue)
var rect = box.new(0, 11, 1, 10)
var points = array.from(chart.point.from_index(0, 10), chart.point.from_index(1, 11))
var shape = polyline.new(points)
var board = table.new(position.top_right, 1, 1)
if barstate.isrealtime
    label.set_y(tag, close)
    line.set_y2(edge, close)
    box.set_top(rect, close)
    linefill.set_color(fill, close > 25 ? color.red : color.blue)
    table.set_bgcolor(board, close > 25 ? color.red : color.blue)
    if close > 20
        label.new(bar_index, close)
        line.new(bar_index, close, bar_index + 1, close)
        box.new(bar_index, close, bar_index + 1, close)
        linefill.new(edge, other, color.red)
        polyline.delete(shape)
        polyline.new(points)
        table.new(position.bottom_right, 1, 1)
plot(close)
"#,
    );
    runtime
        .seed_historical(&[bar(0, 10.), bar(1, 10.), bar(2, 10.)])
        .unwrap();
    let mut replica = runtime.replica();
    let first = runtime
        .apply_update(BarUpdate::forming(bar(3, 24.)))
        .unwrap();
    replica.apply(&first).unwrap();
    let before = runtime.result();
    assert_eq!(replica.result(), &before);
    let replaced = runtime
        .apply_update(BarUpdate::forming(bar(3, 30.)))
        .unwrap();
    replica.apply(&replaced).unwrap();
    let after = runtime.result();
    assert_eq!(
        before.labels[0].snapshots.len(),
        after.labels[0].snapshots.len()
    );
    assert_ne!(before.labels[0], after.labels[0]);
    assert_eq!(before.tables[0].snapshots, after.tables[0].snapshots);
    assert_ne!(before.tables[0].bg_color, after.tables[0].bg_color);
    assert_eq!(replica.result(), &after);
    let reverted = runtime
        .apply_update(BarUpdate::forming(bar(3, 10.)))
        .unwrap();
    for family in [
        DrawingFamily::Label,
        DrawingFamily::Line,
        DrawingFamily::LineFill,
        DrawingFamily::Box,
        DrawingFamily::Polyline,
        DrawingFamily::Table,
    ] {
        assert!(
            reverted
                .drawings
                .iter()
                .any(|change| change.family == family
                    && matches!(change.action, DrawingAction::Delete)),
            "missing removal for {family:?}"
        );
    }
    replica.apply(&reverted).unwrap();
    assert_eq!(replica.result(), &runtime.result());
    let confirmed = runtime
        .apply_update(BarUpdate::confirmed(bar(3, 12.)))
        .unwrap();
    replica.apply(&confirmed).unwrap();
    assert_eq!(replica.result(), &runtime.result());
}

#[test]
fn retention_gaps_preserve_all_drawing_families_and_preview_rollback() {
    let mut runtime = runtime(r#"//@version=6
indicator("retained drawings", max_labels_count=1, max_lines_count=2, max_boxes_count=1, max_polylines_count=1)
label.new(bar_index, close)
a = line.new(bar_index, close, bar_index + 1, close)
b = line.new(bar_index, close + 1, bar_index + 1, close + 1)
f = linefill.new(a, b, color.blue)
linefill.delete(f)
box.new(bar_index, close, bar_index + 1, close)
points = array.from(chart.point.from_index(bar_index, close), chart.point.from_index(bar_index + 1, close))
polyline.new(points)
t = table.new(position.top_right, 1, 1)
table.delete(t)
if close > 20
    label.new(bar_index, close + 1)
plot(close)
"#).with_output_retention(OutputRetention::keep_confirmed_bars(3));
    let history: Vec<_> = (0..20).map(|i| bar(i, 10.)).collect();
    runtime.seed_historical(&history).unwrap();
    let seed = runtime.result();
    assert!(seed.labels.first().unwrap().id > 1);
    assert!(seed.polylines.first().unwrap().id > 1);
    let mut replica = runtime.replica();
    for update in [
        BarUpdate::forming(bar(20, 30.)),
        BarUpdate::forming(bar(20, 10.)),
        BarUpdate::confirmed(bar(20, 12.)),
        BarUpdate::confirmed(bar(21, 13.)),
    ] {
        let changes = runtime.apply_update(update).unwrap();
        replica.apply(&changes).unwrap();
        assert_eq!(replica.result(), &runtime.result());
    }
}

#[test]
fn retention_preserves_deleted_handle_semantics_in_all_drawing_families() {
    let source = r#"//@version=6
indicator("retired handles")
var tag = label.new(0, 10)
var edge = line.new(0, 10, 1, 10)
var other = line.new(0, 11, 1, 11)
var fill = linefill.new(edge, other, color.blue)
var rect = box.new(0, 11, 1, 10)
var points = array.from(chart.point.from_index(0, 10), chart.point.from_index(1, 11))
var shape = polyline.new(points)
var board = table.new(position.top_right, 1, 1)
if bar_index == 0
    label.delete(tag)
    linefill.delete(fill)
    line.delete(edge)
    line.delete(other)
    box.delete(rect)
    polyline.delete(shape)
    table.delete(board)
label.set_y(tag, close)
label.delete(tag)
line.set_y2(edge, close)
line.delete(edge)
linefill.set_color(fill, color.red)
linefill.delete(fill)
box.set_top(rect, close)
box.delete(rect)
polyline.delete(shape)
table.set_bgcolor(board, color.red)
table.cell(board, 0, 0, "deleted")
table.clear(board, 0, 0, 0, 0)
table.merge_cells(board, 0, 0, 0, 0)
table.delete(board)
plot(na(label.get_x(tag)) ? 1 : 0)
plot(na(label.get_text(tag)) ? 1 : 0)
plot(na(label.copy(tag)) ? 1 : 0)
plot(na(line.get_y1(edge)) ? 1 : 0)
plot(na(line.get_price(edge, bar_index)) ? 1 : 0)
plot(na(line.copy(edge)) ? 1 : 0)
plot(na(linefill.get_line1(fill)) ? 1 : 0)
plot(na(linefill.get_line2(fill)) ? 1 : 0)
plot(na(linefill.new(edge, other, color.blue)) ? 1 : 0)
plot(na(box.get_top(rect)) ? 1 : 0)
plot(na(box.copy(rect)) ? 1 : 0)
"#;
    let mut full = runtime(source);
    let mut retained =
        runtime(source).with_output_retention(OutputRetention::keep_confirmed_bars(3));
    let history: Vec<_> = (0..20).map(|i| bar(i, 10.)).collect();
    full.seed_historical(&history).unwrap();
    retained.seed_historical(&history).unwrap();
    let seed = retained.result();
    assert!(seed.labels.is_empty() && seed.lines.is_empty() && seed.boxes.is_empty());
    assert!(seed.line_fills.is_empty() && seed.polylines.is_empty() && seed.tables.is_empty());
    let mut replica = retained.replica();
    for update in [
        BarUpdate::forming(bar(20, 30.)),
        BarUpdate::forming(bar(20, 10.)),
        BarUpdate::confirmed(bar(20, 12.)),
    ] {
        full.apply_update(update).unwrap();
        let changes = retained.apply_update(update).unwrap();
        replica.apply(&changes).unwrap();
        let result = retained.result();
        assert_eq!(replica.result(), &result);
        for (actual, expected) in result.plots.iter().zip(full.result().plots) {
            assert_eq!(
                actual.values,
                expected.values[expected.values.len() - actual.values.len()..]
            );
        }
        assert!(result.labels.is_empty() && result.lines.is_empty() && result.boxes.is_empty());
        assert!(
            result.line_fills.is_empty() && result.polylines.is_empty() && result.tables.is_empty()
        );
    }
}

#[test]
fn retention_replaces_and_restores_old_live_drawing_fallbacks() {
    let mut runtime = runtime(
        r#"//@version=6
indicator("old fallback")
var tag = label.new(0, 10)
var edge = line.new(0, 10, 1, 10)
var other = line.new(0, 11, 1, 11)
var fill = linefill.new(edge, other, color.blue)
var rect = box.new(0, 11, 1, 10)
var points = array.from(chart.point.from_index(0, 10), chart.point.from_index(1, 11))
var shape = polyline.new(points)
var board = table.new(position.top_right, 1, 1)
if close > 20
    label.set_y(tag, close)
    line.set_y2(edge, close)
    linefill.set_color(fill, color.red)
    box.set_top(rect, close)
    polyline.delete(shape)
    table.cell(board, 0, 0, "preview")
"#,
    )
    .with_output_retention(OutputRetention::keep_confirmed_bars(3));
    let history: Vec<_> = (0..20).map(|i| bar(i, 10.)).collect();
    runtime.seed_historical(&history).unwrap();
    let original = runtime.result();
    assert_eq!(original.labels[0].snapshots[0].bar_index, 0);
    let mut replica = runtime.replica();
    for update in [
        BarUpdate::forming(bar(20, 30.)),
        BarUpdate::forming(bar(20, 10.)),
        BarUpdate::forming(bar(20, 40.)),
        BarUpdate::confirmed(bar(20, 10.)),
    ] {
        let changes = runtime.apply_update(update).unwrap();
        replica.apply(&changes).unwrap();
        let result = runtime.result();
        assert_eq!(replica.result(), &result);
        assert_eq!(result.labels[0].snapshots.len(), 1);
        assert_eq!(result.lines[0].snapshots.len(), 1);
        assert_eq!(result.line_fills[0].snapshots.len(), 1);
        assert_eq!(result.boxes[0].snapshots.len(), 1);
        assert_eq!(result.polylines[0].snapshots.len(), 1);
        assert_eq!(result.tables[0].snapshots.len(), 1);
    }
    assert_eq!(runtime.result(), original);
}
