//! Incremental output encoded with the same field rules as complete snapshots.
use super::*;
use crate::output::changes::{DrawingChange, EventChange, FillChange, HLineChange, ListSplice};
use std::io::{self, Write};

/// Stream the public changes schema to a caller-owned sink.
///
/// Changes and their nested histories are borrowed for the duration of encoding.
/// I/O failures are returned immediately; a failed sink may contain a JSON prefix.
pub fn write_public_runtime_changes_json<W: Write + ?Sized>(
    changes: &RuntimeChanges,
    output: &mut W,
) -> io::Result<()> {
    write!(
        output,
        "{{\"schemaVersion\":{},\"revision\":{},\"baseRevision\":{},\"retainedFrom\":{},\"visibility\":\"{}\",\"series\":",
        changes.schema_version,
        changes.revision,
        changes.base_revision,
        changes.retained_from,
        changes.visibility.as_str()
    )?;
    writer::write_series_array(output, &changes.series, series)?;
    output.write_all(b",\"hlines\":")?;
    writer::write_series_array(output, &changes.hlines, hline)?;
    output.write_all(b",\"fills\":")?;
    writer::write_series_array(output, &changes.fills, fill)?;
    output.write_all(b",\"drawings\":")?;
    writer::write_series_array(output, &changes.drawings, drawing)?;
    output.write_all(b",\"alerts\":")?;
    writer::write_series_array(output, &changes.alerts, event)?;
    if let Some(changes) = &changes.strategy {
        output.write_all(b",\"strategy\":")?;
        strategy(changes, output)?;
    }
    output.write_all(b",\"diagnostics\":")?;
    writer::write_series_array(output, &changes.diagnostics, records_writer::diagnostics)?;
    output.write_all(b"}")
}

/// Serialize changes in one pass, growing the output allocation as needed.
pub fn public_runtime_changes_json(changes: &RuntimeChanges) -> String {
    let mut output = Vec::new();
    write_public_runtime_changes_json(changes, &mut output).expect("writing to a Vec cannot fail");
    String::from_utf8(output).expect("JSON encoders produce UTF-8")
}

fn optional_values<W: Write + ?Sized>(
    name: &str,
    values: &[PineValue],
    output: &mut W,
) -> io::Result<()> {
    if values.is_empty() {
        return Ok(());
    }
    values_field(name, values, output)
}

fn values_field<W: Write + ?Sized>(
    name: &str,
    values: &[PineValue],
    output: &mut W,
) -> io::Result<()> {
    write!(output, ",\"{name}\":")?;
    writer::write_series_array(output, values, value_writer::value)
}

fn series<W: Write + ?Sized>(change: &SeriesChange, output: &mut W) -> io::Result<()> {
    write!(
        output,
        "{{\"family\":\"{}\",\"id\":{},\"op\":\"{}\",\"start\":{}",
        change.family.as_str(),
        change.id,
        change.op.as_str(),
        change.start
    )?;
    macro_rules! field {
        ($name:literal, $member:ident) => {
            optional_values($name, &change.fields.$member, output)?;
        };
    }
    field!("values", values);
    field!("colors", colors);
    field!("chars", chars);
    field!("locations", locations);
    field!("texts", texts);
    field!("textColors", text_colors);
    field!("sizes", sizes);
    field!("styles", styles);
    field!("colorUps", color_ups);
    field!("colorDowns", color_downs);
    field!("minHeights", min_heights);
    field!("maxHeights", max_heights);
    field!("opens", opens);
    field!("highs", highs);
    field!("lows", lows);
    field!("closes", closes);
    field!("wickColors", wick_colors);
    field!("borderColors", border_colors);
    if let Some(header) = &change.header {
        output.write_all(b",\"header\":")?;
        metadata_writer::series_header(header, output)?;
    }
    output.write_all(b"}")
}

fn hline<W: Write + ?Sized>(change: &HLineChange, output: &mut W) -> io::Result<()> {
    let action = match change.action {
        HLineAction::Add(_) => "add",
        HLineAction::Replace(_) => "replace",
        HLineAction::Delete => "delete",
    };
    write!(output, "{{\"id\":{},\"action\":\"{action}\"", change.id)?;
    if let HLineAction::Add(item) | HLineAction::Replace(item) = &change.action {
        output.write_all(b",\"object\":")?;
        series_writer::hlines(item, output)?;
    }
    output.write_all(b"}")
}

fn fill<W: Write + ?Sized>(change: &FillChange, output: &mut W) -> io::Result<()> {
    let action = match change.action {
        FillAction::Add(_) => "add",
        FillAction::Delete => "delete",
        FillAction::SetColors { .. } => "setColors",
        FillAction::SetGradient { .. } => "setGradient",
    };
    write!(output, "{{\"id\":{},\"action\":\"{action}\"", change.id)?;
    match &change.action {
        FillAction::Add(item) => {
            output.write_all(b",\"object\":")?;
            series_writer::fills(&item.into(), output)?;
        }
        FillAction::Delete => {}
        FillAction::SetColors { start, values } => {
            write!(output, ",\"start\":{start}")?;
            values_field("values", values, output)?;
        }
        FillAction::SetGradient { start, values } => {
            write!(output, ",\"start\":{start},\"values\":")?;
            writer::write_series_array(output, values, series_writer::gradient_sample)?;
        }
    }
    output.write_all(b"}")
}

fn drawing<W: Write + ?Sized>(change: &DrawingChange, output: &mut W) -> io::Result<()> {
    write!(
        output,
        "{{\"family\":\"{}\",\"id\":{},\"action\":\"{}\"",
        change.family.as_str(),
        change.id,
        change.action.as_str()
    )?;
    match &change.action {
        DrawingAction::Add(object) => {
            output.write_all(b",\"object\":")?;
            drawing_object(object, output)?;
        }
        DrawingAction::SetTail { start, object } => {
            write!(output, ",\"start\":{start},\"object\":")?;
            drawing_object(object, output)?;
        }
        DrawingAction::Delete => {}
    }
    output.write_all(b"}")
}

fn drawing_object<W: Write + ?Sized>(object: &DrawingObject, output: &mut W) -> io::Result<()> {
    match object {
        DrawingObject::Label(item) => drawings_writer::labels(&item.into(), output),
        DrawingObject::Line(item) => drawings_writer::lines(&item.into(), output),
        DrawingObject::LineFill(item) => drawings_writer::line_fills(&item.into(), output),
        DrawingObject::Polyline(item) => drawings_writer::polylines(&item.into(), output),
        DrawingObject::Box(item) => drawings_writer::boxes(&item.into(), output),
        DrawingObject::Table(item) => drawings_writer::tables(&item.as_ref().into(), output),
    }
}

fn event<W: Write + ?Sized>(change: &EventChange<AlertEvent>, output: &mut W) -> io::Result<()> {
    write!(
        output,
        "{{\"action\":\"{}\",\"event\":",
        change.action.as_str()
    )?;
    records_writer::alerts(&change.event, output)?;
    output.write_all(b"}")
}

fn strategy<W: Write + ?Sized>(changes: &StrategyChanges, output: &mut W) -> io::Result<()> {
    output.write_all(b"{")?;
    let mut first = true;
    splice(
        "orders",
        &changes.orders,
        &mut first,
        output,
        records_writer::orders,
    )?;
    splice(
        "trades",
        &changes.trades,
        &mut first,
        output,
        records_writer::trades,
    )?;
    splice(
        "alerts",
        &changes.alerts,
        &mut first,
        output,
        records_writer::order_fill_alerts,
    )?;
    splice(
        "position",
        &changes.position,
        &mut first,
        output,
        records_writer::position,
    )?;
    splice(
        "equity",
        &changes.equity,
        &mut first,
        output,
        records_writer::equity,
    )?;
    if let Some(items) = &changes.diagnostics {
        if !first {
            output.write_all(b",")?;
        }
        output.write_all(b"\"diagnostics\":")?;
        writer::write_series_array(output, items, records_writer::diagnostics)?;
    }
    output.write_all(b"}")
}

fn splice<W: Write + ?Sized, T>(
    name: &str,
    splice: &Option<ListSplice<T>>,
    first: &mut bool,
    output: &mut W,
    encode: impl Fn(&T, &mut W) -> io::Result<()>,
) -> io::Result<()> {
    let Some(splice) = splice else {
        return Ok(());
    };
    if !*first {
        output.write_all(b",")?;
    }
    *first = false;
    write!(output, "\"{name}\":{{\"start\":{},\"items\":", splice.start)?;
    writer::write_series_array(output, &splice.items, encode)?;
    output.write_all(b"}")
}
