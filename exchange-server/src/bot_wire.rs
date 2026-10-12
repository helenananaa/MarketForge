//! Transport caches contain prior host inputs, never worker-produced strategy state.
use exchange_core::{BotDecisionRequest, BotError};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};

const CACHE_LIMIT: usize = 512;
const PATCH_LIMIT: usize = 256;

#[derive(Serialize)]
#[serde(untagged)]
enum Part {
    Key(String),
    Index(usize),
}
#[derive(Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Patch {
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

fn parts(path: &[Part]) -> Vec<Part> {
    path.iter()
        .map(|p| match p {
            Part::Key(k) => Part::Key(k.clone()),
            Part::Index(i) => Part::Index(*i),
        })
        .collect()
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
        (Value::Object(old), Value::Object(new)) => {
            old.len() == new.len()
                && old
                    .iter()
                    .all(|(key, value)| new.get(key).is_some_and(|other| same(value, other)))
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

fn diff(old: &Value, new: &Value, path: &mut Vec<Part>, patches: &mut Vec<Patch>) -> Option<i128> {
    if patches.len() > PATCH_LIMIT {
        return None;
    }
    if same(old, new) {
        return Some(0);
    }
    let mut length_delta = 0;
    match (old, new) {
        (Value::Object(old), Value::Object(new)) => {
            length_delta +=
                new.len().saturating_sub(1) as i128 - old.len().saturating_sub(1) as i128;
            for key in old.keys().filter(|key| !new.contains_key(*key)) {
                length_delta -= (json_len(key) + 1 + json_len(&old[key])) as i128;
                path.push(Part::Key(key.clone()));
                patches.push(Patch::Remove { path: parts(path) });
                path.pop();
            }
            for (key, value) in new {
                path.push(Part::Key(key.clone()));
                match old.get(key) {
                    Some(old) => length_delta += diff(old, value, path, patches)?,
                    None => {
                        length_delta += (json_len(key) + 1 + json_len(value)) as i128;
                        patches.push(Patch::Set {
                            path: parts(path),
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
            let tail = &new[old.len()..];
            length_delta += (json_len(tail) - 2 + usize::from(!old.is_empty())) as i128;
            patches.push(Patch::Append {
                path: parts(path),
                from: old.len(),
                values: new[old.len()..].to_vec(),
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
        _ => {
            length_delta += json_len(new) as i128 - json_len(old) as i128;
            patches.push(Patch::Set {
                path: parts(path),
                value: new.clone(),
            });
        }
    }
    (patches.len() <= PATCH_LIMIT).then_some(length_delta)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let first = cache.encode(&request, 9, usize::MAX).unwrap();
        let initial_len = first.bytes.len();
        cache.accept(first);
        request.state["bars"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("\\\"\n"));
        // Include the 9 -> 10 sequence digit boundary in expanded size checks.
        let full = RequestCache::default()
            .encode(&request, 10, usize::MAX)
            .unwrap();
        assert!(full.bytes.len() > initial_len);
        assert!(cache.encode(&request, 10, full.bytes.len() - 1).is_err());
        let delta = cache.encode(&request, 10, full.bytes.len()).unwrap();
        let wire: Value = serde_json::from_slice(&delta.bytes).unwrap();
        assert_eq!(wire["base"], 9);
        assert!(delta.bytes.len() < full.bytes.len());
        // Encoding without success/accept must not advance the transport base.
        assert_eq!(
            serde_json::from_slice::<Value>(&cache.encode(&request, 11, usize::MAX).unwrap().bytes)
                .unwrap()["base"],
            9
        );
        cache.accept(delta);
        assert_eq!(
            serde_json::from_slice::<Value>(&cache.encode(&request, 12, usize::MAX).unwrap().bytes)
                .unwrap()["base"],
            10
        );
    }
}

#[derive(Default)]
pub(super) struct RequestCache {
    entries: BTreeMap<String, (u64, Value, usize)>,
    recency: VecDeque<String>,
}
pub(super) struct Packet {
    pub bytes: Vec<u8>,
    key: String,
    id: u64,
    value: Value,
    value_bytes: usize,
}
impl RequestCache {
    pub fn encode(
        &self,
        request: &BotDecisionRequest,
        id: u64,
        limit: usize,
    ) -> Result<Packet, BotError> {
        let participant =
            serde_json::to_vec(&request.participant).map_err(|e| BotError(e.to_string()))?;
        let key = format!("{:x}", Sha256::digest(participant));
        let value = serde_json::to_value(request).map_err(|e| BotError(e.to_string()))?;
        #[derive(Serialize)]
        struct Full<'a> {
            protocol: &'static str,
            id: u64,
            cache_key: &'a str,
            request: &'a Value,
        }
        let full = Full {
            protocol: "bot.worker.v1",
            id,
            cache_key: &key,
            request: &value,
        };
        let overhead = json_len(&Full {
            request: &Value::Null,
            ..full
        }) - 4;
        let mut selected = None;
        if let Some((base, old, old_bytes)) = self.entries.get(&key) {
            let mut patches = Vec::new();
            if let Some(change) = diff(old, &value, &mut Vec::new(), &mut patches) {
                let value_bytes =
                    usize::try_from(*old_bytes as i128 + change).expect("exact JSON length delta");
                // A compact delta must never hide an oversized expanded input.
                if value_bytes + overhead + 1 > limit {
                    return Err(BotError("worker request exceeds size limit".into()));
                }
                #[derive(Serialize)]
                struct Delta<'a> {
                    protocol: &'static str,
                    id: u64,
                    cache_key: &'a str,
                    base: u64,
                    request_delta: &'a [Patch],
                }
                let delta = serde_json::to_vec(&Delta {
                    protocol: "bot.worker.v1",
                    id,
                    cache_key: &key,
                    base: *base,
                    request_delta: &patches,
                })
                .map_err(|e| BotError(e.to_string()))?;
                if delta.len() < value_bytes + overhead {
                    selected = Some((delta, value_bytes));
                }
            }
        }
        let (mut bytes, value_bytes) = match selected {
            Some(selected) => selected,
            None => {
                let bytes = serde_json::to_vec(&full).map_err(|e| BotError(e.to_string()))?;
                if bytes.len() + 1 > limit {
                    return Err(BotError("worker request exceeds size limit".into()));
                }
                let value_bytes = bytes.len() - overhead;
                (bytes, value_bytes)
            }
        };
        bytes.push(b'\n');
        Ok(Packet {
            bytes,
            key,
            id,
            value,
            value_bytes,
        })
    }
    pub fn accept(&mut self, packet: Packet) {
        if let Some(index) = self.recency.iter().position(|key| *key == packet.key) {
            self.recency.remove(index);
        }
        self.recency.push_back(packet.key.clone());
        self.entries
            .insert(packet.key, (packet.id, packet.value, packet.value_bytes));
        while self.recency.len() > CACHE_LIMIT {
            let oldest = self.recency.pop_front().unwrap();
            self.entries.remove(&oldest);
        }
    }
}
