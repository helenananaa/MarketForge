use super::*;
use crate::{
    journal::{InMemoryJournalStore, JournalStore, PendingJournalMutation, RoomMutation},
    scheduler_delta::SchedulerDelta,
};
use exchange_core::{SchedulerMode, SchedulerState};
use serde_json::json;

fn data(bars: usize) -> Value {
    let archived = bars / 8 * 8;
    json!({"broker_schema":4,"identity":"source-account-inputs","evaluated_bars":bars,
        "vm_checkpoint":{"kind":"marketforge.pine-replay/1","bars":archived,
            "chunks":(0..bars/8).map(|i| format!("{i:08x}{}", "abcdef".repeat(512))).collect::<Vec<_>>(),
            "previous_time":archived.checked_sub(1),"tail_batches":vec![1;bars%8]},
        "vm_tail":(archived..bars).map(|i|json!({"bar":i,"cash":9007199254740993_u64})).collect::<Vec<_>>(),
        "vm_tail_intents":(archived..bars).map(|i|json!({"bar_index":i,"qty":{"$f64":"8000000000000000"}})).collect::<Vec<_>>(),
        "execution_seal":format!("seal-{bars}"),"orders":{},"ledgers":{},"last_bar_time":bars,
        "candle_audits":{"BTC:1S":{"count":bars}},"obsolete":"removed later"})
}

fn state(bars: usize) -> SchedulerState {
    let spec: exchange_core::population::BackgroundMarket = serde_json::from_str(include_str!(
        "../../../scripts/fixtures/microstructure_market.json"
    ))
    .unwrap();
    let mut s = SchedulerState::new(
        "replay-append-test",
        spec.agents,
        SchedulerMode::Auto { interval_ms: 1000 },
    );
    for agent in s.agents.iter_mut().take(2) {
        agent.kind_state = PersistedAgentKindState::Plugin {
            plugin_id: "pine.strategy".into(),
            plugin_version: "2.0.0".into(),
            state_version: 1,
            data: Arc::new(data(bars)),
        };
    }
    s
}

fn payload(s: &mut PersistedAgentKindState) -> &mut Value {
    let PersistedAgentKindState::Plugin { data, .. } = s else {
        panic!("expected plugin")
    };
    Arc::make_mut(data)
}

fn next(prior: &SchedulerState, bars: usize) -> SchedulerState {
    let mut s = prior.clone();
    s.revision += 1;
    for agent in s.agents.iter_mut().take(2) {
        let mut v = data(bars);
        v.as_object_mut().unwrap().remove("obsolete");
        v["ledgers"] = json!({"fills":[{"id":bars,"fee":17,"qty":2}],"position":2});
        *payload(&mut agent.kind_state) = v;
    }
    s
}

fn delta(a: &SchedulerState, b: &SchedulerState) -> SchedulerDelta {
    SchedulerDelta::between(a, b)
        .unwrap()
        .into_durable(a)
        .unwrap()
}
fn decoded(d: &SchedulerDelta) -> SchedulerDelta {
    serde_json::from_slice(&serde_json::to_vec(d).unwrap()).unwrap()
}

#[test]
fn schema4_tails_rollovers_multi_chunks_and_feedback_recover_exactly() {
    for (a, b) in [(1, 2), (7, 8), (8, 25), (51, 51), (55, 56), (512, 513)] {
        let prior = state(a);
        let next = next(&prior, b);
        let d = delta(&prior, &next);
        assert!(d.changes.is_empty());
        assert_eq!(d.pine_changes.len(), 2);
        let hint = &d.pine_changes[0].state;
        assert!(hint.arrays.is_empty());
        assert!(hint.providers.is_empty());
        assert!(!hint.metadata.contains_key("vm_checkpoint"));
        let mut live = prior.clone();
        d.clone().apply_owned(&mut live).unwrap();
        assert!(same_identity(
            &live.agents[0].kind_state,
            &next.agents[0].kind_state
        ));
        let mut restored = prior;
        decoded(&d).apply_owned(&mut restored).unwrap();
        assert_eq!(
            serde_json::to_vec(&restored).unwrap(),
            serde_json::to_vec(&next).unwrap()
        );
        if a >= 512 {
            assert!(
                serde_json::to_vec(&d).unwrap().len() * 20
                    < serde_json::to_vec(&next).unwrap().len()
            );
        }
    }
}

#[test]
fn schema4_corrections_initialization_and_identity_changes_use_complete_state() {
    for change in 0..5 {
        let prior = state(if change == 0 { 0 } else { 16 });
        let mut next = next(&prior, 17);
        for agent in next.agents.iter_mut().take(2) {
            let v = payload(&mut agent.kind_state);
            match change {
                1 => v["vm_checkpoint"]["chunks"][0] = json!("corrected-history"),
                2 => {
                    v["vm_checkpoint"]["chunks"].as_array_mut().unwrap().pop();
                }
                3 => v["identity"] = json!("changed-source"),
                4 => v["vm_checkpoint"]["kind"] = json!("changed-format"),
                _ => {}
            }
        }
        let d = delta(&prior, &next);
        assert!(d.pine_changes.is_empty());
        assert_eq!(d.changes.len(), 2);
    }
}

#[test]
fn schema4_corrupted_hint_and_wrong_predecessor_never_partially_install() {
    let prior = state(15);
    let next = next(&prior, 16);
    let good = delta(&prior, &next);
    for mode in 0..11 {
        let mut bad = decoded(&good);
        let hint = &mut bad.pine_changes[1].state;
        match mode {
            0 => hint.patch_sha256[0] ^= 1,
            1 => hint.base_sha256[0] ^= 1,
            2 => hint.replay = None,
            3 => Arc::make_mut(&mut hint.metadata)
                .insert("vm_checkpoint".into(), json!({}))
                .map(|_| ())
                .unwrap_or(()),
            4 => Arc::make_mut(&mut hint.metadata)
                .insert("broker_schema".into(), json!(3))
                .map(|_| ())
                .unwrap_or(()),
            5 => {
                hint.arrays = Arc::new(BTreeMap::from([(
                    "vm_tail".into(),
                    Append {
                        base_len: 0,
                        items: vec![],
                    },
                )]));
            }
            _ => {
                let mut replay = serde_json::to_value(&hint.replay).unwrap();
                match mode {
                    6 => replay["chunks"]["base_len"] = json!(999),
                    7 => replay["chunks"]["items"] = json!([null]),
                    8 => replay["metadata"]["chunks"] = json!([]),
                    9 => replay["metadata"]["kind"] = json!("unknown"),
                    _ => replay["metadata"]["bars"] = json!(-1),
                }
                hint.replay = serde_json::from_value(replay).unwrap();
            }
        }
        if mode > 0 {
            hint.patch_sha256 = hint.checksum().unwrap();
        }
        let mut unchanged = prior.clone();
        assert!(bad.apply_owned(&mut unchanged).is_err());
        assert_eq!(unchanged, prior);
    }
    let mut wrong = prior.clone();
    payload(&mut wrong.agents[1].kind_state)["vm_checkpoint"]["chunks"][0] = json!("edited");
    let frozen = wrong.clone();
    assert!(decoded(&good).apply_owned(&mut wrong).is_err());
    assert_eq!(wrong, frozen);
}

#[test]
fn schema4_memory_journal_recovers_rollovers_and_does_not_retain_history_owners() {
    let mut store = InMemoryJournalStore::new();
    let (weak, expected) = {
        let mut prior = state(7);
        let mut weak = vec![Arc::downgrade(
            plugin(&prior.agents[0].kind_state).unwrap().2,
        )];
        store
            .append_room_mutation(
                &PendingJournalMutation::new(
                    "replay-append-test",
                    0,
                    RoomMutation::SchedulerProgress {
                        clock_steps: 0,
                        state: prior.clone(),
                        training: None,
                    },
                ),
                &[],
                &[],
                None,
            )
            .unwrap();
        for n in 8..=33 {
            let next = next(&prior, n);
            weak.push(Arc::downgrade(
                plugin(&next.agents[0].kind_state).unwrap().2,
            ));
            let delta = delta(&prior, &next);
            assert_eq!(delta.version, 2);
            store
                .append_room_mutation(
                    &PendingJournalMutation::new(
                        "replay-append-test",
                        0,
                        RoomMutation::SchedulerDelta {
                            clock_steps: 1,
                            delta,
                            training: None,
                        },
                    ),
                    &[],
                    &[],
                    None,
                )
                .unwrap();
            prior = next;
        }
        (weak, serde_json::to_vec(&prior).unwrap())
    };
    assert!(weak.iter().all(|w| w.upgrade().is_none()));
    let recovery = store.load_recovery().unwrap();
    let restored = crate::scheduler_states_from_recovery(&recovery).unwrap();
    assert_eq!(
        serde_json::to_vec(&restored["replay-append-test"]).unwrap(),
        expected
    );
    let mut missing = recovery;
    missing.mutations.remove(2);
    assert!(crate::scheduler_states_from_recovery(&missing).is_err());
}
