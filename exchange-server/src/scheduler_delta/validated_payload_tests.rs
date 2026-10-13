// Included inside pine::tests so the existing schema 2 fixtures stay private.
thread_local! {
    static HINT_CHECKSUM_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(super) fn record_hint_checksum() {
    HINT_CHECKSUM_CALLS.with(|calls| calls.set(calls.get() + 1));
}

fn measured_hint_checksums<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    HINT_CHECKSUM_CALLS.with(|calls| calls.set(0));
    let result = operation();
    (result, HINT_CHECKSUM_CALLS.with(std::cell::Cell::get))
}

fn validated_payload_states(schema: u64) -> (SchedulerState, SchedulerState) {
    let mut prior = state(8);
    let mut result = next(&prior, 9);
    if schema == 3 {
        for (scheduler, count) in [(&mut prior, 8), (&mut result, 9)] {
            for agent in scheduler.agents.iter_mut().take(2) {
                let PersistedAgentKindState::Plugin { data, .. } = &mut agent.kind_state else {
                    unreachable!()
                };
                let map = Arc::make_mut(data).as_object_mut().unwrap();
                let tail = map.remove("bars").unwrap();
                map.remove("accounts");
                map.remove("evaluations");
                map.remove("providers");
                map.insert("broker_schema".into(), 3.into());
                map.insert("evaluated_bars".into(), count.into());
                map.insert("vm_checkpoint".into(), serde_json::json!({"bars": 0}));
                map.insert("vm_tail".into(), tail);
                map.insert("vm_tail_intents".into(), serde_json::json!([]));
                map.insert(
                    "execution_seal".into(),
                    format!("fixture-seal-{count}").into(),
                );
            }
        }
    }
    (prior, result)
}

fn validated_payload_hint(schema: u64) -> PineStateHint {
    let (prior, result) = validated_payload_states(schema);
    PineStateHint::prepare(&prior.agents[0].kind_state, &result.agents[0].kind_state).unwrap()
}

#[test]
fn validated_payload_prepares_once_and_clones_reuse_exact_proof() {
    for schema in [2, 3] {
        let (prior, result) = validated_payload_states(schema);
        let (hint, calls) = measured_hint_checksums(|| {
            PineStateHint::prepare(&prior.agents[0].kind_state, &result.agents[0].kind_state)
                .unwrap()
        });
        assert_eq!(calls, 1);
        let copy = hint.clone();
        assert!(Arc::ptr_eq(
            hint.validated_payload.as_ref().unwrap(),
            copy.validated_payload.as_ref().unwrap(),
        ));
        let ((), calls) = measured_hint_checksums(|| {
            for value in [&hint, &copy] {
                assert!(value.validated_payload.as_ref().unwrap().matches(value));
                value.validate().unwrap();
                assert!(value.matches(&prior.agents[0].kind_state, &result.agents[0].kind_state));
                let installed = value.materialize(&prior.agents[0].kind_state).unwrap();
                assert!(same_identity(&installed, &result.agents[0].kind_state));
            }
        });
        assert_eq!(calls, 0, "unchanged private payload must not be rehashed");
    }
}

#[test]
fn validated_payload_owns_each_allocation_and_cow_always_invalidates_identity() {
    for field in 0..3 {
        let mut hint = validated_payload_hint(2);
        // Even with no other PineStateHint clone, the proof holds strong Arcs.
        assert!(Arc::get_mut(&mut hint.metadata).is_none());
        assert!(Arc::get_mut(&mut hint.arrays).is_none());
        assert!(Arc::get_mut(&mut hint.providers).is_none());
        match field {
            0 => {
                let _ = Arc::make_mut(&mut hint.metadata);
            }
            1 => {
                let _ = Arc::make_mut(&mut hint.arrays);
            }
            _ => {
                let _ = Arc::make_mut(&mut hint.providers);
            }
        }
        assert!(!hint.validated_payload.as_ref().unwrap().matches(&hint));
        let ((), calls) = measured_hint_checksums(|| hint.validate().unwrap());
        assert_eq!(calls, 1, "equal content at a new address must be checked");
    }
}

type HintMutation = (&'static str, fn(&mut PineStateHint));

#[test]
fn validated_payload_detects_mutation_of_every_wire_field() {
    let mutations: &[HintMutation] = &[
        ("plugin_version", |hint| {
            hint.plugin_version.push_str("-changed")
        }),
        ("state_version", |hint| hint.state_version += 1),
        ("base_sha256", |hint| hint.base_sha256[31] ^= 1),
        ("patch_sha256", |hint| hint.patch_sha256[31] ^= 1),
        ("metadata value", |hint| {
            Arc::make_mut(&mut hint.metadata)
                .insert("orders".into(), serde_json::json!({"changed": true}));
        }),
        ("metadata identity", |hint| {
            Arc::make_mut(&mut hint.metadata).insert("identity".into(), "changed".into());
        }),
        ("metadata missing identity", |hint| {
            Arc::make_mut(&mut hint.metadata).remove("identity");
        }),
        ("metadata empty identity", |hint| {
            Arc::make_mut(&mut hint.metadata).insert("identity".into(), "".into());
        }),
        ("metadata schema", |hint| {
            Arc::make_mut(&mut hint.metadata).insert("broker_schema".into(), 4.into());
        }),
        ("metadata schema null", |hint| {
            Arc::make_mut(&mut hint.metadata).insert("broker_schema".into(), Value::Null);
        }),
        ("metadata forbidden history", |hint| {
            Arc::make_mut(&mut hint.metadata).insert("bars".into(), serde_json::json!([]));
        }),
        ("metadata forbidden reconstructed field", |hint| {
            Arc::make_mut(&mut hint.metadata).insert("providers".into(), serde_json::json!({}));
        }),
        ("array removed", |hint| {
            Arc::make_mut(&mut hint.arrays).remove("accounts");
        }),
        ("array added", |hint| {
            Arc::make_mut(&mut hint.arrays).insert(
                "unknown".into(),
                Append {
                    base_len: 0,
                    items: vec![],
                },
            );
        }),
        ("array base_len", |hint| {
            Arc::make_mut(&mut hint.arrays)
                .get_mut("bars")
                .unwrap()
                .base_len += 1;
        }),
        ("array item", |hint| {
            Arc::make_mut(&mut hint.arrays)
                .get_mut("bars")
                .unwrap()
                .items[0]["close"] = 998.into();
        }),
        ("array item removed", |hint| {
            Arc::make_mut(&mut hint.arrays)
                .get_mut("bars")
                .unwrap()
                .items
                .clear();
        }),
        ("array item added", |hint| {
            Arc::make_mut(&mut hint.arrays)
                .get_mut("bars")
                .unwrap()
                .items
                .push(Value::Null);
        }),
        ("array length overflow", |hint| {
            Arc::make_mut(&mut hint.arrays)
                .get_mut("bars")
                .unwrap()
                .base_len = usize::MAX;
        }),
        ("provider removed", |hint| {
            Arc::make_mut(&mut hint.providers).remove("TEST:10S");
        }),
        ("provider added", |hint| {
            Arc::make_mut(&mut hint.providers).insert(
                "NEW:1S".into(),
                Append {
                    base_len: 0,
                    items: vec![],
                },
            );
        }),
        ("provider base_len", |hint| {
            Arc::make_mut(&mut hint.providers)
                .get_mut("TEST:10S")
                .unwrap()
                .base_len += 1;
        }),
        ("provider item", |hint| {
            Arc::make_mut(&mut hint.providers)
                .get_mut("TEST:10S")
                .unwrap()
                .items[0]["close"] = 998.into();
        }),
        ("provider length overflow", |hint| {
            Arc::make_mut(&mut hint.providers)
                .get_mut("TEST:10S")
                .unwrap()
                .base_len = usize::MAX;
        }),
    ];
    let original = validated_payload_hint(2);
    for &(name, mutate) in mutations {
        let mut corrupted = original.clone();
        mutate(&mut corrupted);
        assert!(
            !corrupted
                .validated_payload
                .as_ref()
                .unwrap()
                .matches(&corrupted),
            "{name}"
        );
        assert!(corrupted.validate().is_err(), "{name}");
        original.validate().unwrap();
    }
}

#[test]
fn validated_payload_stale_proof_never_skips_shape_even_after_rehash() {
    for schema in [2, 3] {
        for case in 0..5 {
            let mut corrupted = validated_payload_hint(schema);
            let key = history_arrays(schema).unwrap()[0];
            match case {
                0 => {
                    Arc::make_mut(&mut corrupted.arrays).remove(key);
                }
                1 => {
                    Arc::make_mut(&mut corrupted.metadata).insert(key.into(), Value::Null);
                }
                2 => {
                    Arc::make_mut(&mut corrupted.metadata)
                        .insert(reconstructed_field(schema).into(), Value::Null);
                }
                3 => {
                    Arc::make_mut(&mut corrupted.metadata).remove("identity");
                }
                _ => {
                    Arc::make_mut(&mut corrupted.arrays)
                        .get_mut(key)
                        .unwrap()
                        .base_len = usize::MAX;
                }
            }
            corrupted.patch_sha256 = corrupted.checksum().unwrap();
            let (outcome, calls) = measured_hint_checksums(|| corrupted.validate());
            assert!(outcome.is_err(), "schema {schema}, case {case}");
            assert_eq!(calls, 0, "original shape checks precede checksum");
        }
    }
    let mut schema3 = validated_payload_hint(3);
    Arc::make_mut(&mut schema3.providers).insert(
        "TEST:10S".into(),
        Append {
            base_len: 0,
            items: vec![],
        },
    );
    schema3.patch_sha256 = schema3.checksum().unwrap();
    assert!(schema3.validate().is_err());
}

#[test]
fn validated_payload_proof_is_not_serialized_and_old_wire_is_byte_identical() {
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
    for schema in [2, 3] {
        let (prior, result) = validated_payload_states(schema);
        let hint =
            PineStateHint::prepare(&prior.agents[0].kind_state, &result.agents[0].kind_state)
                .unwrap();
        let bytes = serde_json::to_vec(&hint).unwrap();
        let legacy = LegacyHint {
            plugin_version: &hint.plugin_version,
            state_version: hint.state_version,
            base_sha256: &hint.base_sha256,
            metadata: &hint.metadata,
            arrays: &hint.arrays,
            providers: &hint.providers,
            patch_sha256: &hint.patch_sha256,
        };
        assert_eq!(bytes, serde_json::to_vec(&legacy).unwrap());
        let decoded: PineStateHint = serde_json::from_slice(&bytes).unwrap();
        assert!(decoded.validated_payload.is_none());
        assert!(decoded.prepared.is_none());
        assert!(!decoded.matches(&prior.agents[0].kind_state, &result.agents[0].kind_state));
        let ((), calls) = measured_hint_checksums(|| decoded.validate().unwrap());
        assert_eq!(calls, 1, "wire input always receives strict validation");
        let recovered = decoded.materialize(&prior.agents[0].kind_state).unwrap();
        assert_eq!(recovered, result.agents[0].kind_state);
        assert!(!same_identity(&recovered, &result.agents[0].kind_state));
        assert_eq!(bytes, serde_json::to_vec(&decoded).unwrap());
    }
}

#[test]
fn validated_payload_header_restore_reuses_only_exact_original_certificate() {
    let mut hint = validated_payload_hint(2);
    let version = hint.plugin_version.clone();
    hint.plugin_version.push_str("-changed");
    assert!(hint.validate().is_err());
    hint.plugin_version = version;
    let ((), calls) = measured_hint_checksums(|| hint.validate().unwrap());
    assert_eq!(
        calls, 0,
        "exact original headers and allocations are valid again"
    );

    let alien = validated_payload_hint(2);
    hint.validated_payload = alien.validated_payload;
    let ((), calls) = measured_hint_checksums(|| hint.validate().unwrap());
    assert_eq!(
        calls, 1,
        "equal bytes in an unrelated proof cannot authorize allocations"
    );
}

#[test]
fn validated_payload_never_authorizes_wrong_prior_or_result_identity() {
    for schema in [2, 3] {
        let (prior, result) = validated_payload_states(schema);
        let hint =
            PineStateHint::prepare(&prior.agents[0].kind_state, &result.agents[0].kind_state)
                .unwrap();
        let mut wrong = prior.clone();
        let PersistedAgentKindState::Plugin { data, .. } = &mut wrong.agents[0].kind_state else {
            unreachable!()
        };
        Arc::make_mut(data)["orders"] = serde_json::json!({"wrong": true});
        assert!(!hint.matches(&wrong.agents[0].kind_state, &result.agents[0].kind_state));
        assert!(hint.materialize(&wrong.agents[0].kind_state).is_err());
        let detached_result: PersistedAgentKindState =
            serde_json::from_slice(&serde_json::to_vec(&result.agents[0].kind_state).unwrap())
                .unwrap();
        assert!(!hint.matches(&prior.agents[0].kind_state, &detached_result));
        // A byte-identical but independently allocated predecessor may recover
        // through the original digest path; it may not use live pair matching.
        let detached_prior: PersistedAgentKindState =
            serde_json::from_slice(&serde_json::to_vec(&prior.agents[0].kind_state).unwrap())
                .unwrap();
        assert!(!hint.matches(&detached_prior, &result.agents[0].kind_state));
        let recovered = hint.materialize(&detached_prior).unwrap();
        assert_eq!(recovered, result.agents[0].kind_state);
        assert!(!same_identity(&recovered, &result.agents[0].kind_state));

        let hints = [(0, hint)].into();
        let delta = SchedulerDelta::between(&prior, &result)
            .unwrap()
            .into_durable_with_hints(&prior, &hints)
            .unwrap();
        let before = wrong.clone();
        assert!(delta.clone().apply_owned(&mut wrong).is_err());
        assert_eq!(wrong, before, "wrong predecessor must reject atomically");
        let mut live = prior.clone();
        let (outcome, calls) = measured_hint_checksums(|| delta.clone().apply_owned(&mut live));
        outcome.unwrap();
        assert_eq!(calls, 0);
        assert!(same_identity(
            &live.agents[0].kind_state,
            &result.agents[0].kind_state
        ));
        let decoded: SchedulerDelta =
            serde_json::from_slice(&serde_json::to_vec(&delta).unwrap()).unwrap();
        assert!(
            decoded
                .pine_changes
                .iter()
                .all(|change| change.state.validated_payload.is_none())
        );
        let mut recovered = prior;
        let pine_changes = decoded.pine_changes.len();
        let (outcome, calls) = measured_hint_checksums(|| decoded.apply_owned(&mut recovered));
        outcome.unwrap();
        assert!(calls >= pine_changes);
        assert_eq!(recovered, result);
    }
}

#[test]
fn validated_payload_bad_second_live_hint_rejects_before_any_installation() {
    let prior = state(8);
    let result = next(&prior, 9);
    let delta = SchedulerDelta::between(&prior, &result)
        .unwrap()
        .into_durable(&prior)
        .unwrap();
    for field in 0..3 {
        let mut corrupted = delta.clone();
        let hint = &mut corrupted.pine_changes[1].state;
        match field {
            0 => {
                Arc::make_mut(&mut hint.metadata).insert("tampered".into(), true.into());
            }
            1 => {
                Arc::make_mut(&mut hint.arrays)
                    .get_mut("bars")
                    .unwrap()
                    .items[0]["close"] = 999.into();
            }
            _ => {
                Arc::make_mut(&mut hint.providers)
                    .get_mut("TEST:10S")
                    .unwrap()
                    .items[0]["close"] = 999.into();
            }
        }
        for owned in [false, true] {
            let mut restored = prior.clone();
            let outcome = if owned {
                corrupted.clone().apply_owned(&mut restored)
            } else {
                corrupted.apply(&mut restored)
            };
            assert!(outcome.is_err());
            assert_eq!(restored, prior);
        }
    }
}

#[test]
fn validated_payload_rehashed_cow_uses_wire_result_and_never_old_prepared_result() {
    for schema in [2, 3] {
        let (prior, result) = validated_payload_states(schema);
        let original =
            PineStateHint::prepare(&prior.agents[0].kind_state, &result.agents[0].kind_state)
                .unwrap();
        for case in 0..if schema == 2 { 3 } else { 2 } {
            let mut hint = original.clone();
            match case {
                0 => {
                    Arc::make_mut(&mut hint.metadata).insert(
                        "new_valid_metadata".into(),
                        serde_json::json!({"exact": u64::MAX}),
                    );
                }
                1 => {
                    let key = history_arrays(schema).unwrap()[0];
                    Arc::make_mut(&mut hint.arrays).get_mut(key).unwrap().items[0]["close"] =
                        707.into();
                }
                _ => {
                    Arc::make_mut(&mut hint.providers)
                        .get_mut("TEST:10S")
                        .unwrap()
                        .items[0]["close"] = 808.into();
                }
            }
            hint.patch_sha256 = hint.checksum().unwrap();
            hint.validate().unwrap();
            assert!(!hint.has_validated_payload());
            assert!(!hint.matches(&prior.agents[0].kind_state, &result.agents[0].kind_state));
            let wire: PineStateHint =
                serde_json::from_slice(&serde_json::to_vec(&hint).unwrap()).unwrap();
            let expected = wire.materialize(&prior.agents[0].kind_state).unwrap();
            let actual = hint.materialize(&prior.agents[0].kind_state).unwrap();
            assert_eq!(actual, expected, "schema {schema}, case {case}");
            assert_ne!(actual, result.agents[0].kind_state);
            assert!(!same_identity(&actual, &result.agents[0].kind_state));

            // The durable builder must ignore this stale prepared hint and
            // derive a fresh one from its requested exact base/result pair.
            let supplied = [(0, hint.clone())].into();
            let delta = SchedulerDelta::between(&prior, &result)
                .unwrap()
                .into_durable_with_hints(&prior, &supplied)
                .unwrap();
            let mut installed = prior.clone();
            delta.apply_owned(&mut installed).unwrap();
            assert_eq!(installed, result);
            assert!(same_identity(
                &installed.agents[0].kind_state,
                &result.agents[0].kind_state
            ));

            // A serialized/recovered delta and a modified live capsule must
            // materialize identical bytes if both carry this recomputed patch.
            let mut delta = SchedulerDelta::between(&prior, &result)
                .unwrap()
                .into_durable(&prior)
                .unwrap();
            delta.pine_changes[0].state = hint;
            let decoded: SchedulerDelta =
                serde_json::from_slice(&serde_json::to_vec(&delta).unwrap()).unwrap();
            let mut live = prior.clone();
            let mut recovered = prior.clone();
            delta.apply_owned(&mut live).unwrap();
            decoded.apply_owned(&mut recovered).unwrap();
            assert_eq!(live, recovered);
            assert_eq!(live.agents[0].kind_state, expected);
        }
    }
}

#[test]
fn validated_payload_rehashed_header_changes_cannot_use_prepared_prior() {
    let (prior, result) = validated_payload_states(2);
    let original =
        PineStateHint::prepare(&prior.agents[0].kind_state, &result.agents[0].kind_state).unwrap();
    for case in 0..6 {
        let mut hint = original.clone();
        match case {
            0 => hint.plugin_version.push_str("-changed"),
            1 => hint.state_version += 1,
            2 => hint.base_sha256[0] ^= 1,
            3 => {
                Arc::make_mut(&mut hint.arrays)
                    .get_mut("bars")
                    .unwrap()
                    .base_len += 1
            }
            4 => {
                Arc::make_mut(&mut hint.providers)
                    .get_mut("TEST:10S")
                    .unwrap()
                    .base_len += 1
            }
            _ => {
                Arc::make_mut(&mut hint.providers).remove("TEST:10S");
            }
        }
        hint.patch_sha256 = hint.checksum().unwrap();
        hint.validate().unwrap();
        assert!(!hint.matches(&prior.agents[0].kind_state, &result.agents[0].kind_state));
        assert!(
            hint.materialize(&prior.agents[0].kind_state).is_err(),
            "case {case}"
        );
    }
}

#[test]
fn validated_payload_proof_drops_with_last_hint_and_has_no_history_chain() {
    let (prior, result) = validated_payload_states(2);
    let base = Arc::downgrade(plugin(&prior.agents[0].kind_state).unwrap().2);
    let next = Arc::downgrade(plugin(&result.agents[0].kind_state).unwrap().2);
    let hint =
        PineStateHint::prepare(&prior.agents[0].kind_state, &result.agents[0].kind_state).unwrap();
    let metadata = Arc::downgrade(&hint.metadata);
    let arrays = Arc::downgrade(&hint.arrays);
    let providers = Arc::downgrade(&hint.providers);
    let proof = Arc::clone(hint.validated_payload.as_ref().unwrap());
    drop(hint);
    drop(prior);
    drop(result);
    assert!(
        base.upgrade().is_none(),
        "proof must not own prepared predecessor"
    );
    assert!(
        next.upgrade().is_none(),
        "proof must not own prepared result"
    );
    assert!(metadata.upgrade().is_some());
    assert!(arrays.upgrade().is_some());
    assert!(providers.upgrade().is_some());
    drop(proof);
    assert!(metadata.upgrade().is_none());
    assert!(arrays.upgrade().is_none());
    assert!(providers.upgrade().is_none());
}

#[test]
fn validated_payload_builder_rejects_unauthorized_invalid_append_shape() {
    for schema in [2, 3] {
        let (prior, result) = validated_payload_states(schema);
        let original = SchedulerDelta::between(&prior, &result)
            .unwrap()
            .into_durable(&prior)
            .unwrap();
        for case in 0..5 {
            let mut corrupted = original.clone();
            let hint = &mut corrupted.pine_changes[0].state;
            match case {
                0 => {
                    Arc::make_mut(&mut hint.metadata).insert("identity".into(), "".into());
                }
                1 => {
                    Arc::make_mut(&mut hint.metadata).remove("identity");
                }
                2 => {
                    Arc::make_mut(&mut hint.metadata).insert("identity".into(), Value::Null);
                }
                3 => {
                    Arc::make_mut(&mut hint.metadata).insert("broker_schema".into(), 99.into());
                }
                _ => {
                    Arc::make_mut(&mut hint.arrays).remove(history_arrays(schema).unwrap()[0]);
                }
            }
            hint.patch_sha256 = hint.checksum().unwrap();
            hint.validated_payload = None;
            assert!(hint.validate_shape().is_err());
            assert!(hint.validate().is_err());
            assert!(!hint.matches(&prior.agents[0].kind_state, &result.agents[0].kind_state));
            // An already-constructed invalid hint remains an error in either
            // durable builder. It must not disappear into a full replacement.
            assert!(
                corrupted.clone().into_durable(&prior).is_err(),
                "schema {schema}, case {case}"
            );
            assert!(
                corrupted
                    .into_durable_with_hints(&prior, &BTreeMap::new())
                    .is_err()
            );
        }
    }
}

#[test]
fn validated_payload_preserves_preexisting_empty_identity_full_fallback() {
    // The pre-proof prepare implementation rejects these pairs at its first
    // identity guard. This is already a full fallback, not a shape-check error.
    // Preserve that boundary rather than accidentally changing its semantics.
    for schema in [2, 3] {
        for case in 0..3 {
            let (mut prior, mut result) = validated_payload_states(schema);
            for (is_prior, scheduler) in [(true, &mut prior), (false, &mut result)] {
                if case == 2 || (case == 0) == is_prior {
                    for agent in scheduler.agents.iter_mut().take(2) {
                        let PersistedAgentKindState::Plugin { data, .. } = &mut agent.kind_state
                        else {
                            unreachable!()
                        };
                        Arc::make_mut(data)["identity"] = "".into();
                    }
                }
            }
            assert!(
                PineStateHint::prepare(&prior.agents[0].kind_state, &result.agents[0].kind_state)
                    .is_none()
            );
            let delta = SchedulerDelta::between(&prior, &result)
                .unwrap()
                .into_durable(&prior)
                .unwrap();
            assert!(delta.pine_changes.is_empty());
            assert_eq!(delta.changes.len(), 2);
            let mut recovered = prior;
            delta.apply_owned(&mut recovered).unwrap();
            assert_eq!(recovered, result);
        }
    }
}
