//! Typed default omission shared by complete and incremental output.
use super::{PineValue, PlotSeriesView, SeriesHeader, value_writer};
use crate::OutputMetadata;
use std::io::{self, Write};

/// Borrow string defaults instead of allocating temporary `PineValue::String`s.
pub(super) enum DefaultValue<'a> {
    Int(i64),
    Bool(bool),
    Color(u64),
    Str(&'a str),
    Na,
}

impl DefaultValue<'_> {
    fn matches(&self, value: &PineValue) -> bool {
        match (self, value) {
            (Self::Int(default), PineValue::Int(value)) => default == value,
            (Self::Bool(default), PineValue::Bool(value)) => default == value,
            (Self::Color(default), PineValue::Color(value)) => default == value,
            (Self::Str(default), PineValue::String(value)) => *default == value,
            (Self::Na, PineValue::Na) => true,
            _ => false,
        }
    }
}

pub(super) fn non_default<W: Write + ?Sized>(
    output: &mut W,
    name: &str,
    value: &PineValue,
    default: DefaultValue<'_>,
) -> io::Result<()> {
    if default.matches(value) {
        return Ok(());
    }
    write!(output, ",\"{name}\":")?;
    value_writer::value(value, output)
}

fn object_non_default<W: Write + ?Sized>(
    output: &mut W,
    first: &mut bool,
    name: &str,
    value: &PineValue,
    default: DefaultValue<'_>,
) -> io::Result<()> {
    if default.matches(value) {
        return Ok(());
    }
    if !*first {
        output.write_all(b",")?;
    }
    *first = false;
    write!(output, "\"{name}\":")?;
    value_writer::value(value, output)
}

pub(super) fn output_metadata<W: Write + ?Sized>(
    output: &mut W,
    metadata: &OutputMetadata,
) -> io::Result<()> {
    use DefaultValue::{Bool, Int, Na, Str};
    non_default(output, "title", &metadata.title, Str(""))?;
    non_default(output, "offset", &metadata.offset, Int(0))?;
    non_default(output, "editable", &metadata.editable, Bool(true))?;
    non_default(output, "showLast", &metadata.show_last, Na)?;
    non_default(output, "display", &metadata.display, Str("display.all"))?;
    non_default(output, "forceOverlay", &metadata.force_overlay, Bool(false))
}

pub(super) fn plot_fields<W: Write + ?Sized>(
    plot: &PlotSeriesView<'_>,
    output: &mut W,
) -> io::Result<()> {
    use DefaultValue::{Bool, Int, Na, Str};
    non_default(output, "linewidth", plot.linewidth, Int(1))?;
    non_default(output, "style", plot.style, Str("plot.style_line"))?;
    non_default(output, "trackPrice", plot.track_price, Bool(false))?;
    non_default(output, "histBase", plot.hist_base, Int(0))?;
    non_default(output, "join", plot.join, Bool(false))?;
    non_default(output, "format", plot.format, Str("format.inherit"))?;
    non_default(output, "precision", plot.precision, Na)?;
    non_default(
        output,
        "linestyle",
        plot.linestyle,
        Str("plot.linestyle_solid"),
    )
}

pub(super) fn series_header<W: Write + ?Sized>(
    header: &SeriesHeader,
    output: &mut W,
) -> io::Result<()> {
    use DefaultValue::{Bool, Int, Na, Str};
    output.write_all(b"{")?;
    let mut first = true;
    macro_rules! field {
        ($name:literal, $value:expr, $default:expr) => {
            object_non_default(output, &mut first, $name, $value, $default)?;
        };
    }
    field!("title", &header.metadata.title, Str(""));
    field!("offset", &header.metadata.offset, Int(0));
    field!("editable", &header.metadata.editable, Bool(true));
    field!("showLast", &header.metadata.show_last, Na);
    field!("display", &header.metadata.display, Str("display.all"));
    field!("forceOverlay", &header.metadata.force_overlay, Bool(false));
    field!("linewidth", &header.linewidth, Int(1));
    field!("style", &header.style, Str("plot.style_line"));
    field!("trackPrice", &header.track_price, Bool(false));
    field!("histBase", &header.hist_base, Int(0));
    field!("join", &header.join, Bool(false));
    field!("format", &header.format, Str("format.inherit"));
    field!("precision", &header.precision, Na);
    field!("linestyle", &header.linestyle, Str("plot.linestyle_solid"));
    output.write_all(b"}")
}
