//! Borrowed public output views. Histories remain in their persistent runtime storage.
//!
//! Views borrow an immutable runtime or owned result. They cannot outlive it or
//! overlap a mutable update; creating them does not copy historical values.
use crate::*;
use std::borrow::Cow;

mod history;
pub use history::{HistoryView, HistoryViewIter};

#[derive(Clone, Copy)]
pub struct PlotSeriesView<'a> {
    pub id: u32,
    pub values: HistoryView<'a, PineValue>,
    pub colors: HistoryView<'a, PineValue>,
    pub metadata: &'a OutputMetadata,
    pub linewidth: &'a PineValue,
    pub style: &'a PineValue,
    pub track_price: &'a PineValue,
    pub hist_base: &'a PineValue,
    pub join: &'a PineValue,
    pub format: &'a PineValue,
    pub precision: &'a PineValue,
    pub linestyle: &'a PineValue,
}
impl<'a> From<&'a PlotSeries> for PlotSeriesView<'a> {
    fn from(item: &'a PlotSeries) -> Self {
        Self {
            id: item.id,
            values: HistoryView::from_slice(&item.values),
            colors: HistoryView::from_slice(&item.colors),
            metadata: &item.metadata,
            linewidth: &item.linewidth,
            style: &item.style,
            track_price: &item.track_price,
            hist_base: &item.hist_base,
            join: &item.join,
            format: &item.format,
            precision: &item.precision,
            linestyle: &item.linestyle,
        }
    }
}
impl<'a> From<&PlotSeriesView<'a>> for PlotSeriesView<'a> {
    fn from(item: &PlotSeriesView<'a>) -> Self {
        *item
    }
}

#[derive(Clone, Copy)]
pub struct PlotCharSeriesView<'a> {
    pub id: u32,
    pub values: HistoryView<'a, PineValue>,
    pub chars: HistoryView<'a, PineValue>,
    pub colors: HistoryView<'a, PineValue>,
    pub locations: HistoryView<'a, PineValue>,
    pub texts: HistoryView<'a, PineValue>,
    pub text_colors: HistoryView<'a, PineValue>,
    pub sizes: HistoryView<'a, PineValue>,
    pub metadata: &'a OutputMetadata,
}
impl<'a> From<&'a PlotCharSeries> for PlotCharSeriesView<'a> {
    fn from(item: &'a PlotCharSeries) -> Self {
        Self {
            id: item.id,
            values: HistoryView::from_slice(&item.values),
            chars: HistoryView::from_slice(&item.chars),
            colors: HistoryView::from_slice(&item.colors),
            locations: HistoryView::from_slice(&item.locations),
            texts: HistoryView::from_slice(&item.texts),
            text_colors: HistoryView::from_slice(&item.text_colors),
            sizes: HistoryView::from_slice(&item.sizes),
            metadata: &item.metadata,
        }
    }
}
impl<'a> From<&PlotCharSeriesView<'a>> for PlotCharSeriesView<'a> {
    fn from(item: &PlotCharSeriesView<'a>) -> Self {
        *item
    }
}

#[derive(Clone, Copy)]
pub struct PlotShapeSeriesView<'a> {
    pub id: u32,
    pub values: HistoryView<'a, PineValue>,
    pub styles: HistoryView<'a, PineValue>,
    pub locations: HistoryView<'a, PineValue>,
    pub colors: HistoryView<'a, PineValue>,
    pub texts: HistoryView<'a, PineValue>,
    pub text_colors: HistoryView<'a, PineValue>,
    pub sizes: HistoryView<'a, PineValue>,
    pub metadata: &'a OutputMetadata,
}
impl<'a> From<&'a PlotShapeSeries> for PlotShapeSeriesView<'a> {
    fn from(item: &'a PlotShapeSeries) -> Self {
        Self {
            id: item.id,
            values: HistoryView::from_slice(&item.values),
            styles: HistoryView::from_slice(&item.styles),
            locations: HistoryView::from_slice(&item.locations),
            colors: HistoryView::from_slice(&item.colors),
            texts: HistoryView::from_slice(&item.texts),
            text_colors: HistoryView::from_slice(&item.text_colors),
            sizes: HistoryView::from_slice(&item.sizes),
            metadata: &item.metadata,
        }
    }
}
impl<'a> From<&PlotShapeSeriesView<'a>> for PlotShapeSeriesView<'a> {
    fn from(item: &PlotShapeSeriesView<'a>) -> Self {
        *item
    }
}

#[derive(Clone, Copy)]
pub struct PlotArrowSeriesView<'a> {
    pub id: u32,
    pub values: HistoryView<'a, PineValue>,
    pub color_ups: HistoryView<'a, PineValue>,
    pub color_downs: HistoryView<'a, PineValue>,
    pub min_heights: HistoryView<'a, PineValue>,
    pub max_heights: HistoryView<'a, PineValue>,
    pub metadata: &'a OutputMetadata,
}
impl<'a> From<&'a PlotArrowSeries> for PlotArrowSeriesView<'a> {
    fn from(item: &'a PlotArrowSeries) -> Self {
        Self {
            id: item.id,
            values: HistoryView::from_slice(&item.values),
            color_ups: HistoryView::from_slice(&item.color_ups),
            color_downs: HistoryView::from_slice(&item.color_downs),
            min_heights: HistoryView::from_slice(&item.min_heights),
            max_heights: HistoryView::from_slice(&item.max_heights),
            metadata: &item.metadata,
        }
    }
}
impl<'a> From<&PlotArrowSeriesView<'a>> for PlotArrowSeriesView<'a> {
    fn from(item: &PlotArrowSeriesView<'a>) -> Self {
        *item
    }
}

#[derive(Clone, Copy)]
pub struct PlotBarSeriesView<'a> {
    pub id: u32,
    pub opens: HistoryView<'a, PineValue>,
    pub highs: HistoryView<'a, PineValue>,
    pub lows: HistoryView<'a, PineValue>,
    pub closes: HistoryView<'a, PineValue>,
    pub colors: HistoryView<'a, PineValue>,
    pub metadata: &'a OutputMetadata,
}
impl<'a> From<&'a PlotBarSeries> for PlotBarSeriesView<'a> {
    fn from(item: &'a PlotBarSeries) -> Self {
        Self {
            id: item.id,
            opens: HistoryView::from_slice(&item.opens),
            highs: HistoryView::from_slice(&item.highs),
            lows: HistoryView::from_slice(&item.lows),
            closes: HistoryView::from_slice(&item.closes),
            colors: HistoryView::from_slice(&item.colors),
            metadata: &item.metadata,
        }
    }
}
impl<'a> From<&PlotBarSeriesView<'a>> for PlotBarSeriesView<'a> {
    fn from(item: &PlotBarSeriesView<'a>) -> Self {
        *item
    }
}

#[derive(Clone, Copy)]
pub struct PlotCandleSeriesView<'a> {
    pub id: u32,
    pub opens: HistoryView<'a, PineValue>,
    pub highs: HistoryView<'a, PineValue>,
    pub lows: HistoryView<'a, PineValue>,
    pub closes: HistoryView<'a, PineValue>,
    pub colors: HistoryView<'a, PineValue>,
    pub wick_colors: HistoryView<'a, PineValue>,
    pub border_colors: HistoryView<'a, PineValue>,
    pub metadata: &'a OutputMetadata,
}
impl<'a> From<&'a PlotCandleSeries> for PlotCandleSeriesView<'a> {
    fn from(item: &'a PlotCandleSeries) -> Self {
        Self {
            id: item.id,
            opens: HistoryView::from_slice(&item.opens),
            highs: HistoryView::from_slice(&item.highs),
            lows: HistoryView::from_slice(&item.lows),
            closes: HistoryView::from_slice(&item.closes),
            colors: HistoryView::from_slice(&item.colors),
            wick_colors: HistoryView::from_slice(&item.wick_colors),
            border_colors: HistoryView::from_slice(&item.border_colors),
            metadata: &item.metadata,
        }
    }
}
impl<'a> From<&PlotCandleSeriesView<'a>> for PlotCandleSeriesView<'a> {
    fn from(item: &PlotCandleSeriesView<'a>) -> Self {
        *item
    }
}

#[derive(Clone, Copy)]
pub struct ColorSeriesView<'a> {
    pub id: u32,
    pub values: HistoryView<'a, PineValue>,
    pub metadata: &'a OutputMetadata,
}
impl<'a> From<&'a ColorSeries> for ColorSeriesView<'a> {
    fn from(item: &'a ColorSeries) -> Self {
        Self {
            id: item.id,
            values: HistoryView::from_slice(&item.values),
            metadata: &item.metadata,
        }
    }
}
impl<'a> From<&ColorSeriesView<'a>> for ColorSeriesView<'a> {
    fn from(item: &ColorSeriesView<'a>) -> Self {
        *item
    }
}

#[derive(Clone, Copy)]
pub struct FillOutputView<'a> {
    pub id: u32,
    pub first_id: u32,
    pub second_id: u32,
    pub first_is_hline: bool,
    pub second_is_hline: bool,
    pub colors: HistoryView<'a, PineValue>,
    pub gradient: Option<HistoryView<'a, FillGradientSample>>,
    pub title: &'a PineValue,
    pub editable: &'a PineValue,
    pub show_last: &'a PineValue,
    pub fill_gaps: &'a PineValue,
    pub display: &'a PineValue,
}
impl<'a> From<&'a FillOutput> for FillOutputView<'a> {
    fn from(item: &'a FillOutput) -> Self {
        Self {
            id: item.id,
            first_id: item.first_id,
            second_id: item.second_id,
            first_is_hline: item.first_is_hline,
            second_is_hline: item.second_is_hline,
            colors: HistoryView::from_slice(&item.colors),
            gradient: item.gradient.as_deref().map(HistoryView::from_slice),
            title: &item.title,
            editable: &item.editable,
            show_last: &item.show_last,
            fill_gaps: &item.fill_gaps,
            display: &item.display,
        }
    }
}
impl<'a> From<&FillOutputView<'a>> for FillOutputView<'a> {
    fn from(item: &FillOutputView<'a>) -> Self {
        *item
    }
}

#[derive(Clone, Copy)]
pub struct LabelOutputView<'a> {
    pub id: u32,
    pub snapshots: HistoryView<'a, LabelSnapshot>,
}
impl<'a> From<&'a LabelOutput> for LabelOutputView<'a> {
    fn from(item: &'a LabelOutput) -> Self {
        Self {
            id: item.id,
            snapshots: HistoryView::from_slice(&item.snapshots),
        }
    }
}
impl<'a> From<&LabelOutputView<'a>> for LabelOutputView<'a> {
    fn from(item: &LabelOutputView<'a>) -> Self {
        *item
    }
}

#[derive(Clone, Copy)]
pub struct LineOutputView<'a> {
    pub id: u32,
    pub snapshots: HistoryView<'a, LineSnapshot>,
}
impl<'a> From<&'a LineOutput> for LineOutputView<'a> {
    fn from(item: &'a LineOutput) -> Self {
        Self {
            id: item.id,
            snapshots: HistoryView::from_slice(&item.snapshots),
        }
    }
}
impl<'a> From<&LineOutputView<'a>> for LineOutputView<'a> {
    fn from(item: &LineOutputView<'a>) -> Self {
        *item
    }
}

#[derive(Clone, Copy)]
pub struct LineFillOutputView<'a> {
    pub id: u32,
    pub snapshots: HistoryView<'a, LineFillSnapshot>,
}
impl<'a> From<&'a LineFillOutput> for LineFillOutputView<'a> {
    fn from(item: &'a LineFillOutput) -> Self {
        Self {
            id: item.id,
            snapshots: HistoryView::from_slice(&item.snapshots),
        }
    }
}
impl<'a> From<&LineFillOutputView<'a>> for LineFillOutputView<'a> {
    fn from(item: &LineFillOutputView<'a>) -> Self {
        *item
    }
}

#[derive(Clone, Copy)]
pub struct PolylineOutputView<'a> {
    pub id: u32,
    pub snapshots: HistoryView<'a, PolylineSnapshot>,
}
impl<'a> From<&'a PolylineOutput> for PolylineOutputView<'a> {
    fn from(item: &'a PolylineOutput) -> Self {
        Self {
            id: item.id,
            snapshots: HistoryView::from_slice(&item.snapshots),
        }
    }
}
impl<'a> From<&PolylineOutputView<'a>> for PolylineOutputView<'a> {
    fn from(item: &PolylineOutputView<'a>) -> Self {
        *item
    }
}

#[derive(Clone, Copy)]
pub struct BoxOutputView<'a> {
    pub id: u32,
    pub snapshots: HistoryView<'a, BoxSnapshot>,
}
impl<'a> From<&'a BoxOutput> for BoxOutputView<'a> {
    fn from(item: &'a BoxOutput) -> Self {
        Self {
            id: item.id,
            snapshots: HistoryView::from_slice(&item.snapshots),
        }
    }
}
impl<'a> From<&BoxOutputView<'a>> for BoxOutputView<'a> {
    fn from(item: &BoxOutputView<'a>) -> Self {
        *item
    }
}

#[derive(Clone, Copy)]
pub struct TableOutputView<'a> {
    pub id: u32,
    pub columns: i64,
    pub rows: i64,
    pub position: &'a PineValue,
    pub bg_color: &'a PineValue,
    pub frame_color: &'a PineValue,
    pub frame_width: &'a PineValue,
    pub border_color: &'a PineValue,
    pub border_width: &'a PineValue,
    pub snapshots: HistoryView<'a, TableSnapshot>,
}
impl<'a> From<&'a TableOutput> for TableOutputView<'a> {
    fn from(item: &'a TableOutput) -> Self {
        Self {
            id: item.id,
            columns: item.columns,
            rows: item.rows,
            snapshots: HistoryView::from_slice(&item.snapshots),
            position: &item.position,
            bg_color: &item.bg_color,
            frame_color: &item.frame_color,
            frame_width: &item.frame_width,
            border_color: &item.border_color,
            border_width: &item.border_width,
        }
    }
}
impl<'a> From<&TableOutputView<'a>> for TableOutputView<'a> {
    fn from(item: &TableOutputView<'a>) -> Self {
        *item
    }
}

#[derive(Clone, Copy)]
pub struct StrategyResultView<'a> {
    pub orders: HistoryView<'a, StrategyOrderEvent>,
    pub trades: HistoryView<'a, StrategyTrade>,
    pub position: HistoryView<'a, StrategyPositionSnapshot>,
    pub equity: HistoryView<'a, StrategyEquitySnapshot>,
    pub alerts: HistoryView<'a, StrategyOrderFillAlertOutput>,
    pub diagnostics: &'a [RuntimeDiagnostic],
}
impl<'a> From<&'a StrategyResult> for StrategyResultView<'a> {
    fn from(item: &'a StrategyResult) -> Self {
        Self {
            orders: HistoryView::from_slice(&item.orders),
            trades: HistoryView::from_slice(&item.trades),
            position: HistoryView::from_slice(&item.position),
            equity: HistoryView::from_slice(&item.equity),
            alerts: HistoryView::from_slice(&item.alerts),
            diagnostics: &item.diagnostics,
        }
    }
}

/// Public output borrowing histories and metadata. Only small view headers and
/// synthesized runtime diagnostics are owned; no history values are cloned.
pub struct RuntimeResultView<'a> {
    pub plots: Vec<PlotSeriesView<'a>>,
    pub plot_chars: Vec<PlotCharSeriesView<'a>>,
    pub plot_shapes: Vec<PlotShapeSeriesView<'a>>,
    pub plot_arrows: Vec<PlotArrowSeriesView<'a>>,
    pub plot_bars: Vec<PlotBarSeriesView<'a>>,
    pub plot_candles: Vec<PlotCandleSeriesView<'a>>,
    pub bg_colors: Vec<ColorSeriesView<'a>>,
    pub bar_colors: Vec<ColorSeriesView<'a>>,
    pub hlines: &'a [HLineOutput],
    pub fills: Vec<FillOutputView<'a>>,
    pub labels: Vec<LabelOutputView<'a>>,
    pub lines: Vec<LineOutputView<'a>>,
    pub line_fills: Vec<LineFillOutputView<'a>>,
    pub polylines: Vec<PolylineOutputView<'a>>,
    pub boxes: Vec<BoxOutputView<'a>>,
    pub tables: Vec<TableOutputView<'a>>,
    pub alerts: HistoryView<'a, AlertEvent>,
    pub strategy: Option<StrategyResultView<'a>>,
    pub diagnostics: Cow<'a, [RuntimeDiagnostic]>,
}
impl RuntimeResult {
    /// Borrow public output without copying any historical values.
    #[must_use]
    pub fn view(&self) -> RuntimeResultView<'_> {
        RuntimeResultView {
            plots: self.plots.iter().map(Into::into).collect(),
            plot_chars: self.plot_chars.iter().map(Into::into).collect(),
            plot_shapes: self.plot_shapes.iter().map(Into::into).collect(),
            plot_arrows: self.plot_arrows.iter().map(Into::into).collect(),
            plot_bars: self.plot_bars.iter().map(Into::into).collect(),
            plot_candles: self.plot_candles.iter().map(Into::into).collect(),
            bg_colors: self.bg_colors.iter().map(Into::into).collect(),
            bar_colors: self.bar_colors.iter().map(Into::into).collect(),
            fills: self.fills.iter().map(Into::into).collect(),
            labels: self.labels.iter().map(Into::into).collect(),
            lines: self.lines.iter().map(Into::into).collect(),
            line_fills: self.line_fills.iter().map(Into::into).collect(),
            polylines: self.polylines.iter().map(Into::into).collect(),
            boxes: self.boxes.iter().map(Into::into).collect(),
            tables: self.tables.iter().map(Into::into).collect(),
            hlines: &self.hlines,
            alerts: HistoryView::from_slice(&self.alerts),
            strategy: self.strategy.as_ref().map(Into::into),
            diagnostics: Cow::Borrowed(&self.diagnostics),
        }
    }
}
