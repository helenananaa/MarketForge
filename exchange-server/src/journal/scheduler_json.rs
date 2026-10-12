//! Derived, bounded encoding cache for immutable plugin JSON. Journal records
//! still contain the complete original state with the same checked JSON schema.
use super::{JournalError, RecoveryJson, RoomMutation};
use crate::scheduler_delta::{AgentDelta, SchedulerDelta};
use exchange_core::{PersistedAgent, PersistedAgentKindState, SchedulerState};
use serde::{
    Serialize, Serializer,
    ser::{SerializeSeq, SerializeStruct, SerializeStructVariant},
};
use serde_json::{Value, value::RawValue};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Weak},
};

const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;

#[derive(Clone, Debug)]
struct Entry {
    source: Weak<Value>,
    json: Arc<RawValue>,
    touched: u64,
}

#[derive(Clone, Debug, Default)]
pub(super) struct Cache {
    entries: BTreeMap<usize, Entry>,
    recency: BTreeSet<(u64, usize)>,
    bytes: usize,
    clock: u64,
}

impl Cache {
    fn get(&mut self, data: &Arc<Value>) -> Result<Arc<RawValue>, JournalError> {
        if self.clock == u64::MAX {
            // Preserve LRU order even if the logical access counter wraps.
            let keys: Vec<_> = self.recency.iter().map(|(_, key)| *key).collect();
            self.recency.clear();
            for (touched, key) in keys.into_iter().enumerate() {
                self.entries.get_mut(&key).unwrap().touched = touched as u64;
                self.recency.insert((touched as u64, key));
            }
            self.clock = self.entries.len() as u64;
        }
        self.clock += 1;
        let key = Arc::as_ptr(data) as usize;
        if let Some(entry) = self.entries.get_mut(&key)
            && entry
                .source
                .upgrade()
                .is_some_and(|source| Arc::ptr_eq(&source, data))
        {
            self.recency.remove(&(entry.touched, key));
            entry.touched = self.clock;
            self.recency.insert((entry.touched, key));
            return Ok(entry.json.clone());
        }
        // This fragment is already a serde_json::Value: its numbers have the
        // recovery representation's i64/u64/f64 range. to_raw_value emits JSON
        // directly, without reparsing a string we just serialized. The enclosing
        // typed mutation still uses RecoveryJson's i128/u128 range checks.
        // Weak identity also prevents reuse after expiration or a COW update.
        let json: Arc<RawValue> = serde_json::value::to_raw_value(data.as_ref())
            .map_err(JournalError::Serialize)?
            .into();
        if let Some(old) = self.entries.remove(&key) {
            self.recency.remove(&(old.touched, key));
            self.bytes -= old.json.get().len();
        }
        let size = json.get().len();
        if size > MAX_BYTES {
            return Ok(json);
        }
        // A retained journal may keep every source alive. Scanning the whole
        // cache for expired entries and an LRU victim on each saturated miss
        // made insertion quadratic in the working set. Evict by ordered access
        // index instead; dead entries are bounded by the same limits.
        while self.bytes.saturating_add(size) > MAX_BYTES || self.entries.len() >= MAX_ENTRIES {
            let (_, key) = self.recency.pop_first().unwrap();
            self.bytes -= self.entries.remove(&key).unwrap().json.get().len();
        }
        self.bytes += size;
        self.recency.insert((self.clock, key));
        self.entries.insert(
            key,
            Entry {
                source: Arc::downgrade(data),
                json: json.clone(),
                touched: self.clock,
            },
        );
        Ok(json)
    }
}

pub(super) fn encode(
    mutation: &RoomMutation,
    cache: &mut Cache,
) -> Result<RecoveryJson, JournalError> {
    // Small native states change on every decision. Fragment encoding and
    // cache maintenance cost more than their original direct serialization.
    let is_pine = |state: &PersistedAgentKindState| matches!(state, PersistedAgentKindState::Plugin { plugin_id, .. } if plugin_id == "pine.strategy");
    let has_pine = match mutation {
        RoomMutation::SchedulerProgress { state, .. } => {
            state.agents.iter().any(|agent| is_pine(&agent.kind_state))
        }
        RoomMutation::SchedulerDelta { delta, .. } => {
            delta
                .state
                .agents
                .iter()
                .any(|agent| is_pine(&agent.kind_state))
                || delta.changes.iter().any(|agent| is_pine(&agent.kind_state))
        }
        _ => false,
    };
    if !has_pine {
        return RecoveryJson::encode(mutation);
    }
    // RefCell allows serde's shared Serialize receiver to access this one-call
    // exclusive cache borrow. It is neither shared across threads nor durable.
    let cache = RefCell::new(cache);
    RecoveryJson::encode(&Mutation {
        mutation,
        cache: &cache,
    })
}

struct Mutation<'a, 'b> {
    mutation: &'a RoomMutation,
    cache: &'a RefCell<&'b mut Cache>,
}
impl Serialize for Mutation<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.mutation {
            RoomMutation::SchedulerProgress {
                clock_steps,
                state,
                training,
            } => {
                let mut s = serializer
                    .serialize_struct("RoomMutation", 3 + usize::from(training.is_some()))?;
                s.serialize_field("kind", "scheduler_progress")?;
                s.serialize_field("clock_steps", clock_steps)?;
                s.serialize_field(
                    "state",
                    &State {
                        state,
                        cache: self.cache,
                    },
                )?;
                if let Some(training) = training {
                    s.serialize_field("training", training)?;
                }
                s.end()
            }
            RoomMutation::SchedulerDelta {
                clock_steps,
                delta,
                training,
            } => {
                let mut s = serializer
                    .serialize_struct("RoomMutation", 3 + usize::from(training.is_some()))?;
                s.serialize_field("kind", "scheduler_delta")?;
                s.serialize_field("clock_steps", clock_steps)?;
                s.serialize_field(
                    "delta",
                    &Delta {
                        delta,
                        cache: self.cache,
                    },
                )?;
                if let Some(training) = training {
                    s.serialize_field("training", training)?;
                }
                s.end()
            }
            other => other.serialize(serializer),
        }
    }
}

struct State<'a, 'b> {
    state: &'a SchedulerState,
    cache: &'a RefCell<&'b mut Cache>,
}
impl Serialize for State<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Exhaustive destructuring makes future schema additions compile errors
        // until the cached wire serializer is updated too.
        let SchedulerState {
            version,
            revision,
            room_id,
            mode,
            bots_enabled,
            catch_up_limit,
            lagged,
            continuity,
            phase,
            agents,
        } = self.state;
        let mut s = serializer.serialize_struct("SchedulerState", 10)?;
        s.serialize_field("version", version)?;
        s.serialize_field("revision", revision)?;
        s.serialize_field("room_id", room_id)?;
        s.serialize_field("mode", mode)?;
        s.serialize_field("bots_enabled", bots_enabled)?;
        s.serialize_field("catch_up_limit", catch_up_limit)?;
        s.serialize_field("lagged", lagged)?;
        s.serialize_field("continuity", continuity)?;
        s.serialize_field("phase", phase)?;
        s.serialize_field(
            "agents",
            &Agents {
                agents,
                cache: self.cache,
            },
        )?;
        s.end()
    }
}

struct Agents<'a, 'b> {
    agents: &'a [PersistedAgent],
    cache: &'a RefCell<&'b mut Cache>,
}
impl Serialize for Agents<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_seq(Some(self.agents.len()))?;
        for PersistedAgent {
            version,
            config_version,
            template,
            kind_state,
            unfinished_actions,
        } in self.agents
        {
            #[derive(Serialize)]
            struct Agent<'a, 'b> {
                version: &'a u16,
                config_version: &'a u16,
                template: &'a exchange_core::AgentTemplate,
                kind_state: Kind<'a, 'b>,
                unfinished_actions: &'a [exchange_core::OrderAction],
            }
            s.serialize_element(&Agent {
                version,
                config_version,
                template,
                kind_state: Kind {
                    state: kind_state,
                    cache: self.cache,
                },
                unfinished_actions,
            })?;
        }
        s.end()
    }
}

struct Kind<'a, 'b> {
    state: &'a PersistedAgentKindState,
    cache: &'a RefCell<&'b mut Cache>,
}
impl Serialize for Kind<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let PersistedAgentKindState::Plugin {
            plugin_id,
            plugin_version,
            state_version,
            data,
        } = self.state
        else {
            return self.state.serialize(serializer);
        };
        if plugin_id != "pine.strategy" {
            return self.state.serialize(serializer);
        }
        let json = self
            .cache
            .borrow_mut()
            .get(data)
            .map_err(serde::ser::Error::custom)?;
        let mut s =
            serializer.serialize_struct_variant("PersistedAgentKindState", 0, "Plugin", 4)?;
        s.serialize_field("plugin_id", plugin_id)?;
        s.serialize_field("plugin_version", plugin_version)?;
        s.serialize_field("state_version", state_version)?;
        s.serialize_field("data", json.as_ref())?;
        s.end()
    }
}

struct Delta<'a, 'b> {
    delta: &'a SchedulerDelta,
    cache: &'a RefCell<&'b mut Cache>,
}
impl Serialize for Delta<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let SchedulerDelta {
            version,
            base_revision,
            agent_count,
            state,
            changes,
        } = self.delta;
        let mut s = serializer.serialize_struct("SchedulerDelta", 5)?;
        s.serialize_field("version", version)?;
        s.serialize_field("base_revision", base_revision)?;
        s.serialize_field("agent_count", agent_count)?;
        s.serialize_field(
            "state",
            &State {
                state,
                cache: self.cache,
            },
        )?;
        s.serialize_field(
            "changes",
            &Changes {
                changes,
                cache: self.cache,
            },
        )?;
        s.end()
    }
}

struct Changes<'a, 'b> {
    changes: &'a [AgentDelta],
    cache: &'a RefCell<&'b mut Cache>,
}
impl Serialize for Changes<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_seq(Some(self.changes.len()))?;
        for AgentDelta {
            index,
            kind_state,
            unfinished_actions,
        } in self.changes
        {
            #[derive(Serialize)]
            struct Change<'a, 'b> {
                index: &'a usize,
                kind_state: Kind<'a, 'b>,
                unfinished_actions: &'a [exchange_core::OrderAction],
            }
            s.serialize_element(&Change {
                index,
                kind_state: Kind {
                    state: kind_state,
                    cache: self.cache,
                },
                unfinished_actions,
            })?;
        }
        s.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> SchedulerState {
        let recipe: exchange_core::population::BackgroundMarket = serde_json::from_str(
            include_str!("../../../scripts/fixtures/microstructure_market.json"),
        )
        .unwrap();
        let mut state = SchedulerState::new(
            "cache-\"\\房间",
            recipe.agents,
            exchange_core::SchedulerMode::Auto { interval_ms: 1000 },
        );
        state.revision = 177;
        state.bots_enabled = false;
        state.catch_up_limit = 17;
        state.lagged = true;
        state.continuity = exchange_core::AgentContinuity::LegacyNonContinuous;
        state.phase = exchange_core::SchedulerPhase::Submitting {
            step: 7,
            participant_index: 2,
            action_index: 1,
        };
        for agent in &mut state.agents {
            let PersistedAgentKindState::Plugin {
                data, plugin_id, ..
            } = &mut agent.kind_state
            else {
                unreachable!()
            };
            *plugin_id = "pine.strategy".into();
            *data = serde_json::json!({"bars":[{"close":1.25}], "limit":u64::MAX,
                "signed":i64::MIN, "quoted":"\\\"雪", "null":null, "flag":true})
            .into();
        }
        state.agents[0].kind_state = PersistedAgentKindState::Noise { rng_state: 19 };
        if let PersistedAgentKindState::Plugin { plugin_id, .. } = &mut state.agents[1].kind_state {
            *plugin_id = "builtin.native-test".into();
        }
        state
    }

    #[test]
    fn native_small_states_keep_original_encoder_and_do_not_enter_cache() {
        let mut state = state();
        for agent in &mut state.agents {
            if let PersistedAgentKindState::Plugin { plugin_id, .. } = &mut agent.kind_state {
                *plugin_id = "builtin.native-test".into();
            }
        }
        let mutation = RoomMutation::SchedulerProgress {
            clock_steps: 1,
            state,
            training: None,
        };
        let mut cache = Cache::default();
        assert_eq!(
            encode(&mutation, &mut cache).unwrap().0,
            RecoveryJson::encode(&mutation).unwrap().0
        );
        assert!(cache.entries.is_empty());
        assert_eq!(cache.bytes, 0);
    }

    #[test]
    fn cached_full_and_delta_keep_exact_wire_and_recover_all_metadata() {
        let state = state();
        let recipe: exchange_core::population::BackgroundMarket = serde_json::from_str(
            include_str!("../../../scripts/fixtures/microstructure_market.json"),
        )
        .unwrap();
        let training = exchange_core::TrainingRun::new(
            exchange_core::TrainingSpec::low_slippage_buy(
                "cache-training",
                recipe.scenario,
                recipe.agents,
                20,
                1,
                7,
                100,
            )
            .unwrap(),
        );
        let delta = SchedulerDelta {
            version: 1,
            base_revision: 176,
            agent_count: state.agents.len(),
            state: SchedulerDelta::metadata(&state),
            changes: state
                .agents
                .iter()
                .enumerate()
                .map(|(index, a)| AgentDelta {
                    index,
                    kind_state: a.kind_state.clone(),
                    unfinished_actions: a.unfinished_actions.clone(),
                })
                .collect(),
        };
        let mut cache = Cache::default();
        for mut mutation in [
            RoomMutation::SchedulerDelta {
                clock_steps: 3,
                delta,
                training: None,
            },
            RoomMutation::SchedulerProgress {
                clock_steps: 2,
                state,
                training: None,
            },
        ] {
            for with_training in [false, true] {
                if with_training {
                    match &mut mutation {
                        RoomMutation::SchedulerDelta {
                            training: value, ..
                        }
                        | RoomMutation::SchedulerProgress {
                            training: value, ..
                        } => *value = Some(Box::new(training.clone())),
                        _ => unreachable!(),
                    }
                }
                let legacy = RecoveryJson::encode(&mutation).unwrap().0;
                let encoded = encode(&mutation, &mut cache).unwrap().0;
                assert_eq!(
                    std::str::from_utf8(&encoded).unwrap(),
                    std::str::from_utf8(&legacy).unwrap()
                );
                let recovered: RoomMutation = serde_json::from_slice(&encoded).unwrap();
                assert_eq!(
                    serde_json::to_value(recovered).unwrap(),
                    serde_json::to_value(&mutation).unwrap()
                );
            }
            match &mut mutation {
                RoomMutation::SchedulerDelta {
                    training: Some(value),
                    ..
                }
                | RoomMutation::SchedulerProgress {
                    training: Some(value),
                    ..
                } => value.fees_paid = i128::from(u64::MAX) + 1,
                _ => unreachable!(),
            }
            assert!(RecoveryJson::encode(&mutation).is_err());
            assert!(
                encode(&mutation, &mut cache).is_err(),
                "warm fragments cannot bypass recovery-range checks"
            );
        }
        assert_eq!(cache.entries.len(), 36);
    }

    #[test]
    fn immutable_identity_cache_cannot_hide_cow_updates_or_expired_pointer_reuse() {
        let mut cache = Cache::default();
        let mut value = Arc::new(serde_json::json!({"cash": 100}));
        let original = value.clone();
        let encoded = cache.get(&value).unwrap();
        assert!(Arc::ptr_eq(&encoded, &cache.get(&value).unwrap()));
        Arc::make_mut(&mut value)["cash"] = 71.into();
        assert_eq!(cache.get(&value).unwrap().get(), "{\"cash\":71}");
        assert_eq!(cache.get(&original).unwrap().get(), "{\"cash\":100}");
        drop(original);
        drop(value);
        // Explicitly expire a weak source under the next value's address,
        // modelling allocator address reuse without relying on the allocator.
        let next = Arc::new(serde_json::json!({"cash": 49}));
        cache.entries.clear();
        cache.recency.clear();
        cache.recency.insert((0, Arc::as_ptr(&next) as usize));
        cache.bytes = encoded.get().len();
        cache.entries.insert(
            Arc::as_ptr(&next) as usize,
            Entry {
                source: Weak::new(),
                json: encoded,
                touched: 0,
            },
        );
        assert_eq!(cache.get(&next).unwrap().get(), "{\"cash\":49}");
        assert_eq!(cache.recency.len(), cache.entries.len());
    }

    #[test]
    fn saturated_cache_evicts_least_recent_after_hits_and_counter_rollover() {
        let mut cache = Cache::default();
        let values: Vec<_> = (0..MAX_ENTRIES + 2)
            .map(|n| Arc::new(serde_json::json!({"n":n})))
            .collect();
        for value in &values[..MAX_ENTRIES] {
            cache.get(value).unwrap();
        }
        let first = cache.get(&values[0]).unwrap();
        cache.clock = u64::MAX;
        cache.get(&values[MAX_ENTRIES]).unwrap();
        assert!(Arc::ptr_eq(&first, &cache.get(&values[0]).unwrap()));
        assert!(
            !cache
                .entries
                .contains_key(&(Arc::as_ptr(&values[1]) as usize))
        );
        cache.get(&values[MAX_ENTRIES + 1]).unwrap();
        assert!(
            !cache
                .entries
                .contains_key(&(Arc::as_ptr(&values[2]) as usize))
        );
        assert_eq!(cache.recency.len(), MAX_ENTRIES);
        assert_eq!(cache.entries.len(), MAX_ENTRIES);
        assert_eq!(
            cache.bytes,
            cache
                .entries
                .values()
                .map(|e| e.json.get().len())
                .sum::<usize>()
        );
        for (touched, key) in &cache.recency {
            assert_eq!(cache.entries[key].touched, *touched);
        }
        assert_eq!(cache.get(&values[1]).unwrap().get(), "{\"n\":1}");
    }

    #[test]
    fn cache_is_bounded_and_eviction_does_not_change_payloads() {
        let mut cache = Cache::default();
        let values: Vec<_> = (0..MAX_ENTRIES + 1)
            .map(|n| Arc::new(serde_json::json!({"n":n})))
            .collect();
        for value in &values {
            cache.get(value).unwrap();
        }
        assert_eq!(cache.entries.len(), MAX_ENTRIES);
        assert!(cache.bytes <= MAX_BYTES);
        assert_eq!(cache.get(&values[0]).unwrap().get(), "{\"n\":0}");
        let large = Arc::new(Value::String("x".repeat(MAX_BYTES)));
        assert_eq!(cache.get(&large).unwrap().get().len(), MAX_BYTES + 2);
        assert_eq!(cache.entries.len(), MAX_ENTRIES);
        assert!(cache.bytes <= MAX_BYTES);
    }
}
