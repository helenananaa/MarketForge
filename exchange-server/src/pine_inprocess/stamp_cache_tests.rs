use super::*;
use serde_json::json;

fn old_stamp(state: &Value) -> Value {
    json!({"checkpoint": state["vm_checkpoint"], "tail": state["vm_tail"], "intents": state["vm_tail_intents"]})
}

fn old_matches(saved: &Value, state: &Value) -> bool {
    saved.as_object().is_some_and(|object| {
        object.len() == 3
            && object.get("checkpoint") == Some(&state["vm_checkpoint"])
            && object.get("tail") == Some(&state["vm_tail"])
            && object.get("intents") == Some(&state["vm_tail_intents"])
    })
}

fn encode(state: &Value, buffer: &mut Vec<u8>) -> Result<(), serde_json::Error> {
    let mut stamp = Stamp {
        bytes: std::mem::take(buffer),
    };
    let result = stamp.update(state);
    *buffer = stamp.bytes;
    result
}

fn byte_matches(saved: &[u8], state: &Value) -> bool {
    Stamp {
        bytes: saved.to_vec(),
    }
    .matches(state)
}

fn json_bytes(bytes: &[u8]) -> Result<Value, serde_json::Error> {
    serde_json::from_slice(bytes)
}

fn state(value: Value) -> Value {
    json!({"vm_checkpoint":value, "vm_tail":[value], "vm_tail_intents":{"v":value}})
}

fn encoded(value: &Value) -> Vec<u8> {
    let mut bytes = Vec::new();
    encode(value, &mut bytes).unwrap();
    bytes
}

#[test]
fn all_prefixes_trailing_bytes_and_chunk_boundaries_are_exact() {
    let value = state(json!({"a":"a\0b\"c\\d\n", "unicode":"策略🐱", "list":[null,1,true]}));
    let bytes = encoded(&value);
    assert!(byte_matches(&bytes, &value));
    for end in 0..bytes.len() {
        assert!(!byte_matches(&bytes[..end], &value));
    }
    for tail in [b" ".as_slice(), b"\0".as_slice(), b"null".as_slice()] {
        let mut extra = bytes.clone();
        extra.extend_from_slice(tail);
        assert!(!byte_matches(&extra, &value));
    }
    for index in 0..bytes.len() {
        let mut changed = bytes.clone();
        changed[index] ^= 1;
        assert!(!byte_matches(&changed, &value));
    }
    for size in [1, 2, 3, 7, 31, 1000] {
        let mut writer = CompareWriter::new(&bytes);
        for chunk in bytes.chunks(size) {
            writer.write_all(chunk).unwrap();
        }
        assert!(writer.finished());
        writer.write_all(b"extra").unwrap();
        assert!(!writer.finished());
    }
    let mut writer = CompareWriter::new(b"abc");
    writer.write_all(b"x").unwrap();
    writer.write_all(b"bc").unwrap();
    assert!(
        !writer.finished(),
        "a later equal chunk cannot clear the mismatch"
    );
}

#[test]
fn missing_and_null_fields_match_old_indexing_without_omission() {
    let states = [
        Value::Null,
        json!({}),
        json!([]),
        json!({"vm_checkpoint":null}),
        json!({"vm_checkpoint":null,"vm_tail":null,"vm_tail_intents":null}),
    ];
    for before in &states {
        for after in &states {
            assert_eq!(
                byte_matches(&encoded(before), after),
                old_matches(&old_stamp(before), after)
            );
        }
    }
    assert_ne!(encoded(&json!({"vm_tail":[]})), encoded(&json!({})));
    assert_ne!(
        encoded(&json!({"vm_tail":{}})),
        encoded(&json!({"vm_tail":[]}))
    );
}

fn number_values() -> Vec<Value> {
    let mut values = vec![
        json!(i64::MIN),
        json!(i64::MAX),
        json!(u64::MAX),
        json!(0),
        json!(1),
        json!(-1),
        json!(9_007_199_254_740_991u64),
        json!(9_007_199_254_740_992u64),
    ];
    for value in [
        0.0,
        -0.0,
        1.0,
        -1.0,
        f64::from_bits(1),
        f64::MIN_POSITIVE,
        f64::MAX,
        1e-7,
        1e-6,
        1e20,
        1e21,
        9_007_199_254_740_992.0,
    ] {
        values.push(Value::Number(serde_json::Number::from_f64(value).unwrap()));
    }
    for text in ["1e0", "1.0", "18446744073709551616", "-9223372036854775809"] {
        values.push(serde_json::from_str(text).unwrap());
    }
    values
}

#[test]
fn numeric_edges_have_no_false_warm_matches_and_signed_zero_is_conservative() {
    let values: Vec<_> = number_values().into_iter().map(state).collect();
    for before in &values {
        let bytes = encoded(before);
        assert_eq!(bytes, serde_json::to_vec(&old_stamp(before)).unwrap());
        for after in &values {
            if byte_matches(&bytes, after) {
                assert!(
                    old_matches(&old_stamp(before), after),
                    "byte equality admitted unequal Number"
                );
            }
        }
    }
    let positive = state(json!(0.0));
    let negative = state(json!(-0.0));
    assert!(old_matches(&old_stamp(&positive), &negative));
    assert!(!byte_matches(&encoded(&positive), &negative));
    assert!(!old_matches(
        &old_stamp(&state(json!(1))),
        &state(json!(1.0))
    ));
    assert!(!byte_matches(
        &encoded(&state(json!(1))),
        &state(json!(1.0))
    ));
    assert!(serde_json::Number::from_f64(f64::NAN).is_none());
    assert!(serde_json::Number::from_f64(f64::INFINITY).is_none());
}

#[test]
fn reordered_objects_and_escaped_unicode_keep_exact_complete_bytes() {
    let first: Value = serde_json::from_str(r#"{"z":[2,1],"a":{"x":1,"b":2}}"#).unwrap();
    let second: Value = serde_json::from_str(r#"{"a":{"b":2,"x":1},"z":[2,1]}"#).unwrap();
    assert_eq!(encoded(&state(first)), encoded(&state(second)));
    let mut strings: Vec<String> = (0..=127)
        .map(|c| char::from_u32(c).unwrap().to_string())
        .collect();
    strings.extend(
        [
            "策略🐱",
            "\u{80}\u{7ff}\u{800}\u{ffff}\u{10000}\u{10ffff}",
            "\u{2028}\u{2029}",
            "é",
            "e\u{301}",
            "\\u0000",
            "\0",
            "\"\\\n\t",
        ]
        .map(str::to_owned),
    );
    for text in strings {
        let value = state(json!({(text.clone()):[text.clone(),true,null]}));
        let bytes = encoded(&value);
        assert_eq!(bytes, serde_json::to_vec(&old_stamp(&value)).unwrap());
        assert!(byte_matches(&bytes, &value));
        assert_eq!(json_bytes(&bytes).unwrap(), old_stamp(&value));
    }
    assert_ne!(
        encoded(&state(json!("é"))),
        encoded(&state(json!("e\u{301}")))
    );
}

fn next(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

fn generated(seed: &mut u64, depth: usize) -> Value {
    let token = next(seed);
    match if depth == 0 { token % 5 } else { token % 7 } {
        0 => Value::Null,
        1 => json!(token & 1 != 0),
        2 => json!(token),
        3 => serde_json::Number::from_f64(f64::from_bits(token))
            .map(Value::Number)
            .unwrap_or(Value::Null),
        4 => json!(format!("{token}:\0\"\\\n策略🐱")),
        5 => Value::Array((0..token % 4).map(|_| generated(seed, depth - 1)).collect()),
        _ => {
            let mut object = serde_json::Map::new();
            for index in 0..token % 4 {
                object.insert(format!("k{index}"), generated(seed, depth - 1));
            }
            Value::Object(object)
        }
    }
}

#[test]
fn deterministic_property_byte_equality_implies_old_structural_equality() {
    let mut seed = 0x7149_382a_6bef_09c3;
    let values: Vec<_> = (0..128).map(|_| state(generated(&mut seed, 3))).collect();
    let stamps: Vec<_> = values.iter().map(old_stamp).collect();
    let encoded: Vec<_> = values.iter().map(encoded).collect();
    for (index, before) in encoded.iter().enumerate() {
        for after in &values {
            if byte_matches(before, after) {
                assert!(old_matches(&stamps[index], after));
            }
        }
        assert!(byte_matches(before, &values[index]));
    }
}

#[test]
fn independent_saved_bytes_survive_cow_and_non_vm_changes() {
    let mut committed = std::sync::Arc::new(state(json!({"bars":32})));
    let original = committed.clone();
    let bytes = encoded(&committed);
    std::sync::Arc::make_mut(&mut committed)["vm_tail"] = json!([1, 2, 3]);
    assert!(byte_matches(&bytes, &original));
    assert!(!byte_matches(&bytes, &committed));
    let mut metadata_only = original.as_ref().clone();
    metadata_only["candle_audits"] = json!({"new":true});
    metadata_only["_host_receipts"] = json!(["receipt"]);
    assert!(byte_matches(&bytes, &metadata_only));
}

#[test]
fn reuse_clears_old_suffix_and_keeps_sufficient_capacity() {
    let large = state(json!(vec!["long-content"; 128]));
    let small = state(json!({"short": true}));
    let mut buffer = encoded(&large);
    let capacity = buffer.capacity();
    let pointer = buffer.as_ptr();
    encode(&small, &mut buffer).unwrap();
    assert_eq!(buffer, serde_json::to_vec(&old_stamp(&small)).unwrap());
    assert_eq!(buffer.capacity(), capacity);
    assert_eq!(buffer.as_ptr(), pointer);
    assert!(byte_matches(&buffer, &small));
    assert!(!byte_matches(&buffer, &large));

    let mut writer = CompareWriter::new(b"");
    writer.offset = usize::MAX;
    writer.write_all(b"x").unwrap();
    assert!(
        !writer.finished(),
        "offset overflow must not create an equal cache"
    );
}

use super::super::Host;
use exchange_core::{Candle, candle_history_hex, candle_history_seed};
use std::path::Path;

fn host(compact: bool) -> Host {
    Host::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../bot-plugins/pine-strategy"))
        .unwrap()
        .with_checkpoint_mode(compact)
}

fn request(script: &str, count: usize, state: Value) -> Value {
    let manifest: Value =
        serde_json::from_str(include_str!("../../../bot-plugins/pine-strategy/bot.json")).unwrap();
    let mut config = Value::Object(
        manifest["bot"]["parameters"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| (key.clone(), value["default"].clone()))
            .collect(),
    );
    config["script"] = json!(script);
    config["bar_interval_ms"] = json!(1000);
    let candles: Vec<_> = (0..count)
        .map(|i| {
            let close = 100 + ((i * 73 + i * i * 13) % 101) as i64;
            Candle {
                schema_version: 1,
                open_time_ms: i as u64 * 1000,
                close_time_ms: (i as u64 + 1) * 1000,
                open_tick: close,
                high_tick: close + 3,
                low_tick: close - 4,
                close_tick: close,
                volume: 1,
                quote_volume: i128::from(close),
                trades: 1,
                taker_buy_base: Some(0),
                taker_buy_quote: Some(0),
                is_final: true,
            }
        })
        .collect();
    let last = candles.last().unwrap();
    json!({"protocol_version":"bot.v1","plugin_id":"pine.strategy","plugin_version":"2.0.0",
        "state_version":1,"seed":7,"config":config,"state":state,
        "participant":{"participant_id":"pine","room_id":"stamp-cache-test","account_id":300,"instrument_id":"V-BTC-SPOT","kind":"RuleAgent"},
        "observation":{"version":1,"room_id":"stamp-cache-test","instrument_id":"V-BTC-SPOT","status":"Running",
            "step":count,"market_time_ms":last.close_time_ms,"instrument":{"tick_size":1},
            "book":{"bids":[{"price_tick":last.close_tick-1,"qty":100}],"asks":[{"price_tick":last.close_tick+1,"qty":100}]},
            "own_orders":[],"own_account":{"Spot":{"account_id":300,"cash_balance":10000,"position_qty":0,"fees_paid":0,"available_cash":10000,"available_position":0}},
            "bot_market_data":{"interval_ms":1000,"candles":candles,"own_fills":[],"fill_details":[],
                "truncated":false,"private_history_complete":true,
                "history_proof":{"version":1,"prefix_bars":0,"prefix_sha256":candle_history_hex(&candle_history_seed(1000))}}}})
}

fn slot(
    host: &Host,
    state: &Value,
    seed: u64,
    compact: bool,
) -> std::sync::Arc<std::sync::Mutex<Option<super::super::Session>>> {
    let identity = state["identity"].as_str().unwrap();
    let key = if compact {
        format!("checkpoint:{identity}:{seed}")
    } else {
        format!("{identity}:{seed}")
    };
    host.sessions.lock().unwrap().entries[&key].clone()
}

fn assert_saved(host: &Host, state: &Value, seed: u64) {
    let slot = slot(host, state, seed, true);
    let guard = slot.lock().unwrap();
    let session = guard.as_ref().unwrap();
    assert!(
        session.transcript.is_null(),
        "schema 3 must not retain a Value stamp as well"
    );
    let stamp = session.checkpoint_stamp.as_ref().unwrap();
    assert!(stamp.matches(state));
    assert_eq!(stamp.bytes, serde_json::to_vec(&old_stamp(state)).unwrap());
}

#[test]
fn private_stamp_cold_warm_rollover_and_eviction_keep_exact_response_bytes() {
    let live = host(true);
    let mut committed = Value::Null;
    for count in [31, 32, 33, 63, 64, 65] {
        if !committed.is_null() {
            assert_saved(&live, &committed, 7);
        }
        if count == 65 {
            live.clear_sessions();
            assert!(live.sessions.lock().unwrap().entries.is_empty());
        }
        let input = request("rsi.pine", count, committed);
        let expected = host(true).decide_value(&input, 5000).unwrap();
        let actual = live.decide_value(&input, 5000).unwrap();
        assert_eq!(
            serde_json::to_vec(&actual).unwrap(),
            serde_json::to_vec(&expected).unwrap()
        );
        let state = &actual["state"];
        assert_eq!(state["broker_schema"], 3);
        assert_eq!(state["evaluated_bars"], count);
        assert_eq!(state["vm_tail"].as_array().unwrap().len(), count % 32);
        assert_eq!(
            state["vm_checkpoint"]["bars"].as_u64().unwrap_or(0),
            (count / 32 * 32) as u64
        );
        assert!(state["execution_seal"].is_string());
        assert!(state.get("checkpoint_stamp").is_none());
        assert_saved(&live, state, 7);
        committed = serde_json::from_slice(&serde_json::to_vec(state).unwrap()).unwrap();
    }
}

#[test]
fn private_stamp_rejected_speculative_response_cannot_authorize_old_commit() {
    let live = host(true);
    let initial = request("rsi.pine", 31, Value::Null);
    let committed = live.decide_value(&initial, 5000).unwrap()["state"].clone();
    let original = serde_json::to_vec(&committed).unwrap();
    let mut rejected = request("rsi.pine", 32, committed.clone());
    // Legal opaque state metadata is outside the public VM seal. The request
    // stays below 4 MiB, but its response fails the existing 1 MiB limit only
    // after the speculative runtime and private stamp have been installed.
    rejected["state"]["opaque_test_padding"] = json!("x".repeat(1_048_576));
    let error = live.decide_value(&rejected, 5000).unwrap_err();
    assert!(error.to_string().contains("action/state limits"), "{error}");
    {
        let slot = slot(&live, &committed, 7, true);
        let guard = slot.lock().unwrap();
        let stamp = guard.as_ref().unwrap().checkpoint_stamp.as_ref().unwrap();
        assert!(!stamp.matches(&committed));
        let advanced: Value = serde_json::from_slice(&stamp.bytes).unwrap();
        assert_eq!(advanced["checkpoint"]["bars"], 32);
    }
    let retry = request("rsi.pine", 32, committed.clone());
    assert_eq!(
        live.decide_value(&retry, 5000).unwrap(),
        host(true).decide_value(&retry, 5000).unwrap()
    );
    assert_eq!(serde_json::to_vec(&committed).unwrap(), original);

    // Also cover a successful response discarded by the scheduler rather
    // than committed. Its newer cache cannot skip either incoming bar.
    let discarded = live
        .decide_value(&request("rsi.pine", 33, committed.clone()), 5000)
        .unwrap();
    let retry = request("rsi.pine", 34, committed);
    let actual = live.decide_value(&retry, 5000).unwrap();
    assert_eq!(actual, host(true).decide_value(&retry, 5000).unwrap());
    assert_eq!(discarded["state"]["evaluated_bars"], 33);
    assert_eq!(actual["state"]["evaluated_bars"], 34);
}

#[test]
fn private_stamp_cannot_bypass_public_seal_and_missing_cache_reconstructs() {
    let live = host(true);
    let initial = live
        .decide_value(&request("rsi.pine", 32, Value::Null), 5000)
        .unwrap()["state"]
        .clone();
    let committed = live
        .decide_value(&request("rsi.pine", 33, initial), 5000)
        .unwrap()["state"]
        .clone();
    let valid = request("rsi.pine", 34, committed.clone());
    let mut changed_seal = valid.clone();
    changed_seal["state"]["execution_seal"] = json!("broken");
    assert_saved(&live, &changed_seal["state"], 7);
    let mut changed_vm = valid.clone();
    changed_vm["state"]["vm_tail"] = json!([]);
    for input in [changed_seal, changed_vm] {
        assert!(
            live.decide_value(&input, 5000)
                .unwrap_err()
                .to_string()
                .contains("execution seal")
        );
    }
    assert_saved(&live, &committed, 7);
    slot(&live, &committed, 7, true)
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .checkpoint_stamp = None;
    let actual = live.decide_value(&valid, 5000).unwrap();
    assert_eq!(actual, host(true).decide_value(&valid, 5000).unwrap());
    assert_saved(&live, &actual["state"], 7);

    let mut different_seed = request("rsi.pine", 35, actual["state"].clone());
    different_seed["seed"] = json!(8);
    let next = live.decide_value(&different_seed, 5000).unwrap();
    assert_eq!(
        next,
        host(true).decide_value(&different_seed, 5000).unwrap()
    );
    assert_saved(&live, &next["state"], 8);
    assert_saved(&live, &actual["state"], 7);
}

#[test]
fn private_stamp_schema_two_keeps_the_original_transcript_path() {
    let live = host(false);
    let mut committed = Value::Null;
    for count in [4, 5, 32, 33] {
        let input = request("ma.pine", count, committed);
        let expected = host(false).decide_value(&input, 5000).unwrap();
        let actual = live.decide_value(&input, 5000).unwrap();
        assert_eq!(
            serde_json::to_vec(&actual).unwrap(),
            serde_json::to_vec(&expected).unwrap()
        );
        let state = &actual["state"];
        assert_eq!(state["broker_schema"], 2);
        assert!(state.get("vm_checkpoint").is_none());
        let slot = slot(&live, state, 7, false);
        let guard = slot.lock().unwrap();
        let session = guard.as_ref().unwrap();
        assert!(session.checkpoint_stamp.is_none());
        for key in ["bars", "accounts", "providers", "evaluations"] {
            assert_eq!(session.transcript[key], state[key]);
        }
        committed = state.clone();
    }
    // An enabled host must still honor the existing schema-2 durable contract.
    let next = request("ma.pine", 34, committed);
    assert_eq!(
        host(true).decide_value(&next, 5000).unwrap(),
        live.decide_value(&next, 5000).unwrap()
    );
}

#[test]
fn ecosystem_replay_continues_identically_after_durable_journal_recovery() {
    use crate::journal::{
        InMemoryJournalStore, JournalStore, PendingJournalMutation, RoomMutation,
    };
    use crate::scheduler_delta::SchedulerDelta;
    use exchange_core::{PersistedAgentKindState, SchedulerMode, SchedulerState};
    use std::sync::Arc;

    let spec: exchange_core::population::BackgroundMarket = serde_json::from_str(include_str!(
        "../../../scripts/fixtures/microstructure_market.json"
    ))
    .unwrap();
    let mut scheduler = SchedulerState::new(
        "stamp-cache-test",
        spec.agents.into_iter().take(1).collect(),
        SchedulerMode::Auto { interval_ms: 1000 },
    );
    let live = host(true);
    let script = "ecosystem/library/0068dd1dd9576132.pine";
    let request = |count, state| {
        let mut input = request(script, count, state);
        input["config"]["quantize_strategy_orders"] = json!(true);
        input["config"]["inputs"] = json!({});
        input
    };
    let mut store = InMemoryJournalStore::new();
    let mut committed = Value::Null;
    for (index, count) in [7, 8, 15, 16, 127, 128, 129, 130, 256, 257]
        .into_iter()
        .enumerate()
    {
        let input = request(count, committed);
        let actual = live.decide_value(&input, 5000).unwrap();
        assert_eq!(actual["state"]["broker_schema"], 4);
        let mut next = scheduler.clone();
        next.revision += 1;
        next.agents[0].kind_state = PersistedAgentKindState::Plugin {
            plugin_id: "pine.strategy".into(),
            plugin_version: "2.0.0".into(),
            state_version: 1,
            data: Arc::new(actual["state"].clone()),
        };
        let mutation = if index == 0 {
            RoomMutation::SchedulerProgress {
                clock_steps: 0,
                state: next.clone(),
                training: None,
            }
        } else {
            let delta = SchedulerDelta::between(&scheduler, &next)
                .unwrap()
                .into_durable(&scheduler)
                .unwrap();
            let wire = serde_json::to_value(&delta).unwrap();
            assert_eq!(wire["pine_changes"].as_array().unwrap().len(), 1);
            RoomMutation::SchedulerDelta {
                clock_steps: 1,
                delta,
                training: None,
            }
        };
        store
            .append_room_mutation(
                &PendingJournalMutation::new("stamp-cache-test", 0, mutation),
                &[],
                &[],
                None,
            )
            .unwrap();
        let recovery = store.load_recovery().unwrap();
        let states = crate::scheduler_states_from_recovery(&recovery).unwrap();
        let restored = &states["stamp-cache-test"];
        assert_eq!(
            serde_json::to_vec(restored).unwrap(),
            serde_json::to_vec(&next).unwrap()
        );
        let PersistedAgentKindState::Plugin { data, .. } = &restored.agents[0].kind_state else {
            panic!("journal lost Pine state");
        };
        // A new Host has no private session, stamp, or archive encoding cache.
        // Its next response must include the same intents/actions and seal.
        let cold_input = request(count + 1, data.as_ref().clone());
        let cold = host(true).decide_value(&cold_input, 5000).unwrap();
        let warm = live.decide_value(&cold_input, 5000).unwrap();
        assert_eq!(
            serde_json::to_vec(&cold).unwrap(),
            serde_json::to_vec(&warm).unwrap()
        );
        // Discard that speculative response: the next turn must also recover
        // from the exact journaled state, not assume it was committed.
        committed = data.as_ref().clone();
        scheduler = next;
    }
}
