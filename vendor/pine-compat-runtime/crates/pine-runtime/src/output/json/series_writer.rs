//! Sink encoding of history fields without a complete per-series buffer.
use super::*;
use std::io::{self, Write};

fn values<W: Write + ?Sized>(
    output: &mut W,
    name: &str,
    values: &HistoryView<'_, PineValue>,
) -> io::Result<()> {
    write!(output, ",\"{name}\":[")?;
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            output.write_all(b",")?;
        }
        value_writer::value(value, output)?;
    }
    output.write_all(b"]")
}

fn metadata<W: Write + ?Sized>(
    output: &mut W,
    metadata: &super::super::model::OutputMetadata,
) -> io::Result<()> {
    metadata_writer::output_metadata(output, metadata)?;
    output.write_all(b"}")
}

pub(super) fn plots<W: Write + ?Sized>(
    plot: &PlotSeriesView<'_>,
    output: &mut W,
) -> io::Result<()> {
    write!(output, "{{\"id\":{}", plot.id)?;
    values(output, "values", &plot.values)?;
    if plot.colors.iter().any(|value| *value != PineValue::Na) {
        values(output, "colors", &plot.colors)?;
    }
    metadata_writer::plot_fields(plot, output)?;
    metadata(output, plot.metadata)
}

macro_rules! series {
    ($name:ident, $kind:ident, $( $field:literal => $member:ident ),+ ) => {
        pub(super) fn $name<W: Write + ?Sized>(item: &$kind<'_>, output: &mut W) -> io::Result<()> {
            write!(output, "{{\"id\":{}", item.id)?;
            $(values(output, $field, &item.$member)?;)+
            metadata(output, item.metadata)
        }
    };
}
series!(colors, ColorSeriesView, "values" => values);
series!(chars, PlotCharSeriesView, "values" => values, "chars" => chars, "colors" => colors,
    "locations" => locations, "texts" => texts, "textColors" => text_colors, "sizes" => sizes);
series!(shapes, PlotShapeSeriesView, "values" => values, "styles" => styles, "locations" => locations,
    "colors" => colors, "texts" => texts, "textColors" => text_colors, "sizes" => sizes);
series!(arrows, PlotArrowSeriesView, "values" => values, "colorUps" => color_ups, "colorDowns" => color_downs,
    "minHeights" => min_heights, "maxHeights" => max_heights);
series!(bars, PlotBarSeriesView, "opens" => opens, "highs" => highs, "lows" => lows, "closes" => closes, "colors" => colors);
series!(candles, PlotCandleSeriesView, "opens" => opens, "highs" => highs, "lows" => lows, "closes" => closes,
    "colors" => colors, "wickColors" => wick_colors, "borderColors" => border_colors);

pub(super) fn fills<W: Write + ?Sized>(
    fill: &FillOutputView<'_>,
    output: &mut W,
) -> io::Result<()> {
    write!(
        output,
        "{{\"id\":{},\"firstId\":{},\"secondId\":{},\"firstIsHLine\":{},\"secondIsHLine\":{}",
        fill.id, fill.first_id, fill.second_id, fill.first_is_hline, fill.second_is_hline
    )?;
    values(output, "colors", &fill.colors)?;
    if let Some(samples) = &fill.gradient {
        output.write_all(b",\"gradient\":[")?;
        for (index, sample) in samples.iter().enumerate() {
            if index > 0 {
                output.write_all(b",")?;
            }
            gradient_sample(sample, output)?;
        }
        output.write_all(b"]")?;
    }
    use metadata_writer::DefaultValue::{Bool, Na, Str};
    metadata_writer::non_default(output, "title", fill.title, Str(""))?;
    metadata_writer::non_default(output, "editable", fill.editable, Bool(true))?;
    metadata_writer::non_default(output, "showLast", fill.show_last, Na)?;
    metadata_writer::non_default(output, "fillGaps", fill.fill_gaps, Bool(true))?;
    metadata_writer::non_default(output, "display", fill.display, Str("display.all"))?;
    output.write_all(b"}")
}

pub(super) fn gradient_sample<W: Write + ?Sized>(
    sample: &crate::FillGradientSample,
    output: &mut W,
) -> io::Result<()> {
    // Gradient floats retain serde_json's wire spelling (including `.0`),
    // rather than the PineValue Display spelling used by other histories.
    serde_json::to_writer(output, sample).map_err(io::Error::from)
}

pub(super) fn hlines<W: Write + ?Sized>(item: &HLineOutput, output: &mut W) -> io::Result<()> {
    use metadata_writer::DefaultValue::{Bool, Color, Int, Str};
    write!(output, "{{\"id\":{},\"price\":", item.id)?;
    value_writer::value(&item.price, output)?;
    metadata_writer::non_default(output, "title", &item.title, Str(""))?;
    metadata_writer::non_default(output, "color", &item.color, Color(0x787B86))?;
    metadata_writer::non_default(output, "style", &item.style, Str("hline.style_solid"))?;
    metadata_writer::non_default(output, "linewidth", &item.linewidth, Int(1))?;
    metadata_writer::non_default(output, "editable", &item.editable, Bool(true))?;
    metadata_writer::non_default(output, "display", &item.display, Str("display.all"))?;
    output.write_all(b"}")
}
