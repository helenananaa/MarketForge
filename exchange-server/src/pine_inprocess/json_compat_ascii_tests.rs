//! Exact old/new bytes and an opt-in benchmark over actual saved Pine states.
use super::*;
use std::{hint::black_box, path::PathBuf, time::Instant};

fn legacy_string(value: &str, out: &mut String) -> Result<(), BotError> {
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

// Test-only copy of the previous writer. Number spelling and sorting are
// deliberately unchanged; this is the reference, never a production switch.
fn legacy_value(v: &Value, spaced: bool, out: &mut String) -> Result<(), BotError> {
    let comma = if spaced { ", " } else { "," };
    let colon = if spaced { ": " } else { ":" };
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::String(s) => legacy_string(s, out)?,
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
                legacy_value(value, spaced, out)?;
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
                legacy_string(key, out)?;
                out.push_str(colon);
                legacy_value(value, spaced, out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn legacy_ref(value: PythonJsonRef<'_>, spaced: bool, out: &mut String) -> Result<(), BotError> {
    match value {
        PythonJsonRef::Value(value) => legacy_value(value, spaced, out),
        PythonJsonRef::String(value) => legacy_string(value, out),
        PythonJsonRef::Object(fields) => {
            let mut fields: Vec<_> = fields.iter().collect();
            fields.sort_by(|a, b| a.0.cmp(b.0));
            out.push('{');
            for (index, (key, value)) in fields.into_iter().enumerate() {
                if index > 0 {
                    out.push_str(if spaced { ", " } else { "," });
                }
                legacy_string(key, out)?;
                out.push_str(if spaced { ": " } else { ":" });
                legacy_ref(*value, spaced, out)?;
            }
            out.push('}');
            Ok(())
        }
    }
}

fn seal_bytes(state: &Value, candidate: bool, spaced: bool) -> Result<String, BotError> {
    use PythonJsonRef as Ref;
    let vm = [
        ("checkpoint", Ref::Value(&state["vm_checkpoint"])),
        ("tail", Ref::Value(&state["vm_tail"])),
        ("intents", Ref::Value(&state["vm_tail_intents"])),
    ];
    let fields = [
        ("protocol", Ref::String("marketforge.pine-execution/1")),
        ("vm", Ref::Object(&vm)),
        ("evaluated", Ref::Value(&state["evaluated_bars"])),
        ("last_time", Ref::Value(&state["last_bar_time"])),
        ("audits", Ref::Value(&state["candle_audits"])),
    ];
    if candidate {
        python_json_ref(Ref::Object(&fields), spaced)
    } else {
        let mut out = String::new();
        legacy_ref(Ref::Object(&fields), spaced, &mut out)?;
        Ok(out)
    }
}

fn assert_string_equal(value: &str) {
    let mut expected = "prefix:".to_owned();
    let mut actual = expected.clone();
    legacy_string(value, &mut expected).unwrap();
    string(value, &mut actual).unwrap();
    expected.push_str(":suffix");
    actual.push_str(":suffix");
    assert_eq!(actual, expected, "input {value:?}");
}

#[test]
fn safe_ascii_string_matches_legacy_for_all_ascii_pairs_and_controls() {
    assert_string_equal("");
    for first in 0u8..=127 {
        assert_string_equal(&String::from_utf8(vec![first]).unwrap());
        for second in 0u8..=127 {
            assert_string_equal(&String::from_utf8(vec![first, second]).unwrap());
        }
    }
    for value in [
        "execution_seal",
        "marketforge.pine-execution/1",
        "$f64",
        "0123456789abcdef",
        "a\"b",
        "a\\b",
        "a\nb",
        "a\tb",
        "a\rb",
        "a\0b",
        "\u{7f}",
    ] {
        assert_string_equal(value);
    }
}

#[test]
fn safe_ascii_string_matches_legacy_for_every_unicode_scalar() {
    for scalar in 0..=0x10ffff {
        if let Some(value) = char::from_u32(scalar) {
            let mut bytes = [0; 4];
            assert_string_equal(value.encode_utf8(&mut bytes));
        }
    }
    for value in [
        "ascii策略🐱tail",
        "\u{7f}\u{80}",
        "\u{d7ff}\u{e000}",
        "\u{ffff}\u{10000}",
        "\u{10ffff}",
    ] {
        assert_string_equal(value);
    }
}

#[test]
fn safe_ascii_seal_bytes_preserve_null_missing_numbers_and_key_order() {
    let mut cases = vec![
        Value::Null,
        serde_json::json!({}),
        serde_json::json!({
            "vm_checkpoint":null,"vm_tail":null,"vm_tail_intents":null,
            "evaluated_bars":null,"last_bar_time":null,"candle_audits":null
        }),
    ];
    for number in [
        serde_json::json!(i64::MIN),
        serde_json::json!(u64::MAX),
        serde_json::json!(0),
        serde_json::json!(1.0),
        serde_json::json!(-0.0),
        serde_json::json!(1e-7),
        serde_json::json!(1e20),
        serde_json::json!(f64::from_bits(1)),
        serde_json::json!(f64::MAX),
    ] {
        cases.push(serde_json::json!({
            "vm_checkpoint":{"z":number,"策略🐱":["ascii", "\n\"\\\u{7f}",null,true,false]},
            "vm_tail":[number],"vm_tail_intents":[{"x":number}],
            "evaluated_bars":number,"last_bar_time":number,
            "candle_audits":{"z":number,"a":{"last":number}}
        }));
    }
    cases.extend([
        serde_json::from_str(
            r#"{"vm_checkpoint":{"z":-0.0,"a":1e20},"vm_tail":[],"candle_audits":{"b":2,"a":1}}"#,
        )
        .unwrap(),
        serde_json::from_str(
            r#"{"candle_audits":{"a":1,"b":2},"vm_tail":[],"vm_checkpoint":{"a":1e20,"z":-0.0}}"#,
        )
        .unwrap(),
    ]);
    for state in cases {
        let mut variants = vec![state.clone()];
        if let Some(object) = state.as_object() {
            for key in object.keys() {
                let mut missing = state.clone();
                missing.as_object_mut().unwrap().remove(key);
                variants.push(missing);
            }
        }
        for value in variants {
            for spaced in [false, true] {
                let previous = seal_bytes(&value, false, spaced).unwrap();
                let candidate = seal_bytes(&value, true, spaced).unwrap();
                assert_eq!(candidate, previous);
                assert_eq!(
                    Sha256::digest(candidate.as_bytes()),
                    Sha256::digest(previous.as_bytes())
                );
            }
        }
    }
}

fn ascii_encoding_opportunities(value: &Value) -> (usize, usize) {
    fn eligible(value: &str) -> usize {
        usize::from(
            value
                .bytes()
                .all(|b| b.is_ascii() && b >= b' ' && !matches!(b, b'"' | b'\\')),
        )
    }
    match value {
        Value::String(value) => (1, eligible(value)),
        Value::Array(values) => values
            .iter()
            .map(ascii_encoding_opportunities)
            .fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1)),
        Value::Object(values) => values.iter().fold((0, 0), |a, (key, value)| {
            let b = ascii_encoding_opportunities(value);
            (a.0 + 1 + b.0, a.1 + eligible(key) + b.1)
        }),
        _ => (0, 0),
    }
}

#[test]
#[ignore = "isolated Release old/new seal benchmark; MF_PINE_ASCII_SCHEDULER must name a saved 1000-Pine scheduler"]
fn safe_ascii_saved_1000_pine_seals_old_new_microbenchmark() {
    let path = PathBuf::from(
        std::env::var_os("MF_PINE_ASCII_SCHEDULER").expect("explicit saved scheduler path"),
    );
    let source = std::fs::read(&path).unwrap();
    let source_sha = format!("{:x}", Sha256::digest(&source));
    let scheduler: Value = serde_json::from_slice(&source).unwrap();
    let states: Vec<&Value> = scheduler["agents"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|agent| {
            let plugin = &agent["kind_state"]["Plugin"];
            (plugin["plugin_id"] == "pine.strategy" && plugin["data"]["broker_schema"] == 3)
                .then_some(&plugin["data"])
        })
        .collect();
    assert_eq!(
        states.len(),
        1000,
        "use the actual 1000-Pine pilot scheduler"
    );
    let mut strings = 0usize;
    let mut avoided = 0usize;
    for state in &states {
        let previous = seal_bytes(state, false, false).unwrap();
        let candidate = seal_bytes(state, true, false).unwrap();
        assert_eq!(candidate, previous);
        assert_eq!(
            format!("{:x}", Sha256::digest(candidate.as_bytes())),
            state["execution_seal"].as_str().unwrap()
        );
        let payload: Value = serde_json::from_str(&candidate).unwrap();
        let counts = ascii_encoding_opportunities(&payload);
        strings += counts.0;
        avoided += counts.1;
    }
    eprintln!(
        "{}",
        serde_json::json!({"scope":"encoding plus SHA256 over saved seal fields; excludes source IO, queueing, state clones and VM; not end-to-end latency", "scheduler":path,"scheduler_sha256":source_sha,"states":states.len(),"string_encodings":strings,"temporary_ascii_string_encodings_avoided":avoided,"allocation_count_note":"eligible encoding sites, not allocator instrumentation"})
    );
    let mut reference = None;
    for round in 0..3 {
        // ABBA order reduces first/last placement bias. Do not infer capacity
        // or latency acceptance from these isolated encoding measurements.
        for candidate in [false, true, true, false] {
            let begun = Instant::now();
            let mut bytes = 0usize;
            let mut aggregate = Sha256::new();
            for state in &states {
                let encoded = seal_bytes(black_box(state), candidate, false).unwrap();
                bytes += encoded.len();
                let digest = Sha256::digest(encoded.as_bytes());
                aggregate.update(black_box(digest));
            }
            let elapsed = begun.elapsed();
            let aggregate = format!("{:x}", aggregate.finalize());
            if let Some(expected) = &reference {
                assert_eq!(&aggregate, expected);
            } else {
                reference = Some(aggregate.clone());
            }
            eprintln!(
                "{}",
                serde_json::json!({"round":round,"encoder":if candidate {"safe_ascii"} else {"legacy"},"seals":states.len(),"bytes":bytes,"elapsed_ms":elapsed.as_secs_f64()*1000.0,"aggregate_sha256":aggregate})
            );
        }
    }
}
