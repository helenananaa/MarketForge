//! Python's canonical JSON identities are part of persisted broker schema 2.
use super::{error, io};
use exchange_core::BotError;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn digest(value: &Value) -> Result<String, BotError> {
    Ok(format!(
        "{:x}",
        Sha256::digest(python_json(value, false)?.as_bytes())
    ))
}

/// The wire bound counts Python ensure_ascii output, including optional spaces.
/// Counting avoids allocating a second complete copy of the persisted history.
pub fn python_json_len(value: &Value, spaced: bool) -> Result<usize, BotError> {
    fn string_len(s: &str) -> usize {
        2 + s
            .chars()
            .map(|c| match c {
                '"' | '\\' | '\u{8}' | '\u{c}' | '\n' | '\r' | '\t' => 2,
                c if c < ' ' => 6,
                c if c.is_ascii() => 1,
                c => 6 * c.len_utf16(),
            })
            .sum::<usize>()
    }
    let separator = if spaced { 2 } else { 1 };
    Ok(match value {
        Value::Null => 4,
        Value::Bool(b) => {
            if *b {
                4
            } else {
                5
            }
        }
        Value::String(s) => string_len(s),
        Value::Number(n) => {
            if let Some(number) = n.as_u64() {
                number.checked_ilog10().unwrap_or(0) as usize + 1
            } else if let Some(number) = n.as_i64() {
                // unsigned_abs also handles i64::MIN without overflow.
                number.unsigned_abs().checked_ilog10().unwrap_or(0) as usize + 2
            } else {
                python_float(&n.to_string())?.len()
            }
        }
        Value::Array(a) => {
            let mut len = 2 + separator * a.len().saturating_sub(1);
            for value in a {
                len += python_json_len(value, spaced)?;
            }
            len
        }
        Value::Object(o) => {
            let mut len = 2 + separator * o.len().saturating_sub(1);
            for (key, value) in o {
                len += string_len(key) + separator + python_json_len(value, spaced)?;
            }
            len
        }
    })
}

pub fn encoded_len(value: &impl serde::Serialize) -> Result<usize, BotError> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value).map_err(io)?;
    Ok(counter.0)
}

fn string(value: &str, out: &mut String) -> Result<(), BotError> {
    // Preserve JSON escapes and encode non-ASCII exactly as ensure_ascii=True.
    for ch in serde_json::to_string(value).map_err(io)?.chars() {
        if ch.is_ascii() {
            out.push(ch);
        } else {
            for unit in ch.encode_utf16(&mut [0; 2]) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    Ok(())
}

pub fn python_json(value: &Value, spaced: bool) -> Result<String, BotError> {
    fn write(v: &Value, spaced: bool, out: &mut String) -> Result<(), BotError> {
        let comma = if spaced { ", " } else { "," };
        let colon = if spaced { ": " } else { ":" };
        match v {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::String(s) => string(s, out)?,
            Value::Number(n) => {
                let mut number = n.to_string();
                if n.is_f64() {
                    let f = n.as_f64().ok_or_else(|| error("non-finite Pine state"))?;
                    if !f.is_finite() {
                        return Err(error("non-finite Pine state"));
                    }
                    number = python_float(&number)?;
                }
                out.push_str(&number);
            }
            Value::Array(a) => {
                out.push('[');
                for (index, value) in a.iter().enumerate() {
                    if index > 0 {
                        out.push_str(comma);
                    }
                    write(value, spaced, out)?;
                }
                out.push(']');
            }
            Value::Object(o) => {
                out.push('{');
                let mut entries: Vec<_> = o.iter().collect();
                entries.sort_by(|a, b| a.0.cmp(b.0));
                for (index, (key, value)) in entries.into_iter().enumerate() {
                    if index > 0 {
                        out.push_str(comma);
                    }
                    string(key, out)?;
                    out.push_str(colon);
                    write(value, spaced, out)?;
                }
                out.push('}');
            }
        }
        Ok(())
    }
    let mut out = String::new();
    write(value, spaced, &mut out)?;
    Ok(out)
}

// Reformat shortest round-trip digits without performing another rounded
// floating-point conversion. Python switches to scientific notation at 1e16
// and below 1e-4; Ryu uses different formatting thresholds.
fn python_float(number: &str) -> Result<String, BotError> {
    let (sign, number) = number.strip_prefix('-').map_or(("", number), |v| ("-", v));
    let (mantissa, power) = match number.split_once('e') {
        Some((m, e)) => (m, e.parse::<i32>().map_err(io)?),
        None => (number, 0),
    };
    let point = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let zeros = digits.bytes().take_while(|c| *c == b'0').count();
    let digits = digits[zeros..].trim_end_matches('0');
    if digits.is_empty() {
        return Ok(format!("{sign}0.0"));
    }
    let exponent = point + power - zeros as i32 - 1;
    if !(-4..16).contains(&exponent) {
        let fraction = if digits.len() > 1 {
            format!(".{}", &digits[1..])
        } else {
            String::new()
        };
        return Ok(format!(
            "{sign}{}{fraction}e{}{abs:02}",
            &digits[..1],
            if exponent < 0 { "-" } else { "+" },
            abs = exponent.unsigned_abs()
        ));
    }
    let point = exponent + 1;
    if point <= 0 {
        Ok(format!("{sign}0.{}{digits}", "0".repeat((-point) as usize)))
    } else if point as usize >= digits.len() {
        Ok(format!(
            "{sign}{digits}{}.0",
            "0".repeat(point as usize - digits.len())
        ))
    } else {
        Ok(format!(
            "{sign}{}.{}",
            &digits[..point as usize],
            &digits[point as usize..]
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_persisted_identity_and_ascii_bound() {
        let value = serde_json::json!({"unicode":"策略🐱", "qty":2.0, "zero":-0.0});
        assert_eq!(
            python_json(&value, false).unwrap(),
            "{\"qty\":2.0,\"unicode\":\"\\u7b56\\u7565\\ud83d\\udc31\",\"zero\":-0.0}"
        );
        assert_eq!(
            python_json(&serde_json::json!([1e-7, 1e20]), false).unwrap(),
            "[1e-07,1e+20]"
        );
    }
    #[test]
    fn counted_bounds_equal_full_encodings() {
        let value = serde_json::json!({"控制\u{0}🐱": [null, true, false, "\"\\\n\t\r\u{8}\u{c}\u{1f}策略", -0.0, 1e-7, 1e20, u64::MAX]});
        for spaced in [false, true] {
            assert_eq!(
                python_json_len(&value, spaced).unwrap(),
                python_json(&value, spaced).unwrap().len()
            );
        }
        assert_eq!(
            encoded_len(&value).unwrap(),
            serde_json::to_vec(&value).unwrap().len()
        );
    }
    #[test]
    fn integer_lengths_match_wire_at_every_decimal_boundary() {
        let mut values = vec![serde_json::json!(i64::MIN), serde_json::json!(u64::MAX)];
        for power in 0..20 {
            let center = 10u64.pow(power);
            for number in [center - 1, center, center + 1] {
                values.push(serde_json::json!(number));
                if let Ok(signed) = i64::try_from(number) {
                    values.push(serde_json::json!(-signed));
                }
            }
        }
        values.extend([
            serde_json::json!(0.0),
            serde_json::json!(-0.0),
            serde_json::json!(1.0),
        ]);
        for value in values {
            for spaced in [false, true] {
                assert_eq!(
                    python_json_len(&value, spaced).unwrap(),
                    python_json(&value, spaced).unwrap().len(),
                    "{value}"
                );
            }
        }
    }
    #[test]
    fn python_float_formatting_boundaries_preserve_shortest_digits() {
        for (value, expected) in [
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.234567890123456e-5, "1.234567890123456e-05"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (f64::from_bits(1), "5e-324"),
            (-0.0, "-0.0"),
        ] {
            assert_eq!(
                python_json(&serde_json::json!(value), false).unwrap(),
                expected
            );
        }
    }
}
