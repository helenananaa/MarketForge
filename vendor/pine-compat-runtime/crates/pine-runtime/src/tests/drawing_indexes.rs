use super::*;
use pine_syntax::SourceFile;

#[test]
fn drawing_active_indexes_preserve_copy_eviction_delete_and_all_order() {
    let analysis = analyze_source(&SourceFile::new(
        "drawing_indexes.pine",
        r#"//@version=6
indicator("drawing indexes", max_labels_count=2, max_lines_count=2, max_boxes_count=2)
a = label.new(10, close, "original")
b = label.new(20, close)
label.delete(b)
label.delete(b)
c = label.new(30, close)
d = label.copy(a)
label.set_x(a, 999)
plot(na(label.get_x(a)) ? 1 : 0)
plot(label.get_x(array.get(label.all, 0)))
plot(label.get_x(array.get(label.all, 1)))
la = line.new(10, close, 11, close)
lb = line.new(20, close, 21, close)
line.delete(lb)
line.delete(lb)
lc = line.new(30, close, 31, close)
ld = line.copy(la)
line.set_x1(la, 999)
plot(na(line.get_x1(la)) ? 1 : 0)
plot(line.get_x1(array.get(line.all, 0)))
plot(line.get_x1(array.get(line.all, 1)))
ba = box.new(10, high, 11, low)
bb = box.new(20, high, 21, low)
box.delete(bb)
box.delete(bb)
bc = box.new(30, high, 31, low)
bd = box.copy(ba)
box.set_left(ba, 999)
plot(na(box.get_left(ba)) ? 1 : 0)
plot(box.get_left(array.get(box.all, 0)))
plot(box.get_left(array.get(box.all, 1)))
"#,
    ));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let result = run_historical(&analysis.hir.unwrap(), &[bar(1.0)]).unwrap();
    for (plot, expected) in result.plots.iter().zip([1.0, 30.0, 10.0].repeat(3)) {
        assert_values_close(&plot.values, &[expected]);
    }
    macro_rules! check {
        ($items:expr) => {{
            let items = $items;
            assert_eq!(items.len(), 4);
            for (index, item) in items.iter().enumerate() {
                assert_eq!(item.id, index as u32 + 1);
                assert_eq!(item.snapshots.len(), if index < 2 { 2 } else { 1 });
                assert_eq!(item.snapshots.last().unwrap().exists, index >= 2);
            }
        }};
    }
    check!(&result.labels);
    check!(&result.lines);
    check!(&result.boxes);
}

#[test]
fn drawing_active_indexes_retain_complete_history_and_survive_display_pruning() {
    let analysis = analyze_source(&SourceFile::new(
        "drawing_history_indexes.pine",
        r#"//@version=6
indicator("drawing history indexes", max_labels_count=1, max_lines_count=1, max_boxes_count=1)
l = label.new(bar_index, close)
label.set_text(l, "changed")
a = line.new(bar_index, close, bar_index + 1, close)
line.set_y1(a, close + 1)
b = box.new(bar_index, high, bar_index + 1, low)
box.set_top(b, high + 1)
plot(array.size(label.all) + array.size(line.all) + array.size(box.all))
"#,
    ));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.unwrap();
    let mut runtime = HistoricalRuntime::new(&hir);
    for i in 0..256 {
        runtime.append_bar(bar(i as f64)).unwrap();
    }
    let before = runtime.result();
    assert_eq!(before.labels.len(), 256);
    assert_eq!(before.lines.len(), 256);
    assert_eq!(before.boxes.len(), 256);
    assert_eq!(before.labels[0].snapshots.len(), 3);
    assert_eq!(before.labels[0].snapshots.last().unwrap().bar_index, 1);
    assert!(!before.labels[0].snapshots.last().unwrap().exists);
    runtime.apply_display_origin(254);
    // ID positions are no longer ID - 1 after pruning; lookups must allow gaps.
    runtime.append_bar(bar(256.0)).unwrap();
    let after = runtime.result();
    for plot in &after.plots {
        assert_values_close(&plot.values, &[3.0, 3.0, 3.0]);
    }
    assert_eq!(after.labels.last().unwrap().id, 257);
    assert_eq!(after.lines.last().unwrap().id, 257);
    assert_eq!(after.boxes.last().unwrap().id, 257);
    assert_eq!(
        runtime.active_labels.iter().copied().collect::<Vec<_>>(),
        [257]
    );
    assert_eq!(
        runtime.active_lines.iter().copied().collect::<Vec<_>>(),
        [257]
    );
    assert_eq!(
        runtime.active_boxes.iter().copied().collect::<Vec<_>>(),
        [257]
    );
}

#[test]
fn drawing_active_indexes_rollback_preview_and_identity_eviction() {
    let analysis = analyze_source(&SourceFile::new(
        "drawing_rollback_indexes.pine",
        r#"//@version=6
indicator("drawing rollback indexes", max_labels_count=1, max_lines_count=1, max_boxes_count=1)
l = label.new(bar_index, close)
a = line.new(bar_index, close, bar_index + 1, close)
b = box.new(bar_index, high, bar_index + 1, low)
if close > 5
    label.copy(l)
    line.copy(a)
    box.copy(b)
plot(label.get_y(array.get(label.all, 0)))
plot(line.get_y1(array.get(line.all, 0)))
plot(box.get_top(array.get(box.all, 0)))
"#,
    ));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.unwrap();
    let mut runtime = RealtimeRuntime::new(&hir);
    runtime
        .seed_historical(&[at_time(0, bar(1.0)), at_time(60_000, bar(2.0))])
        .unwrap();
    let preview = runtime
        .update(BarUpdate::forming(at_time(120_000, bar(10.0))))
        .unwrap();
    assert_eq!(preview.labels.last().unwrap().id, 4);
    let replacement = runtime
        .update(BarUpdate::forming(at_time(120_000, bar(3.0))))
        .unwrap();
    assert_eq!(replacement.labels.last().unwrap().id, 3);
    let confirmed = runtime
        .update(BarUpdate::confirmed(at_time(120_000, bar(4.0))))
        .unwrap();
    let expected = run_historical(
        &hir,
        &[
            at_time(0, bar(1.0)),
            at_time(60_000, bar(2.0)),
            at_time(120_000, bar(4.0)),
        ],
    )
    .unwrap();
    assert_eq!(confirmed.labels, expected.labels);
    assert_eq!(confirmed.lines, expected.lines);
    assert_eq!(confirmed.boxes, expected.boxes);
    assert_eq!(confirmed.plots, expected.plots);
}

#[test]
fn drawing_active_indexes_share_checkpoints_until_membership_changes() {
    let analysis = analyze_source(&SourceFile::new(
        "drawing_checkpoint_indexes.pine",
        r#"//@version=6
indicator("drawing checkpoint indexes", max_labels_count=1, max_lines_count=1, max_boxes_count=1)
if bar_index != 1
    label.new(bar_index, close)
    line.new(bar_index, close, bar_index + 1, close)
    box.new(bar_index, high, bar_index + 1, low)
"#,
    ));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let hir = analysis.hir.unwrap();
    let mut runtime = HistoricalRuntime::new(&hir);
    runtime.append_bar(bar(1.0)).unwrap();
    let checkpoint = runtime.clone();
    macro_rules! assert_shared {
        ($expected:expr) => {{
            assert_eq!(
                std::sync::Arc::ptr_eq(&runtime.active_labels, &checkpoint.active_labels),
                $expected
            );
            assert_eq!(
                std::sync::Arc::ptr_eq(&runtime.active_lines, &checkpoint.active_lines),
                $expected
            );
            assert_eq!(
                std::sync::Arc::ptr_eq(&runtime.active_boxes, &checkpoint.active_boxes),
                $expected
            );
        }};
    }
    assert_shared!(true);
    runtime.append_bar(bar(2.0)).unwrap();
    assert_shared!(true);
    runtime.append_bar(bar(3.0)).unwrap();
    assert_shared!(false);
    assert_eq!(
        checkpoint.active_labels.iter().copied().collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(
        checkpoint.active_lines.iter().copied().collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(
        checkpoint.active_boxes.iter().copied().collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(
        runtime.active_labels.iter().copied().collect::<Vec<_>>(),
        [2]
    );
    assert_eq!(
        runtime.active_lines.iter().copied().collect::<Vec<_>>(),
        [2]
    );
    assert_eq!(
        runtime.active_boxes.iter().copied().collect::<Vec<_>>(),
        [2]
    );
}

fn at_time(time: i64, mut bar: Bar) -> Bar {
    bar.time = time;
    bar
}
