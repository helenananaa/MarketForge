//! Versioned scheduler journal patches. A roster/configuration change always
//! requires a full checkpoint; patches carry only changed bot runtime state.
use crate::journal::JournalError;
use exchange_core::{OrderAction, PersistedAgentKindState, SchedulerState};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

mod pine;
pub use pine::{PineAgentDelta, PineStateHint};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SchedulerDelta {
    pub version: u16,
    pub base_revision: u64,
    pub agent_count: usize,
    /// All scheduler metadata, with an empty agents vector.
    pub state: SchedulerState,
    pub changes: Vec<AgentDelta>,
    /// Version 2 append-only Pine changes. Version 1 remains byte-compatible.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pine_changes: Vec<PineAgentDelta>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{
        JournalMutation, JournalRecovery, ROOM_MUTATION_SCHEMA_VERSION, RoomMutation,
    };
    use exchange_core::{PersistedAgent, SchedulerMode, SchedulerPhase};
    use std::sync::Arc;

    fn sample() -> SchedulerState {
        let spec: exchange_core::population::BackgroundMarket = serde_json::from_str(include_str!(
            "../../scripts/fixtures/microstructure_market.json"
        ))
        .unwrap();
        SchedulerState::new(
            "patch-test",
            spec.agents,
            SchedulerMode::Auto { interval_ms: 25 },
        )
    }

    fn record(seq: u64, mutation: RoomMutation) -> JournalMutation {
        JournalMutation {
            room_id: "patch-test".into(),
            mutation_seq: seq,
            command_cursor: 0,
            schema_version: ROOM_MUTATION_SCHEMA_VERSION,
            mutation,
        }
    }

    #[test]
    fn full_checkpoint_shares_plugin_payload_but_isolates_updates_and_keeps_json_schema() {
        let mut prior = sample();
        let index = prior
            .agents
            .iter()
            .position(|agent| matches!(agent.kind_state, PersistedAgentKindState::Plugin { .. }))
            .unwrap();
        let payload = serde_json::json!({
            "bars": (0..512).map(|i| serde_json::json!({"time": i * 1000, "close": 100 + i})).collect::<Vec<_>>(),
            "_host_receipts": [{"order_id": 71, "accepted": true}],
        });
        let PersistedAgentKindState::Plugin { data, .. } = &mut prior.agents[index].kind_state
        else {
            unreachable!()
        };
        *data = payload.clone().into();
        let original = data.clone();
        let mut checkpoint = Candidate::Full(prior.clone()).materialize(None);
        let PersistedAgentKindState::Plugin {
            data,
            plugin_id,
            plugin_version,
            state_version,
        } = &mut checkpoint.agents[index].kind_state
        else {
            unreachable!()
        };
        assert!(
            Arc::ptr_eq(&original, data),
            "full checkpoint must not copy historical JSON"
        );
        let expected = serde_json::json!({"Plugin": {
            "plugin_id": plugin_id, "plugin_version": plugin_version,
            "state_version": state_version, "data": payload,
        }});
        assert_eq!(
            serde_json::to_value(&prior.agents[index].kind_state).unwrap(),
            expected
        );
        Arc::make_mut(data)["_host_receipts"][0]["order_id"] = 72.into();
        assert!(!Arc::ptr_eq(&original, data));
        assert_eq!(original["_host_receipts"][0]["order_id"], 71);
        checkpoint.revision = 1;
        let delta = SchedulerDelta::between(&prior, &checkpoint).unwrap();
        let recovered = Candidate::Patch(delta.clone()).materialize(Some(&prior));
        assert_eq!(recovered, checkpoint);
        assert_eq!(
            prior.revision, 0,
            "candidate construction must not install progress"
        );
        let encoded = serde_json::to_vec(&checkpoint).unwrap();
        let restored: SchedulerState = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(restored, checkpoint);
        assert_eq!(serde_json::to_vec(&restored).unwrap(), encoded);
        let mut installed = prior;
        delta.apply_owned(&mut installed).unwrap();
        assert_eq!(installed, restored);
        assert_eq!(original["_host_receipts"][0]["order_id"], 71);
    }

    #[test]
    fn patch_replays_runtime_and_metadata_without_repeating_templates() {
        let mut prior = sample();
        prior.revision = 40;
        let mut next = prior.clone();
        next.revision += 1;
        next.bots_enabled = false;
        next.phase = SchedulerPhase::StepComplete { step: 71 };
        next.agents[3]
            .unfinished_actions
            .push(OrderAction::PlaceLimit {
                side: exchange_core::Side::Buy,
                price_tick: 10,
                qty: 1,
            });
        let delta = SchedulerDelta::between(&prior, &next).unwrap();
        assert_eq!(delta.changes.len(), 1);
        let encoded = serde_json::to_vec(&delta).unwrap();
        assert!(encoded.len() * 4 < serde_json::to_vec(&next).unwrap().len());
        let restored: SchedulerDelta = serde_json::from_slice(&encoded).unwrap();
        restored.apply(&mut prior).unwrap();
        assert_eq!(prior, next);
        assert!(
            restored.apply(&mut prior).is_err(),
            "duplicate patch must fail"
        );
        assert_eq!(prior, next);
    }

    #[test]
    fn corrupt_patch_fails_without_partial_mutation_and_roster_change_needs_checkpoint() {
        let prior = sample();
        let mut next = prior.clone();
        next.revision = 1;
        next.agents[0]
            .unfinished_actions
            .push(OrderAction::PlaceLimit {
                side: exchange_core::Side::Buy,
                price_tick: 10,
                qty: 1,
            });
        let delta = SchedulerDelta::between(&prior, &next).unwrap();
        for case in 0..6 {
            let mut invalid = delta.clone();
            match case {
                0 => invalid.version = 99,
                1 => invalid.base_revision = 22,
                2 => invalid.agent_count += 1,
                3 => invalid.changes.push(invalid.changes[0].clone()),
                4 => invalid.changes[0].index = usize::MAX,
                _ => invalid.state.room_id = "wrong-room".into(),
            }
            let mut restored = prior.clone();
            assert!(invalid.apply(&mut restored).is_err());
            assert_eq!(restored, prior);
        }
        next.agents.swap(0, 1);
        assert!(SchedulerDelta::between(&prior, &next).is_none());
        next.agents.push(PersistedAgent::from_template(
            prior.agents[0].template.clone(),
        ));
        assert!(SchedulerDelta::between(&prior, &next).is_none());
    }

    #[test]
    fn mixed_legacy_checkpoint_and_delta_tail_require_continuous_revisions() {
        let initial = sample();
        let mut legacy = serde_json::to_value(&initial).unwrap();
        legacy.as_object_mut().unwrap().remove("revision");
        let initial: SchedulerState = serde_json::from_value(legacy).unwrap();
        assert_eq!(initial.revision, 0);
        let mut next = initial.clone();
        next.revision = 1;
        next.phase = SchedulerPhase::StepComplete { step: 1 };
        let mut last = next.clone();
        last.revision = 2;
        last.bots_enabled = false;
        let mut recovery = JournalRecovery {
            mutations: vec![
                record(
                    1,
                    RoomMutation::SchedulerProgress {
                        clock_steps: 0,
                        state: initial.clone(),
                        training: None,
                    },
                ),
                record(
                    2,
                    RoomMutation::SchedulerDelta {
                        clock_steps: 1,
                        delta: SchedulerDelta::between(&initial, &next).unwrap(),
                        training: None,
                    },
                ),
                record(
                    3,
                    RoomMutation::SchedulerDelta {
                        clock_steps: 0,
                        delta: SchedulerDelta::between(&next, &last).unwrap(),
                        training: None,
                    },
                ),
            ],
            ..JournalRecovery::default()
        };
        recovery.mutations.reverse(); // Storage results need not be pre-sorted.
        assert_eq!(
            crate::scheduler_states_from_recovery(&recovery).unwrap()["patch-test"],
            last
        );
        let encoded = serde_json::to_vec(&recovery).unwrap();
        let mut missing: JournalRecovery = serde_json::from_slice(&encoded).unwrap();
        missing.mutations.retain(|m| m.mutation_seq != 2);
        assert!(crate::scheduler_states_from_recovery(&missing).is_err());
        missing.mutations.retain(|m| m.mutation_seq != 1);
        assert!(crate::scheduler_states_from_recovery(&missing).is_err());
        recovery.mutations.push(record(
            4,
            RoomMutation::SchedulerProgress {
                clock_steps: 0,
                state: last.clone(),
                training: None,
            },
        ));
        recovery.mutations.retain(|m| m.mutation_seq != 2);
        assert_eq!(
            crate::scheduler_states_from_recovery(&recovery).unwrap()["patch-test"],
            last
        );
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentDelta {
    pub index: usize,
    pub kind_state: PersistedAgentKindState,
    pub unfinished_actions: Vec<OrderAction>,
}

impl SchedulerDelta {
    pub(super) fn metadata(state: &SchedulerState) -> SchedulerState {
        SchedulerState {
            version: state.version,
            revision: state.revision,
            room_id: state.room_id.clone(),
            mode: state.mode,
            bots_enabled: state.bots_enabled,
            catch_up_limit: state.catch_up_limit,
            lagged: state.lagged,
            continuity: state.continuity,
            phase: state.phase.clone(),
            agents: Vec::new(),
        }
    }

    pub fn between(prior: &SchedulerState, next: &SchedulerState) -> Option<Self> {
        #[cfg(feature = "bench-execution-timing")]
        let _timer = crate::bench_execution_timing::Timer::start(
            &crate::bench_execution_timing::SCHEDULER_DELTA_BETWEEN,
        );
        if prior.room_id != next.room_id
            || prior.version != next.version
            || prior.revision.checked_add(1) != Some(next.revision)
            || prior.agents.len() != next.agents.len()
            || prior.agents.iter().zip(&next.agents).any(|(a, b)| {
                a.version != b.version
                    || a.config_version != b.config_version
                    || a.template != b.template
            })
        {
            return None;
        }
        let state = Self::metadata(next);
        let changes = prior
            .agents
            .iter()
            .zip(&next.agents)
            .enumerate()
            .filter(|(_, (a, b))| {
                a.kind_state != b.kind_state || a.unfinished_actions != b.unfinished_actions
            })
            .map(|(index, (_, b))| AgentDelta {
                index,
                kind_state: b.kind_state.clone(),
                unfinished_actions: b.unfinished_actions.clone(),
            })
            .collect();
        Some(Self {
            version: 1,
            base_revision: prior.revision,
            agent_count: prior.agents.len(),
            state,
            changes,
            pine_changes: Vec::new(),
        })
    }

    /// Turn complete live changes into bounded append records where the exact
    /// predecessor permits it. Native-only deltas retain the original v1 wire.
    pub fn into_durable(self, prior: &SchedulerState) -> Result<Self, JournalError> {
        self.into_durable_with_hints(prior, &BTreeMap::new())
    }

    pub fn into_durable_with_hints(
        mut self,
        prior: &SchedulerState,
        hints: &BTreeMap<usize, PineStateHint>,
    ) -> Result<Self, JournalError> {
        self.validate_predecessor(prior)?;
        if self.changes.iter().all(|change| {
            !matches!(
                &change.kind_state,
                PersistedAgentKindState::Plugin { plugin_id, .. } if plugin_id == "pine.strategy"
            )
        }) {
            return Ok(self);
        }
        let mut full = Vec::with_capacity(self.changes.len());
        for change in self.changes {
            let base = &prior.agents[change.index].kind_state;
            let hint = hints
                .get(&change.index)
                .filter(|hint| hint.matches(base, &change.kind_state))
                .cloned()
                .or_else(|| PineStateHint::prepare(base, &change.kind_state));
            match hint {
                Some(hint) => self.pine_changes.push(PineAgentDelta::new(
                    change.index,
                    hint,
                    change.unfinished_actions,
                )),
                None => full.push(change),
            }
        }
        self.changes = full;
        self.pine_changes.sort_by_key(|change| change.index);
        self.version = if self.pine_changes.is_empty() { 1 } else { 2 };
        self.validate(&prior.room_id)?;
        Ok(self)
    }

    pub fn changed_indices(&self) -> impl Iterator<Item = usize> + '_ {
        let mut full = self.changes.iter().map(|change| change.index).peekable();
        let mut pine = self
            .pine_changes
            .iter()
            .map(|change| change.index)
            .peekable();
        std::iter::from_fn(move || match (full.peek(), pine.peek()) {
            (Some(a), Some(b)) if a < b => full.next(),
            (_, Some(_)) => pine.next(),
            (Some(_), None) => full.next(),
            (None, None) => None,
        })
    }

    pub fn validate(&self, room: &str) -> Result<(), JournalError> {
        if !matches!(self.version, 1 | 2)
            || (self.version == 1 && !self.pine_changes.is_empty())
            || (self.version == 2 && self.pine_changes.is_empty())
            || self.state.version != exchange_core::scheduler::SCHEDULER_STATE_VERSION
            || self.state.room_id != room
            || !self.state.agents.is_empty()
            || self.base_revision.checked_add(1) != Some(self.state.revision)
            || self
                .changes
                .iter()
                .any(|change| change.index >= self.agent_count)
            || self
                .changes
                .windows(2)
                .any(|pair| pair[0].index >= pair[1].index)
            || self
                .pine_changes
                .iter()
                .any(|change| change.index >= self.agent_count)
            || self
                .pine_changes
                .windows(2)
                .any(|pair| pair[0].index >= pair[1].index)
        {
            return Err(JournalError::Recovery(
                "invalid scheduler delta version, revision, room or agent indexes".into(),
            ));
        }
        if !self.pine_changes.is_empty() {
            let mut last = None;
            for index in self.changed_indices() {
                if last == Some(index) {
                    return Err(JournalError::Recovery(
                        "duplicate scheduler delta agent index".into(),
                    ));
                }
                last = Some(index);
            }
            for change in &self.pine_changes {
                change.validate()?;
            }
        }
        Ok(())
    }

    pub fn apply(&self, prior: &mut SchedulerState) -> Result<(), JournalError> {
        self.validate_predecessor(prior)?;
        let pine = self.prepare_pine(prior)?;
        for change in &self.changes {
            prior.agents[change.index].kind_state = change.kind_state.clone();
            prior.agents[change.index].unfinished_actions = change.unfinished_actions.clone();
        }
        for change in pine {
            prior.agents[change.index].kind_state = change.kind_state;
            prior.agents[change.index].unfinished_actions = change.unfinished_actions;
        }
        let agents = std::mem::take(&mut prior.agents);
        *prior = self.state.clone();
        prior.agents = agents;
        Ok(())
    }

    fn prepare_pine(&self, prior: &SchedulerState) -> Result<Vec<AgentDelta>, JournalError> {
        self.pine_changes
            .iter()
            .map(|change| change.materialize(&prior.agents[change.index].kind_state))
            .collect()
    }

    pub(super) fn validate_predecessor(&self, prior: &SchedulerState) -> Result<(), JournalError> {
        self.validate(&prior.room_id)?;
        if prior.revision != self.base_revision
            || prior.agents.len() != self.agent_count
            || prior.version != self.state.version
        {
            return Err(JournalError::Recovery(
                "scheduler delta missing or incompatible predecessor".into(),
            ));
        }
        Ok(())
    }

    /// Consume an already prepared update without cloning bot runtime data.
    /// Validation still precedes every mutation, including recovery callers.
    pub(super) fn apply_owned(self, prior: &mut SchedulerState) -> Result<(), JournalError> {
        self.validate_predecessor(prior)?;
        let pine = self.prepare_pine(prior)?;
        // Validate every field before mutating the recovered state.
        for change in self.changes {
            prior.agents[change.index].kind_state = change.kind_state;
            prior.agents[change.index].unfinished_actions = change.unfinished_actions;
        }
        for change in pine {
            prior.agents[change.index].kind_state = change.kind_state;
            prior.agents[change.index].unfinished_actions = change.unfinished_actions;
        }
        let agents = std::mem::take(&mut prior.agents);
        *prior = self.state;
        prior.agents = agents;
        Ok(())
    }
}

/// Full candidates remain necessary for deterministic/manual steps. Live work
/// changes only metadata and a bounded set of agents under the same writer lock.
pub(super) enum Candidate {
    Full(SchedulerState),
    Patch(SchedulerDelta),
}

impl Candidate {
    pub fn state(&self) -> &SchedulerState {
        match self {
            Self::Full(state) => state,
            Self::Patch(delta) => &delta.state,
        }
    }

    pub fn state_mut(&mut self) -> &mut SchedulerState {
        match self {
            Self::Full(state) => state,
            Self::Patch(delta) => &mut delta.state,
        }
    }

    pub fn materialize(&self, prior: Option<&SchedulerState>) -> SchedulerState {
        #[cfg(feature = "bench-execution-timing")]
        let _timer = crate::bench_execution_timing::Timer::start(
            &crate::bench_execution_timing::SCHEDULER_MATERIALIZE,
        );
        match self {
            Self::Full(state) => state.clone(),
            Self::Patch(delta) => {
                let mut state = prior.expect("live update has a locked predecessor").clone();
                delta.apply(&mut state).expect("live update was validated");
                state
            }
        }
    }
}
