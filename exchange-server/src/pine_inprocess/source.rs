//! Source preparation shared by the hash-qualified closed-bar profiles.
//! Admission is still an exact source hash; this is not a general Pine rewriter.
use super::{error, io};
use exchange_core::BotError;

pub fn normalize(source: &str) -> String {
    source
        .strip_prefix('\u{feff}')
        .unwrap_or(source)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

/// Match the process broker's external_pass_source, including nested title
/// expressions and explicit calc_on_order_fills=false. No strategy.order
/// sources are admitted by this profile.
pub fn external_pass(source: &str) -> Result<String, BotError> {
    let mut offset = 0;
    let mut start = None;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if let Some(tail) = trimmed.strip_prefix("strategy") {
            let tail = tail.trim_start();
            if tail.starts_with('(') {
                start = Some(offset + line.len() - tail.len() + 1);
                break;
            }
        }
        offset += line.len();
    }
    let start = start.ok_or_else(|| error("Pine broker requires a strategy declaration"))?;
    let bytes = source.as_bytes();
    let (mut cursor, mut depth, mut quote) = (start, 1, None);
    while cursor < bytes.len() && depth > 0 {
        let c = bytes[cursor];
        if let Some(q) = quote {
            if c == b'\\' {
                cursor += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
        } else if matches!(c, b'\'' | b'"') {
            quote = Some(c);
        } else if c == b'(' {
            depth += 1;
        } else if c == b')' {
            depth -= 1;
        }
        cursor += 1;
    }
    if depth != 0 {
        return Err(error("unterminated strategy declaration"));
    }
    let declaration = &source[start..cursor - 1];
    let declaration = if declaration.contains("calc_on_order_fills") {
        // Only replace an actual named argument, never the title's text.
        // Rejection is safe for unusual declarations outside this profile.
        let mut result = declaration.to_owned();
        let mut found = false;
        let mut i = 0;
        let mut q = None;
        while i < declaration.len() {
            let b = declaration.as_bytes()[i];
            if let Some(quote) = q {
                if b == b'\\' {
                    i += 2;
                    continue;
                }
                if b == quote {
                    q = None;
                }
            } else if matches!(b, b'\'' | b'"') {
                q = Some(b);
            } else if declaration[i..].starts_with("calc_on_order_fills") {
                let after = i + "calc_on_order_fills".len();
                let tail = declaration[after..].trim_start();
                let Some(tail) = tail.strip_prefix('=') else {
                    return Err(error("invalid fill setting"));
                };
                let tail = tail.trim_start();
                let Some(rest) = tail.strip_prefix("false") else {
                    return Err(error(
                        "fill-triggered Pine recalculation is not a bar-close strategy",
                    ));
                };
                if rest.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                    return Err(error("invalid fill setting"));
                }
                let end = declaration.len() - rest.len();
                result.replace_range(i..end, "calc_on_order_fills=true");
                found = true;
                break;
            }
            i += declaration[i..]
                .chars()
                .next()
                .ok_or_else(|| io("invalid source"))?
                .len_utf8();
        }
        if !found {
            return Err(error("unqualified fill setting"));
        }
        result
    } else {
        format!("{declaration}, calc_on_order_fills=true")
    };
    Ok(format!(
        "{}{declaration}{}",
        &source[..start],
        &source[cursor - 1..]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalization_matches_utf8_sig_and_universal_newlines() {
        assert_eq!(normalize("\u{feff}a\r\nb\rc"), "a\nb\nc");
        assert_eq!(normalize("\u{feff}\u{feff}a"), "\u{feff}a");
    }
    #[test]
    fn nested_titles_quotes_and_explicit_false() {
        for title in ["\"simple\"", "\"标题 )\"", "str.tostring(12)"] {
            let input = format!("//@version=6\nstrategy({title}, calc_on_order_fills = false)\n");
            assert_eq!(
                external_pass(&input).unwrap(),
                input.replace("calc_on_order_fills = false", "calc_on_order_fills=true")
            );
        }
        assert_eq!(
            external_pass("strategy(\"x\")").unwrap(),
            "strategy(\"x\", calc_on_order_fills=true)"
        );
        assert!(external_pass("strategy(\"x\", calc_on_order_fills=true)").is_err());
        assert!(external_pass("strategy(\"x\"").is_err());
    }
}
