//! Fixed-order Release diagnostic: old per-bot owned delivery versus the actual
//! native batch dispatch. Both commit identical results through the live gateway.
use super::*;
use journal::JournalStore;
use sha2::{Digest, Sha256};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "bounded fixed-order Release delivery comparison; run alone"]
async fn fixed_native_delivery_workload() {
    #[cfg(feature = "memory-diagnostics")]
    crate::memory_diagnostics::configure_reference();
    let spec: exchange_core::population::BackgroundMarket =
        serde_json::from_slice(&std::fs::read(std::env::var("MF_FIXED_RECIPE").unwrap()).unwrap())
            .unwrap();
    let mode = std::env::var("MF_DELIVERY_MODE").unwrap();
    assert!(matches!(mode.as_str(), "owned" | "native"));
    let steps: usize = std::env::var("MF_FIXED_STEPS")
        .unwrap_or("64".into())
        .parse()
        .unwrap();
    assert!((1..=128).contains(&steps));
    let room = spec.scenario.room_id.clone();
    let mut store = journal::SharedInMemoryJournalStore::new();
    let shared = Arc::new(ServerState::new(AppState::new_with_journal(
        "http://localhost",
        Box::new(store.clone()),
    )));
    let _ = create_room(
        State(shared.clone()),
        HeaderMap::new(),
        Json(serde_json::to_value(spec.scenario).unwrap()),
    )
    .await
    .unwrap();
    let initial = SchedulerState::new(
        &room,
        spec.agents,
        exchange_core::SchedulerMode::Auto { interval_ms: 25 },
    );
    let count = initial.agents.len();
    {
        let mut app = shared.app.lock_room(&room).await;
        let cursor = next_persisted_command_cursor(&app, &room).unwrap();
        app.append_room_mutation(
            &PendingJournalMutation::new(
                &room,
                cursor,
                RoomMutation::SchedulerProgress {
                    clock_steps: 0,
                    state: initial.clone(),
                    training: None,
                },
            ),
            &[],
            &[],
            None,
        )
        .await
        .unwrap();
        app.schedulers.insert(room.clone(), initial);
    }
    let control = Control::default();
    let registry = exchange_core::BotRegistry::with_builtins();
    let mut instances = BTreeMap::<String, CachedBot>::new();
    let mut actions_count = 0;
    let mut empty_count = 0;
    let mut delivery_ms = 0.0;
    let mut dispatches = 0;
    let started = Instant::now();
    for _ in 0..steps {
        commit_realtime_work(shared.clone(), room.clone(), Work::Clock(control.clone()))
            .await
            .unwrap();
        let priors: Vec<PersistedAgent> = shared.app.lock_room(&room).await.schedulers[&room]
            .agents
            .clone();
        let rooms = (*shared.app.read_room(&room).await.rooms).clone();
        let decision_started = Instant::now();
        let mut jobs = tokio::task::JoinSet::new();
        if mode == "native" {
            let frozen = Arc::new(rooms.into_native_observation_wave(room.clone()));
            let mut inputs = Vec::new();
            for prior in &priors {
                let cached = instances
                    .remove(prior.template.participant_id())
                    .filter(|cached| cached.matches(0, prior, &registry));
                inputs.push((prior.clone().into(), cached));
                if inputs.len() == NATIVE_DECISIONS_PER_TASK {
                    spawn_decision_batch(
                        &mut jobs,
                        frozen.clone(),
                        registry.clone(),
                        std::mem::take(&mut inputs),
                        0,
                        true,
                    );
                    dispatches += 1;
                }
            }
            if !inputs.is_empty() {
                spawn_decision_batch(&mut jobs, frozen, registry.clone(), inputs, 0, true);
                dispatches += 1;
            }
        } else {
            let mut observations = rooms.observation_batch(&room);
            for prior in &priors {
                let id = prior.template.participant_id().to_string();
                let cached = instances
                    .remove(&id)
                    .filter(|cached| cached.matches(0, prior, &registry));
                let request = match &cached {
                    Some(cached) => cached.request.clone(),
                    None => Arc::new(ObservationRequest {
                        history: registry.market_data_request(&prior.template).unwrap(),
                        related: registry.related_instruments(&prior.template).unwrap(),
                        intervals: registry.additional_intervals(&prior.template).unwrap(),
                        account_events: registry.account_events(&prior.template),
                    }),
                };
                let mut view = observations
                    .bot_observation(
                        prior.requires_instrument().unwrap(),
                        prior.account_id(),
                        request.history,
                        &request.related,
                    )
                    .unwrap();
                observations
                    .enrich_data(
                        &mut view,
                        request.history,
                        &request.related,
                        &request.intervals,
                        request.account_events,
                        prior.account_id(),
                    )
                    .unwrap();
                let registry = registry.clone();
                let prior = prior.clone().into();
                jobs.spawn_blocking(move || {
                    vec![(
                        id,
                        0,
                        compute_decision(prior, view, registry, cached, request, 0),
                    )]
                });
                dispatches += 1;
            }
        }
        let mut computed = BTreeMap::new();
        while let Some(packet) = jobs.join_next().await {
            for (id, _, result) in packet.unwrap() {
                let (decision, cached) = result.unwrap();
                if let Some(cached) = cached {
                    instances.insert(id.clone(), cached);
                }
                computed.insert(id, decision);
            }
        }
        delivery_ms += decision_started.elapsed().as_secs_f64() * 1000.0;
        let wave: Vec<_> = priors
            .iter()
            .map(|prior| computed.remove(prior.template.participant_id()).unwrap())
            .collect();
        for decision in &wave {
            actions_count += decision.actions.len();
            empty_count += usize::from(decision.actions.is_empty());
        }
        for chunk in wave.chunks(MAX_READY_DECISIONS) {
            commit_realtime_work(
                shared.clone(),
                room.clone(),
                Work::Bots {
                    control: control.clone(),
                    epoch: 0,
                    decisions: chunk.to_vec(),
                },
            )
            .await
            .unwrap();
        }
    }
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    #[cfg(feature = "memory-diagnostics")]
    let memory = serde_json::json!({
        "heap_before_receipt":crate::memory_diagnostics::snapshot(),
        "journal":store.memory_diagnostics(),
        "original_capacity":crate::memory_diagnostics::KEEP_SPARE_CAPACITY.load(std::sync::atomic::Ordering::Relaxed),
        "definition":"fixed-work requested heap before final state/execution receipt serialization"});
    let app = shared.app.lock_room(&room).await;
    let seq = latest_persisted_command_seq(&app, &room).unwrap_or(0);
    let mut payload = serde_json::json!({
        "snapshot": current_room_snapshot(&app.rooms, &room, seq), "scheduler": app.schedulers[&room],
        "executions": store.load_recovery().unwrap().executions,
    });
    // This snapshot contains an unordered ID set; normalize only its iteration
    // order, retaining every account, action, fee, clock and bot-state value.
    fn canonical_sets(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(fields) => {
                for (key, value) in fields {
                    if key == "seen_order_ids" {
                        value
                            .as_array_mut()
                            .unwrap()
                            .sort_by_key(|id| id.as_u64().unwrap());
                    } else {
                        canonical_sets(value);
                    }
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    canonical_sets(value);
                }
            }
            _ => {}
        }
    }
    canonical_sets(&mut payload["snapshot"]);
    let receipt = serde_json::to_vec(&payload).unwrap();
    let output = std::env::var("MF_FIXED_OUTPUT").unwrap();
    std::fs::write(format!("{output}.receipt.json"), &receipt).unwrap();
    let report = serde_json::json!({"mode":mode, "steps":steps,"bots":count,
        "decisions":steps*count,"actions":actions_count,"empty":empty_count,"dispatches":dispatches,
        "elapsed_ms":elapsed_ms,"delivery_ms":delivery_ms,"receipt_sha256":format!("{:x}",Sha256::digest(&receipt))});
    #[cfg(feature = "memory-diagnostics")]
    std::fs::write(
        format!("{output}.memory.json"),
        serde_json::to_vec_pretty(&memory).unwrap(),
    )
    .unwrap();
    std::fs::write(&output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    println!("{report}");
}
