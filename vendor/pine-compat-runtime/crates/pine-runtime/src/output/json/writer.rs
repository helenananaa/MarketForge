use std::borrow::Borrow;
use std::io::{self, Write};

use super::*;
use crate::output::model::{
    ColorSeries, FillOutput, PlotArrowSeries, PlotBarSeries, PlotCandleSeries, PlotCharSeries,
    PlotSeries, PlotShapeSeries,
};

/// Write the public result schema to a caller-owned sink, propagating I/O errors.
///
/// This avoids allocating the complete serialized result. History fields are
/// emitted one value at a time, including drawing snapshots and strategy records.
/// Callers can buffer their sink and choose its storage and lifecycle independently.
pub fn write_public_runtime_result_json<W: Write + ?Sized>(
    result: &RuntimeResult,
    output: &mut W,
) -> io::Result<()> {
    write_public_runtime_result_view_json(&result.view(), output)
}

/// Stream borrowed runtime output directly from persistent histories.
pub fn write_public_runtime_result_view_json<W: Write + ?Sized>(
    result: &crate::RuntimeResultView<'_>,
    output: &mut W,
) -> io::Result<()> {
    write!(
        output,
        "{{\"schemaVersion\":{},\"renderMetadataVersion\":{}",
        PUBLIC_RUNTIME_SCHEMA_VERSION, PUBLIC_RENDER_METADATA_VERSION
    )?;
    macro_rules! history {
        ($name:literal, $values:expr, $serializer:ident) => {
            output.write_all(concat!(",\"", $name, "\":").as_bytes())?;
            write_series_array(output, $values, series_writer::$serializer)?;
        };
    }
    macro_rules! drawing {
        ($name:literal, $values:expr, $serializer:ident) => {
            output.write_all(concat!(",\"", $name, "\":").as_bytes())?;
            write_series_array(output, $values, drawings_writer::$serializer)?;
        };
    }
    macro_rules! record {
        ($name:literal, $values:expr, $encoder:ident) => {
            output.write_all(concat!(",\"", $name, "\":").as_bytes())?;
            write_series_array(output, $values, records_writer::$encoder)?;
        };
    }
    history!("plots", result.plots.iter(), plots);
    history!("plotChars", result.plot_chars.iter(), chars);
    history!("plotShapes", result.plot_shapes.iter(), shapes);
    history!("plotArrows", result.plot_arrows.iter(), arrows);
    history!("plotBars", result.plot_bars.iter(), bars);
    history!("plotCandles", result.plot_candles.iter(), candles);
    history!("bgColors", result.bg_colors.iter(), colors);
    history!("barColors", result.bar_colors.iter(), colors);
    output.write_all(b",\"hlines\":")?;
    write_series_array(output, result.hlines, series_writer::hlines)?;
    history!("fills", result.fills.iter(), fills);
    drawing!("labels", result.labels.iter(), labels);
    drawing!("lines", result.lines.iter(), lines);
    drawing!("lineFills", result.line_fills.iter(), line_fills);
    drawing!("polylines", result.polylines.iter(), polylines);
    drawing!("boxes", result.boxes.iter(), boxes);
    drawing!("tables", result.tables.iter(), tables);
    record!("alerts", result.alerts, alerts);
    if let Some(strategy) = &result.strategy {
        output.write_all(b",\"strategy\":{\"orders\":")?;
        write_series_array(output, strategy.orders, records_writer::orders)?;
        record!("trades", strategy.trades, trades);
        record!("position", strategy.position, position);
        record!("equity", strategy.equity, equity);
        record!("alerts", strategy.alerts, order_fill_alerts);
        record!("diagnostics", strategy.diagnostics, diagnostics);
        output.write_all(b"}")?;
    }
    record!("diagnostics", result.diagnostics.as_ref(), diagnostics);
    output.write_all(b"}")
}

/// Serialize borrowed output into one exactly sized output allocation.
pub fn public_runtime_result_view_json(result: &crate::RuntimeResultView<'_>) -> String {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    write_public_runtime_result_view_json(result, &mut counter).expect("counting cannot fail");
    let mut bytes = Vec::with_capacity(counter.0);
    write_public_runtime_result_view_json(result, &mut bytes)
        .expect("writing to a Vec cannot fail");
    String::from_utf8(bytes).expect("JSON serializers produce UTF-8")
}

pub(super) fn write_series_array<W: Write + ?Sized, T, I: IntoIterator>(
    output: &mut W,
    items: I,
    serialize: impl Fn(&T, &mut W) -> io::Result<()>,
) -> io::Result<()>
where
    I::Item: Borrow<T>,
{
    output.write_all(b"[")?;
    for (index, item) in items.into_iter().enumerate() {
        if index > 0 {
            output.write_all(b",")?;
        }
        serialize(item.borrow(), output)?;
    }
    output.write_all(b"]")
}

/// Serialize an owned snapshot, releasing each source series after encoding it.
///
/// Encode into small chunks as source series are released, then allocate the
/// complete String after the source has been dropped. This avoids reserving a
/// result-sized buffer while all source histories are still alive. Field,
/// number and escaping rules match the borrowed serializer.
pub fn into_public_runtime_result_json(result: RuntimeResult) -> String {
    let mut chunks = JsonChunks::default();
    write_owned_result(result, &mut chunks).expect("writing to memory cannot fail");
    let mut bytes = Vec::with_capacity(chunks.len);
    for chunk in chunks.parts {
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).expect("JSON serializers produce UTF-8")
}

#[derive(Default)]
struct JsonChunks {
    parts: Vec<Vec<u8>>,
    len: usize,
}

impl Write for JsonChunks {
    fn write(&mut self, mut bytes: &[u8]) -> io::Result<usize> {
        const CHUNK_BYTES: usize = 65536;
        let size = bytes.len();
        while !bytes.is_empty() {
            if self
                .parts
                .last()
                .is_none_or(|part| part.len() == CHUNK_BYTES)
            {
                self.parts.push(Vec::with_capacity(CHUNK_BYTES));
            }
            let part = self.parts.last_mut().unwrap();
            let count = bytes.len().min(CHUNK_BYTES - part.len());
            part.extend_from_slice(&bytes[..count]);
            bytes = &bytes[count..];
        }
        self.len += size;
        Ok(size)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn write_owned_result<W: Write + ?Sized>(result: RuntimeResult, output: &mut W) -> io::Result<()> {
    write!(
        output,
        "{{\"schemaVersion\":{},\"renderMetadataVersion\":{}",
        PUBLIC_RUNTIME_SCHEMA_VERSION, PUBLIC_RENDER_METADATA_VERSION
    )?;
    macro_rules! history {
        ($name:literal, $values:expr, $serializer:ident, $kind:ty) => {
            output.write_all(concat!(",\"", $name, "\":").as_bytes())?;
            write_series_array(output, $values, |item: &$kind, sink| {
                series_writer::$serializer(&item.into(), sink)
            })?;
        };
    }
    macro_rules! drawing {
        ($name:literal, $values:expr, $serializer:ident, $kind:ty) => {
            output.write_all(concat!(",\"", $name, "\":").as_bytes())?;
            write_series_array(output, $values, |item: &$kind, sink| {
                drawings_writer::$serializer(&item.into(), sink)
            })?;
        };
    }
    macro_rules! record {
        ($name:literal, $values:expr, $encoder:ident) => {
            output.write_all(concat!(",\"", $name, "\":").as_bytes())?;
            write_series_array(output, $values, records_writer::$encoder)?;
        };
    }
    history!("plots", result.plots, plots, PlotSeries);
    history!("plotChars", result.plot_chars, chars, PlotCharSeries);
    history!("plotShapes", result.plot_shapes, shapes, PlotShapeSeries);
    history!("plotArrows", result.plot_arrows, arrows, PlotArrowSeries);
    history!("plotBars", result.plot_bars, bars, PlotBarSeries);
    history!(
        "plotCandles",
        result.plot_candles,
        candles,
        PlotCandleSeries
    );
    history!("bgColors", result.bg_colors, colors, ColorSeries);
    history!("barColors", result.bar_colors, colors, ColorSeries);
    output.write_all(b",\"hlines\":")?;
    write_series_array(output, result.hlines, series_writer::hlines)?;
    history!("fills", result.fills, fills, FillOutput);
    drawing!("labels", result.labels, labels, crate::LabelOutput);
    drawing!("lines", result.lines, lines, crate::LineOutput);
    drawing!(
        "lineFills",
        result.line_fills,
        line_fills,
        crate::LineFillOutput
    );
    drawing!(
        "polylines",
        result.polylines,
        polylines,
        crate::PolylineOutput
    );
    drawing!("boxes", result.boxes, boxes, crate::BoxOutput);
    drawing!("tables", result.tables, tables, crate::TableOutput);
    record!("alerts", result.alerts, alerts);
    if let Some(strategy) = result.strategy {
        output.write_all(b",\"strategy\":{\"orders\":")?;
        write_series_array(output, strategy.orders, records_writer::orders)?;
        record!("trades", strategy.trades, trades);
        record!("position", strategy.position, position);
        record!("equity", strategy.equity, equity);
        record!("alerts", strategy.alerts, order_fill_alerts);
        record!("diagnostics", strategy.diagnostics, diagnostics);
        output.write_all(b"}")?;
    }
    record!("diagnostics", result.diagnostics, diagnostics);
    output.write_all(b"}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_chunks_preserve_multibyte_escaping_and_complete_output() {
        let mut result = super::super::tests::empty_result();
        result.diagnostics.push(crate::RuntimeDiagnostic {
            code: "large".to_owned(),
            message: "浣犲ソ馃檪\"\n\\".repeat(20000),
        });
        let expected = public_runtime_result_json(&result);
        assert!(expected.len() > 4 * 65536);
        assert_eq!(into_public_runtime_result_json(result), expected);
    }

    #[test]
    fn sink_and_consuming_json_match_all_history_families_and_gradients() {
        for name in [
            "plotchar",
            "plotshape",
            "plotarrow",
            "plotbar",
            "plotcandle",
            "color_outputs",
            "gradient_fill",
            "fill_transp",
            "plot_dynamic_style",
            "plot_linestyle",
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/runtime")
                .join(format!("{name}.pine"));
            let text = std::fs::read_to_string(path).unwrap();
            let source = pine_syntax::SourceFile::new(name, text);
            let hir = pine_sema::analyze_source(&source).hir.unwrap();
            let mut runtime = crate::HistoricalRuntime::new(&hir);
            runtime
                .append_bars(
                    &(0..32)
                        .map(|index| crate::Bar {
                            time: index * 60_000,
                            open: 10.,
                            high: 13.,
                            low: 8.,
                            close: 10. + (index % 3) as f64,
                            volume: 10.,
                        })
                        .collect::<Vec<_>>(),
                )
                .unwrap();
            let result = runtime.result();
            assert!(result.diagnostics.is_empty(), "{name}");
            let expected = public_runtime_result_json(&result);
            let mut bytes = Vec::new();
            write_public_runtime_result_json(&result, &mut bytes).unwrap();
            assert_eq!(bytes, expected.as_bytes(), "{name}");
            assert_eq!(into_public_runtime_result_json(result), expected, "{name}");
        }
        let mut result = RuntimeResult::default();
        result.plots.push(PlotSeries::new(
            1,
            vec![
                PineValue::Float(-0.0),
                PineValue::Float(f64::NAN),
                PineValue::Float(f64::INFINITY),
                PineValue::String("浣犲ソ\n\"\\".into()),
                PineValue::Tuple(vec![PineValue::Na]),
            ],
        ));
        let expected = public_runtime_result_json(&result);
        let mut bytes = Vec::new();
        write_public_runtime_result_json(&result, &mut bytes).unwrap();
        assert_eq!(bytes, expected.as_bytes());
        assert_eq!(into_public_runtime_result_json(result), expected);
    }

    #[test]
    fn matches_owned_serializer_with_strategy_drawings_and_escaping() {
        let source = pine_syntax::SourceFile::new(
            "writer.pine",
            "//@version=6\nstrategy(\"writer\")\nplot(close)\nplot(open)\nplotshape(close > open)\nplotshape(close < open)\nbgcolor(color.red)\nlabel.new(bar_index, close, \"浣犲ソ\\n\\\"\")\nstrategy.entry(\"buy\", strategy.long)\n",
        );
        let hir = pine_sema::analyze_source(&source).hir.unwrap();
        let mut runtime = crate::HistoricalRuntime::new(&hir);
        runtime
            .append_bars(&[
                crate::Bar {
                    time: 0,
                    open: 1.0,
                    high: 3.0,
                    low: 0.5,
                    close: 2.0,
                    volume: 10.0,
                },
                crate::Bar {
                    time: 60_000,
                    open: 2.0,
                    high: 4.0,
                    low: 1.0,
                    close: 3.0,
                    volume: 10.0,
                },
            ])
            .unwrap();
        let result = runtime.result();
        assert!(result.diagnostics.is_empty());
        assert!(result.strategy.is_some());
        assert_eq!(result.plots.len(), 2);
        assert_eq!(result.plot_shapes.len(), 2);
        assert_eq!(result.labels.len(), 2);
        let mut bytes = Vec::new();
        write_public_runtime_result_json(&result, &mut bytes).unwrap();
        let expected = public_runtime_result_json(&result);
        assert_eq!(bytes, expected.as_bytes());
        assert_eq!(into_public_runtime_result_json(result), expected);
    }

    #[test]
    fn retries_short_writes_and_propagates_sink_failure() {
        struct Sink {
            bytes: Vec<u8>,
            limit: usize,
        }
        impl Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.bytes.len() >= self.limit {
                    return Err(io::Error::new(io::ErrorKind::StorageFull, "sink full"));
                }
                let count = bytes.len().min(3).min(self.limit - self.bytes.len());
                self.bytes.extend_from_slice(&bytes[..count]);
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let result = super::super::tests::empty_result();
        let expected = public_runtime_result_json(&result);
        let mut sink = Sink {
            bytes: Vec::new(),
            limit: usize::MAX,
        };
        write_public_runtime_result_json(&result, &mut sink).unwrap();
        assert_eq!(sink.bytes, expected.as_bytes());
        for limit in [0, 30, expected.len() - 1] {
            let mut sink = Sink {
                bytes: Vec::new(),
                limit,
            };
            let error = write_public_runtime_result_json(&result, &mut sink).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::StorageFull);
            assert_eq!(sink.bytes, &expected.as_bytes()[..limit]);
        }
    }
}
