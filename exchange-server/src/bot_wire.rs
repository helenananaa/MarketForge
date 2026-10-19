//! Transport caches contain prior host inputs. A state delta that the host
//! accepted is folded into the cached request exactly as into the bot's state.
use exchange_core::{
    BotDecisionRequest, BotError, Candle, ParticipantConfig, ParticipantObservation, Trade,
    observation::{BotAccountEvent, BotFillDetail},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const CACHE_LIMIT: usize = 512;
const PATCH_LIMIT: usize = 256;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub(super) enum Part {
    Key(String),
    Index(usize),
}
/// One change to a JSON value; the same form in both directions of the wire.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(super) enum Patch {
    Set {
        path: Vec<Part>,
        value: Value,
    },
    Remove {
        path: Vec<Part>,
    },
    Append {
        path: Vec<Part>,
        from: usize,
        values: Vec<Value>,
    },
}
impl Patch {
    fn path(&self) -> &[Part] {
        match self {
            Self::Set { path, .. } | Self::Remove { path } | Self::Append { path, .. } => path,
        }
    }
}

// JSON equality must preserve float representation, including signed zero:
// Pine inputs/configuration identities can distinguish -0.0 from 0.0.
fn same(old: &Value, new: &Value) -> bool {
    match (old, new) {
        (Value::Number(old), Value::Number(new)) if old.is_f64() && new.is_f64() => {
            old.as_f64().unwrap().to_bits() == new.as_f64().unwrap().to_bits()
        }
        (Value::Array(old), Value::Array(new)) => {
            old.len() == new.len() && old.iter().zip(new).all(|(a, b)| same(a, b))
        }
        // Maps iterate in key order, so equal maps pair up entry by entry;
        // an insertion-ordered map can only report a false difference.
        (Value::Object(old), Value::Object(new)) => {
            old.len() == new.len()
                && old
                    .iter()
                    .zip(new)
                    .all(|((key, value), (other_key, other))| key == other_key && same(value, other))
        }
        _ => old == new,
    }
}
// Count using the very same serializer/escaping as the full wire. On cache
// hits only changed subtrees need serialization to enforce the expanded bound.
fn json_len<T: Serialize + ?Sized>(value: &T) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    serde_json::to_writer(&mut count, value).expect("serializable JSON wire value");
    count.0
}


// Each node is compared once: objects key by key, arrays by their common prefix.
fn diff(old: &Value, new: &Value, path: &mut Vec<Part>, patches: &mut Vec<Patch>) -> Option<i128> {
    if patches.len() > PATCH_LIMIT {
        return None;
    }
    let mut length_delta = 0;
    match (old, new) {
        (Value::Object(old), Value::Object(new)) => {
            length_delta +=
                new.len().saturating_sub(1) as i128 - old.len().saturating_sub(1) as i128;
            for key in old.keys().filter(|key| !new.contains_key(*key)) {
                length_delta -= (json_len(key) + 1 + json_len(&old[key])) as i128;
                path.push(Part::Key(key.clone()));
                patches.push(Patch::Remove { path: path.clone() });
                path.pop();
            }
            for (key, value) in new {
                path.push(Part::Key(key.clone()));
                match old.get(key) {
                    Some(old) => length_delta += diff(old, value, path, patches)?,
                    None => {
                        length_delta += (json_len(key) + 1 + json_len(value)) as i128;
                        patches.push(Patch::Set {
                            path: path.clone(),
                            value: value.clone(),
                        });
                    }
                }
                path.pop();
                if patches.len() > PATCH_LIMIT {
                    return None;
                }
            }
        }
        (Value::Array(old), Value::Array(new))
            if new.len() >= old.len() && old.iter().zip(new).all(|(a, b)| same(a, b)) =>
        {
            if new.len() == old.len() {
                return Some(0);
            }
            let tail = &new[old.len()..];
            length_delta += (json_len(tail) - 2 + usize::from(!old.is_empty())) as i128;
            patches.push(Patch::Append {
                path: path.clone(),
                from: old.len(),
                values: tail.to_vec(),
            });
        }
        // Small tuples/related-market vectors can change one member while
        // containing large unchanged histories. Diff these by index.
        (Value::Array(old), Value::Array(new)) if old.len() == new.len() && old.len() <= 16 => {
            for (index, (old, new)) in old.iter().zip(new).enumerate() {
                path.push(Part::Index(index));
                length_delta += diff(old, new, path, patches)?;
                path.pop();
            }
        }
        _ if same(old, new) => return Some(0),
        _ => {
            length_delta += json_len(new) as i128 - json_len(old) as i128;
            patches.push(Patch::Set {
                path: path.clone(),
                value: new.clone(),
            });
        }
    }
    (patches.len() <= PATCH_LIMIT).then_some(length_delta)
}

fn child<'a>(value: &'a Value, part: &Part) -> Option<&'a Value> {
    match (value, part) {
        (Value::Object(map), Part::Key(key)) => map.get(key),
        (Value::Array(items), Part::Index(index)) => items.get(*index),
        _ => None,
    }
}
fn child_mut<'a>(value: &'a mut Value, part: &Part) -> Option<&'a mut Value> {
    match (value, part) {
        (Value::Object(map), Part::Key(key)) => map.get_mut(key),
        (Value::Array(items), Part::Index(index)) => items.get_mut(*index),
        _ => None,
    }
}

/// Applies patches whose paths start after `skip` leading parts, returning
/// the change in compact JSON length. No path may contain another, so every
/// patch is checked against the unchanged value first: a rejected delta
/// leaves `value` as it was.
pub(super) fn apply_patches(value: &mut Value, patches: &[Patch], skip: usize) -> Result<i128, BotError> {
    let invalid = || BotError("invalid worker state delta".into());
    for (index, patch) in patches.iter().enumerate() {
        let path = patch.path().get(skip..).ok_or_else(invalid)?;
        let (last, parents) = path.split_last().ok_or_else(invalid)?;
        let parent = parents
            .iter()
            .try_fold(&*value, |value, part| child(value, part))
            .ok_or_else(invalid)?;
        let valid = match (patch, parent, last) {
            (Patch::Set { .. }, Value::Object(_), Part::Key(_)) => true,
            (Patch::Set { .. }, Value::Array(items), Part::Index(index)) => *index < items.len(),
            (Patch::Remove { .. }, Value::Object(map), Part::Key(key)) => map.contains_key(key),
            (Patch::Append { from, .. }, parent, last) => child(parent, last)
                .and_then(Value::as_array)
                .is_some_and(|items| items.len() == *from),
            _ => false,
        };
        let overlapping = patches[..index].iter().any(|other| {
            let other = &other.path()[skip..];
            other.starts_with(path) || path.starts_with(other)
        });
        if !valid || overlapping {
            return Err(invalid());
        }
    }
    let mut change = 0;
    for patch in patches {
        let (last, parents) = patch.path()[skip..].split_last().unwrap();
        let parent = parents
            .iter()
            .try_fold(&mut *value, |value, part| child_mut(value, part))
            .unwrap();
        match (patch, parent, last) {
            (Patch::Set { value, .. }, Value::Object(map), Part::Key(key)) => {
                change += match map.get(key) {
                    Some(old) => json_len(value) as i128 - json_len(old) as i128,
                    None => (json_len(key) + 1 + json_len(value) + usize::from(!map.is_empty())) as i128,
                };
                map.insert(key.clone(), value.clone());
            }
            (Patch::Set { value, .. }, Value::Array(items), Part::Index(index)) => {
                change += json_len(value) as i128 - json_len(&items[*index]) as i128;
                items[*index] = value.clone();
            }
            (Patch::Remove { .. }, Value::Object(map), Part::Key(key)) => {
                let old = map.remove(key).unwrap();
                change -= (json_len(key) + 1 + json_len(&old) + usize::from(!map.is_empty())) as i128;
            }
            (Patch::Append { values, .. }, parent, last) => {
                let items = child_mut(parent, last).and_then(Value::as_array_mut).unwrap();
                if !values.is_empty() {
                    change += (values.iter().map(json_len).sum::<usize>() + values.len()
                        - usize::from(items.is_empty())) as i128;
                }
                items.extend(values.iter().cloned());
            }
            _ => unreachable!("checked above"),
        }
    }
    Ok(change)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_history_lists_reach_the_worker_exactly_as_full_requests() {
        use exchange_core::{BotMarketData, ParticipantConfig, ParticipantKind, RoomManager, Side};
        use exchange_core::observation::{BotAccountEvent, BotAccountFrame, BotFillDetail};
        let fixture: Value =
            serde_json::from_str(include_str!("../../scripts/fixtures/f6_batch_spec.json")).unwrap();
        let mut rooms = RoomManager::new();
        rooms
            .create_room(serde_json::from_value(fixture["scenario"].clone()).unwrap())
            .unwrap();
        let mut observation = rooms.participant_observation("f6-batch", "V-BTC-SPOT", 20).unwrap();
        observation.bot_market_data = Some(BotMarketData {
            interval_ms: 1000,
            external_volume_qty: "0".into(),
            candles: vec![],
            history_proof: None,
            private_history_complete: true,
            own_fills: vec![],
            fill_details: vec![],
            truncated: false,
            streams: vec![],
            account_events: vec![],
            conditional_orders: vec![],
        });
        let mut request = BotDecisionRequest {
            protocol_version: "bot.v1".into(),
            plugin_id: "pine.strategy".into(),
            plugin_version: "2.0.0".into(),
            state_version: 1,
            seed: 7,
            participant: ParticipantConfig {
                participant_id: "buyer".into(),
                kind: ParticipantKind::RuleAgent,
                room_id: "f6-batch".into(),
                account_id: 20,
                instrument_id: Some("V-BTC-SPOT".into()),
            },
            config: serde_json::json!({}),
            state: serde_json::json!({"n": 0}),
            observation,
        };
        let candle = |i: u64| Candle {
            schema_version: 1,
            open_time_ms: i * 1000,
            close_time_ms: (i + 1) * 1000,
            open_tick: 100,
            high_tick: 101 + i as i64,
            low_tick: 99,
            close_tick: 100,
            volume: 3,
            quote_volume: 300,
            trades: 2,
            taker_buy_base: None,
            taker_buy_quote: None,
            is_final: true,
        };
        let mut cache = RequestCache::default();
        let mut worker: Option<Value> = None;
        for step in 0..14u64 {
            let data = request.observation.bot_market_data.as_mut().unwrap();
            data.candles.push(candle(step));
            if step % 3 == 1 {
                let trade_id = data.own_fills.len() as u64 + 1;
                data.own_fills.push(Trade {
                    trade_id,
                    maker_order_id: 1,
                    maker_account_id: 10,
                    taker_order_id: 2,
                    taker_account_id: 20,
                    price_tick: 100,
                    qty: 1,
                    taker_side: Side::Buy,
                    maker_position_side: Default::default(),
                    taker_position_side: Default::default(),
                });
                data.fill_details.push(BotFillDetail { trade_id, market_time_ms: step * 1000, fee_paid: Some(1) });
                data.account_events.push(BotAccountEvent {
                    instrument_id: "V-BTC-SPOT".into(),
                    command_seq: step,
                    market_time_ms: step * 1000,
                    account: BotAccountFrame::Spot { account_id: 20, cash_balance: 1000, position_qty: 1, fees_paid: 1 },
                });
            }
            match step {
                // A corrected committed candle, an emptied omitted list, and a
                // related market that appears and then changes its own history.
                6 => data.candles[2].high_tick = 7,
                9 => data.account_events.clear(),
                _ => {}
            }
            if step == 7 {
                let mut related = request.observation.clone();
                related.related_markets.clear();
                request.observation.related_markets = vec![related];
            }
            if step >= 8 {
                let related = request.observation.related_markets[0].bot_market_data.as_mut().unwrap();
                related.candles.push(candle(100 + step));
            }
            request.state["n"] = Value::from(step);
            let packet = cache.encode(&request, step + 1, usize::MAX, false, None).unwrap();
            let wire: Value = serde_json::from_slice(&packet.bytes).unwrap();
            let decoded = match wire.get("request") {
                Some(full) => full.clone(),
                None => {
                    let patches: Vec<Patch> = serde_json::from_value(wire["request_delta"].clone()).unwrap();
                    let mut base = worker.take().unwrap();
                    // The worker applies patches in order.
                    for patch in &patches {
                        apply_patches(&mut base, std::slice::from_ref(patch), 0).unwrap();
                    }
                    base
                }
            };
            let full = serde_json::to_value(&request).unwrap();
            assert!(same(&decoded, &full), "step {step}");
            assert_eq!(packet.value_bytes, json_len(&full), "step {step}");
            assert_eq!(step > 0, wire.get("base").is_some(), "step {step}");
            cache.accept(packet);
            worker = Some(decoded);
        }
    }

    #[test]
    fn state_diffs_and_folds_reproduce_every_request_the_worker_decodes() {
        use exchange_core::{ParticipantConfig, ParticipantKind, RoomManager};
        let fixture: Value =
            serde_json::from_str(include_str!("../../scripts/fixtures/f6_batch_spec.json")).unwrap();
        let mut rooms = RoomManager::new();
        rooms
            .create_room(serde_json::from_value(fixture["scenario"].clone()).unwrap())
            .unwrap();
        let mut request = BotDecisionRequest {
            protocol_version: "bot.v1".into(),
            plugin_id: "pine.strategy".into(),
            plugin_version: "2.0.0".into(),
            state_version: 1,
            seed: 7,
            participant: ParticipantConfig {
                participant_id: "buyer".into(),
                kind: ParticipantKind::RuleAgent,
                room_id: "f6-batch".into(),
                account_id: 20,
                instrument_id: Some("V-BTC-SPOT".into()),
            },
            config: serde_json::json!({}),
            state: Value::Null,
            observation: rooms.participant_observation("f6-batch", "V-BTC-SPOT", 20).unwrap(),
        };
        let mut cache = RequestCache::default();
        let mut worker: Option<Value> = None; // the worker's decoded base request
        let mut hint = None;
        for step in 0..12u64 {
            // Alternate the hinted comparison of changed keys with a full diff.
            let hinted = hint.take().filter(|_| step % 2 == 0);
            let packet = cache.encode(&request, step + 1, usize::MAX, true, hinted.as_ref()).unwrap();
            let wire: Value = serde_json::from_slice(&packet.bytes).unwrap();
            let decoded = match wire.get("request") {
                Some(full) => full.clone(),
                None => {
                    let patches: Vec<Patch> = serde_json::from_value(wire["request_delta"].clone()).unwrap();
                    let mut base = worker.take().unwrap();
                    apply_patches(&mut base, &patches, 0).unwrap();
                    base
                }
            };
            assert!(same(&decoded, &serde_json::to_value(&request).unwrap()), "step {step}");
            assert_eq!(wire["reply_state_delta"], true);
            assert_eq!(step > 0, wire.get("base").is_some());
            let key = packet.key().to_owned();
            cache.accept(packet);
            if step == 0 {
                // A full reply to the first, null state: nothing is folded.
                request.state = serde_json::json!({"bars": [], "n": 0});
                worker = Some(decoded);
                continue;
            }
            // The worker's reply: one more bar and a changed counter, folded on
            // both sides; then a host-side change before the next request.
            let reply: Vec<Patch> = serde_json::from_value(serde_json::json!([
                {"op": "append", "path": ["bars"], "from": step - 1, "values": [{"close": (step as f64 + 0.5)}]},
                {"op": "set", "path": ["n"], "value": step}
            ]))
            .unwrap();
            cache.fold(&key, &reply).unwrap();
            let mut base = decoded;
            apply_patches(base.get_mut("state").unwrap(), &reply, 0).unwrap();
            worker = Some(base);
            apply_patches(&mut request.state, &reply, 0).unwrap();
            request.state["_host_observation"] = serde_json::json!([step, step * 1000]);
            if step % 4 == 3 {
                request.state.as_object_mut().unwrap().remove("_host_observation");
            }
            hint = Some(StateHint {
                base: step + 1,
                changed: ["_host_observation".to_owned()].into(),
            });
        }
    }

    #[test]
    fn state_patches_apply_exactly_or_not_at_all() {
        let state = serde_json::json!({"bars": [1, 2], "empty": [], "map": {"a": 1, "b": "字"}, "n": -0.0});
        let patches: Vec<Patch> = serde_json::from_value(serde_json::json!([
            {"op": "append", "path": ["bars"], "from": 2, "values": [3, {"x": 1.5}]},
            {"op": "append", "path": ["empty"], "from": 0, "values": ["\\\""]},
            {"op": "set", "path": ["map", "c"], "value": [true, null]},
            {"op": "set", "path": ["map", "a"], "value": 1.0},
            {"op": "remove", "path": ["map", "b"]},
            {"op": "set", "path": ["n"], "value": 0.0},
            {"op": "set", "path": ["new"], "value": {}}
        ]))
        .unwrap();
        let mut applied = state.clone();
        let change = apply_patches(&mut applied, &patches, 0).unwrap();
        let expected = serde_json::json!({"bars": [1, 2, 3, {"x": 1.5}], "empty": ["\\\""],
            "map": {"a": 1.0, "c": [true, null]}, "n": 0.0, "new": {}});
        assert!(same(&applied, &expected));
        assert_eq!(json_len(&state) as i128 + change, json_len(&applied) as i128);
        for bad in [
            serde_json::json!([{"op": "append", "path": ["bars"], "from": 1, "values": [9]}]),
            serde_json::json!([{"op": "remove", "path": ["map", "z"]}]),
            serde_json::json!([{"op": "set", "path": ["missing", "x"], "value": 1}]),
            serde_json::json!([{"op": "set", "path": [], "value": 1}]),
            serde_json::json!([{"op": "set", "path": ["n"], "value": 1}, {"op": "set", "path": ["map"], "value": 1},
                               {"op": "set", "path": ["map", "a"], "value": 2}]),
        ] {
            let patches: Vec<Patch> = serde_json::from_value(bad).unwrap();
            let mut unchanged = state.clone();
            assert!(apply_patches(&mut unchanged, &patches, 0).is_err());
            assert!(same(&unchanged, &state));
        }
    }

    #[test]
    fn delta_preserves_signed_zero_and_number_types() {
        let before = serde_json::json!({"inputs":{"zero":-0.0,"quantity":1.0}});
        let after = serde_json::json!({"inputs":{"zero":0.0,"quantity":1}});
        let mut patches = Vec::new();
        diff(&before, &after, &mut Vec::new(), &mut patches);
        let encoded = serde_json::to_value(&patches).unwrap();
        assert_eq!(patches.len(), 2);
        assert_eq!(
            encoded[0]["path"],
            serde_json::json!(["inputs", "quantity"])
        );
        assert!(encoded[0]["value"].is_u64());
        assert_eq!(
            encoded[1]["value"].as_f64().unwrap().to_bits(),
            0.0_f64.to_bits()
        );
    }

    #[test]
    fn delta_lengths_match_full_json_for_edits_appends_and_escaping() {
        let values = [
            serde_json::json!(null),
            serde_json::json!({}),
            serde_json::json!({"a": [], "escaped\"汉字": "\\\n", "n": -0.0}),
            serde_json::json!({"a": [1, 2, 3], "n": 0.0}),
            serde_json::json!({"a": [1, 2], "n": 0}),
            serde_json::json!({"a": [1, 7, 3], "nested": {"x": [1.0, "字"]}}),
            serde_json::json!([[], [1], {}, {"a": 2}]),
            serde_json::json!([[], [1, 2], {"x": 0}, {}]),
        ];
        for old in &values {
            for new in &values {
                let mut patches = Vec::new();
                let change = diff(old, new, &mut Vec::new(), &mut patches).unwrap();
                assert_eq!(json_len(old) as i128 + change, json_len(new) as i128);
                assert_eq!(json_len(new), serde_json::to_vec(new).unwrap().len());
            }
        }
    }

    #[test]
    fn cached_delta_still_enforces_exact_expanded_limit_and_unaccepted_base() {
        use exchange_core::{ParticipantConfig, ParticipantKind, RoomManager};
        let fixture: Value =
            serde_json::from_str(include_str!("../../scripts/fixtures/f6_batch_spec.json"))
                .unwrap();
        let mut rooms = RoomManager::new();
        rooms
            .create_room(serde_json::from_value(fixture["scenario"].clone()).unwrap())
            .unwrap();
        let mut request = BotDecisionRequest {
            protocol_version: "bot.v1".into(),
            plugin_id: "pine.strategy".into(),
            plugin_version: "2.0.0".into(),
            state_version: 1,
            seed: 7,
            participant: ParticipantConfig {
                participant_id: "buyer".into(),
                kind: ParticipantKind::RuleAgent,
                room_id: "f6-batch".into(),
                account_id: 20,
                instrument_id: Some("V-BTC-SPOT".into()),
            },
            config: serde_json::json!({}),
            state: serde_json::json!({"bars": vec!["字"; 100]}),
            observation: rooms
                .participant_observation("f6-batch", "V-BTC-SPOT", 20)
                .unwrap(),
        };
        let mut cache = RequestCache::default();
        let first = cache.encode(&request, 9, usize::MAX, false, None).unwrap();
        let initial_len = first.bytes.len();
        cache.accept(first);
        request.state["bars"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("\\\"\n"));
        // Include the 9 -> 10 sequence digit boundary in expanded size checks.
        let full = RequestCache::default()
            .encode(&request, 10, usize::MAX, false, None)
            .unwrap();
        assert!(full.bytes.len() > initial_len);
        assert!(cache.encode(&request, 10, full.bytes.len() - 1, false, None).is_err());
        let delta = cache.encode(&request, 10, full.bytes.len(), false, None).unwrap();
        let wire: Value = serde_json::from_slice(&delta.bytes).unwrap();
        assert_eq!(wire["base"], 9);
        assert!(delta.bytes.len() < full.bytes.len());
        // Encoding without success/accept must not advance the transport base.
        assert_eq!(
            serde_json::from_slice::<Value>(&cache.encode(&request, 11, usize::MAX, false, None).unwrap().bytes)
                .unwrap()["base"],
            9
        );
        cache.accept(delta);
        assert_eq!(
            serde_json::from_slice::<Value>(&cache.encode(&request, 12, usize::MAX, false, None).unwrap().bytes)
                .unwrap()["base"],
            10
        );
    }
}


/// A growing history list of an observation. Lists are compared as typed data,
/// so only their new items become JSON values; the cached head holds them
/// empty (or omitted, for lists serialized only when not empty).
enum History {
    Candles(Vec<Candle>),
    Trades(Vec<Trade>),
    Details(Vec<BotFillDetail>),
    Events(Vec<BotAccountEvent>),
}
struct Slot {
    path: Vec<Part>,
    omit_empty: bool,
    history: History,
    /// Compact JSON length of the list.
    bytes: usize,
}
impl Slot {
    fn is_empty(&self) -> bool {
        match &self.history {
            History::Candles(items) => items.is_empty(),
            History::Trades(items) => items.is_empty(),
            History::Details(items) => items.is_empty(),
            History::Events(items) => items.is_empty(),
        }
    }
    /// What the list adds to the request's JSON beyond the cached head.
    fn extra(&self) -> usize {
        match (self.omit_empty, self.is_empty()) {
            (false, _) => self.bytes - 2,
            (true, true) => 0,
            // `,"name":[...]`
            (true, false) => json_len(self.path.last().unwrap()) + 1 + self.bytes + 1,
        }
    }
}

/// Moves the history lists out of `observation` into `slots`.
fn take_histories(observation: &mut ParticipantObservation, path: &mut Vec<Part>, slots: &mut Vec<Slot>) {
    if let Some(data) = observation.bot_market_data.as_mut() {
        path.push(Part::Key("bot_market_data".into()));
        let lists = [
            ("candles", false, History::Candles(std::mem::take(&mut data.candles))),
            ("own_fills", false, History::Trades(std::mem::take(&mut data.own_fills))),
            ("fill_details", false, History::Details(std::mem::take(&mut data.fill_details))),
            ("account_events", true, History::Events(std::mem::take(&mut data.account_events))),
        ];
        for (name, omit_empty, history) in lists {
            let mut path = path.clone();
            path.push(Part::Key(name.into()));
            slots.push(Slot { path, omit_empty, history, bytes: 0 });
        }
        for (index, stream) in data.streams.iter_mut().enumerate() {
            let mut path = path.clone();
            path.extend([Part::Key("streams".into()), Part::Index(index), Part::Key("candles".into())]);
            let history = History::Candles(std::mem::take(&mut stream.candles));
            slots.push(Slot { path, omit_empty: false, history, bytes: 0 });
        }
        path.pop();
    }
    for (index, related) in observation.related_markets.iter_mut().enumerate() {
        path.extend([Part::Key("related_markets".into()), Part::Index(index)]);
        take_histories(related, path, slots);
        path.truncate(path.len() - 2);
    }
}

/// The patch taking the worker's copy of a list from `old` (None: the cached
/// head's empty form) to `new`, and the new list's JSON length.
fn list_patch<T: Serialize + PartialEq>(
    path: &[Part],
    omit_empty: bool,
    old: Option<(&[T], usize)>,
    new: &[T],
) -> Result<(Option<Patch>, usize), BotError> {
    let wire = |e: serde_json::Error| BotError(e.to_string());
    let (old_items, old_bytes) = old.unwrap_or((&[], 2));
    // An omitted empty list has no key in the worker's copy.
    let present = !omit_empty || !old_items.is_empty();
    if new.len() >= old_items.len() && new[..old_items.len()] == *old_items {
        if new.len() == old_items.len() {
            return Ok((None, old_bytes));
        }
        let values = new[old_items.len()..]
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(wire)?;
        if present {
            let bytes = old_bytes + values.iter().map(json_len).sum::<usize>() + values.len()
                - usize::from(old_items.is_empty());
            let patch = Patch::Append {
                path: path.to_vec(),
                from: old_items.len(),
                values,
            };
            return Ok((Some(patch), bytes));
        }
        let value = Value::Array(values);
        let bytes = json_len(&value);
        return Ok((Some(Patch::Set { path: path.to_vec(), value }), bytes));
    }
    if omit_empty && new.is_empty() {
        return Ok((Some(Patch::Remove { path: path.to_vec() }), 2));
    }
    let value = serde_json::to_value(new).map_err(wire)?;
    let bytes = json_len(&value);
    Ok((Some(Patch::Set { path: path.to_vec(), value }), bytes))
}

/// `list_patch` for a slot against an older slot (None: none, or replaced).
fn slot_patch(slot: &Slot, old: Option<&Slot>) -> Result<(Option<Patch>, usize), BotError> {
    macro_rules! typed {
        ($variant:ident, $items:expr) => {{
            let old = match old.map(|old| (&old.history, old.bytes)) {
                Some((History::$variant(items), bytes)) => Some((items.as_slice(), bytes)),
                _ => None,
            };
            list_patch(&slot.path, slot.omit_empty, old, $items)
        }};
    }
    match &slot.history {
        History::Candles(items) => typed!(Candles, items),
        History::Trades(items) => typed!(Trades, items),
        History::Details(items) => typed!(Details, items),
        History::Events(items) => typed!(Events, items),
    }
}

fn list_bytes(slot: &Slot) -> usize {
    match &slot.history {
        History::Candles(items) => json_len(items),
        History::Trades(items) => json_len(items),
        History::Details(items) => json_len(items),
        History::Events(items) => json_len(items),
    }
}

/// What a bot knows about a worker's cached state: equal to the bot's state
/// except at the `changed` top-level keys, which the host set after the worker
/// request `base` whose reply delta both sides folded.
pub(super) struct StateHint {
    pub base: u64,
    pub changed: BTreeSet<String>,
}

/// The state patches of a `StateHint`: only the changed keys are compared.
fn changed_keys(
    old: &serde_json::Map<String, Value>,
    new: &serde_json::Map<String, Value>,
    changed: &BTreeSet<String>,
    patches: &mut Vec<Patch>,
) -> Option<i128> {
    let mut change = new.len().saturating_sub(1) as i128 - old.len().saturating_sub(1) as i128;
    let mut path = vec![Part::Key("state".into())];
    for key in changed {
        path.push(Part::Key(key.clone()));
        match (old.get(key), new.get(key)) {
            (Some(old), Some(new)) => change += diff(old, new, &mut path, patches)?,
            (None, Some(new)) => {
                change += (json_len(key) + 1 + json_len(new)) as i128;
                patches.push(Patch::Set {
                    path: path.clone(),
                    value: new.clone(),
                });
            }
            (Some(old), None) => {
                change -= (json_len(key) + 1 + json_len(old)) as i128;
                patches.push(Patch::Remove { path: path.clone() });
            }
            (None, None) => {}
        }
        path.pop();
    }
    (patches.len() <= PATCH_LIMIT).then_some(change)
}

/// A cached request: the request without its state (`state` null) and with
/// its history lists emptied, those lists, and the state. Lists and state are
/// diffed directly instead of being converted on every call.
struct Entry {
    id: u64,
    head: Value,
    slots: Vec<Slot>,
    state: Value,
    bytes: usize,
}
#[derive(Default)]
pub(super) struct RequestCache {
    entries: BTreeMap<String, Entry>,
    recency: VecDeque<String>,
}
enum Sent {
    Full(Value),
    /// Patches after the first `.0`, all below `state`, change the cached state.
    Delta(usize, Vec<Patch>),
}
pub(super) struct Packet {
    pub bytes: Vec<u8>,
    key: String,
    id: u64,
    head: Value,
    slots: Vec<Slot>,
    state: Sent,
    value_bytes: usize,
}
impl Packet {
    pub fn key(&self) -> &str {
        &self.key
    }
}
#[derive(Serialize)]
struct Head<'a> {
    protocol_version: &'a str,
    plugin_id: &'a str,
    plugin_version: &'a str,
    state_version: u16,
    participant: &'a ParticipantConfig,
    seed: u64,
    config: &'a Value,
    state: (),
    observation: &'a ParticipantObservation,
}
impl RequestCache {
    /// `reply_state_delta` lets the worker reply with a state delta, which
    /// `fold` then applies to the cached request. With a `hint` naming the
    /// cached request, only its changed state keys are compared.
    pub fn encode(
        &self,
        request: &BotDecisionRequest,
        id: u64,
        limit: usize,
        reply_state_delta: bool,
        hint: Option<&StateHint>,
    ) -> Result<Packet, BotError> {
        let wire = |e: serde_json::Error| BotError(e.to_string());
        let participant = serde_json::to_vec(&request.participant).map_err(wire)?;
        let key = format!("{:x}", Sha256::digest(participant));
        #[cfg(feature = "bench-execution-timing")]
        let head_timer =
            crate::bench_execution_timing::Timer::start(&crate::bench_execution_timing::BOT_WIRE_HEAD);
        let mut observation = request.observation.clone();
        let mut slots = Vec::new();
        take_histories(&mut observation, &mut vec![Part::Key("observation".into())], &mut slots);
        let head = serde_json::to_value(Head {
            protocol_version: &request.protocol_version,
            plugin_id: &request.plugin_id,
            plugin_version: &request.plugin_version,
            state_version: request.state_version,
            participant: &request.participant,
            seed: request.seed,
            config: &request.config,
            state: (),
            observation: &observation,
        })
        .map_err(wire)?;
        #[derive(Serialize)]
        struct Full<'a, T: Serialize> {
            protocol: &'static str,
            id: u64,
            cache_key: &'a str,
            #[serde(skip_serializing_if = "std::ops::Not::not")]
            reply_state_delta: bool,
            request: &'a T,
        }
        let overhead = json_len(&Full {
            protocol: "bot.worker.v1",
            id,
            cache_key: &key,
            reply_state_delta,
            request: &Value::Null,
        }) - 4;
        let mut selected = None;
        if let Some(entry) = self.entries.get(&key) {
            let mut patches = Vec::new();
            let mut change = diff(&entry.head, &head, &mut Vec::new(), &mut patches);
            // A head patch on a list's container replaced the worker's copy
            // of the list with the head's empty form.
            let replaced = |path: &[Part]| patches.iter().any(|patch| path.starts_with(patch.path()));
            let mut lists = Vec::new();
            for slot in slots.iter_mut() {
                let old = entry
                    .slots
                    .iter()
                    .find(|old| old.path == slot.path)
                    .filter(|old| !replaced(&old.path));
                let (patch, bytes) = slot_patch(slot, old)?;
                slot.bytes = bytes;
                lists.extend(patch);
            }
            change = change.map(|change| {
                change + slots.iter().map(|slot| slot.extra() as i128).sum::<i128>()
                    - entry.slots.iter().map(|slot| slot.extra() as i128).sum::<i128>()
            });
            patches.extend(lists);
            #[cfg(feature = "bench-execution-timing")]
            drop(head_timer);
            #[cfg(feature = "bench-execution-timing")]
            let _state_timer =
                crate::bench_execution_timing::Timer::start(&crate::bench_execution_timing::BOT_WIRE_STATE_DIFF);
            let split = patches.len();
            let mut path = vec![Part::Key("state".into())];
            let change = change.filter(|_| patches.len() <= PATCH_LIMIT).and_then(|change| {
                let state = match (hint, &entry.state, &request.state) {
                    (Some(hint), Value::Object(old), Value::Object(new)) if hint.base == entry.id => {
                        changed_keys(old, new, &hint.changed, &mut patches)
                    }
                    _ => diff(&entry.state, &request.state, &mut path, &mut patches),
                };
                Some(change + state?)
            });
            if let Some(change) = change {
                let value_bytes =
                    usize::try_from(entry.bytes as i128 + change).expect("exact JSON length delta");
                // A compact delta must never hide an oversized expanded input.
                if value_bytes + overhead + 1 > limit {
                    return Err(BotError("worker request exceeds size limit".into()));
                }
                #[derive(Serialize)]
                struct Delta<'a> {
                    protocol: &'static str,
                    id: u64,
                    cache_key: &'a str,
                    #[serde(skip_serializing_if = "std::ops::Not::not")]
                    reply_state_delta: bool,
                    base: u64,
                    request_delta: &'a [Patch],
                }
                let delta = serde_json::to_vec(&Delta {
                    protocol: "bot.worker.v1",
                    id,
                    cache_key: &key,
                    reply_state_delta,
                    base: entry.id,
                    request_delta: &patches,
                })
                .map_err(wire)?;
                if delta.len() < value_bytes + overhead {
                    selected = Some((delta, value_bytes, Sent::Delta(split, patches)));
                }
            }
        }
        let (mut bytes, value_bytes, state) = match selected {
            Some(selected) => selected,
            None => {
                let bytes = serde_json::to_vec(&Full {
                    protocol: "bot.worker.v1",
                    id,
                    cache_key: &key,
                    reply_state_delta,
                    request,
                })
                .map_err(wire)?;
                if bytes.len() + 1 > limit {
                    return Err(BotError("worker request exceeds size limit".into()));
                }
                for slot in slots.iter_mut() {
                    slot.bytes = list_bytes(slot);
                }
                let value_bytes = bytes.len() - overhead;
                (bytes, value_bytes, Sent::Full(request.state.clone()))
            }
        };
        bytes.push(b'\n');
        Ok(Packet {
            bytes,
            key,
            id,
            head,
            slots,
            state,
            value_bytes,
        })
    }
    /// Makes the accepted request's cached state the worker's new state, as
    /// the worker does once it sends a state delta.
    pub fn fold(&mut self, key: &str, patches: &[Patch]) -> Result<(), BotError> {
        let entry = self
            .entries
            .get_mut(key)
            .ok_or_else(|| BotError("worker state delta has no cached request".into()))?;
        let change = apply_patches(&mut entry.state, patches, 0)?;
        entry.bytes = usize::try_from(entry.bytes as i128 + change).expect("exact JSON length delta");
        Ok(())
    }
    pub fn accept(&mut self, packet: Packet) {
        if let Some(index) = self.recency.iter().position(|key| *key == packet.key) {
            self.recency.remove(index);
        }
        self.recency.push_back(packet.key.clone());
        let state = match packet.state {
            Sent::Full(state) => state,
            Sent::Delta(split, patches) => {
                // The delta was computed from this entry, which nothing changed since.
                let mut state = std::mem::take(&mut self.entries.get_mut(&packet.key).unwrap().state);
                let replaced = match &patches[split..] {
                    // A state that is not an object (the first, null) is replaced whole.
                    [Patch::Set { path, value }] if path.len() == 1 => {
                        state = value.clone();
                        Ok(0)
                    }
                    patches => apply_patches(&mut state, patches, 1),
                };
                if replaced.is_err() {
                    // Never expected; the next request for this bot is sent in full.
                    self.entries.remove(&packet.key);
                    return;
                }
                state
            }
        };
        self.entries.insert(
            packet.key,
            Entry {
                id: packet.id,
                head: packet.head,
                slots: packet.slots,
                state,
                bytes: packet.value_bytes,
            },
        );
        while self.recency.len() > CACHE_LIMIT {
            let oldest = self.recency.pop_front().unwrap();
            self.entries.remove(&oldest);
        }
    }
}
