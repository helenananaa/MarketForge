use super::{append_history::AppendHistory, historical::HistoricalRuntime};
use crate::output::align::{
    BarAlignedOutput, PlotArrowPoint, PlotBarPoint, PlotCandlePoint, PlotCharPoint, PlotShapePoint,
};
use crate::output::model::SeriesOutput;
use crate::{
    ColorSeries, FillOutput, OutputMetadata, PineValue, PlotArrowSeries, PlotBarSeries,
    PlotCandleSeries, PlotCharSeries, PlotSeries, PlotShapeSeries,
};
use std::sync::Arc;

pub(crate) fn na_history(len: usize) -> AppendHistory<PineValue> {
    AppendHistory::from_compact_values(std::iter::repeat_n(PineValue::Na, len))
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimePlot {
    pub(crate) id: u32,
    pub(crate) values: AppendHistory<PineValue>,
    pub(crate) colors: AppendHistory<PineValue>,
    pub(crate) metadata: OutputMetadata,
    pub(crate) linewidth: PineValue,
    pub(crate) style: PineValue,
    pub(crate) track_price: PineValue,
    pub(crate) hist_base: PineValue,
    pub(crate) join: PineValue,
    pub(crate) format: PineValue,
    pub(crate) precision: PineValue,
    pub(crate) linestyle: PineValue,
}
impl RuntimePlot {
    pub(crate) fn new(id: u32, values: Vec<PineValue>) -> Self {
        let public = PlotSeries::new(id, values);
        Self {
            id: public.id,
            values: AppendHistory::from_compact_values(public.values),
            colors: AppendHistory::from_compact_values(public.colors),
            metadata: public.metadata,
            linewidth: public.linewidth,
            style: public.style,
            track_price: public.track_price,
            hist_base: public.hist_base,
            join: public.join,
            format: public.format,
            precision: public.precision,
            linestyle: public.linestyle,
        }
    }
    pub(crate) fn snapshot_from(&self, skip: usize) -> PlotSeries {
        PlotSeries {
            id: self.id,
            values: self.values.tail(skip),
            colors: self.colors.tail(skip),
            metadata: self.metadata.clone(),
            linewidth: self.linewidth.clone(),
            style: self.style.clone(),
            track_price: self.track_price.clone(),
            hist_base: self.hist_base.clone(),
            join: self.join.clone(),
            format: self.format.clone(),
            precision: self.precision.clone(),
            linestyle: self.linestyle.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimePlotChar {
    pub(crate) id: u32,
    pub(crate) values: AppendHistory<PineValue>,
    pub(crate) chars: AppendHistory<PineValue>,
    pub(crate) colors: AppendHistory<PineValue>,
    pub(crate) locations: AppendHistory<PineValue>,
    pub(crate) texts: AppendHistory<PineValue>,
    pub(crate) text_colors: AppendHistory<PineValue>,
    pub(crate) sizes: AppendHistory<PineValue>,
    pub(crate) metadata: OutputMetadata,
}

impl RuntimePlotChar {
    pub(crate) fn snapshot_from(&self, skip: usize) -> PlotCharSeries {
        PlotCharSeries {
            id: self.id,
            values: self.values.tail(skip),
            chars: self.chars.tail(skip),
            colors: self.colors.tail(skip),
            locations: self.locations.tail(skip),
            texts: self.texts.tail(skip),
            text_colors: self.text_colors.tail(skip),
            sizes: self.sizes.tail(skip),
            metadata: self.metadata.clone(),
        }
    }
}

impl BarAlignedOutput for RuntimePlotChar {
    type Point = PlotCharPoint;

    fn id(&self) -> u32 {
        self.id
    }

    fn new_padded(id: u32, current_bar: usize) -> Self {
        Self {
            id,
            values: na_history(current_bar),
            chars: na_history(current_bar),
            colors: na_history(current_bar),
            locations: na_history(current_bar),
            texts: na_history(current_bar),
            text_colors: na_history(current_bar),
            sizes: na_history(current_bar),
            metadata: OutputMetadata::default(),
        }
    }

    fn len(&self) -> usize {
        self.values.len()
    }

    fn pad_to(&mut self, current_bar: usize) {
        while self.values.len() < current_bar {
            self.push_na_point();
        }
    }

    fn push_point(&mut self, point: Self::Point) {
        self.values.push_compact(point.value);
        self.chars.push_compact(point.char_value);
        self.colors.push_compact(point.color);
        self.locations.push_compact(point.location);
        self.texts.push_compact(point.text);
        self.text_colors.push_compact(point.text_color);
        self.sizes.push_compact(point.size);
    }

    fn update_point(&mut self, point: Self::Point) {
        if let Some(current) = self.values.last_mut() {
            *current = point.value;
        }
        if let Some(current) = self.chars.last_mut() {
            *current = point.char_value;
        }
        if let Some(current) = self.colors.last_mut() {
            *current = point.color;
        }
        if let Some(current) = self.locations.last_mut() {
            *current = point.location;
        }
        if let Some(current) = self.texts.last_mut() {
            *current = point.text;
        }
        if let Some(current) = self.text_colors.last_mut() {
            *current = point.text_color;
        }
        if let Some(current) = self.sizes.last_mut() {
            *current = point.size;
        }
    }

    fn push_na_point(&mut self) {
        self.values.push_compact(PineValue::Na);
        self.chars.push_compact(PineValue::Na);
        self.colors.push_compact(PineValue::Na);
        self.locations.push_compact(PineValue::Na);
        self.texts.push_compact(PineValue::Na);
        self.text_colors.push_compact(PineValue::Na);
        self.sizes.push_compact(PineValue::Na);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimePlotShape {
    pub(crate) id: u32,
    pub(crate) values: AppendHistory<PineValue>,
    pub(crate) styles: AppendHistory<PineValue>,
    pub(crate) locations: AppendHistory<PineValue>,
    pub(crate) colors: AppendHistory<PineValue>,
    pub(crate) texts: AppendHistory<PineValue>,
    pub(crate) text_colors: AppendHistory<PineValue>,
    pub(crate) sizes: AppendHistory<PineValue>,
    pub(crate) metadata: OutputMetadata,
}

impl RuntimePlotShape {
    pub(crate) fn snapshot_from(&self, skip: usize) -> PlotShapeSeries {
        PlotShapeSeries {
            id: self.id,
            values: self.values.tail(skip),
            styles: self.styles.tail(skip),
            locations: self.locations.tail(skip),
            colors: self.colors.tail(skip),
            texts: self.texts.tail(skip),
            text_colors: self.text_colors.tail(skip),
            sizes: self.sizes.tail(skip),
            metadata: self.metadata.clone(),
        }
    }
}

impl BarAlignedOutput for RuntimePlotShape {
    type Point = PlotShapePoint;

    fn id(&self) -> u32 {
        self.id
    }

    fn new_padded(id: u32, current_bar: usize) -> Self {
        Self {
            id,
            values: na_history(current_bar),
            styles: na_history(current_bar),
            locations: na_history(current_bar),
            colors: na_history(current_bar),
            texts: na_history(current_bar),
            text_colors: na_history(current_bar),
            sizes: na_history(current_bar),
            metadata: OutputMetadata::default(),
        }
    }

    fn len(&self) -> usize {
        self.values.len()
    }

    fn pad_to(&mut self, current_bar: usize) {
        while self.values.len() < current_bar {
            self.push_na_point();
        }
    }

    fn push_point(&mut self, point: Self::Point) {
        self.values.push_compact(point.value);
        self.styles.push_compact(point.style);
        self.locations.push_compact(point.location);
        self.colors.push_compact(point.color);
        self.texts.push_compact(point.text);
        self.text_colors.push_compact(point.text_color);
        self.sizes.push_compact(point.size);
    }

    fn update_point(&mut self, point: Self::Point) {
        if let Some(current) = self.values.last_mut() {
            *current = point.value;
        }
        if let Some(current) = self.styles.last_mut() {
            *current = point.style;
        }
        if let Some(current) = self.locations.last_mut() {
            *current = point.location;
        }
        if let Some(current) = self.colors.last_mut() {
            *current = point.color;
        }
        if let Some(current) = self.texts.last_mut() {
            *current = point.text;
        }
        if let Some(current) = self.text_colors.last_mut() {
            *current = point.text_color;
        }
        if let Some(current) = self.sizes.last_mut() {
            *current = point.size;
        }
    }

    fn push_na_point(&mut self) {
        self.values.push_compact(PineValue::Na);
        self.styles.push_compact(PineValue::Na);
        self.locations.push_compact(PineValue::Na);
        self.colors.push_compact(PineValue::Na);
        self.texts.push_compact(PineValue::Na);
        self.text_colors.push_compact(PineValue::Na);
        self.sizes.push_compact(PineValue::Na);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimePlotArrow {
    pub(crate) id: u32,
    pub(crate) values: AppendHistory<PineValue>,
    pub(crate) color_ups: AppendHistory<PineValue>,
    pub(crate) color_downs: AppendHistory<PineValue>,
    pub(crate) min_heights: AppendHistory<PineValue>,
    pub(crate) max_heights: AppendHistory<PineValue>,
    pub(crate) metadata: OutputMetadata,
}

impl RuntimePlotArrow {
    pub(crate) fn snapshot_from(&self, skip: usize) -> PlotArrowSeries {
        PlotArrowSeries {
            id: self.id,
            values: self.values.tail(skip),
            color_ups: self.color_ups.tail(skip),
            color_downs: self.color_downs.tail(skip),
            min_heights: self.min_heights.tail(skip),
            max_heights: self.max_heights.tail(skip),
            metadata: self.metadata.clone(),
        }
    }
}

impl BarAlignedOutput for RuntimePlotArrow {
    type Point = PlotArrowPoint;

    fn id(&self) -> u32 {
        self.id
    }

    fn new_padded(id: u32, current_bar: usize) -> Self {
        Self {
            id,
            values: na_history(current_bar),
            color_ups: na_history(current_bar),
            color_downs: na_history(current_bar),
            min_heights: na_history(current_bar),
            max_heights: na_history(current_bar),
            metadata: OutputMetadata::default(),
        }
    }

    fn len(&self) -> usize {
        self.values.len()
    }

    fn pad_to(&mut self, current_bar: usize) {
        while self.values.len() < current_bar {
            self.push_na_point();
        }
    }

    fn push_point(&mut self, point: Self::Point) {
        self.values.push_compact(point.value);
        self.color_ups.push_compact(point.color_up);
        self.color_downs.push_compact(point.color_down);
        self.min_heights.push_compact(point.min_height);
        self.max_heights.push_compact(point.max_height);
    }

    fn update_point(&mut self, point: Self::Point) {
        if let Some(current) = self.values.last_mut() {
            *current = point.value;
        }
        if let Some(current) = self.color_ups.last_mut() {
            *current = point.color_up;
        }
        if let Some(current) = self.color_downs.last_mut() {
            *current = point.color_down;
        }
        if let Some(current) = self.min_heights.last_mut() {
            *current = point.min_height;
        }
        if let Some(current) = self.max_heights.last_mut() {
            *current = point.max_height;
        }
    }

    fn push_na_point(&mut self) {
        self.values.push_compact(PineValue::Na);
        self.color_ups.push_compact(PineValue::Na);
        self.color_downs.push_compact(PineValue::Na);
        self.min_heights.push_compact(PineValue::Na);
        self.max_heights.push_compact(PineValue::Na);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimePlotBar {
    pub(crate) id: u32,
    pub(crate) opens: AppendHistory<PineValue>,
    pub(crate) highs: AppendHistory<PineValue>,
    pub(crate) lows: AppendHistory<PineValue>,
    pub(crate) closes: AppendHistory<PineValue>,
    pub(crate) colors: AppendHistory<PineValue>,
    pub(crate) metadata: OutputMetadata,
}

impl RuntimePlotBar {
    pub(crate) fn snapshot_from(&self, skip: usize) -> PlotBarSeries {
        PlotBarSeries {
            id: self.id,
            opens: self.opens.tail(skip),
            highs: self.highs.tail(skip),
            lows: self.lows.tail(skip),
            closes: self.closes.tail(skip),
            colors: self.colors.tail(skip),
            metadata: self.metadata.clone(),
        }
    }
}

impl BarAlignedOutput for RuntimePlotBar {
    type Point = PlotBarPoint;

    fn id(&self) -> u32 {
        self.id
    }

    fn new_padded(id: u32, current_bar: usize) -> Self {
        Self {
            id,
            opens: na_history(current_bar),
            highs: na_history(current_bar),
            lows: na_history(current_bar),
            closes: na_history(current_bar),
            colors: na_history(current_bar),
            metadata: OutputMetadata::default(),
        }
    }

    fn len(&self) -> usize {
        self.opens.len()
    }

    fn pad_to(&mut self, current_bar: usize) {
        while self.opens.len() < current_bar {
            self.push_na_point();
        }
    }

    fn push_point(&mut self, point: Self::Point) {
        self.opens.push_compact(point.open);
        self.highs.push_compact(point.high);
        self.lows.push_compact(point.low);
        self.closes.push_compact(point.close);
        self.colors.push_compact(point.color);
    }

    fn update_point(&mut self, point: Self::Point) {
        if let Some(current) = self.opens.last_mut() {
            *current = point.open;
        }
        if let Some(current) = self.highs.last_mut() {
            *current = point.high;
        }
        if let Some(current) = self.lows.last_mut() {
            *current = point.low;
        }
        if let Some(current) = self.closes.last_mut() {
            *current = point.close;
        }
        if let Some(current) = self.colors.last_mut() {
            *current = point.color;
        }
    }

    fn push_na_point(&mut self) {
        self.opens.push_compact(PineValue::Na);
        self.highs.push_compact(PineValue::Na);
        self.lows.push_compact(PineValue::Na);
        self.closes.push_compact(PineValue::Na);
        self.colors.push_compact(PineValue::Na);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimePlotCandle {
    pub(crate) id: u32,
    pub(crate) opens: AppendHistory<PineValue>,
    pub(crate) highs: AppendHistory<PineValue>,
    pub(crate) lows: AppendHistory<PineValue>,
    pub(crate) closes: AppendHistory<PineValue>,
    pub(crate) colors: AppendHistory<PineValue>,
    pub(crate) wick_colors: AppendHistory<PineValue>,
    pub(crate) border_colors: AppendHistory<PineValue>,
    pub(crate) metadata: OutputMetadata,
}

impl RuntimePlotCandle {
    pub(crate) fn snapshot_from(&self, skip: usize) -> PlotCandleSeries {
        PlotCandleSeries {
            id: self.id,
            opens: self.opens.tail(skip),
            highs: self.highs.tail(skip),
            lows: self.lows.tail(skip),
            closes: self.closes.tail(skip),
            colors: self.colors.tail(skip),
            wick_colors: self.wick_colors.tail(skip),
            border_colors: self.border_colors.tail(skip),
            metadata: self.metadata.clone(),
        }
    }
}

impl BarAlignedOutput for RuntimePlotCandle {
    type Point = PlotCandlePoint;

    fn id(&self) -> u32 {
        self.id
    }

    fn new_padded(id: u32, current_bar: usize) -> Self {
        Self {
            id,
            opens: na_history(current_bar),
            highs: na_history(current_bar),
            lows: na_history(current_bar),
            closes: na_history(current_bar),
            colors: na_history(current_bar),
            wick_colors: na_history(current_bar),
            border_colors: na_history(current_bar),
            metadata: OutputMetadata::default(),
        }
    }

    fn len(&self) -> usize {
        self.opens.len()
    }

    fn pad_to(&mut self, current_bar: usize) {
        while self.opens.len() < current_bar {
            self.push_na_point();
        }
    }

    fn push_point(&mut self, point: Self::Point) {
        self.opens.push_compact(point.open);
        self.highs.push_compact(point.high);
        self.lows.push_compact(point.low);
        self.closes.push_compact(point.close);
        self.colors.push_compact(point.color);
        self.wick_colors.push_compact(point.wick_color);
        self.border_colors.push_compact(point.border_color);
    }

    fn update_point(&mut self, point: Self::Point) {
        if let Some(current) = self.opens.last_mut() {
            *current = point.open;
        }
        if let Some(current) = self.highs.last_mut() {
            *current = point.high;
        }
        if let Some(current) = self.lows.last_mut() {
            *current = point.low;
        }
        if let Some(current) = self.closes.last_mut() {
            *current = point.close;
        }
        if let Some(current) = self.colors.last_mut() {
            *current = point.color;
        }
        if let Some(current) = self.wick_colors.last_mut() {
            *current = point.wick_color;
        }
        if let Some(current) = self.border_colors.last_mut() {
            *current = point.border_color;
        }
    }

    fn push_na_point(&mut self) {
        self.opens.push_compact(PineValue::Na);
        self.highs.push_compact(PineValue::Na);
        self.lows.push_compact(PineValue::Na);
        self.closes.push_compact(PineValue::Na);
        self.colors.push_compact(PineValue::Na);
        self.wick_colors.push_compact(PineValue::Na);
        self.border_colors.push_compact(PineValue::Na);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeColorSeries {
    pub(crate) id: u32,
    pub(crate) values: AppendHistory<PineValue>,
    pub(crate) metadata: OutputMetadata,
}

impl RuntimeColorSeries {
    pub(crate) fn snapshot_from(&self, skip: usize) -> ColorSeries {
        ColorSeries {
            id: self.id,
            values: self.values.tail(skip),
            metadata: self.metadata.clone(),
        }
    }
}

impl SeriesOutput for RuntimeColorSeries {
    fn new(id: u32, values: AppendHistory<PineValue>) -> Self {
        Self {
            id,
            values,
            metadata: OutputMetadata::default(),
        }
    }

    fn id(&self) -> u32 {
        self.id
    }

    fn values_mut(&mut self) -> &mut AppendHistory<PineValue> {
        &mut self.values
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeFill {
    pub(crate) id: u32,
    pub(crate) first_id: u32,
    pub(crate) second_id: u32,
    pub(crate) first_is_hline: bool,
    pub(crate) second_is_hline: bool,
    pub(crate) colors: AppendHistory<PineValue>,
    pub(crate) gradient: Option<AppendHistory<crate::FillGradientSample>>,
    pub(crate) title: PineValue,
    pub(crate) editable: PineValue,
    pub(crate) show_last: PineValue,
    pub(crate) fill_gaps: PineValue,
    pub(crate) display: PineValue,
}

impl RuntimeFill {
    pub(crate) fn snapshot_from(&self, skip: usize) -> FillOutput {
        FillOutput {
            id: self.id,
            first_id: self.first_id,
            second_id: self.second_id,
            first_is_hline: self.first_is_hline,
            second_is_hline: self.second_is_hline,
            colors: self.colors.tail(skip),
            gradient: self.gradient.as_ref().map(|samples| samples.tail(skip)),
            title: self.title.clone(),
            editable: self.editable.clone(),
            show_last: self.show_last.clone(),
            fill_gaps: self.fill_gaps.clone(),
            display: self.display.clone(),
        }
    }
}

impl HistoricalRuntime<'_> {
    pub(crate) fn plots_mut(&mut self) -> &mut Vec<RuntimePlot> {
        Arc::make_mut(&mut self.plots)
    }
}
