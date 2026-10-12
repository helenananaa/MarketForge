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
fn diff(old: &Value, new: &Value, path: &mut Vec<Part>, patches: &mut Vec<Patch>) {
    if same(old, new) || patches.len() > PATCH_LIMIT {
        return;
    }
    match (old, new) {
        (Value::Object(old), Value::Object(new)) => {
            for key in old.keys().filter(|key| !new.contains_key(*key)) {
                path.push(Part::Key(key.clone()));
                patches.push(Patch::Remove { path: parts(path) });
                path.pop();
            }
            for (key, value) in new {
                path.push(Part::Key(key.clone()));
                match old.get(key) {
                    Some(old) => diff(old, value, path, patches),
                    None => patches.push(Patch::Set {
                        path: parts(path),
                        value: value.clone(),
                    }),
                }
                path.pop();
                if patches.len() > PATCH_LIMIT {
                    return;
                }
            }
        }
        (Value::Array(old), Value::Array(new))
            if new.len() >= old.len() && old.iter().zip(new).all(|(a, b)| same(a, b)) =>
        {
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
                diff(old, new, path, patches);
                path.pop();
            }
        }
        _ => patches.push(Patch::Set {
            path: parts(path),
            value: new.clone(),
        }),
    }
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
}

#[derive(Default)]
pub(super) struct RequestCache {
    entries: BTreeMap<String, (u64, Value)>,
    recency: VecDeque<String>,
}
pub(super) struct Packet {
    pub bytes: Vec<u8>,
    key: String,
    id: u64,
    value: Value,
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
        let mut bytes = serde_json::to_vec(&Full {
            protocol: "bot.worker.v1",
            id,
            cache_key: &key,
            request: &value,
        })
        .map_err(|e| BotError(e.to_string()))?;
        // Check the expanded request too: a compact wire must never hide an
        // input larger than the ordinary bot protocol allows.
        if bytes.len() + 1 > limit {
            return Err(BotError("worker request exceeds size limit".into()));
        }
        if let Some((base, old)) = self.entries.get(&key) {
            let mut patches = Vec::new();
            diff(old, &value, &mut Vec::new(), &mut patches);
            if patches.len() <= PATCH_LIMIT {
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
                if delta.len() < bytes.len() {
                    bytes = delta;
                }
            }
        }
        bytes.push(b'\n');
        Ok(Packet {
            bytes,
            key,
            id,
            value,
        })
    }
    pub fn accept(&mut self, packet: Packet) {
        if let Some(index) = self.recency.iter().position(|key| *key == packet.key) {
            self.recency.remove(index);
        }
        self.recency.push_back(packet.key.clone());
        self.entries.insert(packet.key, (packet.id, packet.value));
        while self.recency.len() > CACHE_LIMIT {
            let oldest = self.recency.pop_front().unwrap();
            self.entries.remove(&oldest);
        }
    }
}
