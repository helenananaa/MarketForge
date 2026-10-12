use super::display_retention::dropped_snapshot_count;
use super::historical::HistoricalRuntime;
use crate::*;
use pine_ir::ScriptMode;
use std::borrow::Cow;

impl HistoricalRuntime<'_> {
    /// Borrow the visible output. A mutable update cannot overlap this view.
    #[must_use]
    pub fn result_view(&self) -> RuntimeResultView<'_> {
        let skip = self.display_skip();
        RuntimeResultView {
            plots: self
                .plots
                .iter()
                .map(|item| PlotSeriesView {
                    id: item.id,
                    values: HistoryView::persistent(&item.values, skip),
                    colors: HistoryView::persistent(&item.colors, skip),
                    metadata: &item.metadata,
                    linewidth: &item.linewidth,
                    style: &item.style,
                    track_price: &item.track_price,
                    hist_base: &item.hist_base,
                    join: &item.join,
                    format: &item.format,
                    precision: &item.precision,
                    linestyle: &item.linestyle,
                })
                .collect(),
            plot_chars: self
                .plot_chars
                .iter()
                .map(|item| PlotCharSeriesView {
                    id: item.id,
                    values: HistoryView::persistent(&item.values, skip),
                    chars: HistoryView::persistent(&item.chars, skip),
                    colors: HistoryView::persistent(&item.colors, skip),
                    locations: HistoryView::persistent(&item.locations, skip),
                    texts: HistoryView::persistent(&item.texts, skip),
                    text_colors: HistoryView::persistent(&item.text_colors, skip),
                    sizes: HistoryView::persistent(&item.sizes, skip),
                    metadata: &item.metadata,
                })
                .collect(),
            plot_shapes: self
                .plot_shapes
                .iter()
                .map(|item| PlotShapeSeriesView {
                    id: item.id,
                    values: HistoryView::persistent(&item.values, skip),
                    styles: HistoryView::persistent(&item.styles, skip),
                    locations: HistoryView::persistent(&item.locations, skip),
                    colors: HistoryView::persistent(&item.colors, skip),
                    texts: HistoryView::persistent(&item.texts, skip),
                    text_colors: HistoryView::persistent(&item.text_colors, skip),
                    sizes: HistoryView::persistent(&item.sizes, skip),
                    metadata: &item.metadata,
                })
                .collect(),
            plot_arrows: self
                .plot_arrows
                .iter()
                .map(|item| PlotArrowSeriesView {
                    id: item.id,
                    values: HistoryView::persistent(&item.values, skip),
                    color_ups: HistoryView::persistent(&item.color_ups, skip),
                    color_downs: HistoryView::persistent(&item.color_downs, skip),
                    min_heights: HistoryView::persistent(&item.min_heights, skip),
                    max_heights: HistoryView::persistent(&item.max_heights, skip),
                    metadata: &item.metadata,
                })
                .collect(),
            plot_bars: self
                .plot_bars
                .iter()
                .map(|item| PlotBarSeriesView {
                    id: item.id,
                    opens: HistoryView::persistent(&item.opens, skip),
                    highs: HistoryView::persistent(&item.highs, skip),
                    lows: HistoryView::persistent(&item.lows, skip),
                    closes: HistoryView::persistent(&item.closes, skip),
                    colors: HistoryView::persistent(&item.colors, skip),
                    metadata: &item.metadata,
                })
                .collect(),
            plot_candles: self
                .plot_candles
                .iter()
                .map(|item| PlotCandleSeriesView {
                    id: item.id,
                    opens: HistoryView::persistent(&item.opens, skip),
                    highs: HistoryView::persistent(&item.highs, skip),
                    lows: HistoryView::persistent(&item.lows, skip),
                    closes: HistoryView::persistent(&item.closes, skip),
                    colors: HistoryView::persistent(&item.colors, skip),
                    wick_colors: HistoryView::persistent(&item.wick_colors, skip),
                    border_colors: HistoryView::persistent(&item.border_colors, skip),
                    metadata: &item.metadata,
                })
                .collect(),
            bg_colors: self
                .bg_colors
                .iter()
                .map(|item| ColorSeriesView {
                    id: item.id,
                    values: HistoryView::persistent(&item.values, skip),
                    metadata: &item.metadata,
                })
                .collect(),
            bar_colors: self
                .bar_colors
                .iter()
                .map(|item| ColorSeriesView {
                    id: item.id,
                    values: HistoryView::persistent(&item.values, skip),
                    metadata: &item.metadata,
                })
                .collect(),
            hlines: &self.hlines,
            fills: self
                .fills
                .iter()
                .map(|item| FillOutputView {
                    id: item.id,
                    first_id: item.first_id,
                    second_id: item.second_id,
                    first_is_hline: item.first_is_hline,
                    second_is_hline: item.second_is_hline,
                    colors: HistoryView::persistent(&item.colors, skip),
                    gradient: item
                        .gradient
                        .as_ref()
                        .map(|history| HistoryView::persistent(history, skip)),
                    title: &item.title,
                    editable: &item.editable,
                    show_last: &item.show_last,
                    fill_gaps: &item.fill_gaps,
                    display: &item.display,
                })
                .collect(),
            labels: self
                .labels
                .iter()
                .filter_map(|item| {
                    let snapshots = HistoryView::persistent(
                        &item.snapshots,
                        dropped_snapshot_count(&item.snapshots, self.display_origin),
                    );
                    (!snapshots.is_empty()).then_some(LabelOutputView {
                        id: item.id,
                        snapshots,
                    })
                })
                .collect(),
            lines: self
                .lines
                .iter()
                .filter_map(|item| {
                    let snapshots = HistoryView::persistent(
                        &item.snapshots,
                        dropped_snapshot_count(&item.snapshots, self.display_origin),
                    );
                    (!snapshots.is_empty()).then_some(LineOutputView {
                        id: item.id,
                        snapshots,
                    })
                })
                .collect(),
            line_fills: self
                .line_fills
                .iter()
                .filter_map(|item| {
                    let snapshots = HistoryView::persistent(
                        &item.snapshots,
                        dropped_snapshot_count(&item.snapshots, self.display_origin),
                    );
                    (!snapshots.is_empty()).then_some(LineFillOutputView {
                        id: item.id,
                        snapshots,
                    })
                })
                .collect(),
            polylines: self
                .polylines
                .iter()
                .filter_map(|item| {
                    let snapshots = HistoryView::persistent(
                        &item.snapshots,
                        dropped_snapshot_count(&item.snapshots, self.display_origin),
                    );
                    (!snapshots.is_empty()).then_some(PolylineOutputView {
                        id: item.id,
                        snapshots,
                    })
                })
                .collect(),
            boxes: self
                .boxes
                .iter()
                .filter_map(|item| {
                    let snapshots = HistoryView::persistent(
                        &item.snapshots,
                        dropped_snapshot_count(&item.snapshots, self.display_origin),
                    );
                    (!snapshots.is_empty()).then_some(BoxOutputView {
                        id: item.id,
                        snapshots,
                    })
                })
                .collect(),
            tables: self
                .tables
                .iter()
                .filter_map(|item| {
                    let snapshots = HistoryView::persistent(
                        &item.snapshots,
                        dropped_snapshot_count(&item.snapshots, self.display_origin),
                    );
                    (!snapshots.is_empty()).then_some(TableOutputView {
                        id: item.id,
                        columns: item.columns,
                        rows: item.rows,
                        snapshots,
                        position: &item.position,
                        bg_color: &item.bg_color,
                        frame_color: &item.frame_color,
                        frame_width: &item.frame_width,
                        border_color: &item.border_color,
                        border_width: &item.border_width,
                    })
                })
                .collect(),
            alerts: HistoryView::persistent(
                &self.alerts,
                self.alerts
                    .partition_point(|event| event.bar_index < self.display_origin),
            ),
            strategy: (self.program.script_mode == ScriptMode::Strategy
                && self.external_execution.is_none())
            .then(|| self.strategy_broker.result_view()),
            diagnostics: Cow::Owned(self.runtime_diagnostics()),
        }
    }
}
