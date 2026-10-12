//! Drawing records written directly to the sink, including large cell and point
//! collections. Field order is shared by full output and delta output.
use super::*;
use crate::output::drawings::{
    BoxSnapshot, LabelSnapshot, LineFillSnapshot, LineSnapshot, PolylineSnapshot,
};
use std::io::{self, Write};

macro_rules! fields {
    ($output:expr, $item:expr; $($name:literal => $field:ident),+ $(,)?) => {
        $(
            $output.write_all(concat!(",\"", $name, "\":").as_bytes())?;
            value_writer::value(&$item.$field, $output)?;
        )+
    };
}

fn snapshots<'a, W: Write + ?Sized, T: 'a>(
    history: HistoryView<'a, T>,
    output: &mut W,
    write_snapshot: impl Fn(&T, &mut W) -> io::Result<()>,
) -> io::Result<()> {
    for (index, snapshot) in history.iter().enumerate() {
        if index > 0 {
            output.write_all(b",")?;
        }
        write_snapshot(snapshot, output)?;
    }
    output.write_all(b"]}")
}

macro_rules! drawing {
    ($function:ident, $view:ident, $snapshot:ident, |$item:ident, $output:ident| $body:block) => {
        pub(super) fn $function<W: Write + ?Sized>(
            drawing: &$view<'_>,
            output: &mut W,
        ) -> io::Result<()> {
            write!(output, "{{\"id\":{},\"snapshots\":[", drawing.id)?;
            snapshots(drawing.snapshots, output, |$item: &$snapshot, $output| {
                write!($output, "{{\"barIndex\":{},\"exists\":{}", $item.bar_index, $item.exists)?;
                if $item.exists $body
                $output.write_all(b"}")
            })
        }
    };
}

drawing!(labels, LabelOutputView, LabelSnapshot, |item, output| {
    fields!(output, item;
        "x" => x, "y" => y, "text" => text, "xloc" => xloc, "yloc" => yloc,
        "color" => color, "style" => style, "textColor" => text_color,
        "size" => size, "tooltip" => tooltip, "textAlign" => text_align,
        "textFontFamily" => text_font_family, "textFormatting" => text_formatting
    );
});
drawing!(lines, LineOutputView, LineSnapshot, |item, output| {
    fields!(output, item;
        "x1" => x1, "y1" => y1, "x2" => x2, "y2" => y2, "xloc" => xloc,
        "color" => color, "width" => width, "style" => style, "extend" => extend
    );
});
drawing!(
    line_fills,
    LineFillOutputView,
    LineFillSnapshot,
    |item, output| {
        write!(output, ",\"line1\":{},\"line2\":{}", item.line1, item.line2)?;
        fields!(output, item; "color" => color);
    }
);
drawing!(
    polylines,
    PolylineOutputView,
    PolylineSnapshot,
    |item, output| {
        output.write_all(b",\"points\":[")?;
        for (index, point) in item.points.iter().enumerate() {
            if index > 0 {
                output.write_all(b",")?;
            }
            value_writer::value(point, output)?;
        }
        output.write_all(b"]")?;
        fields!(output, item;
            "curved" => curved, "closed" => closed, "xloc" => xloc,
            "lineColor" => line_color, "fillColor" => fill_color,
            "lineStyle" => line_style, "lineWidth" => line_width,
            "forceOverlay" => force_overlay
        );
    }
);
drawing!(boxes, BoxOutputView, BoxSnapshot, |item, output| {
    fields!(output, item;
        "left" => left, "top" => top, "right" => right, "bottom" => bottom,
        "xloc" => xloc, "bgColor" => bg_color, "borderColor" => border_color,
        "borderWidth" => border_width, "borderStyle" => border_style,
        "extend" => extend, "text" => text, "textColor" => text_color,
        "textSize" => text_size, "textHalign" => text_halign,
        "textValign" => text_valign, "textWrap" => text_wrap,
        "textFontFamily" => text_font_family, "textFormatting" => text_formatting
    );
});

pub(super) fn tables<W: Write + ?Sized>(
    table: &TableOutputView<'_>,
    output: &mut W,
) -> io::Result<()> {
    write!(output, "{{\"id\":{},\"position\":", table.id)?;
    value_writer::value(table.position, output)?;
    fields!(output, table;
        "bgColor" => bg_color, "frameColor" => frame_color,
        "frameWidth" => frame_width, "borderColor" => border_color,
        "borderWidth" => border_width
    );
    write!(
        output,
        ",\"columns\":{},\"rows\":{},\"snapshots\":[",
        table.columns, table.rows
    )?;
    snapshots(table.snapshots, output, |snapshot, output| {
        write!(
            output,
            "{{\"barIndex\":{},\"exists\":{}",
            snapshot.bar_index, snapshot.exists
        )?;
        if snapshot.exists {
            output.write_all(b",\"cells\":[")?;
            for (index, cell) in snapshot.cells.iter().enumerate() {
                if index > 0 {
                    output.write_all(b",")?;
                }
                write!(
                    output,
                    "{{\"column\":{},\"row\":{},\"text\":",
                    cell.column, cell.row
                )?;
                value_writer::value(&cell.text, output)?;
                fields!(output, cell;
                    "bgColor" => bg_color, "textColor" => text_color,
                    "width" => width, "height" => height, "textSize" => text_size,
                    "textHalign" => text_halign, "textValign" => text_valign,
                    "textWrap" => text_wrap, "tooltip" => tooltip,
                    "textFontFamily" => text_font_family, "textFormatting" => text_formatting
                );
                output.write_all(b"}")?;
            }
            output.write_all(b"],\"mergedCells\":[")?;
            for (index, cell) in snapshot.merged_cells.iter().enumerate() {
                if index > 0 {
                    output.write_all(b",")?;
                }
                write!(
                    output,
                    "{{\"startColumn\":{},\"startRow\":{},\"endColumn\":{},\"endRow\":{}}}",
                    cell.start_column, cell.start_row, cell.end_column, cell.end_row
                )?;
            }
            output.write_all(b"]")?;
        }
        output.write_all(b"}")
    })
}
