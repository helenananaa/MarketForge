//! Schema 3 shares the existing checked append wire. Checkpoints are inherited
//! only from the exact predecessor; rollover remains a complete replacement.
use super::*;
use crate::{
    journal::{InMemoryJournalStore, JournalStore, PendingJournalMutation, RoomMutation},
    scheduler_delta::SchedulerDelta,
};
use exchange_core::{SchedulerMode, SchedulerState};
use serde_json::json;

fn data(evaluated: usize) -> Value {
    let checkpoint_bars = evaluated / 32 * 32;
    let checkpoint = (checkpoint_bars > 0).then(|| {
        json!({
            "bars": checkpoint_bars,
            "series": (0..256).map(|i| json!({
                "slot":i,"values":[i as u64, 9007199254740993_u64, u64::MAX],
                "float_bits":{"$f64":"8000000000000000"},
                "checkpoint_only":"must-not-be-repeated-in-append-wire",
            })).collect::<Vec<_>>(),
        })
    });
    json!({
        "identity":"qualified-source-config-account", "broker_schema":3,
        "vm_checkpoint":checkpoint,
        "vm_tail":(checkpoint_bars..evaluated).map(|i| json!({
            "event_time_ms":(i + 1) * 1000,
            "bar":{"time":i * 1000,"close":{"$f64":"4059000000000000"}},
            "account":{"cash":1_000_000,"position":0},
            "request_data":[{"symbol":"BTC","timeframe":"30S","bars":[]}],
        })).collect::<Vec<_>>(),
        "vm_tail_intents":(checkpoint_bars..evaluated).filter(|i| i % 2 == 0)
            .map(|i| json!({"bar_index":i,"name":"long","qty":1})).collect::<Vec<_>>(),
        "evaluated_bars":evaluated,"last_bar_time":evaluated.saturating_sub(1) * 1000,
        "execution_seal":format!("seal-{evaluated}"),
        "candle_audits":{"BTC:1S":{"count":evaluated,"sha256":format!("{evaluated:064x}")}},
        "orders":{},"_host_receipts":[],"obsolete_metadata":"remove on next decision",
    })
}

fn state(evaluated: usize) -> SchedulerState {
    let spec: exchange_core::population::BackgroundMarket = serde_json::from_str(include_str!(
        "../../../scripts/fixtures/microstructure_market.json"
    ))
    .unwrap();
    let mut state = SchedulerState::new(
        "vm-append-test",
        spec.agents,
        SchedulerMode::Auto { interval_ms: 1000 },
    );
    for agent in state.agents.iter_mut().take(2) {
        agent.kind_state = PersistedAgentKindState::Plugin {
            plugin_id: "pine.strategy".into(),
            plugin_version: "2.0.0".into(),
            state_version: 1,
            data: Arc::new(data(evaluated)),
        };
    }
    state
}

fn next(prior: &SchedulerState, evaluated: usize) -> SchedulerState {
    let mut next = prior.clone();
    next.revision += 1;
    for agent in next.agents.iter_mut().take(2) {
        let mut value = data(evaluated);
        value.as_object_mut().unwrap().remove("obsolete_metadata");
        value["orders"] = json!({"long":{"order_id":81,"remaining":7}});
        value["_host_receipts"] = json!([{"order_id":81,"accepted":true}]);
        value["ledgers"] = json!({"BTC":{
            "fills":[{"id":73,"order_id":81,"time":evaluated * 1000,
                "qty":2,"price":100,"buy":true,"fee":17}],
            "position":2,"cost":"200","last_account_time":evaluated * 1000,
            "last_account":{"account_id":7,"cash":999783,"position":2},
        }});
        value["future_metadata"] = json!({"frozen":{"$f64":"7ff8000000000042"}});
        *payload_mut(&mut agent.kind_state) = value;
    }
    next
}

fn payload_mut(state: &mut PersistedAgentKindState) -> &mut Value {
    let PersistedAgentKindState::Plugin { data, .. } = state else {
        panic!("expected Pine state")
    };
    Arc::make_mut(data)
}

fn delta(prior: &SchedulerState, next: &SchedulerState) -> SchedulerDelta {
    SchedulerDelta::between(prior, next)
        .unwrap()
        .into_durable(prior)
        .unwrap()
}

fn decoded(delta: &SchedulerDelta) -> SchedulerDelta {
    serde_json::from_slice(&serde_json::to_vec(delta).unwrap()).unwrap()
}

#[test]
fn schema3_append_wire_omits_checkpoint_and_old_tail_and_recovers_exact_json() {
    for (before, after) in [(1, 2), (32, 35), (62, 63)] {
        let prior = state(before);
        let next = next(&prior, after);
        let delta = delta(&prior, &next);
        assert_eq!(delta.version, 2);
        assert!(delta.changes.is_empty());
        let hint = &delta.pine_changes[0].state;
        assert!(!hint.metadata.contains_key("vm_checkpoint"));
        assert!(hint.providers.is_empty());
        assert_eq!(hint.arrays["vm_tail"].items.len(), after - before);
        assert_eq!(hint.arrays["vm_tail"].base_len, before % 32);
        assert_eq!(
            hint.arrays["vm_tail_intents"].items.len(),
            (before..after).filter(|i| i % 2 == 0).count()
        );
        let encoded = serde_json::to_vec(&delta).unwrap();
        assert!(!String::from_utf8_lossy(&encoded).contains("checkpoint_only"));
        if before >= 32 {
            let full = SchedulerDelta::between(&prior, &next).unwrap();
            assert!(encoded.len() * 8 < serde_json::to_vec(&full).unwrap().len());
        }
        let mut live = prior.clone();
        delta.clone().apply_owned(&mut live).unwrap();
        assert!(same_identity(
            &live.agents[0].kind_state,
            &next.agents[0].kind_state
        ));
        let mut recovered = prior;
        decoded(&delta).apply_owned(&mut recovered).unwrap();
        assert_eq!(
            serde_json::to_vec(&recovered).unwrap(),
            serde_json::to_vec(&next).unwrap()
        );
    }
}

#[test]
fn schema3_metadata_only_feedback_keeps_checkpoint_and_both_tails_exact() {
    let prior = state(51);
    let next = next(&prior, 51);
    let delta = delta(&prior, &next);
    assert_eq!(delta.version, 2);
    assert!(
        delta.pine_changes.iter().all(|change| change
            .state
            .arrays
            .values()
            .all(|a| a.items.is_empty()))
    );
    let mut recovered = prior;
    decoded(&delta).apply(&mut recovered).unwrap();
    assert_eq!(recovered, next);
}

#[test]
fn schema3_shared_hint_payload_preserves_legacy_wire_hash_and_durable_recovery() {
    // Keep the pre-sharing wire layout explicit: Arc must not add wrappers,
    // change field ordering, or affect canonical numeric fingerprinting.
    #[derive(Serialize)]
    struct LegacyHint<'a> {
        plugin_version: &'a str,
        state_version: u16,
        base_sha256: &'a [u8; 32],
        metadata: &'a Map<String, Value>,
        arrays: &'a BTreeMap<String, Append>,
        providers: &'a BTreeMap<String, Append>,
        patch_sha256: &'a [u8; 32],
    }

    let prior = state(45);
    let next = next(&prior, 46);
    let hints = [(
        0,
        PineStateHint::prepare(&prior.agents[0].kind_state, &next.agents[0].kind_state).unwrap(),
    )]
    .into();
    let delta = SchedulerDelta::between(&prior, &next)
        .unwrap()
        .into_durable_with_hints(&prior, &hints)
        .unwrap();
    let supplied = &hints[&0];
    let retained = &delta.pine_changes[0].state;
    let copied = delta.clone();
    for hint in [retained, &copied.pine_changes[0].state] {
        assert!(Arc::ptr_eq(&supplied.metadata, &hint.metadata));
        assert!(Arc::ptr_eq(&supplied.arrays, &hint.arrays));
        assert!(Arc::ptr_eq(&supplied.providers, &hint.providers));
        assert!(hint.matches(&prior.agents[0].kind_state, &next.agents[0].kind_state));
        let legacy = LegacyHint {
            plugin_version: &hint.plugin_version,
            state_version: hint.state_version,
            base_sha256: &hint.base_sha256,
            metadata: hint.metadata.as_ref(),
            arrays: hint.arrays.as_ref(),
            providers: hint.providers.as_ref(),
            patch_sha256: &hint.patch_sha256,
        };
        assert_eq!(
            serde_json::to_vec(hint).unwrap(),
            serde_json::to_vec(&legacy).unwrap()
        );
        assert_eq!(
            hint.patch_sha256,
            fingerprint(&(
                &hint.plugin_version,
                hint.state_version,
                &hint.base_sha256,
                hint.metadata.as_ref(),
                hint.arrays.as_ref(),
                hint.providers.as_ref(),
            ))
            .unwrap()
        );
    }
    let encoded = serde_json::to_vec(&delta).unwrap();
    let mut live = prior.clone();
    copied.apply_owned(&mut live).unwrap();
    assert!(same_identity(
        &live.agents[0].kind_state,
        &next.agents[0].kind_state
    ));

    let mut store = InMemoryJournalStore::new();
    for mutation in [
        RoomMutation::SchedulerProgress {
            clock_steps: 0,
            state: prior,
            training: None,
        },
        RoomMutation::SchedulerDelta {
            clock_steps: 1,
            delta,
            training: None,
        },
    ] {
        store
            .append_room_mutation(
                &PendingJournalMutation::new("vm-append-test", 0, mutation),
                &[],
                &[],
                None,
            )
            .unwrap();
    }
    let recovery = store.load_recovery().unwrap();
    let RoomMutation::SchedulerDelta { delta, .. } = &recovery.mutations[1].mutation else {
        panic!("expected durable Pine delta")
    };
    assert_eq!(serde_json::to_vec(delta).unwrap(), encoded);
    assert!(
        delta
            .pine_changes
            .iter()
            .all(|change| change.state.prepared.is_none())
    );
    let restored = crate::scheduler_states_from_recovery(&recovery).unwrap();
    assert_eq!(
        serde_json::to_vec(&restored["vm-append-test"]).unwrap(),
        serde_json::to_vec(&next).unwrap()
    );
}

#[test]
fn schema3_shared_hint_cow_corruption_isolated_and_rejected_before_any_install() {
    let prior = state(45);
    let next = next(&prior, 46);
    let valid = delta(&prior, &next);
    let encoded = serde_json::to_vec(&valid).unwrap();
    for field in 0..3 {
        let mut corrupt = valid.clone();
        let hint = &mut corrupt.pine_changes[1].state;
        let original = &valid.pine_changes[1].state;
        assert!(Arc::ptr_eq(&hint.metadata, &original.metadata));
        assert!(Arc::ptr_eq(&hint.arrays, &original.arrays));
        assert!(Arc::ptr_eq(&hint.providers, &original.providers));
        match field {
            0 => {
                Arc::make_mut(&mut hint.metadata)
                    .get_mut("ledgers")
                    .unwrap()["BTC"]["fills"][0]["qty"] = 99.into()
            }
            1 => {
                Arc::make_mut(&mut hint.arrays)
                    .get_mut("vm_tail")
                    .unwrap()
                    .items[0]["event_time_ms"] = 9.into()
            }
            _ => {
                Arc::make_mut(&mut hint.providers).insert(
                    "unexpected".into(),
                    Append {
                        base_len: 0,
                        items: vec![],
                    },
                );
            }
        }
        assert_eq!(Arc::ptr_eq(&hint.metadata, &original.metadata), field != 0);
        assert_eq!(Arc::ptr_eq(&hint.arrays, &original.arrays), field != 1);
        assert_eq!(
            Arc::ptr_eq(&hint.providers, &original.providers),
            field != 2
        );
        // Retaining prepared identities cannot authorize a changed payload.
        // COW invalidates its proof before validation or installation.
        assert!(!hint.matches(&prior.agents[1].kind_state, &next.agents[1].kind_state));
        assert!(hint.validate().is_err());
        assert_eq!(serde_json::to_vec(&valid).unwrap(), encoded);
        let mut target = prior.clone();
        assert!(corrupt.apply_owned(&mut target).is_err(), "field {field}");
        assert_eq!(target, prior, "first valid change must not install either");
        valid.clone().apply_owned(&mut target).unwrap();
        assert_eq!(target, next);
    }
}

#[test]
fn schema3_initialization_rollover_corrections_and_identity_changes_use_full_state() {
    for (before, after) in [(0, 1), (31, 32), (63, 64), (65, 96)] {
        let prior = state(before);
        let next = next(&prior, after);
        let delta = delta(&prior, &next);
        assert_eq!(delta.version, 1);
        assert!(delta.pine_changes.is_empty());
        let mut recovered = prior;
        decoded(&delta).apply(&mut recovered).unwrap();
        assert_eq!(recovered, next);
    }
    let prior = state(47);
    for case in 0..7 {
        let mut next = next(&prior, 48);
        for agent in next.agents.iter_mut().take(2) {
            let value = payload_mut(&mut agent.kind_state);
            match case {
                0 => value["vm_checkpoint"]["bars"] = 31.into(),
                1 => value["vm_tail"][0]["event_time_ms"] = 7.into(),
                2 => value["vm_tail_intents"][0]["qty"] = 3.into(),
                3 => {
                    value.as_object_mut().unwrap().remove("vm_checkpoint");
                }
                4 => {
                    value["vm_tail"].as_array_mut().unwrap().truncate(1);
                }
                5 => value["identity"] = "different account".into(),
                _ => value["broker_schema"] = 2.into(),
            }
        }
        let delta = delta(&prior, &next);
        assert_eq!(delta.version, 1, "case {case}");
        let mut recovered = prior.clone();
        decoded(&delta).apply_owned(&mut recovered).unwrap();
        assert_eq!(recovered, next);
    }
}

#[test]
fn schema3_corrupted_patch_or_checkpoint_never_partially_installs() {
    let prior = state(45);
    let mut next = next(&prior, 46);
    next.agents[2]
        .unfinished_actions
        .push(OrderAction::Cancel { order_id: 71 });
    let valid = delta(&prior, &next);
    for case in 0..9 {
        let mut corrupt = decoded(&valid);
        let hint = &mut corrupt.pine_changes[1].state;
        match case {
            0 => hint.base_sha256[0] ^= 1,
            1 => {
                Arc::make_mut(&mut hint.arrays)
                    .get_mut("vm_tail")
                    .unwrap()
                    .base_len += 1
            }
            2 => {
                Arc::make_mut(&mut hint.arrays)
                    .get_mut("vm_tail_intents")
                    .unwrap()
                    .base_len += 1
            }
            3 => {
                Arc::make_mut(&mut hint.metadata).insert("vm_checkpoint".into(), Value::Null);
            }
            4 => {
                Arc::make_mut(&mut hint.providers).insert(
                    "unexpected".into(),
                    Append {
                        base_len: 0,
                        items: vec![],
                    },
                );
            }
            5 => {
                Arc::make_mut(&mut hint.arrays).remove("vm_tail_intents");
            }
            6 => {
                Arc::make_mut(&mut hint.metadata).insert("broker_schema".into(), 4.into());
            }
            7 => {
                Arc::make_mut(&mut hint.arrays)
                    .get_mut("vm_tail")
                    .unwrap()
                    .items[0]["event_time_ms"] = 9.into()
            }
            _ => {
                Arc::make_mut(&mut hint.metadata)
                    .insert("execution_seal".into(), "corrupted".into());
            }
        }
        if case < 7 {
            // Exercise structural/base checks independently of wire corruption.
            hint.patch_sha256 = hint.checksum().unwrap();
        }
        let mut recovered = prior.clone();
        assert!(corrupt.apply(&mut recovered).is_err(), "case {case}");
        assert_eq!(recovered, prior);
        assert!(
            corrupt.apply_owned(&mut recovered).is_err(),
            "owned case {case}"
        );
        assert_eq!(recovered, prior);
    }
    let mut wrong = prior.clone();
    payload_mut(&mut wrong.agents[1].kind_state)["vm_checkpoint"]["series"][0]["values"][0] =
        808.into();
    let unchanged = wrong.clone();
    assert!(decoded(&valid).apply_owned(&mut wrong).is_err());
    assert_eq!(
        wrong, unchanged,
        "same-length checkpoint edits must not inherit"
    );
}

#[test]
fn schema3_memory_journal_recovers_across_rollovers_without_retaining_history_arcs() {
    let mut store = InMemoryJournalStore::new();
    let (weak, expected) = {
        let mut prior = state(30);
        let mut weak = vec![Arc::downgrade(
            plugin(&prior.agents[0].kind_state).unwrap().2,
        )];
        store
            .append_room_mutation(
                &PendingJournalMutation::new(
                    "vm-append-test",
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
        for evaluated in 31..=65 {
            let next = next(&prior, evaluated);
            weak.push(Arc::downgrade(
                plugin(&next.agents[0].kind_state).unwrap().2,
            ));
            let delta = delta(&prior, &next);
            assert_eq!(delta.version, if evaluated % 32 == 0 { 1 } else { 2 });
            store
                .append_room_mutation(
                    &PendingJournalMutation::new(
                        "vm-append-test",
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
    assert!(
        weak.iter().all(|w| w.upgrade().is_none()),
        "journal bytes must not own prepared bases/results"
    );
    let recovery = store.load_recovery().unwrap();
    assert_eq!(
        recovery.mutations.len(),
        36,
        "no historical records were discarded"
    );
    let restored = crate::scheduler_states_from_recovery(&recovery).unwrap();
    assert_eq!(
        serde_json::to_vec(&restored["vm-append-test"]).unwrap(),
        expected
    );
    let mut missing = recovery;
    missing.mutations.remove(2); // The first full rollover cannot be skipped.
    assert!(crate::scheduler_states_from_recovery(&missing).is_err());
}

#[test]
fn schema3_postgres_jsonb_numeric_roundtrip_preserves_checkpoint_binding() {
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
    let mut prior = state(71);
    let mut next = next(&prior, 72);
    for scheduler in [&mut prior, &mut next] {
        for agent in scheduler.agents.iter_mut().take(2) {
            let value = payload_mut(&mut agent.kind_state);
            let numbers = json!([1e18, -0.0, 1e-15, u64::MAX, i64::MIN, 9007199254740993_u64]);
            value["number_forms"] = numbers.clone();
            value["vm_checkpoint"]["number_forms"] = numbers;
        }
    }
    let delta = delta(&prior, &next);
    assert_eq!(delta.version, 2);
    let recovered_base: SchedulerState =
        serde_json::from_value(jsonb(serde_json::to_value(prior).unwrap())).unwrap();
    let recovered_delta: SchedulerDelta =
        serde_json::from_value(jsonb(serde_json::to_value(delta).unwrap())).unwrap();
    let expected: SchedulerState =
        serde_json::from_value(jsonb(serde_json::to_value(next).unwrap())).unwrap();
    let mut recovered = recovered_base.clone();
    recovered_delta.clone().apply_owned(&mut recovered).unwrap();
    assert_eq!(recovered, expected);
    assert_eq!(
        serde_json::to_vec(&recovered).unwrap(),
        serde_json::to_vec(&expected).unwrap()
    );
    let mut wrong = recovered_base;
    payload_mut(&mut wrong.agents[1].kind_state)["vm_checkpoint"]["number_forms"][5] =
        json!(9007199254740992_u64);
    let before = wrong.clone();
    assert!(recovered_delta.apply_owned(&mut wrong).is_err());
    assert_eq!(
        wrong, before,
        "JSONB must not round distinct large integers into the same predecessor"
    );
}
