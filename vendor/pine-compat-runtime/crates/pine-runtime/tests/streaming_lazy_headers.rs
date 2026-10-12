use pine_runtime::{
    Bar, BarUpdate, OutputMetadata, OutputRetention, PineValue, RealtimeRuntime, RuntimeChanges,
    RuntimeReplica, SeriesChangeOp, SeriesFamily, SeriesHeader, public_runtime_changes_json,
    public_runtime_result_json, runtime_changes_from_json,
};
use pine_sema::analyze_source;
use pine_syntax::SourceFile;

const SOURCE: &str = r#"//@version=6
indicator("lazy series headers")
plot(close, title="P 标题", color=color.red, linewidth=3, style=plot.style_stepline, trackprice=true, histbase=-2, offset=2, join=true, editable=false, show_last=7, display=display.data_window, format=format.price, precision=4, force_overlay=true, linestyle=plot.linestyle_dotted)
plotchar(close>10, title="C 标题", char="x", color=color.blue, location=location.abovebar, offset=1, text="字符", textcolor=color.red, editable=false, size=size.small, show_last=8, display=display.data_window)
plotshape(close>10, title="S 标题", style=shape.circle, location=location.belowbar, color=color.green, offset=-1, text="形状", textcolor=color.blue, editable=false, size=size.small, show_last=9, display=display.data_window, force_overlay=true)
plotarrow(close-10, title="A 标题", colorup=color.green, colordown=color.red, offset=3, minheight=5, maxheight=30, editable=false, show_last=10, display=display.data_window, force_overlay=true)
plotbar(open,high,low,close, title="B 标题", color=color.orange, editable=false, show_last=11, display=display.data_window)
plotcandle(open,high,low,close, title="K 标题", color=color.blue, wickcolor=color.red, bordercolor=color.green, editable=false, show_last=12, display=display.data_window)
bgcolor(close>10?color.red:color.blue, title="G 标题", offset=-2, editable=false, show_last=13, display=display.data_window)
barcolor(close>10?color.green:color.orange, title="R 标题", offset=4, editable=false, show_last=14, display=display.data_window)
if volume>10
    runtime.error("lazy header rollback")
"#;

const FAMILIES: [SeriesFamily; 8] = [
    SeriesFamily::Plot,
    SeriesFamily::PlotChar,
    SeriesFamily::PlotShape,
    SeriesFamily::PlotArrow,
    SeriesFamily::PlotBar,
    SeriesFamily::PlotCandle,
    SeriesFamily::BgColor,
    SeriesFamily::BarColor,
];

fn runtime() -> RealtimeRuntime<'static> {
    let analysis = analyze_source(&SourceFile::new("lazy_headers.pine", SOURCE));
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    RealtimeRuntime::from_program(analysis.hir.unwrap())
}

fn bar(index: i64, close: f64, volume: f64) -> Bar {
    Bar {
        time: index * 60_000,
        open: close - 0.5,
        high: close + 1.0,
        low: close - 1.0,
        close,
        volume,
    }
}

fn expected_header(family: SeriesFamily) -> SeriesHeader {
    let (title, offset, show_last, overlay) = match family {
        SeriesFamily::Plot => ("P 标题", 2, 7, true),
        SeriesFamily::PlotChar => ("C 标题", 1, 8, false),
        SeriesFamily::PlotShape => ("S 标题", -1, 9, true),
        SeriesFamily::PlotArrow => ("A 标题", 3, 10, true),
        SeriesFamily::PlotBar => ("B 标题", 0, 11, false),
        SeriesFamily::PlotCandle => ("K 标题", 0, 12, false),
        SeriesFamily::BgColor => ("G 标题", -2, 13, false),
        SeriesFamily::BarColor => ("R 标题", 4, 14, false),
    };
    let mut expected = SeriesHeader {
        metadata: OutputMetadata {
            title: PineValue::String(title.into()),
            offset: PineValue::Int(offset),
            editable: PineValue::Bool(false),
            show_last: PineValue::Int(show_last),
            display: PineValue::String("display.data_window".into()),
            force_overlay: PineValue::Bool(overlay),
        },
        ..SeriesHeader::default()
    };
    if family == SeriesFamily::Plot {
        expected.linewidth = PineValue::Int(3);
        expected.style = PineValue::String("plot.style_stepline".into());
        expected.track_price = PineValue::Bool(true);
        expected.hist_base = PineValue::Int(-2);
        expected.join = PineValue::Bool(true);
        expected.format = PineValue::String("format.price".into());
        expected.precision = PineValue::Int(4);
        expected.linestyle = PineValue::String("plot.linestyle_dotted".into());
    }
    expected
}

fn assert_series(changes: &RuntimeChanges, op: SeriesChangeOp, start: usize, fresh: bool) {
    assert_eq!(changes.series.len(), FAMILIES.len());
    for family in FAMILIES {
        let matches: Vec<_> = changes
            .series
            .iter()
            .filter(|change| change.family == family)
            .collect();
        assert_eq!(matches.len(), 1, "{family:?}");
        let change = matches[0];
        assert_eq!(change.op, op, "{family:?}");
        assert_eq!(change.start, start, "{family:?}");
        if fresh {
            assert_eq!(change.header.as_ref(), Some(&expected_header(family)));
        } else {
            assert_eq!(change.header, None, "{family:?}");
        }
    }
}

fn apply_replica(
    runtime: &RealtimeRuntime<'_>,
    replica: &mut RuntimeReplica,
    changes: &RuntimeChanges,
) {
    let encoded = public_runtime_changes_json(changes);
    let decoded = runtime_changes_from_json(&encoded).unwrap();
    assert_eq!(public_runtime_changes_json(&decoded), encoded);
    // Public JSON deliberately represents both Color and Int as numbers. Check
    // the wire consumer's entire public snapshot separately from typed deltas.
    let mut wire_replica = RuntimeReplica::with_retained_from(
        replica.result().clone(),
        replica.revision(),
        replica.retained_from(),
    );
    assert!(wire_replica.apply(&decoded).unwrap());
    assert!(!wire_replica.apply(&decoded).unwrap());
    assert_eq!(wire_replica.revision(), runtime.revision());
    assert_eq!(wire_replica.retained_from(), runtime.display_origin());
    assert_eq!(
        public_runtime_result_json(wire_replica.result()),
        public_runtime_result_json(&runtime.result())
    );
    assert!(replica.apply(changes).unwrap());
    assert!(!replica.apply(changes).unwrap());
    assert_eq!(replica.revision(), runtime.revision());
    assert_eq!(replica.retained_from(), runtime.display_origin());
    assert_eq!(replica.result(), &runtime.result());
    assert_eq!(
        public_runtime_result_json(replica.result()),
        public_runtime_result_json(&runtime.result())
    );
}

fn assert_failed_update_is_atomic(runtime: &mut RealtimeRuntime<'_>, sample: Bar) {
    let before = public_runtime_result_json(&runtime.result());
    let confirmed = public_runtime_result_json(&runtime.confirmed_result());
    let profile = runtime.profile();
    let confirmed_profile = runtime.confirmed_profile();
    let revision = runtime.revision();
    let changes = runtime.last_changes().cloned();
    let origin = runtime.display_origin();
    let count = runtime.confirmed_bar_count();
    let error = runtime
        .apply_update(BarUpdate::forming(sample))
        .unwrap_err();
    assert_eq!(error.message, "lazy header rollback");
    assert_eq!(public_runtime_result_json(&runtime.result()), before);
    assert_eq!(
        public_runtime_result_json(&runtime.confirmed_result()),
        confirmed
    );
    assert_eq!(runtime.profile(), profile);
    assert_eq!(runtime.confirmed_profile(), confirmed_profile);
    assert_eq!(runtime.revision(), revision);
    assert_eq!(runtime.last_changes(), changes.as_ref());
    assert_eq!(runtime.display_origin(), origin);
    assert_eq!(runtime.confirmed_bar_count(), count);
}

#[test]
fn streaming_lazy_headers_cover_all_series_families_and_later_edits() {
    let mut runtime = runtime();
    let mut replica = runtime.replica();
    let changes = runtime
        .apply_update(BarUpdate::forming(bar(0, 9.0, 1.0)))
        .unwrap();
    assert_series(&changes, SeriesChangeOp::Append, 0, true);
    apply_replica(&runtime, &mut replica, &changes);
    let owned_headers = changes.clone();
    let owned_json = public_runtime_changes_json(&owned_headers);

    for (update, op, start) in [
        (
            BarUpdate::forming(bar(0, 12.0, 1.0)),
            SeriesChangeOp::ReplaceLast,
            0,
        ),
        (
            BarUpdate::confirmed(bar(0, 11.0, 1.0)),
            SeriesChangeOp::ReplaceLast,
            0,
        ),
        (
            BarUpdate::forming(bar(1, 8.0, 1.0)),
            SeriesChangeOp::Append,
            1,
        ),
        (
            BarUpdate::forming(bar(1, 13.0, 1.0)),
            SeriesChangeOp::ReplaceLast,
            1,
        ),
        (
            BarUpdate::confirmed(bar(1, 10.0, 1.0)),
            SeriesChangeOp::ReplaceLast,
            1,
        ),
        (
            BarUpdate::confirmed(bar(2, 14.0, 1.0)),
            SeriesChangeOp::Append,
            2,
        ),
    ] {
        let changes = runtime.apply_update(update).unwrap();
        assert_series(&changes, op, start, false);
        apply_replica(&runtime, &mut replica, &changes);
        assert_eq!(public_runtime_changes_json(&owned_headers), owned_json);
    }
    // Independent fresh historical execution agrees with the fully reconstructed
    // committed output, not only the changed numeric field in each family.
    let mut batch = self::runtime();
    let expected = batch
        .seed_historical(&[bar(0, 11.0, 1.0), bar(1, 10.0, 1.0), bar(2, 14.0, 1.0)])
        .unwrap();
    assert_eq!(runtime.result(), expected);
    assert_eq!(replica.result(), &expected);
}

#[test]
fn streaming_lazy_headers_survive_retention_and_failed_forming_atomically() {
    let mut runtime = runtime().with_output_retention(OutputRetention::keep_confirmed_bars(2));
    let mut replica = runtime.replica();
    // Failure after all eight new series were constructed cannot publish headers
    // or advance the empty cursor. The next successful update is still fresh.
    assert_failed_update_is_atomic(&mut runtime, bar(0, 9.0, 11.0));
    let first = runtime
        .apply_update(BarUpdate::forming(bar(0, 9.0, 1.0)))
        .unwrap();
    assert_series(&first, SeriesChangeOp::Append, 0, true);
    apply_replica(&runtime, &mut replica, &first);
    let first_json = public_runtime_changes_json(&first);
    assert_failed_update_is_atomic(&mut runtime, bar(0, 12.0, 20.0));
    assert_eq!(
        public_runtime_result_json(replica.result()),
        public_runtime_result_json(&runtime.result())
    );

    let changes = runtime
        .apply_update(BarUpdate::confirmed(bar(0, 11.0, 1.0)))
        .unwrap();
    assert_series(&changes, SeriesChangeOp::ReplaceLast, 0, false);
    apply_replica(&runtime, &mut replica, &changes);
    for index in 1..=3 {
        let changes = runtime
            .apply_update(BarUpdate::confirmed(bar(index, 10.0 + index as f64, 1.0)))
            .unwrap();
        assert_series(&changes, SeriesChangeOp::Append, index as usize, false);
        apply_replica(&runtime, &mut replica, &changes);
    }
    assert_eq!(runtime.display_origin(), 2);
    assert_eq!(runtime.result().plots[0].values.len(), 2);
    assert_eq!(public_runtime_changes_json(&first), first_json);

    // Changing retention is an explicit snapshot/cursor replacement. A new
    // consumer starts at that snapshot and later edits still omit old headers.
    runtime.set_output_retention(OutputRetention::keep_confirmed_bars(1));
    replica.reset_with_retained_from(
        runtime.result(),
        runtime.revision(),
        runtime.display_origin(),
    );
    assert_eq!(runtime.display_origin(), 3);
    let preview = runtime
        .apply_update(BarUpdate::forming(bar(4, 8.0, 1.0)))
        .unwrap();
    assert_series(&preview, SeriesChangeOp::Append, 4, false);
    assert_eq!(runtime.display_origin(), 3);
    assert_eq!(runtime.result().plots[0].values.len(), 2);
    apply_replica(&runtime, &mut replica, &preview);
    assert_failed_update_is_atomic(&mut runtime, bar(4, 15.0, 11.0));
    assert_eq!(replica.result(), &runtime.result());
    let confirmed = runtime
        .apply_update(BarUpdate::confirmed(bar(4, 14.0, 1.0)))
        .unwrap();
    assert_series(&confirmed, SeriesChangeOp::ReplaceLast, 4, false);
    apply_replica(&runtime, &mut replica, &confirmed);
    assert_eq!(runtime.display_origin(), 4);
    assert_eq!(
        runtime.result().plots[0].values,
        vec![PineValue::Float(14.0)]
    );

    // Mutating a consumer-owned header does not modify the producer's metadata
    // or an earlier delta retained by a different consumer.
    let mut other_consumer = first.clone();
    for change in &mut other_consumer.series {
        change.header.as_mut().unwrap().metadata.title = PineValue::String("consumer edit".into());
    }
    assert_eq!(public_runtime_changes_json(&first), first_json);
    assert_eq!(
        runtime.result().plots[0].metadata,
        expected_header(SeriesFamily::Plot).metadata
    );
    assert_eq!(replica.result(), &runtime.result());
    let mut batch = self::runtime().with_output_retention(OutputRetention::keep_confirmed_bars(1));
    let expected = batch
        .seed_historical(&[
            bar(0, 11.0, 1.0),
            bar(1, 11.0, 1.0),
            bar(2, 12.0, 1.0),
            bar(3, 13.0, 1.0),
            bar(4, 14.0, 1.0),
        ])
        .unwrap();
    assert_eq!(runtime.result(), expected);
    assert_eq!(replica.result(), &expected);
}
