//! Append-only Pine journal state. The wire carries tails, not historical JSON.
//! A live hint is reusable only for the exact immutable predecessor and result.
use crate::journal::JournalError;
use exchange_core::{OrderAction, PersistedAgentKindState};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Write, sync::Arc};

const ARRAYS: [&str; 3] = ["bars", "accounts", "evaluations"];
const VM_ARRAYS: [&str; 2] = ["vm_tail", "vm_tail_intents"];

fn history_arrays(schema: u64) -> Option<&'static [&'static str]> {
    match schema {
        2 => Some(&ARRAYS),
        3 => Some(&VM_ARRAYS),
        _ => None,
    }
}

fn reconstructed_field(schema: u64) -> &'static str {
    if schema == 3 {
        "vm_checkpoint"
    } else {
        "providers"
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Append {
    base_len: usize,
    items: Vec<Value>,
}

#[derive(Clone, Debug)]
struct Prepared {
    base: PersistedAgentKindState,
    next: PersistedAgentKindState,
}

// This proof is private and never appears on the wire. Owning strong payload
// references both prevents address reuse and forces any Arc::make_mut edit to
// detach. Header equality and all three allocation identities are required;
// this certifies only shape/checksum validation, never predecessor identity.
#[derive(Debug)]
struct ValidatedPayload {
    plugin_version: String,
    state_version: u16,
    base_sha256: [u8; 32],
    metadata: Arc<Map<String, Value>>,
    arrays: Arc<BTreeMap<String, Append>>,
    providers: Arc<BTreeMap<String, Append>>,
    patch_sha256: [u8; 32],
}

impl ValidatedPayload {
    fn matches(&self, hint: &PineStateHint) -> bool {
        self.plugin_version == hint.plugin_version
            && self.state_version == hint.state_version
            && self.base_sha256 == hint.base_sha256
            && self.patch_sha256 == hint.patch_sha256
            && Arc::ptr_eq(&self.metadata, &hint.metadata)
            && Arc::ptr_eq(&self.arrays, &hint.arrays)
            && Arc::ptr_eq(&self.providers, &hint.providers)
    }
}

/// Construct in a compute task, before entering the ordered room writer.
/// Deserialized hints never receive the in-memory identity authorization.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PineStateHint {
    plugin_version: String,
    state_version: u16,
    base_sha256: [u8; 32],
    // Immutable wire payloads cross the ready queue and ordered writer several
    // times. Sharing them avoids copying metadata and append JSON at each hop.
    // Arc serializes transparently; prepared identity remains separate below.
    metadata: Arc<Map<String, Value>>,
    arrays: Arc<BTreeMap<String, Append>>,
    providers: Arc<BTreeMap<String, Append>>,
    patch_sha256: [u8; 32],
    #[serde(skip)]
    prepared: Option<Prepared>,
    #[serde(skip)]
    validated_payload: Option<Arc<ValidatedPayload>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PineAgentDelta {
    pub(super) index: usize,
    state: PineStateHint,
    unfinished_actions: Vec<OrderAction>,
}

fn plugin(state: &PersistedAgentKindState) -> Option<(&str, u16, &Arc<Value>)> {
    match state {
        PersistedAgentKindState::Plugin {
            plugin_id,
            plugin_version,
            state_version,
            data,
        } if plugin_id == "pine.strategy" => Some((plugin_version, *state_version, data)),
        _ => None,
    }
}

fn same_identity(a: &PersistedAgentKindState, b: &PersistedAgentKindState) -> bool {
    match (plugin(a), plugin(b)) {
        (Some((av, asv, ad)), Some((bv, bsv, bd))) => av == bv && asv == bsv && Arc::ptr_eq(ad, bd),
        _ => false,
    }
}

// PostgreSQL JSONB normalizes number spelling: e.g. 1e18 can return as an
// integer and -0.0 as 0.0. Hash exact decimal digits and exponent, never a lossy
// conversion of i64/u64 through f64. This also makes patch checksums portable
// between the memory wire and JSONB recovery.
struct IdentityFormatter;

fn canonical_number(writer: &mut impl Write, spelling: &str) -> std::io::Result<()> {
    let (negative, spelling) = match spelling.strip_prefix('-') {
        Some(unsigned) => (true, unsigned),
        None => (false, spelling),
    };
    let (mantissa, exponent) =
        spelling
            .split_once(['e', 'E'])
            .map_or((spelling, 0), |(mantissa, exponent)| {
                (
                    mantissa,
                    exponent
                        .parse::<i32>()
                        .expect("finite JSON number exponent"),
                )
            });
    let fraction = mantissa
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len());
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let significant = digits.trim_start_matches('0').trim_end_matches('0');
    if significant.is_empty() {
        return writer.write_all(b"0");
    }
    let trailing = digits.len() - digits.trim_end_matches('0').len();
    let exponent = exponent - fraction as i32 + trailing as i32;
    if negative {
        writer.write_all(b"-")?;
    }
    writer.write_all(significant.as_bytes())?;
    writer.write_all(b"e")?;
    write!(writer, "{exponent}")
}

macro_rules! identity_integer {
    ($name:ident, $ty:ty) => {
        fn $name<W: ?Sized + Write>(&mut self, writer: &mut W, value: $ty) -> std::io::Result<()> {
            canonical_number(&mut &mut *writer, &value.to_string())
        }
    };
}

impl serde_json::ser::Formatter for IdentityFormatter {
    identity_integer!(write_i8, i8);
    identity_integer!(write_i16, i16);
    identity_integer!(write_i32, i32);
    identity_integer!(write_i64, i64);
    identity_integer!(write_i128, i128);
    identity_integer!(write_u8, u8);
    identity_integer!(write_u16, u16);
    identity_integer!(write_u32, u32);
    identity_integer!(write_u64, u64);
    identity_integer!(write_u128, u128);
    identity_integer!(write_f32, f32);
    identity_integer!(write_f64, f64);
}

fn fingerprint(value: &impl Serialize) -> Result<[u8; 32], JournalError> {
    struct HashWriter(Sha256);
    impl Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = HashWriter(Sha256::new());
    let mut serializer = serde_json::Serializer::with_formatter(&mut writer, IdentityFormatter);
    value
        .serialize(&mut serializer)
        .map_err(JournalError::Serialize)?;
    Ok(writer.0.finalize().into())
}

fn append(before: &Value, after: &Value) -> Option<Append> {
    let (before, after) = (before.as_array()?, after.as_array()?);
    if !after.starts_with(before) {
        return None;
    }
    Some(Append {
        base_len: before.len(),
        items: after[before.len()..].to_vec(),
    })
}

fn failure(message: &str) -> JournalError {
    JournalError::Recovery(format!("invalid Pine scheduler append: {message}"))
}

impl PineStateHint {
    /// Fall back to a complete state for initialization, corrections, schema or
    /// identity changes, missing arrays, removed provider streams, and VM
    /// checkpoint rollover. Between rollovers schema 3 inherits the checkpoint
    /// from the complete digest-verified predecessor and appends only new passes.
    pub fn prepare(
        prior: &PersistedAgentKindState,
        next: &PersistedAgentKindState,
    ) -> Option<Self> {
        #[cfg(feature = "bench-execution-timing")]
        let _phase_timer = crate::bench_execution_timing::Timer::start(
            &crate::bench_execution_timing::PINE_DELTA_HINT_PREPARE,
        );
        let (version, state_version, before) = plugin(prior)?;
        let (next_version, next_state_version, after) = plugin(next)?;
        if version != next_version || state_version != next_state_version {
            return None;
        }
        let (before_map, after_map) = (before.as_object()?, after.as_object()?);
        let schema = before_map.get("broker_schema")?.as_u64()?;
        let keys = history_arrays(schema)?;
        if after_map.get("broker_schema")?.as_u64()? != schema
            || before_map.get("identity")?.as_str()?.is_empty()
            || before_map.get("identity")? != after_map.get("identity")?
        {
            return None;
        }
        if schema == 2 {
            if before_map.get("bars")?.as_array()?.is_empty() {
                return None;
            }
        } else if before_map.get("evaluated_bars")?.as_u64()? == 0
            || before_map.get("vm_checkpoint")? != after_map.get("vm_checkpoint")?
        {
            // New checkpoints must be self-contained records. Never inherit an
            // old checkpoint across rollover, even when both tails are empty.
            return None;
        }
        let arrays = keys
            .iter()
            .copied()
            .map(|key| {
                Some((
                    key.to_owned(),
                    append(before_map.get(key)?, after_map.get(key)?)?,
                ))
            })
            .collect::<Option<BTreeMap<_, _>>>()?;
        let mut providers = BTreeMap::new();
        if schema == 2 {
            let before_providers = before_map.get("providers")?.as_object()?;
            let after_providers = after_map.get("providers")?.as_object()?;
            if before_providers
                .keys()
                .any(|key| !after_providers.contains_key(key))
            {
                return None;
            }
            for (key, stream) in after_providers {
                let tail = match before_providers.get(key) {
                    Some(previous) => append(previous, stream)?,
                    None => Append {
                        base_len: 0,
                        items: stream.as_array()?.clone(),
                    },
                };
                providers.insert(key.clone(), tail);
            }
        }
        let metadata = after_map
            .iter()
            .filter(|(key, _)| {
                !keys.contains(&key.as_str()) && key.as_str() != reconstructed_field(schema)
            })
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Map<_, _>>();
        let mut hint = Self {
            plugin_version: version.to_owned(),
            state_version,
            base_sha256: fingerprint(before).ok()?,
            metadata: Arc::new(metadata),
            arrays: Arc::new(arrays),
            providers: Arc::new(providers),
            patch_sha256: [0; 32],
            validated_payload: None,
            prepared: Some(Prepared {
                base: prior.clone(),
                next: next.clone(),
            }),
        };
        hint.patch_sha256 = hint.checksum().ok()?;
        // Only this constructor can issue a proof, after checking the original
        // shape constraints and hashing these exact private, immutable fields.
        // A failed shape check withholds the proof, not the hint: returning None
        // here could turn a later validation error into a full-state fallback.
        if hint.validate_shape().is_ok() {
            hint.validated_payload = Some(Arc::new(ValidatedPayload {
                plugin_version: hint.plugin_version.clone(),
                state_version: hint.state_version,
                base_sha256: hint.base_sha256,
                metadata: Arc::clone(&hint.metadata),
                arrays: Arc::clone(&hint.arrays),
                providers: Arc::clone(&hint.providers),
                patch_sha256: hint.patch_sha256,
            }));
        }
        Some(hint)
    }

    pub(super) fn matches(
        &self,
        prior: &PersistedAgentKindState,
        next: &PersistedAgentKindState,
    ) -> bool {
        self.has_validated_payload()
            && self.prepared.as_ref().is_some_and(|prepared| {
                same_identity(&prepared.base, prior) && same_identity(&prepared.next, next)
            })
    }

    fn has_validated_payload(&self) -> bool {
        self.validated_payload
            .as_ref()
            .is_some_and(|proof| proof.matches(self))
    }

    fn validate(&self) -> Result<(), JournalError> {
        if self.has_validated_payload() {
            return Ok(());
        }
        self.validate_shape()?;
        if self.checksum()? != self.patch_sha256 {
            return Err(failure("patch checksum mismatch"));
        }
        Ok(())
    }

    fn validate_shape(&self) -> Result<(), JournalError> {
        let schema = self
            .metadata
            .get("broker_schema")
            .and_then(Value::as_u64)
            .ok_or_else(|| failure("broker schema missing"))?;
        let keys = history_arrays(schema).ok_or_else(|| failure("unsupported broker schema"))?;
        if self.arrays.len() != keys.len()
            || keys.iter().any(|key| !self.arrays.contains_key(*key))
            || keys.iter().any(|key| self.metadata.contains_key(*key))
            || self.metadata.contains_key(reconstructed_field(schema))
            || (schema == 3 && !self.providers.is_empty())
            || self
                .metadata
                .get("identity")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            || self
                .arrays
                .values()
                .chain(self.providers.values())
                .any(|tail| tail.base_len.checked_add(tail.items.len()).is_none())
        {
            return Err(failure("shape, identity, or append length"));
        }
        Ok(())
    }

    fn checksum(&self) -> Result<[u8; 32], JournalError> {
        #[cfg(test)]
        tests::record_hint_checksum();
        fingerprint(&(
            &self.plugin_version,
            self.state_version,
            &self.base_sha256,
            &self.metadata,
            &self.arrays,
            &self.providers,
        ))
    }

    fn materialize(
        &self,
        prior: &PersistedAgentKindState,
    ) -> Result<PersistedAgentKindState, JournalError> {
        self.validate()?;
        // No hashing, cloning, or replay of history for the live prepared pair.
        if self.has_validated_payload()
            && let Some(prepared) = &self.prepared
            && same_identity(&prepared.base, prior)
        {
            return Ok(prepared.next.clone());
        }
        let (version, state_version, base) =
            plugin(prior).ok_or_else(|| failure("predecessor is not Pine"))?;
        if version != self.plugin_version
            || state_version != self.state_version
            || fingerprint(base)? != self.base_sha256
        {
            return Err(failure("predecessor identity or digest mismatch"));
        }
        let base = base
            .as_object()
            .ok_or_else(|| failure("predecessor is not an object"))?;
        let schema = self.metadata["broker_schema"].as_u64().unwrap();
        if base.get("broker_schema").and_then(Value::as_u64) != Some(schema)
            || base.get("identity") != self.metadata.get("identity")
        {
            return Err(failure("predecessor broker identity mismatch"));
        }
        let mut result = self.metadata.as_ref().clone();
        for &key in history_arrays(schema).unwrap() {
            let previous = base
                .get(key)
                .and_then(Value::as_array)
                .ok_or_else(|| failure("missing history array"))?;
            let tail = &self.arrays[key];
            if previous.len() != tail.base_len {
                return Err(failure("history prefix length mismatch"));
            }
            let mut values = previous.clone();
            values.extend_from_slice(&tail.items);
            result.insert(key.to_owned(), Value::Array(values));
        }
        if schema == 3 {
            result.insert(
                "vm_checkpoint".into(),
                base.get("vm_checkpoint")
                    .ok_or_else(|| failure("missing VM checkpoint"))?
                    .clone(),
            );
        } else {
            let previous = base
                .get("providers")
                .and_then(Value::as_object)
                .ok_or_else(|| failure("missing providers"))?;
            if previous.keys().any(|key| !self.providers.contains_key(key)) {
                return Err(failure("provider removed"));
            }
            let mut providers = Map::new();
            for (key, tail) in self.providers.iter() {
                let mut values = match previous.get(key) {
                    Some(value) => value
                        .as_array()
                        .ok_or_else(|| failure("invalid provider"))?
                        .clone(),
                    None => Vec::new(),
                };
                if values.len() != tail.base_len {
                    return Err(failure("provider prefix length mismatch"));
                }
                values.extend_from_slice(&tail.items);
                providers.insert(key.clone(), Value::Array(values));
            }
            result.insert("providers".into(), Value::Object(providers));
        }
        Ok(PersistedAgentKindState::Plugin {
            plugin_id: "pine.strategy".into(),
            plugin_version: self.plugin_version.clone(),
            state_version: self.state_version,
            data: Arc::new(Value::Object(result)),
        })
    }
}

impl PineAgentDelta {
    pub(super) fn new(
        index: usize,
        state: PineStateHint,
        unfinished_actions: Vec<OrderAction>,
    ) -> Self {
        Self {
            index,
            state,
            unfinished_actions,
        }
    }

    pub(super) fn validate(&self) -> Result<(), JournalError> {
        self.state.validate()
    }

    pub(super) fn materialize(
        &self,
        prior: &PersistedAgentKindState,
    ) -> Result<super::AgentDelta, JournalError> {
        Ok(super::AgentDelta {
            index: self.index,
            kind_state: self.state.materialize(prior)?,
            unfinished_actions: self.unfinished_actions.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler_delta::SchedulerDelta;
    use exchange_core::{SchedulerMode, SchedulerState};

    include!("validated_payload_tests.rs");

    fn data(rows: usize) -> Value {
        let bars: Vec<_> = (0..rows)
            .map(|i| {
                serde_json::json!({
                    "time": i * 10_000, "open": 100, "high": 103, "low": 98,
                    "close": 101, "volume": 1000,
                })
            })
            .collect();
        serde_json::json!({
            "identity": "frozen-source-settings-and-account", "broker_schema": 2,
            "bars": bars, "accounts": (0..rows).map(|i| serde_json::json!({"equity": 1000 + i, "position": 0})).collect::<Vec<_>>(),
            "evaluations": (0..rows).map(|i| serde_json::json!({"time": (i + 1) * 10_000, "providers": {"TEST:10S": i + 1}})).collect::<Vec<_>>(),
            "providers": {"TEST:10S": bars}, "orders": {}, "_host_receipts": [],
        })
    }

    fn state(rows: usize) -> SchedulerState {
        let spec: exchange_core::population::BackgroundMarket = serde_json::from_str(include_str!(
            "../../../scripts/fixtures/microstructure_market.json"
        ))
        .unwrap();
        let mut state = SchedulerState::new(
            "append-test",
            spec.agents,
            SchedulerMode::Auto { interval_ms: 1000 },
        );
        for agent in state.agents.iter_mut().take(2) {
            agent.kind_state = PersistedAgentKindState::Plugin {
                plugin_id: "pine.strategy".into(),
                plugin_version: "2.0.0".into(),
                state_version: 1,
                data: Arc::new(data(rows)),
            };
        }
        state
    }

    fn next(prior: &SchedulerState, rows: usize) -> SchedulerState {
        let mut next = prior.clone();
        next.revision += 1;
        for agent in next.agents.iter_mut().take(2) {
            let PersistedAgentKindState::Plugin { data: payload, .. } = &mut agent.kind_state
            else {
                unreachable!()
            };
            let mut value = data(rows);
            value["orders"] = serde_json::json!({"order": {"remaining": 7}});
            value["_host_receipts"] = serde_json::json!([{"order_id": 81, "accepted": true}]);
            value["new_metadata"] = serde_json::json!({"future_checkpoint": "retained exactly"});
            value["providers"]["OTHER:30S"] = serde_json::json!([{"time": 0, "close": 12}]);
            *payload = Arc::new(value);
        }
        next
    }

    #[test]
    fn append_wire_is_small_and_live_install_preserves_prepared_state_identity() {
        let prior = state(512);
        let next = next(&prior, 513);
        let full = SchedulerDelta::between(&prior, &next).unwrap();
        let full_bytes = serde_json::to_vec(&full).unwrap();
        let hints = [(
            0,
            PineStateHint::prepare(&prior.agents[0].kind_state, &next.agents[0].kind_state)
                .unwrap(),
        )]
        .into();
        let delta = full.into_durable_with_hints(&prior, &hints).unwrap();
        let supplied = &hints[&0];
        let retained = &delta.pine_changes[0].state;
        assert!(Arc::ptr_eq(&supplied.metadata, &retained.metadata));
        assert!(Arc::ptr_eq(&supplied.arrays, &retained.arrays));
        assert!(Arc::ptr_eq(&supplied.providers, &retained.providers));
        let cloned = delta.clone();
        let copied = &cloned.pine_changes[0].state;
        assert!(Arc::ptr_eq(&retained.metadata, &copied.metadata));
        assert!(Arc::ptr_eq(&retained.arrays, &copied.arrays));
        assert!(Arc::ptr_eq(&retained.providers, &copied.providers));
        assert!(copied.matches(&prior.agents[0].kind_state, &next.agents[0].kind_state));
        assert_eq!(delta.version, 2);
        assert!(delta.changes.is_empty());
        assert_eq!(delta.pine_changes.len(), 2);
        let encoded = serde_json::to_vec(&delta).unwrap();
        assert!(
            encoded.len() * 10 < full_bytes.len(),
            "append {} vs full {}",
            encoded.len(),
            full_bytes.len()
        );
        assert_eq!(delta.changed_indices().collect::<Vec<_>>(), vec![0, 1]);
        let mut installed = prior.clone();
        cloned.apply_owned(&mut installed).unwrap();
        assert_eq!(installed, next);
        assert!(same_identity(
            &installed.agents[0].kind_state,
            &next.agents[0].kind_state
        ));
        assert!(same_identity(
            &installed.agents[1].kind_state,
            &next.agents[1].kind_state
        ));
        let decoded: SchedulerDelta = serde_json::from_slice(&encoded).unwrap();
        assert!(
            decoded
                .pine_changes
                .iter()
                .all(|change| change.state.prepared.is_none())
        );
        let mut recovered = prior;
        decoded.apply(&mut recovered).unwrap();
        assert_eq!(recovered, next);
        assert_eq!(
            serde_json::to_vec(&recovered).unwrap(),
            serde_json::to_vec(&next).unwrap()
        );
    }

    #[test]
    fn corrections_missing_history_and_identity_changes_use_full_fallback() {
        let prior = state(4);
        for case in 0..7 {
            let mut changed = next(&prior, 5);
            for agent in changed.agents.iter_mut().take(2) {
                let PersistedAgentKindState::Plugin { data, .. } = &mut agent.kind_state else {
                    unreachable!()
                };
                let data = Arc::make_mut(data);
                match case {
                    0 => data["bars"][0]["close"] = 999.into(),
                    1 => data["providers"]["TEST:10S"][0]["close"] = 999.into(),
                    2 => {
                        data.as_object_mut().unwrap().remove("accounts");
                    }
                    3 => {
                        data["providers"]
                            .as_object_mut()
                            .unwrap()
                            .remove("TEST:10S");
                    }
                    4 => data["identity"] = "other account".into(),
                    5 => data["broker_schema"] = 3.into(),
                    _ => data["evaluations"].as_array_mut().unwrap().truncate(2),
                }
            }
            let delta = SchedulerDelta::between(&prior, &changed)
                .unwrap()
                .into_durable(&prior)
                .unwrap();
            assert_eq!(delta.version, 1);
            assert!(delta.pine_changes.is_empty());
            assert!(
                !serde_json::to_value(&delta)
                    .unwrap()
                    .as_object()
                    .unwrap()
                    .contains_key("pine_changes")
            );
            let mut restored = prior.clone();
            delta.apply(&mut restored).unwrap();
            assert_eq!(restored, changed);
        }
        let empty = state(0);
        let grown = next(&empty, 1);
        assert!(
            PineStateHint::prepare(&empty.agents[0].kind_state, &grown.agents[0].kind_state)
                .is_none()
        );
    }

    #[test]
    fn corrupted_second_append_never_installs_first_or_native_changes() {
        let prior = state(8);
        let mut next = next(&prior, 9);
        next.agents[2]
            .unfinished_actions
            .push(OrderAction::Cancel { order_id: 71 });
        let delta = SchedulerDelta::between(&prior, &next)
            .unwrap()
            .into_durable(&prior)
            .unwrap();
        let bytes = serde_json::to_vec(&delta).unwrap();
        for case in 0..9 {
            let mut corrupted: SchedulerDelta = serde_json::from_slice(&bytes).unwrap();
            let tail = &mut corrupted.pine_changes[1];
            match case {
                0 => corrupted.base_revision += 1,
                1 => corrupted.version = 1,
                2 => tail.index = 0,
                3 => tail.state.base_sha256[0] ^= 1,
                4 => {
                    Arc::make_mut(&mut tail.state.arrays)
                        .get_mut("bars")
                        .unwrap()
                        .base_len += 1
                }
                5 => {
                    Arc::make_mut(&mut tail.state.providers)
                        .get_mut("TEST:10S")
                        .unwrap()
                        .base_len += 1
                }
                6 => Arc::make_mut(&mut tail.state.metadata)
                    .insert("identity".into(), "corrupt".into())
                    .map(|_| ())
                    .unwrap(),
                7 => {
                    Arc::make_mut(&mut tail.state.arrays)
                        .get_mut("bars")
                        .unwrap()
                        .items[0]["close"] = 999.into()
                }
                _ => {
                    Arc::make_mut(&mut tail.state.providers).remove("TEST:10S");
                }
            }
            // For prefix cases also test structural validation with a consistent
            // patch checksum, so the predecessor checks are independently hit.
            if matches!(case, 3..=6 | 8) {
                corrupted.pine_changes[1].state.patch_sha256 =
                    corrupted.pine_changes[1].state.checksum().unwrap();
            }
            let mut borrowed = prior.clone();
            assert!(corrupted.apply(&mut borrowed).is_err(), "case {case}");
            assert_eq!(borrowed, prior);
            let mut owned = prior.clone();
            assert!(
                corrupted.apply_owned(&mut owned).is_err(),
                "owned case {case}"
            );
            assert_eq!(owned, prior);
        }
    }

    #[test]
    fn stale_or_deserialized_hints_cannot_authorize_another_predecessor() {
        let prior = state(5);
        let next = next(&prior, 6);
        let hint = PineStateHint::prepare(&prior.agents[0].kind_state, &next.agents[0].kind_state)
            .unwrap();
        let mut other = prior.clone();
        let PersistedAgentKindState::Plugin { data, .. } = &mut other.agents[0].kind_state else {
            unreachable!()
        };
        Arc::make_mut(data)["bars"][0]["close"] = 333.into();
        assert!(!hint.matches(&other.agents[0].kind_state, &next.agents[0].kind_state));
        let decoded: PineStateHint =
            serde_json::from_slice(&serde_json::to_vec(&hint).unwrap()).unwrap();
        assert!(!decoded.matches(&prior.agents[0].kind_state, &next.agents[0].kind_state));
        assert!(decoded.materialize(&other.agents[0].kind_state).is_err());
        let hints = [(0, hint)].into();
        let delta = SchedulerDelta::between(&other, &next)
            .unwrap()
            .into_durable_with_hints(&other, &hints)
            .unwrap();
        assert!(delta.changes.iter().any(|change| change.index == 0));
        let mut restored = other;
        delta.apply_owned(&mut restored).unwrap();
        assert_eq!(restored, next);
    }

    #[test]
    fn old_checkpoint_v1_and_v2_tails_recover_through_full_fallback() {
        use crate::journal::{
            JournalMutation, JournalRecovery, ROOM_MUTATION_SCHEMA_VERSION, RoomMutation,
        };
        let original = state(6);
        let mut legacy = serde_json::to_value(&original).unwrap();
        legacy.as_object_mut().unwrap().remove("revision");
        let original: SchedulerState = serde_json::from_value(legacy).unwrap();
        let appended = next(&original, 7);
        let v2 = SchedulerDelta::between(&original, &appended)
            .unwrap()
            .into_durable(&original)
            .unwrap();
        let mut replaced = appended.clone();
        replaced.revision += 1;
        let PersistedAgentKindState::Plugin { data, .. } = &mut replaced.agents[0].kind_state
        else {
            unreachable!()
        };
        Arc::make_mut(data)["bars"][0]["close"] = 808.into();
        let v1 = SchedulerDelta::between(&appended, &replaced)
            .unwrap()
            .into_durable(&appended)
            .unwrap();
        assert_eq!(v1.version, 1);
        let mut after_metadata = replaced.clone();
        after_metadata.revision += 1;
        let PersistedAgentKindState::Plugin { data, .. } = &mut after_metadata.agents[0].kind_state
        else {
            unreachable!()
        };
        Arc::make_mut(data)["durable_metadata_after_correction"] = true.into();
        let last = SchedulerDelta::between(&replaced, &after_metadata)
            .unwrap()
            .into_durable(&replaced)
            .unwrap();
        assert_eq!(last.version, 2);
        let mutations = [
            RoomMutation::SchedulerProgress {
                clock_steps: 0,
                state: original,
                training: None,
            },
            RoomMutation::SchedulerDelta {
                clock_steps: 1,
                delta: v2,
                training: None,
            },
            RoomMutation::SchedulerDelta {
                clock_steps: 0,
                delta: v1,
                training: None,
            },
            RoomMutation::SchedulerDelta {
                clock_steps: 0,
                delta: last,
                training: None,
            },
        ]
        .into_iter()
        .enumerate()
        .map(|(index, mutation)| JournalMutation {
            room_id: "append-test".into(),
            mutation_seq: index as u64 + 1,
            command_cursor: 0,
            schema_version: ROOM_MUTATION_SCHEMA_VERSION,
            mutation,
        })
        .collect();
        let recovery = JournalRecovery {
            mutations,
            ..JournalRecovery::default()
        };
        let recovered: JournalRecovery =
            serde_json::from_slice(&serde_json::to_vec(&recovery).unwrap()).unwrap();
        assert_eq!(
            crate::scheduler_states_from_recovery(&recovered).unwrap()["append-test"],
            after_metadata
        );
        let mut missing = recovered;
        missing.mutations.remove(1);
        assert!(crate::scheduler_states_from_recovery(&missing).is_err());
    }

    #[test]
    fn memory_journal_retains_only_wire_not_prepared_history_arcs() {
        use crate::journal::{
            InMemoryJournalStore, JournalStore, PendingJournalMutation, RoomMutation,
        };
        let mut store = InMemoryJournalStore::new();
        let (base_weak, next_weak, expected) = {
            let prior = state(64);
            let next = next(&prior, 65);
            let base_weak = Arc::downgrade(plugin(&prior.agents[0].kind_state).unwrap().2);
            let next_weak = Arc::downgrade(plugin(&next.agents[0].kind_state).unwrap().2);
            let delta = SchedulerDelta::between(&prior, &next)
                .unwrap()
                .into_durable(&prior)
                .unwrap();
            let expected = serde_json::to_vec(&next).unwrap();
            let checkpoint = PendingJournalMutation::new(
                "append-test",
                0,
                RoomMutation::SchedulerProgress {
                    clock_steps: 0,
                    state: prior,
                    training: None,
                },
            );
            store
                .append_room_mutation(&checkpoint, &[], &[], None)
                .unwrap();
            let pending = PendingJournalMutation::new(
                "append-test",
                0,
                RoomMutation::SchedulerDelta {
                    clock_steps: 0,
                    delta,
                    training: None,
                },
            );
            store
                .append_room_mutation(&pending, &[], &[], None)
                .unwrap();
            assert!(base_weak.upgrade().is_some());
            assert!(next_weak.upgrade().is_some());
            (base_weak, next_weak, expected)
        };
        assert!(
            base_weak.upgrade().is_none(),
            "retained journal must not own historical prepared bases"
        );
        assert!(
            next_weak.upgrade().is_none(),
            "retained journal must not own prepared results"
        );
        let recovery = store.load_recovery().unwrap();
        let restored = crate::scheduler_states_from_recovery(&recovery).unwrap();
        assert_eq!(
            serde_json::to_vec(&restored["append-test"]).unwrap(),
            expected
        );
    }

    #[test]
    fn identity_hash_normalizes_jsonb_numbers_without_rounding_large_integers() {
        for (left, right) in [
            ("1e18", "1000000000000000000"),
            ("-0.0", "0"),
            ("1.00", "1"),
            ("0.0010", "1e-3"),
            ("-9223372036854775808", "-9.223372036854776e18"),
        ] {
            let left: Value = serde_json::from_str(left).unwrap();
            let right: Value = serde_json::from_str(right).unwrap();
            // The last floating spelling is not the same exact decimal integer;
            // it deliberately verifies that no integer is rounded through f64.
            if left.as_i64() == Some(i64::MIN) {
                assert_ne!(fingerprint(&left).unwrap(), fingerprint(&right).unwrap());
            } else {
                assert_eq!(fingerprint(&left).unwrap(), fingerprint(&right).unwrap());
            }
        }
        let left: Value = serde_json::from_str("9007199254740993").unwrap();
        let right: Value = serde_json::from_str("9007199254740992").unwrap();
        assert_ne!(fingerprint(&left).unwrap(), fingerprint(&right).unwrap());
    }

    #[test]
    fn postgres_jsonb_append_roundtrip_when_configured() {
        let url = match std::env::var("MARKETFORGE_TEST_DATABASE_URL") {
            Ok(url) if !url.trim().is_empty() => url,
            _ if std::env::var("MARKETFORGE_REQUIRE_POSTGRES_TESTS").as_deref() == Ok("1") => {
                panic!("PostgreSQL test URL required")
            }
            _ => return,
        };
        let mut client = postgres::Client::connect(&url, postgres::NoTls).unwrap();
        let mut jsonb = |value: Value| -> Value {
            client
                .query_one("SELECT $1::jsonb", &[&value])
                .unwrap()
                .get(0)
        };
        let mut prior = state(8);
        let mut next = next(&prior, 9);
        for state in [&mut prior, &mut next] {
            for agent in state.agents.iter_mut().take(2) {
                let PersistedAgentKindState::Plugin { data, .. } = &mut agent.kind_state else {
                    unreachable!()
                };
                Arc::make_mut(data)["number_forms"] =
                    serde_json::json!([1e18, -0.0, 1e-15, u64::MAX, i64::MIN]);
            }
        }
        let delta = SchedulerDelta::between(&prior, &next)
            .unwrap()
            .into_durable(&prior)
            .unwrap();
        assert_eq!(delta.version, 2);
        let mut recovered: SchedulerState =
            serde_json::from_value(jsonb(serde_json::to_value(prior).unwrap())).unwrap();
        let recovered_delta: SchedulerDelta =
            serde_json::from_value(jsonb(serde_json::to_value(delta).unwrap())).unwrap();
        recovered_delta.apply_owned(&mut recovered).unwrap();
        let expected: SchedulerState =
            serde_json::from_value(jsonb(serde_json::to_value(next).unwrap())).unwrap();
        assert_eq!(recovered, expected);
    }
}

#[cfg(test)]
#[path = "schema3_tests.rs"]
mod schema3_tests;
