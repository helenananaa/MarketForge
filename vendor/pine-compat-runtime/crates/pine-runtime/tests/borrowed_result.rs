use pine_runtime::{
    Bar, BarUpdate, HistoricalRuntime, OutputRetention, RealtimeRuntime, RuntimeResultView,
    public_runtime_result_json, write_public_runtime_result_view_json,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    io::{self, Write},
};

struct Allocator;
thread_local! {
    static TRACK: Cell<bool> = const { Cell::new(false) };
    static MAX: Cell<usize> = const { Cell::new(0) };
    static TOTAL: Cell<usize> = const { Cell::new(0) };
}
fn record(size: usize) {
    if TRACK.try_with(Cell::get).unwrap_or(false) {
        let _ = MAX.try_with(|value| value.set(value.get().max(size)));
        let _ = TOTAL.try_with(|value| value.set(value.get().saturating_add(size)));
    }
}
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;
fn track<T>(run: impl FnOnce() -> T) -> (T, usize, usize) {
    MAX.set(0);
    TOTAL.set(0);
    TRACK.set(true);
    let result = run();
    TRACK.set(false);
    (result, MAX.get(), TOTAL.get())
}
fn bar(index: i64) -> Bar {
    Bar {
        time: index * 60_000,
        open: 10.,
        high: 13.,
        low: 8.,
        close: 10. + (index % 3) as f64,
        volume: 10.,
    }
}
fn json(view: &RuntimeResultView<'_>) -> String {
    let mut bytes = Vec::new();
    write_public_runtime_result_view_json(view, &mut bytes).unwrap();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn borrowed_views_match_exact_owned_wire_for_all_output_families() {
    for name in [
        "plotchar",
        "plotshape",
        "plotarrow",
        "plotbar",
        "plotcandle",
        "color_outputs",
        "gradient_fill",
        "fill_transp",
        "plot_dynamic_style",
        "plot_linestyle",
        "label_mutation",
        "line_mutation",
        "linefill_new",
        "polyline_lifecycle",
        "box_mutation",
        "table_cell",
        "table_merge_cells",
        "table_delete",
        "alert",
        "strategy_calc_on_order_fills",
    ] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/runtime")
            .join(format!("{name}.pine"));
        let source = SourceFile::new(name, std::fs::read_to_string(path).unwrap());
        let hir = analyze_source(&source).hir.unwrap();
        let mut runtime = HistoricalRuntime::new(&hir);
        runtime
            .append_bars(&(0..32).map(bar).collect::<Vec<_>>())
            .unwrap();
        let owned = runtime.result();
        let expected = public_runtime_result_json(&owned);
        assert_eq!(json(&runtime.result_view()), expected, "{name}");
        assert_eq!(json(&owned.view()), expected, "{name} owned adapter");
        assert_eq!(
            pine_runtime::into_public_runtime_result_json(owned),
            expected,
            "{name} consuming serializer"
        );
    }
}

#[test]
fn retained_forming_and_confirmed_views_preserve_current_revision_and_drawing_suffix() {
    let source = SourceFile::new(
        "retained.pine",
        r#"//@version=6
strategy("retained")
var label id = label.new(0, close, "你好🙂")
if bar_index % 5 == 0
    label.set_text(id, str.tostring(close))
if bar_index % 2 == 0
    strategy.entry("long", strategy.long)
else
    strategy.close("long")
plot(close)
"#,
    );
    let hir = analyze_source(&source).hir.unwrap();
    let mut runtime =
        RealtimeRuntime::new(&hir).with_output_retention(OutputRetention::keep_confirmed_bars(3));
    runtime
        .seed_historical_without_output(&(0..32).map(bar).collect::<Vec<_>>())
        .unwrap();
    let retained = runtime.result();
    let before = public_runtime_result_json(&retained);
    assert_eq!(json(&runtime.result_view()), before);
    assert_eq!(json(&runtime.confirmed_result_view()), before);
    let revision = runtime.revision();
    struct Fail;
    impl Write for Fail {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::StorageFull, "full"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    assert!(write_public_runtime_result_view_json(&runtime.result_view(), &mut Fail).is_err());
    assert_eq!(runtime.revision(), revision);
    assert_eq!(json(&runtime.result_view()), before);
    for update in [
        BarUpdate::forming(bar(32)),
        BarUpdate::forming(Bar {
            close: 100.,
            ..bar(32)
        }),
        BarUpdate::confirmed(bar(32)),
    ] {
        runtime.apply_update(update).unwrap();
        assert_eq!(
            json(&runtime.result_view()),
            public_runtime_result_json(&runtime.result())
        );
        assert_eq!(
            json(&runtime.confirmed_result_view()),
            public_runtime_result_json(&runtime.confirmed_result())
        );
    }
    assert_eq!(public_runtime_result_json(&retained), before);
}

#[test]
fn view_creation_and_encoding_do_not_materialize_long_history_vectors() {
    let source = SourceFile::new(
        "resource.pine",
        "//@version=6\nindicator(\"resource\")\nplot(close)\nplotcandle(open, high, low, close)\nplotshape(close > open, text=\"你好🙂\")\n",
    );
    let hir = analyze_source(&source).hir.unwrap();
    let mut runtime = HistoricalRuntime::new(&hir);
    runtime
        .append_bars(&(0..10_000).map(bar).collect::<Vec<_>>())
        .unwrap();
    let (view, largest, total) = track(|| runtime.result_view());
    assert!(
        largest < 4096 && total < 8192,
        "view: largest={largest} total={total}"
    );
    let (_, largest, _) =
        track(|| write_public_runtime_result_view_json(&view, &mut io::sink()).unwrap());
    assert!(
        largest < 4096,
        "serialization unexpectedly copied a history: largest={largest}"
    );
    assert_eq!(view.plot_candles[0].opens.len(), 10_000);
    assert_eq!(
        view.plot_shapes[0].texts.iter().last().unwrap(),
        &pine_runtime::PineValue::String("你好🙂".to_owned())
    );
}

#[test]
fn state_only_operations_match_owned_snapshots_revisions_cursors_and_failures() {
    use pine_runtime::RealtimeUpdateContext;
    let source = SourceFile::new(
        "state.pine",
        "//@version=6\nindicator(\"state\")\nif close < 0\n    runtime.error(\"failed\")\nplot(close + timenow)\n",
    );
    let hir = analyze_source(&source).hir.unwrap();
    let mut owned = RealtimeRuntime::new(&hir);
    let mut borrowed = RealtimeRuntime::new(&hir);
    let history: Vec<_> = (0..8).map(bar).collect();
    let times: Vec<_> = (0..8).map(|index| index + 1000).collect();
    owned
        .seed_historical_with_execution_times(&history, &times)
        .unwrap();
    borrowed
        .seed_historical_with_execution_times_without_output(&history, &times)
        .unwrap();
    fn same(left: &RealtimeRuntime<'_>, right: &RealtimeRuntime<'_>) {
        assert_eq!(left.revision(), right.revision());
        assert_eq!(left.last_changes(), right.last_changes());
        assert_eq!(left.display_origin(), right.display_origin());
        assert_eq!(json(&left.result_view()), json(&right.result_view()));
        assert_eq!(
            json(&left.confirmed_result_view()),
            json(&right.confirmed_result_view())
        );
    }
    same(&owned, &borrowed);
    let context = RealtimeUpdateContext {
        execution_time: Some(2000),
        opening_update: None,
    };
    owned
        .apply_update_with_context(BarUpdate::forming(bar(8)), context)
        .unwrap();
    borrowed
        .apply_update_with_context(BarUpdate::forming(bar(8)), context)
        .unwrap();
    assert!(owned.last_changes().is_some());
    for update in [
        BarUpdate::forming(Bar {
            close: 99.,
            ..bar(8)
        }),
        BarUpdate::confirmed(bar(8)),
    ] {
        owned.update_with_context(update, context).unwrap();
        borrowed
            .update_with_context_without_output(update, context)
            .unwrap();
        same(&owned, &borrowed);
        assert!(borrowed.last_changes().is_none());
    }
    let before = json(&borrowed.result_view());
    let revision = borrowed.revision();
    let failed = BarUpdate::forming(Bar {
        close: -1.,
        ..bar(9)
    });
    assert_eq!(
        owned
            .update_with_context(failed, context)
            .unwrap_err()
            .message,
        borrowed
            .update_with_context_without_output(failed, context)
            .unwrap_err()
            .message
    );
    same(&owned, &borrowed);
    assert_eq!(borrowed.revision(), revision);
    assert_eq!(json(&borrowed.result_view()), before);
    assert_eq!(
        owned
            .replay_historical_with_execution_times(&history, &times[..1])
            .unwrap_err()
            .message,
        borrowed
            .replay_historical_with_execution_times_without_output(&history, &times[..1])
            .unwrap_err()
            .message
    );
    same(&owned, &borrowed);
    owned
        .replay_historical_with_execution_times(&history, &times)
        .unwrap();
    borrowed
        .replay_historical_with_execution_times_without_output(&history, &times)
        .unwrap();
    same(&owned, &borrowed);
    let corrected = [
        Bar {
            close: 123.,
            ..bar(6)
        },
        bar(7),
    ];
    assert_eq!(
        owned
            .correct_historical_with_execution_times(6 * 60000, &corrected, &[1])
            .unwrap_err()
            .message,
        borrowed
            .correct_historical_with_execution_times_without_output(6 * 60000, &corrected, &[1])
            .unwrap_err()
            .message
    );
    same(&owned, &borrowed);
    owned
        .correct_historical_with_execution_times(6 * 60000, &corrected, &[3000, 3001])
        .unwrap();
    borrowed
        .correct_historical_with_execution_times_without_output(
            6 * 60000,
            &corrected,
            &[3000, 3001],
        )
        .unwrap();
    same(&owned, &borrowed);
    let a = owned
        .apply_update_with_context(BarUpdate::forming(bar(8)), context)
        .unwrap();
    let b = borrowed
        .apply_update_with_context(BarUpdate::forming(bar(8)), context)
        .unwrap();
    assert_eq!(a, b);
    same(&owned, &borrowed);
}

#[test]
fn state_only_variants_work_without_execution_clocks() {
    let hir = analyze_source(&SourceFile::new(
        "unclocked.pine",
        "//@version=6\nindicator(\"unclocked\")\nplot(close)\n",
    ))
    .hir
    .unwrap();
    let mut owned = RealtimeRuntime::new(&hir);
    let mut borrowed = RealtimeRuntime::new(&hir);
    let history: Vec<_> = (0..8).map(bar).collect();
    owned.seed_historical(&history).unwrap();
    borrowed.seed_historical_without_output(&history).unwrap();
    owned.update(BarUpdate::forming(bar(8))).unwrap();
    borrowed
        .update_without_output(BarUpdate::forming(bar(8)))
        .unwrap();
    assert_eq!(json(&owned.result_view()), json(&borrowed.result_view()));
    owned.replay_historical(&history).unwrap();
    borrowed.replay_historical_without_output(&history).unwrap();
    owned
        .correct_historical(
            7 * 60000,
            &[Bar {
                close: 99.,
                ..bar(7)
            }],
        )
        .unwrap();
    borrowed
        .correct_historical_without_output(
            7 * 60000,
            &[Bar {
                close: 99.,
                ..bar(7)
            }],
        )
        .unwrap();
    assert_eq!(json(&owned.result_view()), json(&borrowed.result_view()));
    assert_eq!(owned.revision(), borrowed.revision());
}

#[test]
fn drawing_sink_encoding_does_not_buffer_a_complete_sparse_history() {
    let source = SourceFile::new(
        "drawing-resource.pine",
        "//@version=6\nindicator(\"drawing-resource\")\nvar label id = label.new(0, close, \"\")\nlabel.set_text(id, str.tostring(bar_index))\n",
    );
    let hir = analyze_source(&source).hir.unwrap();
    let mut runtime = HistoricalRuntime::new(&hir);
    runtime
        .append_bars(&(0..10_000).map(bar).collect::<Vec<_>>())
        .unwrap();
    let (view, largest, total) = track(|| runtime.result_view());
    assert!(
        largest < 4096 && total < 8192,
        "view: largest={largest} total={total}"
    );
    // Creation and the first text mutation each retain their own snapshot.
    assert_eq!(view.labels[0].snapshots.len(), 10_001);
    let (_, largest, total) =
        track(|| write_public_runtime_result_view_json(&view, &mut io::sink()).unwrap());
    // Persistent history iteration allocates a small tree cursor; 10k drawing
    // snapshots must not add per-snapshot allocations or record-sized buffers.
    assert!(
        largest < 4096 && total < 1024,
        "drawing encoding: largest={largest}, total={total}"
    );
}

#[test]
fn large_drawing_and_record_payloads_stream_without_temporary_allocations() {
    use pine_runtime::{
        AlertEvent, ChartPointValue, PineValue as V, PolylineOutput, PolylineSnapshot,
        RuntimeDiagnostic, TableCellSnapshot, TableOutput, TableSnapshot,
    };
    let hir = analyze_source(&SourceFile::new(
        "empty.pine",
        "//@version=6\nindicator(\"empty\")\n",
    ))
    .hir
    .unwrap();
    let mut result = HistoricalRuntime::new(&hir).result();
    let text = "汉🙂\n\0\"\\".repeat(128);
    let cell = TableCellSnapshot {
        column: 0,
        row: 0,
        text: V::String(text.clone()),
        bg_color: V::Color(0),
        text_color: V::Color(u64::MAX),
        width: V::Float(-0.0),
        height: V::Na,
        text_size: V::Na,
        text_halign: V::Na,
        text_valign: V::Na,
        text_wrap: V::Na,
        tooltip: V::String(text.clone()),
        text_font_family: V::Na,
        text_formatting: V::Na,
    };
    result.tables.push(TableOutput {
        id: 1,
        columns: 1024,
        rows: 1,
        position: V::String(text.clone()),
        bg_color: V::Na,
        frame_color: V::Na,
        frame_width: V::Na,
        border_color: V::Na,
        border_width: V::Na,
        snapshots: vec![TableSnapshot {
            bar_index: 0,
            exists: true,
            cells: vec![cell; 1024],
            merged_cells: vec![],
        }],
    });
    result.polylines.push(PolylineOutput {
        id: 2,
        snapshots: vec![PolylineSnapshot {
            bar_index: 0,
            exists: true,
            points: vec![
                V::ChartPoint(ChartPointValue::new(
                    V::Int(i64::MAX),
                    V::Na,
                    V::Float(-0.0)
                ));
                10_000
            ],
            curved: V::Bool(false),
            closed: V::Bool(true),
            xloc: V::Na,
            line_color: V::Na,
            fill_color: V::Na,
            line_style: V::Na,
            line_width: V::Float(1.0),
            force_overlay: V::Bool(false),
        }],
    });
    result.alerts.push(AlertEvent {
        id: 1,
        bar_index: 0,
        time: i64::MIN,
        message: text.repeat(128),
        source: text.clone(),
    });
    result.diagnostics.push(RuntimeDiagnostic {
        code: text.clone(),
        message: text.repeat(128),
    });
    let view = result.view();
    let (_, largest, total) =
        track(|| write_public_runtime_result_view_json(&view, &mut io::sink()).unwrap());
    assert_eq!(
        (largest, total),
        (0, 0),
        "large borrowed payload encoding allocated"
    );
    let encoded = json(&view);
    let parsed: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(
        parsed["tables"][0]["snapshots"][0]["cells"]
            .as_array()
            .unwrap()
            .len(),
        1024
    );
    assert_eq!(
        parsed["polylines"][0]["snapshots"][0]["points"]
            .as_array()
            .unwrap()
            .len(),
        10_000
    );
    assert_eq!(
        parsed["tables"][0]["snapshots"][0]["cells"][0]["text"],
        text
    );
    // A short-writing sink exercises write_all inside a large string; errors
    // propagate without attempting further writes or changing borrowed output.
    struct Limited {
        bytes: Vec<u8>,
        limit: usize,
        failed: bool,
    }
    impl Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            assert!(!self.failed, "encoder continued after a sink error");
            if self.bytes.len() == self.limit {
                self.failed = true;
                return Err(io::Error::new(io::ErrorKind::StorageFull, "full"));
            }
            let count = bytes.len().min(7).min(self.limit - self.bytes.len());
            self.bytes.extend_from_slice(&bytes[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Limited {
        bytes: vec![],
        limit: 70_013,
        failed: false,
    };
    let error = write_public_runtime_result_view_json(&view, &mut sink).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::StorageFull);
    assert_eq!(sink.bytes, encoded.as_bytes()[..sink.limit]);
    assert_eq!(json(&view), encoded);
}

#[test]
fn large_delta_fields_headers_and_gradients_stream_without_temporary_allocations() {
    use pine_runtime::{
        FillAction, FillChange, FillGradientSample, PineValue as V, RuntimeChanges, SeriesChange,
        SeriesChangeOp, SeriesFamily, SeriesFields, SeriesHeader, StreamingVisibility,
        public_runtime_changes_json, write_public_runtime_changes_json,
    };
    let text = "汉🙂\n\0\"\\".repeat(1024);
    let mut changes = RuntimeChanges::new(10, StreamingVisibility::Preview);
    for id in 0..64 {
        let mut header = SeriesHeader::default();
        header.metadata.title = V::String(text.clone());
        header.hist_base = V::Float(-0.0);
        changes.series.push(SeriesChange {
            family: SeriesFamily::Plot,
            id,
            op: SeriesChangeOp::ReplaceLast,
            start: 4,
            fields: SeriesFields {
                values: vec![V::Float(-0.0), V::Na, V::Float(f64::INFINITY)],
                texts: vec![V::String(text.clone())],
                ..Default::default()
            },
            header: Some(header),
        });
    }
    changes.fills.push(FillChange {
        id: 1,
        action: FillAction::SetGradient {
            start: 4,
            values: vec![
                FillGradientSample {
                    top_value: Some(1.0),
                    bottom_value: Some(-0.0),
                    top_color: Some(0xffaaff),
                    bottom_color: None,
                };
                4096
            ],
        },
    });
    let (_, largest, total) =
        track(|| write_public_runtime_changes_json(&changes, &mut io::sink()).unwrap());
    assert_eq!(
        (largest, total),
        (0, 0),
        "delta encoder allocated a field/section buffer"
    );
    let mut bytes = Vec::new();
    write_public_runtime_changes_json(&changes, &mut bytes).unwrap();
    assert_eq!(bytes, public_runtime_changes_json(&changes).as_bytes());
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["series"][0]["header"]["title"], text);
    assert!(parsed["series"][0].get("fields").is_none());
    assert_eq!(parsed["series"][0]["values"][2], serde_json::Value::Null);
    assert_eq!(parsed["fills"][0]["values"].as_array().unwrap().len(), 4096);
}
