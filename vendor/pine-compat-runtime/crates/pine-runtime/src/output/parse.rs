use serde_json::{Map, Value};

use super::alerts::AlertEvent;
use super::changes::{
    DrawingAction, DrawingChange, DrawingFamily, DrawingObject, EventAction, EventChange,
    FillAction, FillChange, HLineAction, HLineChange, ListSplice,
    PUBLIC_RUNTIME_CHANGES_SCHEMA_VERSION, RuntimeChanges, SeriesChange, SeriesChangeOp,
    SeriesFamily, SeriesFields, SeriesHeader, StrategyChanges, StreamingVisibility,
};
use super::drawings::{
    BoxOutput, BoxSnapshot, LabelOutput, LabelSnapshot, LineFillOutput, LineFillSnapshot,
    LineOutput, LineSnapshot, PolylineOutput, PolylineSnapshot, TableCellSnapshot,
    TableMergedCellSnapshot, TableOutput, TableSnapshot,
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
use crate::{ChartPointValue, PineValue};

pub fn runtime_result_from_json(json: &str) -> Result<RuntimeResult, String> {
    let value = parse_object(json, "runtime result")?;
    runtime_result_from_value(&value)
}

pub fn runtime_changes_from_json(json: &str) -> Result<RuntimeChanges, String> {
    let value = parse_object(json, "runtime changes")?;
    let visibility = match required_str(&value, "visibility")?.as_str() {
        "preview" => StreamingVisibility::Preview,
        "confirmed" => StreamingVisibility::Confirmed,
        other => return Err(format!("unsupported visibility `{other}`")),
    };
    let mut changes = RuntimeChanges::new(required_u64(&value, "revision")?, visibility);
    changes.base_revision = required_u64(&value, "baseRevision")?;
    changes.schema_version = required_u32(&value, "schemaVersion")?;
    if changes.schema_version > PUBLIC_RUNTIME_CHANGES_SCHEMA_VERSION {
        return Err(format!(
            "unsupported changes schemaVersion {}",
            changes.schema_version
        ));
    }
    if changes.schema_version >= 3 {
        changes.retained_from = required_usize(&value, "retainedFrom")?;
    }
    if let Some(series) = optional_array(&value, "series")? {
        changes.series = series_changes_from_values(series)?;
    }
    if let Some(hlines) = optional_array(&value, "hlines")? {
        changes.hlines = hline_changes_from_values(hlines)?;
    }
    if let Some(fills) = optional_array(&value, "fills")? {
        changes.fills = fill_changes_from_values(fills)?;
    }
    if changes.schema_version < 4
        && changes.fills.iter().any(|fill| match &fill.action {
            FillAction::SetGradient { .. } => true,
            FillAction::Add(fill) => fill.gradient.is_some(),
            _ => false,
        })
    {
        return Err("gradient fill requires changes schema 4".to_owned());
    }
    if let Some(drawings) = optional_array(&value, "drawings")? {
        changes.drawings = drawing_changes_from_values(drawings)?;
    }
    if let Some(alerts) = optional_array(&value, "alerts")? {
        changes.alerts = event_changes_from_values(alerts)?;
    }
    match value.get("strategy") {
        Some(Value::Null) | None => {}
        Some(strategy) => {
            let object = strategy
                .as_object()
                .ok_or_else(|| "strategy changes must be an object".to_owned())?;
            changes.strategy = Some(strategy_changes_from_object(object)?);
        }
    }
    if let Some(diagnostics) = optional_array(&value, "diagnostics")? {
        changes.diagnostics = diagnostics_from_values(diagnostics)?;
    }
    Ok(changes)
}

fn runtime_result_from_value(value: &Map<String, Value>) -> Result<RuntimeResult, String> {
    if optional_array(value, "fills")?
        .is_some_and(|fills| fills.iter().any(|fill| fill.get("gradient").is_some()))
        && required_u32(value, "schemaVersion")? < 9
    {
        return Err("gradient fill requires runtime schema 9".to_owned());
    }
    Ok(RuntimeResult {
        plots: plots_from_values(optional_array(value, "plots")?.unwrap_or(&[]))?,
        plot_chars: plot_chars_from_values(optional_array(value, "plotChars")?.unwrap_or(&[]))?,
        plot_shapes: plot_shapes_from_values(optional_array(value, "plotShapes")?.unwrap_or(&[]))?,
        plot_arrows: plot_arrows_from_values(optional_array(value, "plotArrows")?.unwrap_or(&[]))?,
        plot_bars: plot_bars_from_values(optional_array(value, "plotBars")?.unwrap_or(&[]))?,
        plot_candles: plot_candles_from_values(
            optional_array(value, "plotCandles")?.unwrap_or(&[]),
        )?,
        bg_colors: colors_from_values(optional_array(value, "bgColors")?.unwrap_or(&[]))?,
        bar_colors: colors_from_values(optional_array(value, "barColors")?.unwrap_or(&[]))?,
        hlines: hlines_from_values(optional_array(value, "hlines")?.unwrap_or(&[]))?,
        fills: fills_from_values(optional_array(value, "fills")?.unwrap_or(&[]))?,
        labels: labels_from_values(optional_array(value, "labels")?.unwrap_or(&[]))?,
        lines: lines_from_values(optional_array(value, "lines")?.unwrap_or(&[]))?,
        line_fills: line_fills_from_values(optional_array(value, "lineFills")?.unwrap_or(&[]))?,
        polylines: polylines_from_values(optional_array(value, "polylines")?.unwrap_or(&[]))?,
        boxes: boxes_from_values(optional_array(value, "boxes")?.unwrap_or(&[]))?,
        tables: tables_from_values(optional_array(value, "tables")?.unwrap_or(&[]))?,
        alerts: alerts_from_values(optional_array(value, "alerts")?.unwrap_or(&[]))?,
        strategy: match value.get("strategy") {
            Some(Value::Null) | None => None,
            Some(strategy) => Some(strategy_from_value(as_object(strategy, "strategy")?)?),
        },
        diagnostics: diagnostics_from_values(optional_array(value, "diagnostics")?.unwrap_or(&[]))?,
    })
}

fn plots_from_values(values: &[Value]) -> Result<Vec<PlotSeries>, String> {
    let mut plots = Vec::with_capacity(values.len());
    for value in values {
        let object = as_object(value, "plot entry")?;
        let series_values = required_pine_values(object, "values")?;
        let mut plot = PlotSeries::new(required_u32(object, "id")?, series_values);
        if let Some(colors) = optional_pine_values(object, "colors")? {
            plot.colors = colors;
        }
        plot.linewidth = optional_pine(object, "linewidth", PineValue::Int(1))?;
        plot.style = optional_pine(
            object,
            "style",
            PineValue::String("plot.style_line".to_owned()),
        )?;
        plot.track_price = optional_pine(object, "trackPrice", PineValue::Bool(false))?;
        plot.hist_base = optional_pine(object, "histBase", PineValue::Int(0))?;
        plot.join = optional_pine(object, "join", PineValue::Bool(false))?;
        plot.format = optional_pine(
            object,
            "format",
            PineValue::String("format.inherit".to_owned()),
        )?;
        plot.precision = optional_pine(object, "precision", PineValue::Na)?;
        plot.linestyle = optional_pine(
            object,
            "linestyle",
            PineValue::String("plot.linestyle_solid".to_owned()),
        )?;
        plot.metadata = metadata_from_object(object)?;
        plots.push(plot);
    }
    Ok(plots)
}

fn plot_chars_from_values(values: &[Value]) -> Result<Vec<PlotCharSeries>, String> {
    values
        .iter()
        .map(|value| {
            let object = as_object(value, "plotChar entry")?;
            Ok(PlotCharSeries {
                id: required_u32(object, "id")?,
                values: optional_pine_values(object, "values")?.unwrap_or_default(),
                chars: optional_pine_values(object, "chars")?.unwrap_or_default(),
                colors: optional_pine_values(object, "colors")?.unwrap_or_default(),
                locations: optional_pine_values(object, "locations")?.unwrap_or_default(),
                texts: optional_pine_values(object, "texts")?.unwrap_or_default(),
                text_colors: optional_pine_values(object, "textColors")?.unwrap_or_default(),
                sizes: optional_pine_values(object, "sizes")?.unwrap_or_default(),
                metadata: metadata_from_object(object)?,
            })
        })
        .collect()
}

fn plot_shapes_from_values(values: &[Value]) -> Result<Vec<PlotShapeSeries>, String> {
    values
        .iter()
        .map(|value| {
            let object = as_object(value, "plotShape entry")?;
            Ok(PlotShapeSeries {
                id: required_u32(object, "id")?,
                values: optional_pine_values(object, "values")?.unwrap_or_default(),
                styles: optional_pine_values(object, "styles")?.unwrap_or_default(),
                locations: optional_pine_values(object, "locations")?.unwrap_or_default(),
                colors: optional_pine_values(object, "colors")?.unwrap_or_default(),
                texts: optional_pine_values(object, "texts")?.unwrap_or_default(),
                text_colors: optional_pine_values(object, "textColors")?.unwrap_or_default(),
                sizes: optional_pine_values(object, "sizes")?.unwrap_or_default(),
                metadata: metadata_from_object(object)?,
            })
        })
        .collect()
}

fn plot_arrows_from_values(values: &[Value]) -> Result<Vec<PlotArrowSeries>, String> {
    values
        .iter()
        .map(|value| {
            let object = as_object(value, "plotArrow entry")?;
            Ok(PlotArrowSeries {
                id: required_u32(object, "id")?,
                values: optional_pine_values(object, "values")?.unwrap_or_default(),
                color_ups: optional_pine_values(object, "colorUps")?.unwrap_or_default(),
                color_downs: optional_pine_values(object, "colorDowns")?.unwrap_or_default(),
                min_heights: optional_pine_values(object, "minHeights")?.unwrap_or_default(),
                max_heights: optional_pine_values(object, "maxHeights")?.unwrap_or_default(),
                metadata: metadata_from_object(object)?,
            })
        })
        .collect()
}

fn plot_bars_from_values(values: &[Value]) -> Result<Vec<PlotBarSeries>, String> {
    values
        .iter()
        .map(|value| {
            let object = as_object(value, "plotBar entry")?;
            Ok(PlotBarSeries {
                id: required_u32(object, "id")?,
                opens: optional_pine_values(object, "opens")?.unwrap_or_default(),
                highs: optional_pine_values(object, "highs")?.unwrap_or_default(),
                lows: optional_pine_values(object, "lows")?.unwrap_or_default(),
                closes: optional_pine_values(object, "closes")?.unwrap_or_default(),
                colors: optional_pine_values(object, "colors")?.unwrap_or_default(),
                metadata: metadata_from_object(object)?,
            })
        })
        .collect()
}

fn plot_candles_from_values(values: &[Value]) -> Result<Vec<PlotCandleSeries>, String> {
    values
        .iter()
        .map(|value| {
            let object = as_object(value, "plotCandle entry")?;
            Ok(PlotCandleSeries {
                id: required_u32(object, "id")?,
                opens: optional_pine_values(object, "opens")?.unwrap_or_default(),
                highs: optional_pine_values(object, "highs")?.unwrap_or_default(),
                lows: optional_pine_values(object, "lows")?.unwrap_or_default(),
                closes: optional_pine_values(object, "closes")?.unwrap_or_default(),
                colors: optional_pine_values(object, "colors")?.unwrap_or_default(),
                wick_colors: optional_pine_values(object, "wickColors")?.unwrap_or_default(),
                border_colors: optional_pine_values(object, "borderColors")?.unwrap_or_default(),
                metadata: metadata_from_object(object)?,
            })
        })
        .collect()
}

fn colors_from_values(values: &[Value]) -> Result<Vec<ColorSeries>, String> {
    values
        .iter()
        .map(|value| {
            let object = as_object(value, "color series entry")?;
            Ok(ColorSeries {
                id: required_u32(object, "id")?,
                values: optional_pine_values(object, "values")?.unwrap_or_default(),
                metadata: metadata_from_object(object)?,
            })
        })
        .collect()
}

fn hlines_from_values(values: &[Value]) -> Result<Vec<HLineOutput>, String> {
    values
        .iter()
        .map(|value| hline_from_object(as_object(value, "hline entry")?))
        .collect()
}

fn hline_from_object(object: &Map<String, Value>) -> Result<HLineOutput, String> {
    Ok(HLineOutput {
        id: required_u32(object, "id")?,
        price: optional_pine(object, "price", PineValue::Na)?,
        title: optional_pine(object, "title", PineValue::String(String::new()))?,
        color: optional_pine(object, "color", PineValue::Color(0x787B86))?,
        style: optional_pine(
            object,
            "style",
            PineValue::String("hline.style_solid".to_owned()),
        )?,
        linewidth: optional_pine(object, "linewidth", PineValue::Int(1))?,
        editable: optional_pine(object, "editable", PineValue::Bool(true))?,
        display: optional_pine(
            object,
            "display",
            PineValue::String("display.all".to_owned()),
        )?,
    })
}

fn fills_from_values(values: &[Value]) -> Result<Vec<FillOutput>, String> {
    values
        .iter()
        .map(|value| fill_from_object(as_object(value, "fill entry")?))
        .collect()
}

fn fill_from_object(object: &Map<String, Value>) -> Result<FillOutput, String> {
    let fill = FillOutput {
        id: required_u32(object, "id")?,
        first_id: required_u32(object, "firstId")?,
        second_id: required_u32(object, "secondId")?,
        first_is_hline: optional_bool(object, "firstIsHLine")?.unwrap_or(false),
        second_is_hline: optional_bool(object, "secondIsHLine")?.unwrap_or(false),
        colors: optional_pine_values(object, "colors")?.unwrap_or_default(),
        gradient: optional_array(object, "gradient")?
            .map(gradient_samples_from_values)
            .transpose()?,
        title: optional_pine(object, "title", PineValue::String(String::new()))?,
        editable: optional_pine(object, "editable", PineValue::Bool(true))?,
        show_last: optional_pine(object, "showLast", PineValue::Na)?,
        fill_gaps: optional_pine(object, "fillGaps", PineValue::Bool(true))?,
        display: optional_pine(
            object,
            "display",
            PineValue::String("display.all".to_owned()),
        )?,
    };
    if fill
        .gradient
        .as_ref()
        .is_some_and(|values| values.len() != fill.colors.len())
    {
        return Err("gradient sample count must match fill colors".to_owned());
    }
    Ok(fill)
}

fn gradient_samples_from_values(
    values: &[Value],
) -> Result<Vec<crate::FillGradientSample>, String> {
    values
        .iter()
        .map(|value| {
            let object = as_object(value, "gradient sample")?;
            for name in ["topValue", "bottomValue", "topColor", "bottomColor"] {
                if !object.contains_key(name) {
                    return Err(format!("gradient sample requires {name}"));
                }
            }
            let sample: crate::FillGradientSample = serde::Deserialize::deserialize(value)
                .map_err(|e| format!("invalid gradient sample: {e}"))?;
            if [sample.top_color, sample.bottom_color]
                .into_iter()
                .flatten()
                .any(|color| !crate::value::is_valid_public_color(color))
            {
                return Err("invalid gradient color encoding".to_owned());
            }
            Ok(sample)
        })
        .collect()
}

fn labels_from_values(values: &[Value]) -> Result<Vec<LabelOutput>, String> {
    values
        .iter()
        .map(|value| label_from_object(as_object(value, "label entry")?))
        .collect()
}

fn label_from_object(object: &Map<String, Value>) -> Result<LabelOutput, String> {
    Ok(LabelOutput {
        id: required_u32(object, "id")?,
        snapshots: optional_array(object, "snapshots")?
            .unwrap_or(&[])
            .iter()
            .map(|value| {
                let snapshot = as_object(value, "label snapshot")?;
                let exists = optional_bool(snapshot, "exists")?.unwrap_or(true);
                Ok(LabelSnapshot {
                    bar_index: required_usize(snapshot, "barIndex")?,
                    exists,
                    x: optional_pine(snapshot, "x", PineValue::Na)?,
                    y: optional_pine(snapshot, "y", PineValue::Na)?,
                    text: optional_pine(snapshot, "text", PineValue::String(String::new()))?,
                    xloc: optional_pine(snapshot, "xloc", PineValue::String(String::new()))?,
                    yloc: optional_pine(snapshot, "yloc", PineValue::String(String::new()))?,
                    color: optional_pine(snapshot, "color", PineValue::Na)?,
                    style: optional_pine(snapshot, "style", PineValue::String(String::new()))?,
                    text_color: optional_pine(snapshot, "textColor", PineValue::Na)?,
                    size: optional_pine(snapshot, "size", PineValue::String(String::new()))?,
                    tooltip: optional_pine(snapshot, "tooltip", PineValue::String(String::new()))?,
                    text_align: optional_pine(
                        snapshot,
                        "textAlign",
                        PineValue::String(String::new()),
                    )?,
                    text_font_family: optional_pine(
                        snapshot,
                        "textFontFamily",
                        PineValue::String(String::new()),
                    )?,
                    text_formatting: optional_pine(snapshot, "textFormatting", PineValue::Int(0))?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?,
    })
}

fn lines_from_values(values: &[Value]) -> Result<Vec<LineOutput>, String> {
    values
        .iter()
        .map(|value| line_from_object(as_object(value, "line entry")?))
        .collect()
}

fn line_from_object(object: &Map<String, Value>) -> Result<LineOutput, String> {
    Ok(LineOutput {
        id: required_u32(object, "id")?,
        snapshots: optional_array(object, "snapshots")?
            .unwrap_or(&[])
            .iter()
            .map(|value| {
                let snapshot = as_object(value, "line snapshot")?;
                Ok(LineSnapshot {
                    bar_index: required_usize(snapshot, "barIndex")?,
                    exists: optional_bool(snapshot, "exists")?.unwrap_or(true),
                    x1: optional_pine(snapshot, "x1", PineValue::Na)?,
                    y1: optional_pine(snapshot, "y1", PineValue::Na)?,
                    x2: optional_pine(snapshot, "x2", PineValue::Na)?,
                    y2: optional_pine(snapshot, "y2", PineValue::Na)?,
                    xloc: optional_pine(snapshot, "xloc", PineValue::String(String::new()))?,
                    color: optional_pine(snapshot, "color", PineValue::Na)?,
                    width: optional_pine(snapshot, "width", PineValue::Int(1))?,
                    style: optional_pine(snapshot, "style", PineValue::String(String::new()))?,
                    extend: optional_pine(snapshot, "extend", PineValue::String(String::new()))?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?,
    })
}

fn line_fills_from_values(values: &[Value]) -> Result<Vec<LineFillOutput>, String> {
    values
        .iter()
        .map(|value| line_fill_from_object(as_object(value, "lineFill entry")?))
        .collect()
}

fn line_fill_from_object(object: &Map<String, Value>) -> Result<LineFillOutput, String> {
    Ok(LineFillOutput {
        id: required_u32(object, "id")?,
        snapshots: optional_array(object, "snapshots")?
            .unwrap_or(&[])
            .iter()
            .map(|value| {
                let snapshot = as_object(value, "lineFill snapshot")?;
                Ok(LineFillSnapshot {
                    bar_index: required_usize(snapshot, "barIndex")?,
                    exists: optional_bool(snapshot, "exists")?.unwrap_or(true),
                    line1: optional_u32(snapshot, "line1")?.unwrap_or(0),
                    line2: optional_u32(snapshot, "line2")?.unwrap_or(0),
                    color: optional_pine(snapshot, "color", PineValue::Na)?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?,
    })
}

fn polylines_from_values(values: &[Value]) -> Result<Vec<PolylineOutput>, String> {
    values
        .iter()
        .map(|value| polyline_from_object(as_object(value, "polyline entry")?))
        .collect()
}

fn polyline_from_object(object: &Map<String, Value>) -> Result<PolylineOutput, String> {
    Ok(PolylineOutput {
        id: required_u32(object, "id")?,
        snapshots: optional_array(object, "snapshots")?
            .unwrap_or(&[])
            .iter()
            .map(|value| {
                let snapshot = as_object(value, "polyline snapshot")?;
                Ok(PolylineSnapshot {
                    bar_index: required_usize(snapshot, "barIndex")?,
                    exists: optional_bool(snapshot, "exists")?.unwrap_or(true),
                    points: optional_pine_values(snapshot, "points")?.unwrap_or_default(),
                    curved: optional_pine(snapshot, "curved", PineValue::Bool(false))?,
                    closed: optional_pine(snapshot, "closed", PineValue::Bool(false))?,
                    xloc: optional_pine(snapshot, "xloc", PineValue::String(String::new()))?,
                    line_color: optional_pine(snapshot, "lineColor", PineValue::Na)?,
                    fill_color: optional_pine(snapshot, "fillColor", PineValue::Na)?,
                    line_style: optional_pine(
                        snapshot,
                        "lineStyle",
                        PineValue::String(String::new()),
                    )?,
                    line_width: optional_pine(snapshot, "lineWidth", PineValue::Int(1))?,
                    force_overlay: optional_pine(snapshot, "forceOverlay", PineValue::Bool(false))?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?,
    })
}

fn boxes_from_values(values: &[Value]) -> Result<Vec<BoxOutput>, String> {
    values
        .iter()
        .map(|value| box_from_object(as_object(value, "box entry")?))
        .collect()
}

fn box_from_object(object: &Map<String, Value>) -> Result<BoxOutput, String> {
    Ok(BoxOutput {
        id: required_u32(object, "id")?,
        snapshots: optional_array(object, "snapshots")?
            .unwrap_or(&[])
            .iter()
            .map(|value| {
                let snapshot = as_object(value, "box snapshot")?;
                Ok(BoxSnapshot {
                    bar_index: required_usize(snapshot, "barIndex")?,
                    exists: optional_bool(snapshot, "exists")?.unwrap_or(true),
                    left: optional_pine(snapshot, "left", PineValue::Na)?,
                    top: optional_pine(snapshot, "top", PineValue::Na)?,
                    right: optional_pine(snapshot, "right", PineValue::Na)?,
                    bottom: optional_pine(snapshot, "bottom", PineValue::Na)?,
                    xloc: optional_pine(snapshot, "xloc", PineValue::String(String::new()))?,
                    bg_color: optional_pine(snapshot, "bgColor", PineValue::Na)?,
                    border_color: optional_pine(snapshot, "borderColor", PineValue::Na)?,
                    border_width: optional_pine(snapshot, "borderWidth", PineValue::Int(1))?,
                    border_style: optional_pine(
                        snapshot,
                        "borderStyle",
                        PineValue::String(String::new()),
                    )?,
                    extend: optional_pine(snapshot, "extend", PineValue::String(String::new()))?,
                    text: optional_pine(snapshot, "text", PineValue::String(String::new()))?,
                    text_color: optional_pine(snapshot, "textColor", PineValue::Na)?,
                    text_size: optional_pine(
                        snapshot,
                        "textSize",
                        PineValue::String(String::new()),
                    )?,
                    text_halign: optional_pine(
                        snapshot,
                        "textHalign",
                        PineValue::String(String::new()),
                    )?,
                    text_valign: optional_pine(
                        snapshot,
                        "textValign",
                        PineValue::String(String::new()),
                    )?,
                    text_wrap: optional_pine(
                        snapshot,
                        "textWrap",
                        PineValue::String(String::new()),
                    )?,
                    text_font_family: optional_pine(
                        snapshot,
                        "textFontFamily",
                        PineValue::String(String::new()),
                    )?,
                    text_formatting: optional_pine(snapshot, "textFormatting", PineValue::Int(0))?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?,
    })
}

fn tables_from_values(values: &[Value]) -> Result<Vec<TableOutput>, String> {
    values
        .iter()
        .map(|value| table_from_object(as_object(value, "table entry")?))
        .collect()
}

fn table_from_object(object: &Map<String, Value>) -> Result<TableOutput, String> {
    Ok(TableOutput {
        id: required_u32(object, "id")?,
        position: optional_pine(object, "position", PineValue::String(String::new()))?,
        bg_color: optional_pine(object, "bgColor", PineValue::Na)?,
        frame_color: optional_pine(object, "frameColor", PineValue::Na)?,
        frame_width: optional_pine(object, "frameWidth", PineValue::Int(0))?,
        border_color: optional_pine(object, "borderColor", PineValue::Na)?,
        border_width: optional_pine(object, "borderWidth", PineValue::Int(0))?,
        columns: optional_i64(object, "columns")?.unwrap_or(0),
        rows: optional_i64(object, "rows")?.unwrap_or(0),
        snapshots: optional_array(object, "snapshots")?
            .unwrap_or(&[])
            .iter()
            .map(table_snapshot_from_value)
            .collect::<Result<Vec<_>, String>>()?,
    })
}

fn table_snapshot_from_value(value: &Value) -> Result<TableSnapshot, String> {
    let snapshot = as_object(value, "table snapshot")?;
    Ok(TableSnapshot {
        bar_index: required_usize(snapshot, "barIndex")?,
        exists: optional_bool(snapshot, "exists")?.unwrap_or(true),
        cells: optional_array(snapshot, "cells")?
            .unwrap_or(&[])
            .iter()
            .map(|value| {
                let cell = as_object(value, "table cell")?;
                Ok(TableCellSnapshot {
                    column: optional_i64(cell, "column")?.unwrap_or(0),
                    row: optional_i64(cell, "row")?.unwrap_or(0),
                    text: optional_pine(cell, "text", PineValue::String(String::new()))?,
                    bg_color: optional_pine(cell, "bgColor", PineValue::Na)?,
                    text_color: optional_pine(cell, "textColor", PineValue::Na)?,
                    width: optional_pine(cell, "width", PineValue::Na)?,
                    height: optional_pine(cell, "height", PineValue::Na)?,
                    text_size: optional_pine(cell, "textSize", PineValue::String(String::new()))?,
                    text_halign: optional_pine(
                        cell,
                        "textHalign",
                        PineValue::String(String::new()),
                    )?,
                    text_valign: optional_pine(
                        cell,
                        "textValign",
                        PineValue::String(String::new()),
                    )?,
                    text_wrap: optional_pine(cell, "textWrap", PineValue::String(String::new()))?,
                    tooltip: optional_pine(cell, "tooltip", PineValue::String(String::new()))?,
                    text_font_family: optional_pine(
                        cell,
                        "textFontFamily",
                        PineValue::String(String::new()),
                    )?,
                    text_formatting: optional_pine(cell, "textFormatting", PineValue::Int(0))?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?,
        merged_cells: optional_array(snapshot, "mergedCells")?
            .unwrap_or(&[])
            .iter()
            .map(|value| {
                let cell = as_object(value, "merged cell")?;
                Ok(TableMergedCellSnapshot {
                    start_column: optional_i64(cell, "startColumn")?.unwrap_or(0),
                    start_row: optional_i64(cell, "startRow")?.unwrap_or(0),
                    end_column: optional_i64(cell, "endColumn")?.unwrap_or(0),
                    end_row: optional_i64(cell, "endRow")?.unwrap_or(0),
                })
            })
            .collect::<Result<Vec<_>, String>>()?,
    })
}

fn alerts_from_values(values: &[Value]) -> Result<Vec<AlertEvent>, String> {
    values.iter().map(alert_from_value).collect()
}

fn alert_from_value(value: &Value) -> Result<AlertEvent, String> {
    let object = as_object(value, "alert event")?;
    Ok(AlertEvent {
        id: required_u32(object, "id")?,
        bar_index: required_usize(object, "barIndex")?,
        time: required_i64(object, "time")?,
        message: required_str(object, "message")?,
        source: required_str(object, "source")?,
    })
}

fn diagnostics_from_values(values: &[Value]) -> Result<Vec<RuntimeDiagnostic>, String> {
    values
        .iter()
        .map(|value| {
            let object = as_object(value, "diagnostic")?;
            Ok(RuntimeDiagnostic {
                code: required_str(object, "code")?,
                message: required_str(object, "message")?,
            })
        })
        .collect()
}

fn strategy_from_value(object: &Map<String, Value>) -> Result<StrategyResult, String> {
    Ok(StrategyResult {
        orders: strategy_orders_from_values(optional_array(object, "orders")?.unwrap_or(&[]))?,
        trades: strategy_trades_from_values(optional_array(object, "trades")?.unwrap_or(&[]))?,
        position: strategy_position_from_values(
            optional_array(object, "position")?.unwrap_or(&[]),
        )?,
        equity: strategy_equity_from_values(optional_array(object, "equity")?.unwrap_or(&[]))?,
        alerts: strategy_alerts_from_values(optional_array(object, "alerts")?.unwrap_or(&[]))?,
        diagnostics: diagnostics_from_values(
            optional_array(object, "diagnostics")?.unwrap_or(&[]),
        )?,
    })
}

fn strategy_orders_from_values(values: &[Value]) -> Result<Vec<StrategyOrderEvent>, String> {
    values
        .iter()
        .map(|value| {
            let object = as_object(value, "strategy order")?;
            Ok(StrategyOrderEvent {
                id: required_str(object, "id")?,
                bar_index: required_usize(object, "barIndex")?,
                time: required_i64(object, "time")?,
                direction: required_str(object, "direction")?,
                qty: required_f64(object, "qty")?,
                price: required_f64(object, "price")?,
            })
        })
        .collect()
}

fn strategy_trades_from_values(values: &[Value]) -> Result<Vec<StrategyTrade>, String> {
    values
        .iter()
        .map(|value| {
            let object = as_object(value, "strategy trade")?;
            Ok(StrategyTrade {
                id: required_str(object, "id")?,
                exit_id: optional_str(object, "exitId")?.unwrap_or_default(),
                entry_bar_index: required_usize(object, "entryBarIndex")?,
                exit_bar_index: required_usize(object, "exitBarIndex")?,
                entry_time: optional_i64(object, "entryTime")?.unwrap_or(0),
                exit_time: optional_i64(object, "exitTime")?.unwrap_or(0),
                entry_price: required_f64(object, "entryPrice")?,
                exit_price: required_f64(object, "exitPrice")?,
                qty: required_f64(object, "qty")?,
                profit: optional_f64(object, "profit")?.unwrap_or(0.0),
            })
        })
        .collect()
}

fn strategy_position_from_values(
    values: &[Value],
) -> Result<Vec<StrategyPositionSnapshot>, String> {
    values
        .iter()
        .map(|value| {
            let object = as_object(value, "strategy position")?;
            Ok(StrategyPositionSnapshot {
                bar_index: required_usize(object, "barIndex")?,
                size: required_f64(object, "size")?,
                avg_price: match object.get("avgPrice") {
                    None | Some(Value::Null) => None,
                    Some(value) => Some(json_f64(value, "avgPrice")?),
                },
            })
        })
        .collect()
}

fn strategy_equity_from_values(values: &[Value]) -> Result<Vec<StrategyEquitySnapshot>, String> {
    values
        .iter()
        .map(|value| {
            let object = as_object(value, "strategy equity")?;
            Ok(StrategyEquitySnapshot {
                bar_index: required_usize(object, "barIndex")?,
                cash: required_f64(object, "cash")?,
                market_value: required_f64(object, "marketValue")?,
                equity: required_f64(object, "equity")?,
                net_profit: required_f64(object, "netProfit")?,
            })
        })
        .collect()
}

fn strategy_alerts_from_values(
    values: &[Value],
) -> Result<Vec<StrategyOrderFillAlertOutput>, String> {
    values
        .iter()
        .map(|value| {
            let object = as_object(value, "strategy fill alert")?;
            Ok(StrategyOrderFillAlertOutput {
                id: required_str(object, "id")?,
                bar_index: required_usize(object, "barIndex")?,
                time: optional_i64(object, "time")?.unwrap_or(0),
                direction: required_str(object, "direction")?,
                qty: required_f64(object, "qty")?,
                price: required_f64(object, "price")?,
                entry_id: optional_str(object, "entryId")?,
                exit_id: optional_str(object, "exitId")?,
                message: optional_str(object, "message")?.unwrap_or_default(),
            })
        })
        .collect()
}

fn series_changes_from_values(values: &[Value]) -> Result<Vec<SeriesChange>, String> {
    values
        .iter()
        .map(|value| {
            let object = as_object(value, "series change")?;
            Ok(SeriesChange {
                family: series_family(&required_str(object, "family")?)?,
                id: required_u32(object, "id")?,
                op: match required_str(object, "op")?.as_str() {
                    "append" => SeriesChangeOp::Append,
                    "replaceLast" => SeriesChangeOp::ReplaceLast,
                    other => return Err(format!("unsupported series op `{other}`")),
                },
                start: required_usize(object, "start")?,
                fields: SeriesFields {
                    values: optional_pine_values(object, "values")?.unwrap_or_default(),
                    colors: optional_pine_values(object, "colors")?.unwrap_or_default(),
                    chars: optional_pine_values(object, "chars")?.unwrap_or_default(),
                    locations: optional_pine_values(object, "locations")?.unwrap_or_default(),
                    texts: optional_pine_values(object, "texts")?.unwrap_or_default(),
                    text_colors: optional_pine_values(object, "textColors")?.unwrap_or_default(),
                    sizes: optional_pine_values(object, "sizes")?.unwrap_or_default(),
                    styles: optional_pine_values(object, "styles")?.unwrap_or_default(),
                    color_ups: optional_pine_values(object, "colorUps")?.unwrap_or_default(),
                    color_downs: optional_pine_values(object, "colorDowns")?.unwrap_or_default(),
                    min_heights: optional_pine_values(object, "minHeights")?.unwrap_or_default(),
                    max_heights: optional_pine_values(object, "maxHeights")?.unwrap_or_default(),
                    opens: optional_pine_values(object, "opens")?.unwrap_or_default(),
                    highs: optional_pine_values(object, "highs")?.unwrap_or_default(),
                    lows: optional_pine_values(object, "lows")?.unwrap_or_default(),
                    closes: optional_pine_values(object, "closes")?.unwrap_or_default(),
                    wick_colors: optional_pine_values(object, "wickColors")?.unwrap_or_default(),
                    border_colors: optional_pine_values(object, "borderColors")?
                        .unwrap_or_default(),
                },
                header: match object.get("header") {
                    Some(Value::Null) | None => None,
                    Some(header) => Some(series_header_from_object(as_object(
                        header,
                        "series change header",
                    )?)?),
                },
            })
        })
        .collect()
}

fn series_header_from_object(object: &Map<String, Value>) -> Result<SeriesHeader, String> {
    Ok(SeriesHeader {
        metadata: metadata_from_object(object)?,
        linewidth: optional_pine(object, "linewidth", PineValue::Int(1))?,
        style: optional_pine(
            object,
            "style",
            PineValue::String("plot.style_line".to_owned()),
        )?,
        track_price: optional_pine(object, "trackPrice", PineValue::Bool(false))?,
        hist_base: optional_pine(object, "histBase", PineValue::Int(0))?,
        join: optional_pine(object, "join", PineValue::Bool(false))?,
        format: optional_pine(
            object,
            "format",
            PineValue::String("format.inherit".to_owned()),
        )?,
        precision: optional_pine(object, "precision", PineValue::Na)?,
        linestyle: optional_pine(
            object,
            "linestyle",
            PineValue::String("plot.linestyle_solid".to_owned()),
        )?,
    })
}

fn series_family(value: &str) -> Result<SeriesFamily, String> {
    Ok(match value {
        "plot" => SeriesFamily::Plot,
        "plotChar" => SeriesFamily::PlotChar,
        "plotShape" => SeriesFamily::PlotShape,
        "plotArrow" => SeriesFamily::PlotArrow,
        "plotBar" => SeriesFamily::PlotBar,
        "plotCandle" => SeriesFamily::PlotCandle,
        "bgColor" => SeriesFamily::BgColor,
        "barColor" => SeriesFamily::BarColor,
        other => return Err(format!("unsupported series family `{other}`")),
    })
}

fn hline_changes_from_values(values: &[Value]) -> Result<Vec<HLineChange>, String> {
    values
        .iter()
        .map(|value| {
            let object = as_object(value, "hline change")?;
            let action = match required_str(object, "action")?.as_str() {
                "delete" => HLineAction::Delete,
                "add" => HLineAction::Add(hline_from_object(required_object(object, "object")?)?),
                "replace" => {
                    HLineAction::Replace(hline_from_object(required_object(object, "object")?)?)
                }
                other => return Err(format!("unsupported hline action `{other}`")),
            };
            Ok(HLineChange {
                id: required_u32(object, "id")?,
                action,
            })
        })
        .collect()
}

fn fill_changes_from_values(values: &[Value]) -> Result<Vec<FillChange>, String> {
    values
        .iter()
        .map(|value| {
            let object = as_object(value, "fill change")?;
            let action = match required_str(object, "action")?.as_str() {
                "delete" => FillAction::Delete,
                "setColors" => FillAction::SetColors {
                    start: required_usize(object, "start")?,
                    values: optional_pine_values(object, "values")?.unwrap_or_default(),
                },
                "setGradient" => FillAction::SetGradient {
                    start: required_usize(object, "start")?,
                    values: gradient_samples_from_values(
                        optional_array(object, "values")?.ok_or("setGradient requires values")?,
                    )?,
                },
                "add" => FillAction::Add(fill_from_object(required_object(object, "object")?)?),
                other => return Err(format!("unsupported fill action `{other}`")),
            };
            Ok(FillChange {
                id: required_u32(object, "id")?,
                action,
            })
        })
        .collect()
}

fn drawing_changes_from_values(values: &[Value]) -> Result<Vec<DrawingChange>, String> {
    values
        .iter()
        .map(|value| {
            let object = as_object(value, "drawing change")?;
            let family = drawing_family(&required_str(object, "family")?)?;
            let action = match required_str(object, "action")?.as_str() {
                "delete" => DrawingAction::Delete,
                "add" => DrawingAction::Add(drawing_object_from_value(
                    family,
                    object
                        .get("object")
                        .ok_or_else(|| "drawing add missing object".to_owned())?,
                )?),
                "setTail" => DrawingAction::SetTail {
                    start: required_usize(object, "start")?,
                    object: drawing_object_from_value(
                        family,
                        object
                            .get("object")
                            .ok_or_else(|| "drawing setTail missing object".to_owned())?,
                    )?,
                },
                other => return Err(format!("unsupported drawing action `{other}`")),
            };
            Ok(DrawingChange {
                family,
                id: required_u32(object, "id")?,
                action,
            })
        })
        .collect()
}

fn drawing_family(value: &str) -> Result<DrawingFamily, String> {
    Ok(match value {
        "label" => DrawingFamily::Label,
        "line" => DrawingFamily::Line,
        "lineFill" => DrawingFamily::LineFill,
        "polyline" => DrawingFamily::Polyline,
        "box" => DrawingFamily::Box,
        "table" => DrawingFamily::Table,
        other => return Err(format!("unsupported drawing family `{other}`")),
    })
}

fn drawing_object_from_value(
    family: DrawingFamily,
    value: &Value,
) -> Result<DrawingObject, String> {
    let object = as_object(value, "drawing object")?;
    Ok(match family {
        DrawingFamily::Label => DrawingObject::Label(label_from_object(object)?),
        DrawingFamily::Line => DrawingObject::Line(line_from_object(object)?),
        DrawingFamily::LineFill => DrawingObject::LineFill(line_fill_from_object(object)?),
        DrawingFamily::Polyline => DrawingObject::Polyline(polyline_from_object(object)?),
        DrawingFamily::Box => DrawingObject::Box(box_from_object(object)?),
        DrawingFamily::Table => DrawingObject::Table(Box::new(table_from_object(object)?)),
    })
}

fn event_changes_from_values(values: &[Value]) -> Result<Vec<EventChange<AlertEvent>>, String> {
    values
        .iter()
        .map(|value| {
            let object = as_object(value, "alert change")?;
            let action = match required_str(object, "action")?.as_str() {
                "add" => EventAction::Add,
                "remove" => EventAction::Remove,
                other => return Err(format!("unsupported event action `{other}`")),
            };
            let event = object
                .get("event")
                .ok_or_else(|| "alert change missing event".to_owned())?;
            Ok(EventChange {
                action,
                event: alert_from_value(event)?,
            })
        })
        .collect()
}

fn strategy_changes_from_object(object: &Map<String, Value>) -> Result<StrategyChanges, String> {
    Ok(StrategyChanges {
        orders: optional_splice(object, "orders", strategy_orders_from_values)?,
        trades: optional_splice(object, "trades", strategy_trades_from_values)?,
        alerts: optional_splice(object, "alerts", strategy_alerts_from_values)?,
        position: optional_splice(object, "position", strategy_position_from_values)?,
        equity: optional_splice(object, "equity", strategy_equity_from_values)?,
        diagnostics: match object.get("diagnostics") {
            Some(Value::Null) | None => None,
            Some(value) => Some(diagnostics_from_values(as_array(
                value,
                "strategy diagnostics",
            )?)?),
        },
    })
}

fn optional_splice<T>(
    object: &Map<String, Value>,
    key: &str,
    parse_items: fn(&[Value]) -> Result<Vec<T>, String>,
) -> Result<Option<ListSplice<T>>, String> {
    match object.get(key) {
        Some(Value::Null) | None => Ok(None),
        Some(value) => {
            let splice = as_object(value, &format!("`{key}` splice"))?;
            Ok(Some(ListSplice {
                start: required_usize(splice, "start")?,
                items: parse_items(
                    splice
                        .get("items")
                        .ok_or_else(|| format!("{key} splice missing items"))
                        .and_then(|items| as_array(items, &format!("{key} items")))?,
                )?,
            }))
        }
    }
}

fn metadata_from_object(object: &Map<String, Value>) -> Result<OutputMetadata, String> {
    Ok(OutputMetadata {
        title: optional_pine(object, "title", PineValue::String(String::new()))?,
        offset: optional_pine(object, "offset", PineValue::Int(0))?,
        editable: optional_pine(object, "editable", PineValue::Bool(true))?,
        show_last: optional_pine(object, "showLast", PineValue::Na)?,
        display: optional_pine(
            object,
            "display",
            PineValue::String("display.all".to_owned()),
        )?,
        force_overlay: optional_pine(object, "forceOverlay", PineValue::Bool(false))?,
    })
}

fn pine_value_from_json(value: &Value) -> Result<PineValue, String> {
    match value {
        Value::Null => Ok(PineValue::Na),
        Value::Bool(value) => Ok(PineValue::Bool(*value)),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                Ok(PineValue::Int(value))
            } else if let Some(value) = number.as_u64() {
                Ok(PineValue::Color(value))
            } else if let Some(value) = number.as_f64() {
                if value.is_finite() {
                    Ok(PineValue::Float(value))
                } else {
                    Ok(PineValue::Na)
                }
            } else {
                Err("unsupported numeric Pine value".to_owned())
            }
        }
        Value::String(value) => Ok(PineValue::String(value.clone())),
        Value::Array(values) => {
            let mut items = Vec::with_capacity(values.len());
            for value in values {
                items.push(pine_value_from_json(value)?);
            }
            Ok(PineValue::Tuple(items))
        }
        Value::Object(object) => {
            if object.contains_key("time")
                && object.contains_key("index")
                && object.contains_key("price")
            {
                Ok(PineValue::ChartPoint(ChartPointValue::new(
                    pine_value_from_json(&object["time"])?,
                    pine_value_from_json(&object["index"])?,
                    pine_value_from_json(&object["price"])?,
                )))
            } else {
                Err("unsupported object Pine value".to_owned())
            }
        }
    }
}

fn parse_object(json: &str, what: &str) -> Result<Map<String, Value>, String> {
    let value: Value =
        serde_json::from_str(json).map_err(|err| format!("{what} must be JSON: {err}"))?;
    match value {
        Value::Object(object) => Ok(object),
        _ => Err(format!("{what} must be a JSON object")),
    }
}

fn as_object<'a>(value: &'a Value, what: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{what} must be an object"))
}

fn as_array<'a>(value: &'a Value, what: &str) -> Result<&'a Vec<Value>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("{what} must be a list"))
}

fn optional_array<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a [Value]>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => Ok(Some(as_array(value, key)?.as_slice())),
    }
}

fn required_object<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a Map<String, Value>, String> {
    as_object(
        object.get(key).ok_or_else(|| format!("missing `{key}`"))?,
        key,
    )
}

fn required_str(object: &Map<String, Value>, key: &str) -> Result<String, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("missing or invalid `{key}`"))
}

fn optional_str(object: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(|value| Some(value.to_owned()))
            .ok_or_else(|| format!("`{key}` must be a string")),
    }
}

fn required_u32(object: &Map<String, Value>, key: &str) -> Result<u32, String> {
    optional_u32(object, key)?.ok_or_else(|| format!("missing `{key}`"))
}

fn optional_u32(object: &Map<String, Value>, key: &str) -> Result<Option<u32>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be an integer")),
    }
}

fn required_u64(object: &Map<String, Value>, key: &str) -> Result<u64, String> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("missing `{key}`"))
}

fn required_usize(object: &Map<String, Value>, key: &str) -> Result<usize, String> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| format!("missing `{key}`"))
}

fn required_i64(object: &Map<String, Value>, key: &str) -> Result<i64, String> {
    optional_i64(object, key)?.ok_or_else(|| format!("missing `{key}`"))
}

fn optional_i64(object: &Map<String, Value>, key: &str) -> Result<Option<i64>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be an integer")),
    }
}

fn optional_bool(object: &Map<String, Value>, key: &str) -> Result<Option<bool>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be a bool")),
    }
}

fn required_f64(object: &Map<String, Value>, key: &str) -> Result<f64, String> {
    optional_f64(object, key)?.ok_or_else(|| format!("missing `{key}`"))
}

fn optional_f64(object: &Map<String, Value>, key: &str) -> Result<Option<f64>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(value) => json_f64(value, key).map(Some),
    }
}

fn json_f64(value: &Value, key: &str) -> Result<f64, String> {
    if value.is_null() {
        return Ok(f64::NAN);
    }
    value
        .as_f64()
        .ok_or_else(|| format!("`{key}` must be a number"))
}

fn required_pine_values(object: &Map<String, Value>, key: &str) -> Result<Vec<PineValue>, String> {
    optional_pine_values(object, key)?.ok_or_else(|| format!("missing `{key}`"))
}

fn optional_pine_values(
    object: &Map<String, Value>,
    key: &str,
) -> Result<Option<Vec<PineValue>>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let values = as_array(value, key)?;
            let mut items = Vec::with_capacity(values.len());
            for value in values {
                items.push(pine_value_from_json(value)?);
            }
            Ok(Some(items))
        }
    }
}

fn optional_pine(
    object: &Map<String, Value>,
    key: &str,
    default: PineValue,
) -> Result<PineValue, String> {
    match object.get(key) {
        None => Ok(default),
        Some(value) => pine_value_from_json(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::public_runtime_changes_json;
    use crate::public_runtime_result_json;

    #[test]
    fn result_json_roundtrips_plots_and_alerts() {
        let mut result = RuntimeResult {
            plots: vec![PlotSeries::new(
                1,
                vec![PineValue::Int(10), PineValue::Int(12)],
            )],
            alerts: vec![AlertEvent {
                id: 1,
                bar_index: 1,
                time: 120_000,
                message: "high".to_owned(),
                source: "alert()".to_owned(),
            }],
            ..RuntimeResult::default()
        };
        result.plots[0].metadata.title = PineValue::String("close".to_owned());
        let json = public_runtime_result_json(&result);
        let parsed = runtime_result_from_json(&json).expect("parse result");
        assert_eq!(public_runtime_result_json(&parsed), json);
        assert_eq!(parsed.plots[0].values, result.plots[0].values);
        assert_eq!(parsed.alerts, result.alerts);
    }

    #[test]
    fn changes_json_roundtrips_series_and_alert_add() {
        let mut changes = RuntimeChanges::new(2, StreamingVisibility::Confirmed);
        changes.retained_from = 0;
        changes.series.push(SeriesChange {
            family: SeriesFamily::Plot,
            id: 1,
            op: SeriesChangeOp::Append,
            start: 1,
            fields: SeriesFields {
                values: vec![PineValue::Int(12)],
                ..SeriesFields::default()
            },
            header: Some(SeriesHeader {
                metadata: OutputMetadata {
                    title: PineValue::String("live".to_owned()),
                    ..OutputMetadata::default()
                },
                linewidth: PineValue::Int(2),
                ..SeriesHeader::default()
            }),
        });
        changes.alerts.push(EventChange {
            action: EventAction::Add,
            event: AlertEvent {
                id: 1,
                bar_index: 1,
                time: 1,
                message: "high".to_owned(),
                source: "alert()".to_owned(),
            },
        });
        let json = public_runtime_changes_json(&changes);
        let parsed = runtime_changes_from_json(&json).expect("parse changes");
        assert_eq!(public_runtime_changes_json(&parsed), json);
        assert_eq!(parsed, changes);
    }
}
