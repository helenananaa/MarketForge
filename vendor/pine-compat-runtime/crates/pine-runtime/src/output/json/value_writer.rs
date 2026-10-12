//! Shared scalar encoding without a temporary String for each history value.
use crate::PineValue;
use std::io::{self, Write};

pub(super) fn value<W: Write + ?Sized>(value: &PineValue, output: &mut W) -> io::Result<()> {
    match value {
        PineValue::Int(value) => write!(output, "{value}"),
        PineValue::Float(value) => super::float_writer::float(*value, output),
        PineValue::Bool(value) => output.write_all(if *value { b"true" } else { b"false" }),
        PineValue::String(value) => {
            output.write_all(b"\"")?;
            escaped(value, output)?;
            output.write_all(b"\"")
        }
        PineValue::Color(value) => write!(output, "{value}"),
        PineValue::Plot(value)
        | PineValue::HLine(value)
        | PineValue::Label(value)
        | PineValue::Line(value)
        | PineValue::LineFill(value)
        | PineValue::Polyline(value)
        | PineValue::Box(value)
        | PineValue::Table(value) => write!(output, "{value}"),
        PineValue::ChartPoint(point) => {
            output.write_all(b"{\"time\":")?;
            self::value(&point.time, output)?;
            output.write_all(b",\"index\":")?;
            self::value(&point.index, output)?;
            output.write_all(b",\"price\":")?;
            self::value(&point.price, output)?;
            output.write_all(b"}")
        }
        PineValue::UserType(values) | PineValue::Tuple(values) => {
            output.write_all(b"[")?;
            for (index, item) in values.iter().enumerate() {
                if index > 0 {
                    output.write_all(b",")?;
                }
                self::value(item, output)?;
            }
            output.write_all(b"]")
        }
        PineValue::Array(_)
        | PineValue::UserTypeRef(_)
        | PineValue::Matrix(_)
        | PineValue::Map(_)
        | PineValue::Na
        | PineValue::Void => output.write_all(b"null"),
    }
}

pub(super) fn escaped<W: Write + ?Sized>(value: &str, output: &mut W) -> io::Result<()> {
    let mut start = 0;
    for (index, byte) in value.bytes().enumerate() {
        if byte >= 0x20 && byte != b'"' && byte != b'\\' {
            continue;
        }
        output.write_all(&value.as_bytes()[start..index])?;
        match byte {
            b'"' => output.write_all(b"\\\"")?,
            b'\\' => output.write_all(b"\\\\")?,
            b'\n' => output.write_all(b"\\n")?,
            b'\r' => output.write_all(b"\\r")?,
            b'\t' => output.write_all(b"\\t")?,
            0x08 => output.write_all(b"\\b")?,
            0x0c => output.write_all(b"\\f")?,
            byte => write!(output, "\\u{byte:04x}")?,
        }
        start = index + 1;
    }
    output.write_all(&value.as_bytes()[start..])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sink_values_preserve_wire_numbers_and_compound_values() {
        for (input, expected) in [
            (PineValue::Int(i64::MIN), "-9223372036854775808"),
            (PineValue::Float(-0.0), "-0"),
            (PineValue::Float(f64::NAN), "null"),
            (PineValue::Float(f64::INFINITY), "null"),
            (
                PineValue::Tuple(vec![
                    PineValue::Int(1),
                    PineValue::Bool(false),
                    PineValue::Na,
                ]),
                "[1,false,null]",
            ),
            (
                PineValue::ChartPoint(crate::ChartPointValue::new(
                    PineValue::Int(2),
                    PineValue::Na,
                    PineValue::Float(3.5),
                )),
                "{\"time\":2,\"index\":null,\"price\":3.5}",
            ),
        ] {
            let mut output = Vec::new();
            value(&input, &mut output).unwrap();
            assert_eq!(output, expected.as_bytes());
        }
    }
    #[test]
    fn escaping_preserves_unicode_and_all_ascii_controls() {
        let text = format!(
            "你好🙂\"\\{}",
            (0..32)
                .map(|value| char::from_u32(value).unwrap())
                .collect::<String>()
        );
        let mut output = Vec::new();
        value(&PineValue::String(text.clone()), &mut output).unwrap();
        assert_eq!(output, serde_json::to_string(&text).unwrap().as_bytes());
    }
}
