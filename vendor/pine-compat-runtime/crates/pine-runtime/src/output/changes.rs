use super::alerts::AlertEvent;
use super::drawings::{
    BoxOutput, LabelOutput, LineFillOutput, LineOutput, PolylineOutput, TableOutput,
};
use super::model::{
    ColorSeries, FillOutput, HLineOutput, OutputMetadata, PlotArrowSeries, PlotBarSeries,
    PlotCandleSeries, PlotCharSeries, PlotSeries, PlotShapeSeries, RuntimeDiagnostic,
    RuntimeResult,
};
use super::strategy::{
    StrategyEquitySnapshot, StrategyOrderEvent, StrategyOrderFillAlertOutput,
    StrategyPositionSnapshot, StrategyResult, StrategyTrade,
};
use crate::PineValue;

pub const PUBLIC_RUNTIME_CHANGES_SCHEMA_VERSION: u32 = 4;
pub const MIN_RUNTIME_CHANGES_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamingVisibility {
    Preview,
    Confirmed,
}

impl StreamingVisibility {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::Confirmed => "confirmed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeriesChangeOp {
    Append,
    ReplaceLast,
}

impl SeriesChangeOp {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Append => "append",
            Self::ReplaceLast => "replaceLast",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeriesFamily {
    Plot,
    PlotChar,
    PlotShape,
    PlotArrow,
    PlotBar,
    PlotCandle,
    BgColor,
    BarColor,
}

impl SeriesFamily {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plot => "plot",
            Self::PlotChar => "plotChar",
            Self::PlotShape => "plotShape",
            Self::PlotArrow => "plotArrow",
            Self::PlotBar => "plotBar",
            Self::PlotCandle => "plotCandle",
            Self::BgColor => "bgColor",
            Self::BarColor => "barColor",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SeriesFields {
    pub values: Vec<PineValue>,
    pub colors: Vec<PineValue>,
    pub chars: Vec<PineValue>,
    pub locations: Vec<PineValue>,
    pub texts: Vec<PineValue>,
    pub text_colors: Vec<PineValue>,
    pub sizes: Vec<PineValue>,
    pub styles: Vec<PineValue>,
    pub color_ups: Vec<PineValue>,
    pub color_downs: Vec<PineValue>,
    pub min_heights: Vec<PineValue>,
    pub max_heights: Vec<PineValue>,
    pub opens: Vec<PineValue>,
    pub highs: Vec<PineValue>,
    pub lows: Vec<PineValue>,
    pub closes: Vec<PineValue>,
    pub wick_colors: Vec<PineValue>,
    pub border_colors: Vec<PineValue>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeriesHeader {
    pub metadata: OutputMetadata,
    pub linewidth: PineValue,
    pub style: PineValue,
    pub track_price: PineValue,
    pub hist_base: PineValue,
    pub join: PineValue,
    pub format: PineValue,
    pub precision: PineValue,
    pub linestyle: PineValue,
}

impl Default for SeriesHeader {
    fn default() -> Self {
        Self {
            metadata: OutputMetadata::default(),
            linewidth: PineValue::Int(1),
            style: PineValue::String("plot.style_line".to_owned()),
            track_price: PineValue::Bool(false),
            hist_base: PineValue::Int(0),
            join: PineValue::Bool(false),
            format: PineValue::String("format.inherit".to_owned()),
            precision: PineValue::Na,
            linestyle: PineValue::String("plot.linestyle_solid".to_owned()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeriesChange {
    pub family: SeriesFamily,
    pub id: u32,
    pub op: SeriesChangeOp,
    pub start: usize,
    pub fields: SeriesFields,
    pub header: Option<SeriesHeader>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawingFamily {
    Label,
    Line,
    LineFill,
    Polyline,
    Box,
    Table,
}

impl DrawingFamily {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Label => "label",
            Self::Line => "line",
            Self::LineFill => "lineFill",
            Self::Polyline => "polyline",
            Self::Box => "box",
            Self::Table => "table",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DrawingObject {
    Label(LabelOutput),
    Line(LineOutput),
    LineFill(LineFillOutput),
    Polyline(PolylineOutput),
    Box(BoxOutput),
    Table(Box<TableOutput>),
}

impl DrawingObject {
    #[must_use]
    pub fn family(&self) -> DrawingFamily {
        match self {
            Self::Label(_) => DrawingFamily::Label,
            Self::Line(_) => DrawingFamily::Line,
            Self::LineFill(_) => DrawingFamily::LineFill,
            Self::Polyline(_) => DrawingFamily::Polyline,
            Self::Box(_) => DrawingFamily::Box,
            Self::Table(_) => DrawingFamily::Table,
        }
    }

    #[must_use]
    pub fn id(&self) -> u32 {
        match self {
            Self::Label(item) => item.id,
            Self::Line(item) => item.id,
            Self::LineFill(item) => item.id,
            Self::Polyline(item) => item.id,
            Self::Box(item) => item.id,
            Self::Table(item) => item.id,
        }
    }

    #[must_use]
    pub fn snapshot_len(&self) -> usize {
        match self {
            Self::Label(item) => item.snapshots.len(),
            Self::Line(item) => item.snapshots.len(),
            Self::LineFill(item) => item.snapshots.len(),
            Self::Polyline(item) => item.snapshots.len(),
            Self::Box(item) => item.snapshots.len(),
            Self::Table(item) => item.snapshots.len(),
        }
    }

    #[must_use]
    pub fn tail(&self, start: usize) -> Self {
        match self {
            Self::Label(item) => Self::Label(LabelOutput {
                id: item.id,
                snapshots: tail(&item.snapshots, start),
            }),
            Self::Line(item) => Self::Line(LineOutput {
                id: item.id,
                snapshots: tail(&item.snapshots, start),
            }),
            Self::LineFill(item) => Self::LineFill(LineFillOutput {
                id: item.id,
                snapshots: tail(&item.snapshots, start),
            }),
            Self::Polyline(item) => Self::Polyline(PolylineOutput {
                id: item.id,
                snapshots: tail(&item.snapshots, start),
            }),
            Self::Box(item) => Self::Box(BoxOutput {
                id: item.id,
                snapshots: tail(&item.snapshots, start),
            }),
            Self::Table(item) => Self::Table(Box::new(TableOutput {
                id: item.id,
                position: item.position.clone(),
                bg_color: item.bg_color.clone(),
                frame_color: item.frame_color.clone(),
                frame_width: item.frame_width.clone(),
                border_color: item.border_color.clone(),
                border_width: item.border_width.clone(),
                columns: item.columns,
                rows: item.rows,
                snapshots: tail(&item.snapshots, start),
            })),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DrawingAction {
    Add(DrawingObject),
    Delete,
    SetTail { start: usize, object: DrawingObject },
}

impl DrawingAction {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Add(_) => "add",
            Self::Delete => "delete",
            Self::SetTail { .. } => "setTail",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DrawingChange {
    pub family: DrawingFamily,
    pub id: u32,
    pub action: DrawingAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventAction {
    Add,
    Remove,
}

impl EventAction {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Remove => "remove",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventChange<T> {
    pub action: EventAction,
    pub event: T,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListSplice<T> {
    pub start: usize,
    pub items: Vec<T>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HLineAction {
    Add(HLineOutput),
    Delete,
    Replace(HLineOutput),
}

#[derive(Debug, Clone, PartialEq)]
pub struct HLineChange {
    pub id: u32,
    pub action: HLineAction,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FillAction {
    Add(FillOutput),
    Delete,
    SetColors {
        start: usize,
        values: Vec<PineValue>,
    },
    SetGradient {
        start: usize,
        values: Vec<crate::FillGradientSample>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct FillChange {
    pub id: u32,
    pub action: FillAction,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct StrategyChanges {
    pub orders: Option<ListSplice<StrategyOrderEvent>>,
    pub trades: Option<ListSplice<StrategyTrade>>,
    pub alerts: Option<ListSplice<StrategyOrderFillAlertOutput>>,
    pub position: Option<ListSplice<StrategyPositionSnapshot>>,
    pub equity: Option<ListSplice<StrategyEquitySnapshot>>,
    pub diagnostics: Option<Vec<RuntimeDiagnostic>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeChanges {
    pub schema_version: u32,
    pub revision: u64,
    pub base_revision: u64,
    pub retained_from: usize,
    pub visibility: StreamingVisibility,
    pub series: Vec<SeriesChange>,
    pub hlines: Vec<HLineChange>,
    pub fills: Vec<FillChange>,
    pub drawings: Vec<DrawingChange>,
    pub alerts: Vec<EventChange<AlertEvent>>,
    pub strategy: Option<StrategyChanges>,
    pub diagnostics: Vec<RuntimeDiagnostic>,
}

impl RuntimeChanges {
    #[must_use]
    pub fn new(revision: u64, visibility: StreamingVisibility) -> Self {
        Self {
            schema_version: PUBLIC_RUNTIME_CHANGES_SCHEMA_VERSION,
            revision,
            base_revision: revision.saturating_sub(1),
            retained_from: 0,
            visibility,
            series: Vec::new(),
            hlines: Vec::new(),
            fills: Vec::new(),
            drawings: Vec::new(),
            alerts: Vec::new(),
            strategy: None,
            diagnostics: Vec::new(),
        }
    }
}

pub(crate) fn apply_runtime_changes_in_place(
    result: &mut RuntimeResult,
    changes: &RuntimeChanges,
    previous_origin: usize,
) {
    let advance = changes.retained_from.saturating_sub(previous_origin);
    drop_display_prefix(result, advance, changes.retained_from);
    for change in &changes.series {
        apply_series_change(result, change, changes.retained_from);
    }
    for change in &changes.hlines {
        apply_hline_change(result, change);
    }
    for change in &changes.fills {
        apply_fill_change(result, change, changes.retained_from);
    }
    for change in &changes.drawings {
        apply_drawing_change(result, change);
    }
    apply_event_changes(&mut result.alerts, &changes.alerts);
    if let Some(strategy_changes) = &changes.strategy {
        let strategy = result.strategy.get_or_insert_with(StrategyResult::default);
        if let Some(orders) = &strategy_changes.orders {
            splice_vec(&mut strategy.orders, orders.start, &orders.items);
        }
        if let Some(trades) = &strategy_changes.trades {
            splice_vec(&mut strategy.trades, trades.start, &trades.items);
        }
        if let Some(alerts) = &strategy_changes.alerts {
            splice_vec(&mut strategy.alerts, alerts.start, &alerts.items);
        }
        if let Some(position) = &strategy_changes.position {
            splice_vec(&mut strategy.position, position.start, &position.items);
        }
        if let Some(equity) = &strategy_changes.equity {
            splice_vec(&mut strategy.equity, equity.start, &equity.items);
        }
        if let Some(diagnostics) = &strategy_changes.diagnostics {
            strategy.diagnostics.clone_from(diagnostics);
        }
    }
    result.diagnostics.clone_from(&changes.diagnostics);
}

fn apply_event_changes<T: Clone + PartialEq>(target: &mut Vec<T>, changes: &[EventChange<T>]) {
    for change in changes {
        match change.action {
            EventAction::Add => target.push(change.event.clone()),
            EventAction::Remove => {
                if let Some(index) = target.iter().rposition(|item| item == &change.event) {
                    target.remove(index);
                }
            }
        }
    }
}

fn local_start(absolute: usize, origin: usize) -> usize {
    absolute.saturating_sub(origin)
}

pub(crate) fn drop_display_prefix(result: &mut RuntimeResult, advance: usize, origin: usize) {
    if advance > 0 {
        for plot in &mut result.plots {
            drain_prefix(&mut plot.values, advance);
            drain_prefix(&mut plot.colors, advance);
        }
        for item in &mut result.plot_chars {
            drain_prefix(&mut item.values, advance);
            drain_prefix(&mut item.chars, advance);
            drain_prefix(&mut item.colors, advance);
            drain_prefix(&mut item.locations, advance);
            drain_prefix(&mut item.texts, advance);
            drain_prefix(&mut item.text_colors, advance);
            drain_prefix(&mut item.sizes, advance);
        }
        for item in &mut result.plot_shapes {
            drain_prefix(&mut item.values, advance);
            drain_prefix(&mut item.styles, advance);
            drain_prefix(&mut item.locations, advance);
            drain_prefix(&mut item.colors, advance);
            drain_prefix(&mut item.texts, advance);
            drain_prefix(&mut item.text_colors, advance);
            drain_prefix(&mut item.sizes, advance);
        }
        for item in &mut result.plot_arrows {
            drain_prefix(&mut item.values, advance);
            drain_prefix(&mut item.color_ups, advance);
            drain_prefix(&mut item.color_downs, advance);
            drain_prefix(&mut item.min_heights, advance);
            drain_prefix(&mut item.max_heights, advance);
        }
        for item in &mut result.plot_bars {
            drain_prefix(&mut item.opens, advance);
            drain_prefix(&mut item.highs, advance);
            drain_prefix(&mut item.lows, advance);
            drain_prefix(&mut item.closes, advance);
            drain_prefix(&mut item.colors, advance);
        }
        for item in &mut result.plot_candles {
            drain_prefix(&mut item.opens, advance);
            drain_prefix(&mut item.highs, advance);
            drain_prefix(&mut item.lows, advance);
            drain_prefix(&mut item.closes, advance);
            drain_prefix(&mut item.colors, advance);
            drain_prefix(&mut item.wick_colors, advance);
            drain_prefix(&mut item.border_colors, advance);
        }
        for item in &mut result.bg_colors {
            drain_prefix(&mut item.values, advance);
        }
        for item in &mut result.bar_colors {
            drain_prefix(&mut item.values, advance);
        }
        for item in &mut result.fills {
            drain_prefix(&mut item.colors, advance);
            if let Some(samples) = &mut item.gradient {
                drain_prefix(samples, advance);
            }
        }
    }
    if origin == 0 {
        return;
    }
    for item in &mut result.labels {
        trim_snapshots(&mut item.snapshots, origin);
    }
    result.labels.retain(|item| !item.snapshots.is_empty());
    for item in &mut result.lines {
        trim_snapshots(&mut item.snapshots, origin);
    }
    result.lines.retain(|item| !item.snapshots.is_empty());
    for item in &mut result.line_fills {
        trim_snapshots(&mut item.snapshots, origin);
    }
    result.line_fills.retain(|item| !item.snapshots.is_empty());
    for item in &mut result.polylines {
        trim_snapshots(&mut item.snapshots, origin);
    }
    result.polylines.retain(|item| !item.snapshots.is_empty());
    for item in &mut result.boxes {
        trim_snapshots(&mut item.snapshots, origin);
    }
    result.boxes.retain(|item| !item.snapshots.is_empty());
    for item in &mut result.tables {
        trim_snapshots(&mut item.snapshots, origin);
    }
    result.tables.retain(|item| !item.snapshots.is_empty());
    result.alerts.retain(|event| event.bar_index >= origin);
}

fn drain_prefix<T>(values: &mut Vec<T>, count: usize) {
    let count = count.min(values.len());
    if count > 0 {
        values.drain(..count);
    }
}

fn trim_snapshots<T>(snapshots: &mut Vec<T>, origin: usize)
where
    T: SnapshotBarIndex,
{
    if snapshots.is_empty() {
        return;
    }
    let keep_last = snapshots
        .last()
        .is_some_and(SnapshotBarIndex::keep_after_trim);
    let mut first_kept = snapshots
        .iter()
        .position(|snapshot| snapshot.bar_index() >= origin)
        .unwrap_or(snapshots.len());
    if keep_last && first_kept == snapshots.len() {
        first_kept = snapshots.len() - 1;
    }
    drain_prefix(snapshots, first_kept);
}

trait SnapshotBarIndex {
    fn bar_index(&self) -> usize;
    fn keep_after_trim(&self) -> bool;
}

impl SnapshotBarIndex for crate::LabelSnapshot {
    fn bar_index(&self) -> usize {
        self.bar_index
    }
    fn keep_after_trim(&self) -> bool {
        self.exists
    }
}
impl SnapshotBarIndex for crate::LineSnapshot {
    fn bar_index(&self) -> usize {
        self.bar_index
    }
    fn keep_after_trim(&self) -> bool {
        self.exists
    }
}
impl SnapshotBarIndex for crate::LineFillSnapshot {
    fn bar_index(&self) -> usize {
        self.bar_index
    }
    fn keep_after_trim(&self) -> bool {
        self.exists
    }
}
impl SnapshotBarIndex for crate::PolylineSnapshot {
    fn bar_index(&self) -> usize {
        self.bar_index
    }
    fn keep_after_trim(&self) -> bool {
        self.exists
    }
}
impl SnapshotBarIndex for crate::BoxSnapshot {
    fn bar_index(&self) -> usize {
        self.bar_index
    }
    fn keep_after_trim(&self) -> bool {
        self.exists
    }
}
impl SnapshotBarIndex for crate::TableSnapshot {
    fn bar_index(&self) -> usize {
        self.bar_index
    }
    fn keep_after_trim(&self) -> bool {
        self.exists
    }
}

fn apply_series_change(result: &mut RuntimeResult, change: &SeriesChange, origin: usize) {
    let mut change = change.clone();
    change.start = local_start(change.start, origin);
    match change.family {
        SeriesFamily::Plot => apply_plot(result, &change),
        SeriesFamily::PlotChar => apply_plot_char(result, &change),
        SeriesFamily::PlotShape => apply_plot_shape(result, &change),
        SeriesFamily::PlotArrow => apply_plot_arrow(result, &change),
        SeriesFamily::PlotBar => apply_plot_bar(result, &change),
        SeriesFamily::PlotCandle => apply_plot_candle(result, &change),
        SeriesFamily::BgColor => apply_color_series(&mut result.bg_colors, &change),
        SeriesFamily::BarColor => apply_color_series(&mut result.bar_colors, &change),
    }
}

fn apply_plot(result: &mut RuntimeResult, change: &SeriesChange) {
    if let Some(plot) = result.plots.iter_mut().find(|item| item.id == change.id) {
        splice_values(&mut plot.values, change.start, &change.fields.values);
        if !change.fields.colors.is_empty() {
            splice_values(&mut plot.colors, change.start, &change.fields.colors);
        }
        align_colors(&mut plot.colors, plot.values.len());
        if let Some(header) = &change.header {
            apply_plot_header(plot, header);
        }
        return;
    }
    let header = change.header.clone().unwrap_or_default();
    let mut plot = PlotSeries::new(change.id, change.fields.values.clone());
    apply_plot_header(&mut plot, &header);
    if !change.fields.colors.is_empty() {
        plot.colors = change.fields.colors.clone();
    }
    align_colors(&mut plot.colors, plot.values.len());
    result.plots.push(plot);
}

fn apply_plot_header(plot: &mut PlotSeries, header: &SeriesHeader) {
    plot.metadata = header.metadata.clone();
    plot.linewidth = header.linewidth.clone();
    plot.style = header.style.clone();
    plot.track_price = header.track_price.clone();
    plot.hist_base = header.hist_base.clone();
    plot.join = header.join.clone();
    plot.format = header.format.clone();
    plot.precision = header.precision.clone();
    plot.linestyle = header.linestyle.clone();
}

fn apply_plot_char(result: &mut RuntimeResult, change: &SeriesChange) {
    if let Some(item) = result
        .plot_chars
        .iter_mut()
        .find(|item| item.id == change.id)
    {
        splice_values(&mut item.values, change.start, &change.fields.values);
        splice_optional(&mut item.chars, change.start, &change.fields.chars);
        splice_optional(&mut item.colors, change.start, &change.fields.colors);
        splice_optional(&mut item.locations, change.start, &change.fields.locations);
        splice_optional(&mut item.texts, change.start, &change.fields.texts);
        splice_optional(
            &mut item.text_colors,
            change.start,
            &change.fields.text_colors,
        );
        splice_optional(&mut item.sizes, change.start, &change.fields.sizes);
        if let Some(header) = &change.header {
            item.metadata = header.metadata.clone();
        }
        return;
    }
    result.plot_chars.push(PlotCharSeries {
        id: change.id,
        values: change.fields.values.clone(),
        chars: change.fields.chars.clone(),
        colors: change.fields.colors.clone(),
        locations: change.fields.locations.clone(),
        texts: change.fields.texts.clone(),
        text_colors: change.fields.text_colors.clone(),
        sizes: change.fields.sizes.clone(),
        metadata: change
            .header
            .as_ref()
            .map(|header| header.metadata.clone())
            .unwrap_or_default(),
    });
}

fn apply_plot_shape(result: &mut RuntimeResult, change: &SeriesChange) {
    if let Some(item) = result
        .plot_shapes
        .iter_mut()
        .find(|item| item.id == change.id)
    {
        splice_values(&mut item.values, change.start, &change.fields.values);
        splice_optional(&mut item.styles, change.start, &change.fields.styles);
        splice_optional(&mut item.locations, change.start, &change.fields.locations);
        splice_optional(&mut item.colors, change.start, &change.fields.colors);
        splice_optional(&mut item.texts, change.start, &change.fields.texts);
        splice_optional(
            &mut item.text_colors,
            change.start,
            &change.fields.text_colors,
        );
        splice_optional(&mut item.sizes, change.start, &change.fields.sizes);
        if let Some(header) = &change.header {
            item.metadata = header.metadata.clone();
        }
        return;
    }
    result.plot_shapes.push(PlotShapeSeries {
        id: change.id,
        values: change.fields.values.clone(),
        styles: change.fields.styles.clone(),
        locations: change.fields.locations.clone(),
        colors: change.fields.colors.clone(),
        texts: change.fields.texts.clone(),
        text_colors: change.fields.text_colors.clone(),
        sizes: change.fields.sizes.clone(),
        metadata: change
            .header
            .as_ref()
            .map(|header| header.metadata.clone())
            .unwrap_or_default(),
    });
}

fn apply_plot_arrow(result: &mut RuntimeResult, change: &SeriesChange) {
    if let Some(item) = result
        .plot_arrows
        .iter_mut()
        .find(|item| item.id == change.id)
    {
        splice_values(&mut item.values, change.start, &change.fields.values);
        splice_optional(&mut item.color_ups, change.start, &change.fields.color_ups);
        splice_optional(
            &mut item.color_downs,
            change.start,
            &change.fields.color_downs,
        );
        splice_optional(
            &mut item.min_heights,
            change.start,
            &change.fields.min_heights,
        );
        splice_optional(
            &mut item.max_heights,
            change.start,
            &change.fields.max_heights,
        );
        if let Some(header) = &change.header {
            item.metadata = header.metadata.clone();
        }
        return;
    }
    result.plot_arrows.push(PlotArrowSeries {
        id: change.id,
        values: change.fields.values.clone(),
        color_ups: change.fields.color_ups.clone(),
        color_downs: change.fields.color_downs.clone(),
        min_heights: change.fields.min_heights.clone(),
        max_heights: change.fields.max_heights.clone(),
        metadata: change
            .header
            .as_ref()
            .map(|header| header.metadata.clone())
            .unwrap_or_default(),
    });
}

fn apply_plot_bar(result: &mut RuntimeResult, change: &SeriesChange) {
    if let Some(item) = result
        .plot_bars
        .iter_mut()
        .find(|item| item.id == change.id)
    {
        splice_optional(&mut item.opens, change.start, &change.fields.opens);
        splice_optional(&mut item.highs, change.start, &change.fields.highs);
        splice_optional(&mut item.lows, change.start, &change.fields.lows);
        splice_optional(&mut item.closes, change.start, &change.fields.closes);
        splice_optional(&mut item.colors, change.start, &change.fields.colors);
        if let Some(header) = &change.header {
            item.metadata = header.metadata.clone();
        }
        return;
    }
    result.plot_bars.push(PlotBarSeries {
        id: change.id,
        opens: change.fields.opens.clone(),
        highs: change.fields.highs.clone(),
        lows: change.fields.lows.clone(),
        closes: change.fields.closes.clone(),
        colors: change.fields.colors.clone(),
        metadata: change
            .header
            .as_ref()
            .map(|header| header.metadata.clone())
            .unwrap_or_default(),
    });
}

fn apply_plot_candle(result: &mut RuntimeResult, change: &SeriesChange) {
    if let Some(item) = result
        .plot_candles
        .iter_mut()
        .find(|item| item.id == change.id)
    {
        splice_optional(&mut item.opens, change.start, &change.fields.opens);
        splice_optional(&mut item.highs, change.start, &change.fields.highs);
        splice_optional(&mut item.lows, change.start, &change.fields.lows);
        splice_optional(&mut item.closes, change.start, &change.fields.closes);
        splice_optional(&mut item.colors, change.start, &change.fields.colors);
        splice_optional(
            &mut item.wick_colors,
            change.start,
            &change.fields.wick_colors,
        );
        splice_optional(
            &mut item.border_colors,
            change.start,
            &change.fields.border_colors,
        );
        if let Some(header) = &change.header {
            item.metadata = header.metadata.clone();
        }
        return;
    }
    result.plot_candles.push(PlotCandleSeries {
        id: change.id,
        opens: change.fields.opens.clone(),
        highs: change.fields.highs.clone(),
        lows: change.fields.lows.clone(),
        closes: change.fields.closes.clone(),
        colors: change.fields.colors.clone(),
        wick_colors: change.fields.wick_colors.clone(),
        border_colors: change.fields.border_colors.clone(),
        metadata: change
            .header
            .as_ref()
            .map(|header| header.metadata.clone())
            .unwrap_or_default(),
    });
}

fn apply_color_series(items: &mut Vec<ColorSeries>, change: &SeriesChange) {
    if let Some(item) = items.iter_mut().find(|item| item.id == change.id) {
        splice_values(&mut item.values, change.start, &change.fields.values);
        if let Some(header) = &change.header {
            item.metadata = header.metadata.clone();
        }
        return;
    }
    items.push(ColorSeries {
        id: change.id,
        values: change.fields.values.clone(),
        metadata: change
            .header
            .as_ref()
            .map(|header| header.metadata.clone())
            .unwrap_or_default(),
    });
}

fn apply_hline_change(result: &mut RuntimeResult, change: &HLineChange) {
    match &change.action {
        HLineAction::Add(item) | HLineAction::Replace(item) => {
            if let Some(existing) = result.hlines.iter_mut().find(|hline| hline.id == change.id) {
                *existing = item.clone();
            } else {
                result.hlines.push(item.clone());
            }
        }
        HLineAction::Delete => result.hlines.retain(|item| item.id != change.id),
    }
}

fn apply_fill_change(result: &mut RuntimeResult, change: &FillChange, origin: usize) {
    match &change.action {
        FillAction::Add(item) => {
            if let Some(existing) = result.fills.iter_mut().find(|fill| fill.id == change.id) {
                *existing = item.clone();
            } else {
                result.fills.push(item.clone());
            }
        }
        FillAction::Delete => result.fills.retain(|item| item.id != change.id),
        FillAction::SetColors { start, values } => {
            if let Some(existing) = result.fills.iter_mut().find(|fill| fill.id == change.id) {
                splice_values(&mut existing.colors, local_start(*start, origin), values);
            }
        }
        FillAction::SetGradient { start, values } => {
            if let Some(existing) = result.fills.iter_mut().find(|fill| fill.id == change.id) {
                let samples = existing.gradient.get_or_insert_with(Vec::new);
                let start = local_start(*start, origin);
                splice_vec(samples, start, values);
            }
        }
    }
}

fn apply_drawing_change(result: &mut RuntimeResult, change: &DrawingChange) {
    match &change.action {
        DrawingAction::Delete => match change.family {
            DrawingFamily::Label => result.labels.retain(|item| item.id != change.id),
            DrawingFamily::Line => result.lines.retain(|item| item.id != change.id),
            DrawingFamily::LineFill => result.line_fills.retain(|item| item.id != change.id),
            DrawingFamily::Polyline => result.polylines.retain(|item| item.id != change.id),
            DrawingFamily::Box => result.boxes.retain(|item| item.id != change.id),
            DrawingFamily::Table => result.tables.retain(|item| item.id != change.id),
        },
        DrawingAction::Add(object) | DrawingAction::SetTail { object, .. } => {
            let start = match &change.action {
                DrawingAction::SetTail { start, .. } => *start,
                _ => 0,
            };
            apply_drawing_object(result, object, start);
        }
    }
}

fn apply_drawing_object(result: &mut RuntimeResult, object: &DrawingObject, start: usize) {
    match object {
        DrawingObject::Label(item) => {
            if let Some(existing) = result.labels.iter_mut().find(|label| label.id == item.id) {
                splice_vec(&mut existing.snapshots, start, &item.snapshots);
            } else {
                result.labels.push(item.clone());
            }
        }
        DrawingObject::Line(item) => {
            if let Some(existing) = result.lines.iter_mut().find(|line| line.id == item.id) {
                splice_vec(&mut existing.snapshots, start, &item.snapshots);
            } else {
                result.lines.push(item.clone());
            }
        }
        DrawingObject::LineFill(item) => {
            if let Some(existing) = result
                .line_fills
                .iter_mut()
                .find(|line_fill| line_fill.id == item.id)
            {
                splice_vec(&mut existing.snapshots, start, &item.snapshots);
            } else {
                result.line_fills.push(item.clone());
            }
        }
        DrawingObject::Polyline(item) => {
            if let Some(existing) = result
                .polylines
                .iter_mut()
                .find(|polyline| polyline.id == item.id)
            {
                splice_vec(&mut existing.snapshots, start, &item.snapshots);
            } else {
                result.polylines.push(item.clone());
            }
        }
        DrawingObject::Box(item) => {
            if let Some(existing) = result
                .boxes
                .iter_mut()
                .find(|box_item| box_item.id == item.id)
            {
                splice_vec(&mut existing.snapshots, start, &item.snapshots);
            } else {
                result.boxes.push(item.clone());
            }
        }
        DrawingObject::Table(item) => {
            if let Some(existing) = result.tables.iter_mut().find(|table| table.id == item.id) {
                existing.position = item.position.clone();
                existing.bg_color = item.bg_color.clone();
                existing.frame_color = item.frame_color.clone();
                existing.frame_width = item.frame_width.clone();
                existing.border_color = item.border_color.clone();
                existing.border_width = item.border_width.clone();
                existing.columns = item.columns;
                existing.rows = item.rows;
                splice_vec(&mut existing.snapshots, start, &item.snapshots);
            } else {
                result.tables.push((**item).clone());
            }
        }
    }
}

fn splice_optional(target: &mut Vec<PineValue>, start: usize, values: &[PineValue]) {
    if !values.is_empty() {
        splice_values(target, start, values);
    }
}

fn splice_values(target: &mut Vec<PineValue>, start: usize, values: &[PineValue]) {
    splice_vec(target, start, values);
}

fn splice_vec<T: Clone>(target: &mut Vec<T>, start: usize, items: &[T]) {
    if start > target.len() {
        return;
    }
    target.truncate(start);
    target.extend_from_slice(items);
}

fn align_colors(colors: &mut Vec<PineValue>, len: usize) {
    if colors.len() < len {
        colors.resize(len, PineValue::Na);
    } else if colors.len() > len {
        colors.truncate(len);
    }
}

fn tail<T: Clone>(items: &[T], start: usize) -> Vec<T> {
    items.get(start..).unwrap_or(&[]).to_vec()
}

pub(crate) fn series_change_from_lens(
    family: SeriesFamily,
    id: u32,
    old_len: Option<usize>,
    new_len: usize,
    display_origin: usize,
    fields_from: impl Fn(usize) -> SeriesFields,
    header: impl FnOnce() -> Option<SeriesHeader>,
) -> Option<SeriesChange> {
    if new_len == 0 && old_len.unwrap_or(0) == 0 {
        return None;
    }
    let (op, start) = match old_len {
        None => (SeriesChangeOp::Append, display_origin),
        Some(old) if new_len > old => (SeriesChangeOp::Append, old.max(display_origin)),
        Some(old) if new_len == old && new_len > display_origin => {
            (SeriesChangeOp::ReplaceLast, new_len - 1)
        }
        Some(_) => (SeriesChangeOp::Append, display_origin),
    };
    Some(SeriesChange {
        family,
        id,
        op,
        start,
        fields: fields_from(start),
        header: old_len.is_none().then(header).flatten(),
    })
}

#[cfg(test)]
#[path = "changes/lazy_header_tests.rs"]
mod lazy_header_tests;
