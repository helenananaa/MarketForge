use super::append_history::AppendHistory;
use super::drawing_history::{
    DrawingStore, RuntimeBox, RuntimeLabel, RuntimeLine, RuntimeLineFill, RuntimePolyline,
    RuntimeTable,
};
use super::historical::HistoricalRuntime;
use crate::output::changes::{
    DrawingAction, DrawingChange, DrawingFamily, DrawingObject, EventAction, EventChange,
    FillAction, FillChange, HLineAction, HLineChange, ListSplice, RuntimeChanges, SeriesFamily,
    SeriesFields, SeriesHeader, StrategyChanges, StreamingVisibility, series_change_from_lens,
};
use crate::{PineValue, StrategyOrderFillAlertOutput};

#[derive(Debug, Clone, Default)]
pub(crate) struct OutputCursor {
    plots: Vec<(u32, usize)>,
    plot_chars: Vec<(u32, usize)>,
    plot_shapes: Vec<(u32, usize)>,
    plot_arrows: Vec<(u32, usize)>,
    plot_bars: Vec<(u32, usize)>,
    plot_candles: Vec<(u32, usize)>,
    bg_colors: Vec<(u32, usize)>,
    bar_colors: Vec<(u32, usize)>,
    hlines: Vec<crate::HLineOutput>,
    fills: Vec<(u32, usize)>,
    labels: DrawingStore<RuntimeLabel>,
    lines: DrawingStore<RuntimeLine>,
    line_fills: DrawingStore<RuntimeLineFill>,
    polylines: DrawingStore<RuntimePolyline>,
    boxes: DrawingStore<RuntimeBox>,
    tables: DrawingStore<RuntimeTable>,
    alerts: Option<AppendHistory<crate::AlertEvent>>,
    alert_bar: usize,
    drawing_bar: usize,
    orders: usize,
    trades: usize,
    fill_alerts: usize,
    position: usize,
    equity: usize,
    has_strategy: bool,
}

impl OutputCursor {
    pub(crate) fn capture(runtime: &HistoricalRuntime<'_>) -> Self {
        let alert_start = runtime
            .alerts
            .partition_point(|event| event.bar_index < runtime.bars.saturating_sub(1));
        // Retain only the current bar's logical events. Prefix pruning shares
        // leaves and copies branch paths, without cloning alert string payloads.
        let alerts = (alert_start < runtime.alerts.len()).then(|| {
            let mut alerts = runtime.alerts.clone();
            alerts.drop_prefix(alert_start);
            alerts
        });
        Self {
            plots: runtime
                .plots
                .iter()
                .map(|item| (item.id, abs_len(runtime, item.values.len())))
                .collect(),
            plot_chars: runtime
                .plot_chars
                .iter()
                .map(|item| (item.id, abs_len(runtime, item.values.len())))
                .collect(),
            plot_shapes: runtime
                .plot_shapes
                .iter()
                .map(|item| (item.id, abs_len(runtime, item.values.len())))
                .collect(),
            plot_arrows: runtime
                .plot_arrows
                .iter()
                .map(|item| (item.id, abs_len(runtime, item.values.len())))
                .collect(),
            plot_bars: runtime
                .plot_bars
                .iter()
                .map(|item| (item.id, abs_len(runtime, item.closes.len())))
                .collect(),
            plot_candles: runtime
                .plot_candles
                .iter()
                .map(|item| (item.id, abs_len(runtime, item.closes.len())))
                .collect(),
            bg_colors: runtime
                .bg_colors
                .iter()
                .map(|item| (item.id, abs_len(runtime, item.values.len())))
                .collect(),
            bar_colors: runtime
                .bar_colors
                .iter()
                .map(|item| (item.id, abs_len(runtime, item.values.len())))
                .collect(),
            hlines: runtime.hlines.clone(),
            fills: runtime
                .fills
                .iter()
                .map(|item| (item.id, abs_len(runtime, item.colors.len())))
                .collect(),
            labels: runtime.labels.clone(),
            lines: runtime.lines.clone(),
            line_fills: runtime.line_fills.clone(),
            polylines: runtime.polylines.clone(),
            boxes: runtime.boxes.clone(),
            tables: runtime.tables.clone(),
            alerts,
            alert_bar: runtime.bars.saturating_sub(1),
            drawing_bar: runtime.bars.saturating_sub(1),
            orders: runtime.strategy_broker.order_len(),
            trades: runtime.strategy_broker.trade_len(),
            fill_alerts: runtime.strategy_broker.fill_alert_len(),
            position: runtime.strategy_broker.position_len(),
            equity: runtime.strategy_broker.equity_len(),
            has_strategy: runtime.program.script_mode == pine_ir::ScriptMode::Strategy,
        }
    }

    pub(crate) fn diff(
        &self,
        runtime: &HistoricalRuntime<'_>,
        revision: u64,
        visibility: StreamingVisibility,
    ) -> RuntimeChanges {
        let mut changes = RuntimeChanges::new(revision, visibility);
        diff_plots(&mut changes, &self.plots, runtime);
        diff_plot_chars(&mut changes, &self.plot_chars, runtime);
        diff_plot_shapes(&mut changes, &self.plot_shapes, runtime);
        diff_plot_arrows(&mut changes, &self.plot_arrows, runtime);
        diff_plot_bars(&mut changes, &self.plot_bars, runtime);
        diff_plot_candles(&mut changes, &self.plot_candles, runtime);
        diff_color_series(
            &mut changes,
            SeriesFamily::BgColor,
            &self.bg_colors,
            runtime,
            &runtime.bg_colors,
        );
        diff_color_series(
            &mut changes,
            SeriesFamily::BarColor,
            &self.bar_colors,
            runtime,
            &runtime.bar_colors,
        );
        diff_hlines(&mut changes, &self.hlines, &runtime.hlines);
        diff_fills(&mut changes, &self.fills, runtime);
        diff_label_drawings(
            &mut changes,
            &self.labels,
            &runtime.labels,
            self.drawing_bar,
            runtime.display_origin,
        );
        diff_line_drawings(
            &mut changes,
            &self.lines,
            &runtime.lines,
            self.drawing_bar,
            runtime.display_origin,
        );
        diff_line_fill_drawings(
            &mut changes,
            &self.line_fills,
            &runtime.line_fills,
            self.drawing_bar,
            runtime.display_origin,
        );
        diff_polyline_drawings(
            &mut changes,
            &self.polylines,
            &runtime.polylines,
            self.drawing_bar,
            runtime.display_origin,
        );
        diff_box_drawings(
            &mut changes,
            &self.boxes,
            &runtime.boxes,
            self.drawing_bar,
            runtime.display_origin,
        );
        diff_table_drawings(
            &mut changes,
            &self.tables,
            &runtime.tables,
            self.drawing_bar,
            runtime.display_origin,
        );
        diff_alerts(
            &mut changes,
            self.alerts.as_ref().map_or(0, AppendHistory::len),
            self.alerts.iter().flat_map(AppendHistory::iter),
            self.alerts.iter().flat_map(AppendHistory::iter_rev),
            runtime.alerts.iter().skip(
                runtime
                    .alerts
                    .partition_point(|event| event.bar_index < self.alert_bar),
            ),
        );
        if runtime.program.script_mode == pine_ir::ScriptMode::Strategy {
            changes.strategy = Some(diff_strategy(self, runtime));
        } else if self.has_strategy {
            changes.strategy = Some(StrategyChanges::default());
        }
        changes.diagnostics = runtime.runtime_diagnostics();
        changes
    }
}

fn lens_get(items: &[(u32, usize)], id: u32) -> Option<usize> {
    items
        .iter()
        .find_map(|(item_id, len)| (*item_id == id).then_some(*len))
}

fn abs_len(runtime: &HistoricalRuntime<'_>, local: usize) -> usize {
    runtime.stored_origin + local
}

fn slice_from(
    values: &super::append_history::AppendHistory<PineValue>,
    start: usize,
    stored_origin: usize,
) -> Vec<PineValue> {
    values.tail(start.saturating_sub(stored_origin))
}

fn plot_header(plot: &super::plot_history::RuntimePlot) -> SeriesHeader {
    SeriesHeader {
        metadata: plot.metadata.clone(),
        linewidth: plot.linewidth.clone(),
        style: plot.style.clone(),
        track_price: plot.track_price.clone(),
        hist_base: plot.hist_base.clone(),
        join: plot.join.clone(),
        format: plot.format.clone(),
        precision: plot.precision.clone(),
        linestyle: plot.linestyle.clone(),
    }
}

fn metadata_header(metadata: &crate::OutputMetadata) -> SeriesHeader {
    SeriesHeader {
        metadata: metadata.clone(),
        ..SeriesHeader::default()
    }
}

fn diff_plots(
    changes: &mut RuntimeChanges,
    cursor: &[(u32, usize)],
    runtime: &HistoricalRuntime<'_>,
) {
    for plot in runtime.plots.iter() {
        if let Some(change) = series_change_from_lens(
            SeriesFamily::Plot,
            plot.id,
            lens_get(cursor, plot.id),
            abs_len(runtime, plot.values.len()),
            runtime.display_origin,
            |start| SeriesFields {
                values: plot
                    .values
                    .tail(start.saturating_sub(runtime.stored_origin)),
                colors: plot
                    .colors
                    .tail(start.saturating_sub(runtime.stored_origin)),
                ..SeriesFields::default()
            },
            || Some(plot_header(plot)),
        ) {
            changes.series.push(change);
        }
    }
}

fn diff_plot_chars(
    changes: &mut RuntimeChanges,
    cursor: &[(u32, usize)],
    runtime: &HistoricalRuntime<'_>,
) {
    for item in &runtime.plot_chars {
        if let Some(change) = series_change_from_lens(
            SeriesFamily::PlotChar,
            item.id,
            lens_get(cursor, item.id),
            abs_len(runtime, item.values.len()),
            runtime.display_origin,
            |start| SeriesFields {
                values: slice_from(&item.values, start, runtime.stored_origin),
                colors: slice_from(&item.colors, start, runtime.stored_origin),
                chars: slice_from(&item.chars, start, runtime.stored_origin),
                locations: slice_from(&item.locations, start, runtime.stored_origin),
                texts: slice_from(&item.texts, start, runtime.stored_origin),
                text_colors: slice_from(&item.text_colors, start, runtime.stored_origin),
                sizes: slice_from(&item.sizes, start, runtime.stored_origin),
                ..SeriesFields::default()
            },
            || Some(metadata_header(&item.metadata)),
        ) {
            changes.series.push(change);
        }
    }
}

fn diff_plot_shapes(
    changes: &mut RuntimeChanges,
    cursor: &[(u32, usize)],
    runtime: &HistoricalRuntime<'_>,
) {
    for item in &runtime.plot_shapes {
        if let Some(change) = series_change_from_lens(
            SeriesFamily::PlotShape,
            item.id,
            lens_get(cursor, item.id),
            abs_len(runtime, item.values.len()),
            runtime.display_origin,
            |start| SeriesFields {
                values: slice_from(&item.values, start, runtime.stored_origin),
                styles: slice_from(&item.styles, start, runtime.stored_origin),
                locations: slice_from(&item.locations, start, runtime.stored_origin),
                colors: slice_from(&item.colors, start, runtime.stored_origin),
                texts: slice_from(&item.texts, start, runtime.stored_origin),
                text_colors: slice_from(&item.text_colors, start, runtime.stored_origin),
                sizes: slice_from(&item.sizes, start, runtime.stored_origin),
                ..SeriesFields::default()
            },
            || Some(metadata_header(&item.metadata)),
        ) {
            changes.series.push(change);
        }
    }
}

fn diff_plot_arrows(
    changes: &mut RuntimeChanges,
    cursor: &[(u32, usize)],
    runtime: &HistoricalRuntime<'_>,
) {
    for item in &runtime.plot_arrows {
        if let Some(change) = series_change_from_lens(
            SeriesFamily::PlotArrow,
            item.id,
            lens_get(cursor, item.id),
            abs_len(runtime, item.values.len()),
            runtime.display_origin,
            |start| SeriesFields {
                values: slice_from(&item.values, start, runtime.stored_origin),
                color_ups: slice_from(&item.color_ups, start, runtime.stored_origin),
                color_downs: slice_from(&item.color_downs, start, runtime.stored_origin),
                min_heights: slice_from(&item.min_heights, start, runtime.stored_origin),
                max_heights: slice_from(&item.max_heights, start, runtime.stored_origin),
                ..SeriesFields::default()
            },
            || Some(metadata_header(&item.metadata)),
        ) {
            changes.series.push(change);
        }
    }
}

fn diff_plot_bars(
    changes: &mut RuntimeChanges,
    cursor: &[(u32, usize)],
    runtime: &HistoricalRuntime<'_>,
) {
    for item in &runtime.plot_bars {
        if let Some(change) = series_change_from_lens(
            SeriesFamily::PlotBar,
            item.id,
            lens_get(cursor, item.id),
            abs_len(runtime, item.closes.len()),
            runtime.display_origin,
            |start| SeriesFields {
                opens: slice_from(&item.opens, start, runtime.stored_origin),
                highs: slice_from(&item.highs, start, runtime.stored_origin),
                lows: slice_from(&item.lows, start, runtime.stored_origin),
                closes: slice_from(&item.closes, start, runtime.stored_origin),
                colors: slice_from(&item.colors, start, runtime.stored_origin),
                ..SeriesFields::default()
            },
            || Some(metadata_header(&item.metadata)),
        ) {
            changes.series.push(change);
        }
    }
}

fn diff_plot_candles(
    changes: &mut RuntimeChanges,
    cursor: &[(u32, usize)],
    runtime: &HistoricalRuntime<'_>,
) {
    for item in &runtime.plot_candles {
        if let Some(change) = series_change_from_lens(
            SeriesFamily::PlotCandle,
            item.id,
            lens_get(cursor, item.id),
            abs_len(runtime, item.closes.len()),
            runtime.display_origin,
            |start| SeriesFields {
                opens: slice_from(&item.opens, start, runtime.stored_origin),
                highs: slice_from(&item.highs, start, runtime.stored_origin),
                lows: slice_from(&item.lows, start, runtime.stored_origin),
                closes: slice_from(&item.closes, start, runtime.stored_origin),
                colors: slice_from(&item.colors, start, runtime.stored_origin),
                wick_colors: slice_from(&item.wick_colors, start, runtime.stored_origin),
                border_colors: slice_from(&item.border_colors, start, runtime.stored_origin),
                ..SeriesFields::default()
            },
            || Some(metadata_header(&item.metadata)),
        ) {
            changes.series.push(change);
        }
    }
}

fn diff_color_series(
    changes: &mut RuntimeChanges,
    family: SeriesFamily,
    cursor: &[(u32, usize)],
    runtime: &HistoricalRuntime<'_>,
    items: &[super::plot_history::RuntimeColorSeries],
) {
    for item in items {
        if let Some(change) = series_change_from_lens(
            family,
            item.id,
            lens_get(cursor, item.id),
            abs_len(runtime, item.values.len()),
            runtime.display_origin,
            |start| SeriesFields {
                values: slice_from(&item.values, start, runtime.stored_origin),
                ..SeriesFields::default()
            },
            || Some(metadata_header(&item.metadata)),
        ) {
            changes.series.push(change);
        }
    }
}

fn diff_hlines(
    changes: &mut RuntimeChanges,
    cursor: &[crate::HLineOutput],
    items: &[crate::HLineOutput],
) {
    for item in items {
        match cursor.iter().find(|hline| hline.id == item.id) {
            None => changes.hlines.push(HLineChange {
                id: item.id,
                action: HLineAction::Add(item.clone()),
            }),
            Some(previous) if previous != item => changes.hlines.push(HLineChange {
                id: item.id,
                action: HLineAction::Replace(item.clone()),
            }),
            Some(_) => {}
        }
    }
    for previous in cursor {
        if !items.iter().any(|item| item.id == previous.id) {
            changes.hlines.push(HLineChange {
                id: previous.id,
                action: HLineAction::Delete,
            });
        }
    }
}

fn diff_fills(
    changes: &mut RuntimeChanges,
    cursor: &[(u32, usize)],
    runtime: &HistoricalRuntime<'_>,
) {
    for item in &runtime.fills {
        match lens_get(cursor, item.id) {
            None => changes.fills.push(FillChange {
                id: item.id,
                action: FillAction::Add(item.snapshot_from(runtime.display_skip())),
            }),
            Some(old_len) => {
                let new_len = abs_len(runtime, item.colors.len());
                let start = delta_start(old_len, new_len).max(runtime.display_origin);
                let values = slice_from(&item.colors, start, runtime.stored_origin);
                if !values.is_empty() || start < old_len {
                    changes.fills.push(FillChange {
                        id: item.id,
                        action: FillAction::SetColors { start, values },
                    });
                }
                if let Some(samples) = &item.gradient {
                    changes.fills.push(FillChange {
                        id: item.id,
                        action: FillAction::SetGradient {
                            start,
                            values: samples.tail(start.saturating_sub(runtime.stored_origin)),
                        },
                    });
                }
            }
        }
    }
    for (id, _) in cursor {
        if !runtime.fills.iter().any(|item| item.id == *id) {
            changes.fills.push(FillChange {
                id: *id,
                action: FillAction::Delete,
            });
        }
    }
}

fn delta_start(old_len: usize, new_len: usize) -> usize {
    if new_len > old_len {
        old_len
    } else if new_len == old_len && new_len > 0 {
        new_len - 1
    } else {
        0
    }
}

// Snapshot indices are chronological. If the last snapshot is already
// stable, every snapshot is stable and there is no boundary to search.
fn stable_drawing_len<S>(
    history: &super::append_history::AppendHistory<S>,
    bar: usize,
    index: impl Fn(&S) -> usize,
) -> usize {
    if history.last().is_none_or(|last| index(last) < bar) {
        history.len()
    } else {
        history.partition_point(|snapshot| index(snapshot) < bar)
    }
}

// Retention projects a live object without recent changes to its last old
// snapshot. Once it changes, that fallback disappears; rollback can restore
// it. Raw history indices therefore are not splice indices for this view.
// Replace only the changed object's retained projection in that mode.
fn drawing_tail_range<S: Clone + super::display_retention::HasBar>(
    snapshots: &super::append_history::AppendHistory<S>,
    bar: usize,
    origin: usize,
    previous: bool,
) -> (usize, usize) {
    if origin > 0 {
        (
            0,
            super::display_retention::dropped_snapshot_count(snapshots, origin),
        )
    } else {
        let start = if previous {
            stable_drawing_len(snapshots, bar, |s| s.bar_index())
        } else {
            0
        };
        (start, start)
    }
}

fn diff_label_drawings(
    changes: &mut RuntimeChanges,
    cursor: &DrawingStore<RuntimeLabel>,
    items: &DrawingStore<RuntimeLabel>,
    bar: usize,
    origin: usize,
) {
    diff_drawings(
        changes,
        DrawingFamily::Label,
        cursor,
        items,
        |id, previous, item| {
            let (start, offset) =
                drawing_tail_range(&item.snapshots, bar, origin, previous.is_some());
            if offset == item.snapshots.len() && origin > 0 {
                return DrawingAction::Delete;
            }
            let object = DrawingObject::Label(crate::LabelOutput {
                id,
                snapshots: item.snapshots.tail(offset),
            });
            if previous.is_none() {
                DrawingAction::Add(object)
            } else {
                DrawingAction::SetTail { start, object }
            }
        },
    );
}

fn diff_line_drawings(
    changes: &mut RuntimeChanges,
    cursor: &DrawingStore<RuntimeLine>,
    items: &DrawingStore<RuntimeLine>,
    bar: usize,
    origin: usize,
) {
    diff_drawings(
        changes,
        DrawingFamily::Line,
        cursor,
        items,
        |id, previous, item| {
            let (start, offset) =
                drawing_tail_range(&item.snapshots, bar, origin, previous.is_some());
            if offset == item.snapshots.len() && origin > 0 {
                return DrawingAction::Delete;
            }
            let object = DrawingObject::Line(crate::LineOutput {
                id,
                snapshots: item.snapshots.tail(offset),
            });
            if previous.is_none() {
                DrawingAction::Add(object)
            } else {
                DrawingAction::SetTail { start, object }
            }
        },
    );
}

fn diff_line_fill_drawings(
    changes: &mut RuntimeChanges,
    cursor: &DrawingStore<RuntimeLineFill>,
    items: &DrawingStore<RuntimeLineFill>,
    bar: usize,
    origin: usize,
) {
    diff_drawings(
        changes,
        DrawingFamily::LineFill,
        cursor,
        items,
        |id, previous, item| {
            let (start, offset) =
                drawing_tail_range(&item.snapshots, bar, origin, previous.is_some());
            if offset == item.snapshots.len() && origin > 0 {
                return DrawingAction::Delete;
            }
            let object = DrawingObject::LineFill(crate::LineFillOutput {
                id,
                snapshots: item.snapshots.tail(offset),
            });
            if previous.is_none() {
                DrawingAction::Add(object)
            } else {
                DrawingAction::SetTail { start, object }
            }
        },
    );
}

fn diff_polyline_drawings(
    changes: &mut RuntimeChanges,
    cursor: &DrawingStore<RuntimePolyline>,
    items: &DrawingStore<RuntimePolyline>,
    bar: usize,
    origin: usize,
) {
    diff_drawings(
        changes,
        DrawingFamily::Polyline,
        cursor,
        items,
        |id, previous, item| {
            let (start, offset) =
                drawing_tail_range(&item.snapshots, bar, origin, previous.is_some());
            if offset == item.snapshots.len() && origin > 0 {
                return DrawingAction::Delete;
            }
            let object = DrawingObject::Polyline(crate::PolylineOutput {
                id,
                snapshots: item.snapshots.tail(offset),
            });
            if previous.is_none() {
                DrawingAction::Add(object)
            } else {
                DrawingAction::SetTail { start, object }
            }
        },
    );
}

fn diff_box_drawings(
    changes: &mut RuntimeChanges,
    cursor: &DrawingStore<RuntimeBox>,
    items: &DrawingStore<RuntimeBox>,
    bar: usize,
    origin: usize,
) {
    diff_drawings(
        changes,
        DrawingFamily::Box,
        cursor,
        items,
        |id, previous, item| {
            let (start, offset) =
                drawing_tail_range(&item.snapshots, bar, origin, previous.is_some());
            if offset == item.snapshots.len() && origin > 0 {
                return DrawingAction::Delete;
            }
            let object = DrawingObject::Box(crate::BoxOutput {
                id,
                snapshots: item.snapshots.tail(offset),
            });
            if previous.is_none() {
                DrawingAction::Add(object)
            } else {
                DrawingAction::SetTail { start, object }
            }
        },
    );
}

fn diff_table_drawings(
    changes: &mut RuntimeChanges,
    cursor: &DrawingStore<RuntimeTable>,
    items: &DrawingStore<RuntimeTable>,
    bar: usize,
    origin: usize,
) {
    diff_drawings(
        changes,
        DrawingFamily::Table,
        cursor,
        items,
        |id, previous, item| {
            let (start, offset) =
                drawing_tail_range(&item.snapshots, bar, origin, previous.is_some());
            if offset == item.snapshots.len() && origin > 0 {
                return DrawingAction::Delete;
            }
            let object = DrawingObject::Table(Box::new(crate::TableOutput {
                id,
                snapshots: item.snapshots.tail(offset),
                position: item.position.clone(),
                bg_color: item.bg_color.clone(),
                frame_color: item.frame_color.clone(),
                frame_width: item.frame_width.clone(),
                border_color: item.border_color.clone(),
                border_width: item.border_width.clone(),
                columns: item.columns,
                rows: item.rows,
            }));
            if previous.is_none() {
                DrawingAction::Add(object)
            } else {
                DrawingAction::SetTail { start, object }
            }
        },
    );
}

// Cursors retain the prior persistent root. A forming replacement can
// diverge from that root in either direction, so visit both trees: this also
// retracts preview-only identities and tails after rollback. Shared identity
// is the only skip criterion; equal lengths and NaN values are not shortcuts.
fn diff_drawings<V: Clone + super::drawing_history::DrawingIdentity>(
    changes: &mut RuntimeChanges,
    family: DrawingFamily,
    previous: &DrawingStore<V>,
    current: &DrawingStore<V>,
    mut action: impl FnMut(u32, Option<&V>, &V) -> DrawingAction,
) {
    let mut removed = Vec::new();
    current.visit_differences(previous, |id, old, new| {
        if let Some(item) = new {
            changes.drawings.push(DrawingChange {
                family,
                id,
                action: action(id, old, item),
            });
        } else {
            removed.push(id);
        }
    });
    // Preserve the existing order: additions/updates first, removals last.
    changes
        .drawings
        .extend(removed.into_iter().map(|id| DrawingChange {
            family,
            id,
            action: DrawingAction::Delete,
        }));
}

fn diff_alerts<'a>(
    changes: &mut RuntimeChanges,
    previous_len: usize,
    previous: impl Iterator<Item = &'a crate::AlertEvent>,
    previous_rev: impl Iterator<Item = &'a crate::AlertEvent>,
    current: impl Iterator<Item = &'a crate::AlertEvent>,
) {
    let mut previous = previous.peekable();
    let mut current = current.peekable();
    let mut prefix = 0;
    while let (Some(a), Some(b)) = (previous.peek(), current.peek()) {
        if a != b {
            break;
        }
        previous.next();
        current.next();
        prefix += 1;
    }
    // Preserve occurrence counts for alert.freq_all calls with identical payloads.
    for event in previous_rev.take(previous_len - prefix) {
        changes.alerts.push(EventChange {
            action: EventAction::Remove,
            event: event.clone(),
        });
    }
    for event in current {
        changes.alerts.push(EventChange {
            action: EventAction::Add,
            event: event.clone(),
        });
    }
}

fn diff_strategy(cursor: &OutputCursor, runtime: &HistoricalRuntime<'_>) -> StrategyChanges {
    let broker = &runtime.strategy_broker;
    StrategyChanges {
        orders: list_splice(cursor.orders, broker.order_len(), |start| {
            broker.order_tail(start)
        }),
        trades: list_splice(cursor.trades, broker.trade_len(), |start| {
            broker.trade_tail(start)
        }),
        alerts: fill_alert_splice(cursor.fill_alerts, broker),
        position: list_splice(cursor.position, broker.position_len(), |start| {
            broker.position_tail(start)
        }),
        equity: list_splice(cursor.equity, broker.equity_len(), |start| {
            broker.equity_tail(start)
        }),
        diagnostics: Some(broker.diagnostics_slice().to_vec()),
    }
}

fn list_splice<T: Clone>(
    old_len: usize,
    new_len: usize,
    suffix: impl FnOnce(usize) -> Vec<T>,
) -> Option<ListSplice<T>> {
    if new_len == old_len && new_len == 0 {
        return None;
    }
    if new_len > old_len {
        return Some(ListSplice {
            start: old_len,
            items: suffix(old_len),
        });
    }
    if new_len == old_len {
        return Some(ListSplice {
            start: new_len - 1,
            items: suffix(new_len - 1),
        });
    }
    Some(ListSplice {
        start: new_len,
        items: Vec::new(),
    })
}

fn fill_alert_splice(
    old_len: usize,
    broker: &crate::BrokerState,
) -> Option<ListSplice<StrategyOrderFillAlertOutput>> {
    let new_len = broker.fill_alert_len();
    if new_len == old_len && new_len == 0 {
        return None;
    }
    if new_len < old_len {
        return Some(ListSplice {
            start: new_len,
            items: Vec::new(),
        });
    }
    let start = delta_start(old_len, new_len);
    Some(ListSplice {
        start,
        items: broker.fill_alerts_from(start),
    })
}

#[cfg(test)]
mod alert_cursor_tests {
    use super::{HistoricalRuntime, OutputCursor};

    #[test]
    fn empty_cursors_have_no_alert_root_and_nonempty_cursors_share_only_a_bounded_tail() {
        assert!(OutputCursor::default().alerts.is_none());
        let program = pine_sema::analyze_source(&pine_syntax::SourceFile::new(
            "cursor.pine",
            "//@version=6\nindicator(\"cursor\")\n",
        ))
        .hir
        .unwrap();
        let mut runtime = HistoricalRuntime::new(&program);
        assert!(OutputCursor::capture(&runtime).alerts.is_none());
        for index in 0..4099 {
            runtime.alerts.push(crate::AlertEvent {
                id: 1,
                bar_index: index / 257,
                time: (index / 257) as i64 * 60_000,
                message: "共享汉🙂".repeat(32),
                source: "alert".to_owned(),
            });
        }
        runtime.bars = 16;
        let cursor = OutputCursor::capture(&runtime);
        let tail = cursor.alerts.as_ref().unwrap();
        assert_eq!(tail.len(), 244);
        assert!(tail.capacity() <= tail.len() + 254);
        for (saved, original) in tail.iter().zip(runtime.alerts.iter().skip(3855)) {
            assert_eq!(saved, original);
            assert_eq!(saved.message.as_ptr(), original.message.as_ptr());
            assert_eq!(saved.source.as_ptr(), original.source.as_ptr());
        }
        // All alerts are now closed; capture must not allocate an empty root
        // or retain the expired historical prefix for the new quiet bar.
        runtime.bars = 17;
        assert!(OutputCursor::capture(&runtime).alerts.is_none());
        assert_eq!(runtime.alerts.len(), 4099);
    }
}

#[cfg(test)]
mod drawing_cursor_tests {

    #[test]
    fn stable_boundary_matches_chronological_reference_after_trim() {
        use crate::runtime::append_history::AppendHistory;
        let mut history = AppendHistory::<usize>::default();
        assert_eq!(super::stable_drawing_len(&history, 0, |value| *value), 0);
        for index in 0..1400 {
            history.push(index / 3);
        }
        for trim in [0, 500] {
            history.drop_prefix(trim);
            for bar in [0, 1, 127, 128, 255, 400, 466, 467, 2000] {
                let expected = history.iter().take_while(|value| **value < bar).count();
                assert_eq!(
                    super::stable_drawing_len(&history, bar, |value| *value),
                    expected
                );
            }
        }
    }
}
