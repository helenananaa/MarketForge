use super::*;
use exchange_core::{
    BotConfig, BotDescriptor, BotError, BotFactory, BotRegistry, ParticipantConfig,
    ParticipantKind, PersistedAgent, PersistedAgentKindState, ScheduledBot, SchedulerState, Side,
};
use std::sync::Condvar;

#[derive(Default)]
struct Gate {
    started: Notify,
    entered: AtomicUsize,
    released: Mutex<bool>,
    wake: Condvar,
}

impl Gate {
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.wake.notify_all();
    }
}

// Always unblock plugin threads, including when an assertion fails.
struct ReleaseOnDrop(Arc<Gate>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

struct Factory {
    descriptor: BotDescriptor,
    gate: Arc<Gate>,
}
struct Bot {
    id: String,
    gate: Arc<Gate>,
    state: PersistedAgentKindState,
}

impl BotFactory for Factory {
    fn descriptor(&self) -> &BotDescriptor {
        &self.descriptor
    }
    fn create(
        &self,
        template: &AgentTemplate,
        state: &PersistedAgentKindState,
    ) -> Result<Box<dyn ScheduledBot>, BotError> {
        Ok(Box::new(Bot {
            id: template.participant_id().into(),
            gate: self.gate.clone(),
            state: state.clone(),
        }))
    }
}

impl ScheduledBot for Bot {
    fn decide(&mut self, _: &ParticipantObservation) -> Result<Vec<OrderAction>, BotError> {
        if self.id == "slow" {
            self.gate.entered.fetch_add(1, Ordering::SeqCst);
            self.gate.started.notify_one();
            let mut released = self.gate.released.lock().unwrap();
            while !*released {
                released = self.gate.wake.wait(released).unwrap();
            }
        }
        if self.id == "fail" {
            return Err(BotError("controlled timeout".into()));
        }
        if self.id == "panic" {
            panic!("controlled bot panic");
        }
        if let PersistedAgentKindState::Plugin { data, .. } = &mut self.state {
            *data = serde_json::json!(data.as_u64().unwrap_or(0) + 1);
        }
        Ok(vec![OrderAction::PlaceLimit {
            side: Side::Buy,
            price_tick: 90,
            qty: 1,
        }])
    }
    fn snapshot(&self) -> PersistedAgentKindState {
        self.state.clone()
    }
}

fn template(room: &str, id: &str) -> AgentTemplate {
    AgentTemplate::Plugin(BotConfig {
        participant: ParticipantConfig {
            participant_id: id.into(),
            kind: ParticipantKind::RuleAgent,
            room_id: room.into(),
            account_id: 20,
            instrument_id: Some("V-BTC-SPOT".into()),
        },
        plugin_id: "test.live".into(),
        plugin_version: "1".into(),
        config_version: 1,
        state_version: 1,
        seed: 1,
        config: serde_json::json!({}),
    })
}

fn scenario(room: &str) -> ScenarioConfig {
    let value: serde_json::Value =
        serde_json::from_str(include_str!("../../scripts/fixtures/f6_batch_spec.json")).unwrap();
    let mut scenario: ScenarioConfig = serde_json::from_value(value["scenario"].clone()).unwrap();
    scenario.room_id = room.into();
    scenario
}

async fn setup(room: &str) -> (SharedState, Arc<Gate>, journal::SharedInMemoryJournalStore) {
    let gate = Arc::new(Gate::default());
    let mut registry = BotRegistry::with_builtins();
    registry
        .register(Factory {
            descriptor: BotDescriptor {
                id: "test.live".into(),
                name: "Test".into(),
                version: "1".into(),
                protocol_version: "bot.v1".into(),
                state_version: 1,
                runtime: "test".into(),
                parameters: BTreeMap::new(),
            },
            gate: gate.clone(),
        })
        .unwrap();
    let store = journal::SharedInMemoryJournalStore::new();
    let mut app = AppState::new_with_journal("http://localhost", Box::new(store.clone()));
    app.bot_registry = registry;
    let shared = Arc::new(ServerState::new(app));
    let _ = create_room(
        State(shared.clone()),
        HeaderMap::new(),
        Json(serde_json::to_value(scenario(room)).unwrap()),
    )
    .await
    .unwrap();
    (shared, gate, store)
}

async fn start(shared: &SharedState, room: &str, ids: &[&str], interval: u64) {
    let _ = start_agents(
        State(shared.clone()),
        HeaderMap::new(),
        Path(room.into()),
        Json(StartAgentsRequest {
            agents: ids.iter().map(|id| template(room, id)).collect(),
            interval_ms: Some(interval),
        }),
    )
    .await
    .unwrap();
}

async fn eventually(shared: &SharedState, condition: impl Fn(&AppState) -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if condition(&*shared.app.lock().await) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("condition must complete while the slow bot is still gated");
}

fn count(app: &AppState, room: &str, id: &str) -> u64 {
    app.schedulers
        .get(room)
        .and_then(|s| s.agents.iter().find(|a| a.template.participant_id() == id))
        .and_then(|a| match &a.kind_state {
            PersistedAgentKindState::Plugin { data, .. } => data.as_u64(),
            _ => None,
        })
        .unwrap_or(0)
}

async fn shutdown(shared: &SharedState) {
    let workers = std::mem::take(&mut shared.app.lock().await.agent_workers);
    for worker in workers.values() {
        worker.request_stop();
    }
    tokio::task::spawn_blocking(move || {
        for (_, worker) in workers {
            worker.shutdown_and_join();
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn realtime_large_wave_yields_between_bounded_durable_batches() {
    let (shared, _, mut store) = setup("bounded").await;
    let mut names: Vec<_> = (0..200).map(|i| format!("fast-{i}")).collect();
    names.sort();
    let ids: Vec<_> = names.iter().map(String::as_str).collect();
    start(&shared, "bounded", &ids, 1000).await;
    eventually(&shared, |app| {
        ids.iter().all(|id| count(app, "bounded", id) >= 1)
    })
    .await;
    shutdown(&shared).await;
    let recovery = store.load_recovery().unwrap();
    let mut previous = BTreeMap::new();
    let mut nonempty_batches = 0;
    for mutation in &recovery.mutations {
        if let RoomMutation::SchedulerDelta { delta, .. } = &mutation.mutation {
            assert!(delta.changes.len() <= 64);
            for change in &delta.changes {
                if let PersistedAgentKindState::Plugin { data, .. } = &change.kind_state {
                    previous.insert(names[change.index].clone(), data.as_u64().unwrap_or(0));
                }
            }
            nonempty_batches += usize::from(!delta.changes.is_empty());
        }
        if let RoomMutation::SchedulerProgress { state, .. } = &mutation.mutation {
            let mut changed = 0;
            for agent in &state.agents {
                if let PersistedAgentKindState::Plugin { data, .. } = &agent.kind_state {
                    let value = data.as_u64().unwrap_or(0);
                    let prior = previous
                        .insert(agent.template.participant_id().to_owned(), value)
                        .unwrap_or(0);
                    changed += usize::from(value != prior);
                }
            }
            assert!(changed <= 64, "one durable batch advanced {changed} bots");
            nonempty_batches += usize::from(changed > 0);
        }
    }
    assert!(nonempty_batches >= 4);
    let restored = scheduler_states_from_recovery(&recovery).unwrap();
    assert_eq!(restored["bounded"].agents.len(), 200);
    assert!(restored["bounded"].agents.iter().all(|agent| matches!(
        &agent.kind_state, PersistedAgentKindState::Plugin { data, .. } if data.as_u64().unwrap_or(0) >= 1
    )));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn realtime_slow_bot_does_not_block_clock_other_bot_http_or_other_room() {
    use axum::body::Body;
    use tower::ServiceExt;
    let (shared, gate, _) = setup("live").await;
    let _release = ReleaseOnDrop(gate.clone());
    let _ = create_room(
        State(shared.clone()),
        HeaderMap::new(),
        Json(serde_json::to_value(scenario("other")).unwrap()),
    )
    .await
    .unwrap();
    start(&shared, "live", &["slow", "fast"], 10).await;
    start(&shared, "other", &[], 10).await;
    tokio::time::timeout(Duration::from_secs(3), gate.started.notified())
        .await
        .unwrap();
    eventually(&shared, |app| {
        app.rooms.clock("live").unwrap().step() >= 4
            && app.rooms.clock("other").unwrap().step() >= 3
            && count(app, "live", "fast") >= 2
    })
    .await;
    assert_eq!(
        gate.entered.load(Ordering::SeqCst),
        1,
        "no overlapping decisions/backlog for a slow bot"
    );
    let router = app_with_cors_origins(shared.clone(), default_cors_origins());
    for room in ["live", "other"] {
        let request = axum::http::Request::builder()
            .method("POST")
            .uri(format!("/rooms/{room}/orders"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"participant_id":"human","account_id":20,
                "action":{"PlaceMarket":{"side":"Buy","qty":1}}})
                .to_string(),
            ))
            .unwrap();
        let response =
            tokio::time::timeout(Duration::from_secs(1), router.clone().oneshot(request))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["accepted"], true);
    }
    assert_eq!(gate.entered.load(Ordering::SeqCst), 1);
    assert!(!*gate.released.lock().unwrap());
    let _ = stop_agents(State(shared.clone()), HeaderMap::new(), Path("live".into()))
        .await
        .unwrap();
    let step = shared.app.lock().await.rooms.clock("live").unwrap().step();
    eventually(&shared, |app| {
        app.rooms.clock("live").unwrap().step() >= step + 2
    })
    .await;
    gate.release();
    shutdown(&shared).await;
    let app = shared.app.lock().await;
    assert_eq!(
        count(&app, "live", "slow"),
        0,
        "stopped bot's late result must be discarded"
    );
    assert!(!app.schedulers["live"].bots_enabled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn realtime_bot_error_and_panic_are_isolated_and_market_keeps_ticking() {
    let (shared, _, _) = setup("errors").await;
    start(&shared, "errors", &["fail", "panic", "fast"], 10).await;
    eventually(&shared, |app| {
        let status = agent_status_for_room(app, "errors");
        status.bot_errors.len() == 2
            && count(app, "errors", "fast") >= 3
            && app.rooms.clock("errors").unwrap().step() >= 4
    })
    .await;
    let app = shared.app.lock().await;
    let status = agent_status_for_room(&app, "errors");
    assert!(status.running && status.market_running);
    assert_eq!(status.bot_errors["fail"], "controlled timeout");
    assert_eq!(status.bot_errors["panic"], "bot panicked");
    assert_eq!(count(&app, "errors", "fail"), 0);
    drop(app);
    shutdown(&shared).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn realtime_process_timeout_does_not_stop_market_or_healthy_bot() {
    struct Package(std::path::PathBuf);
    impl Drop for Package {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let root = Package(
        std::env::temp_dir().join(format!("marketforge-live-timeout-{}", std::process::id())),
    );
    let package = root.0.join("slow");
    std::fs::create_dir_all(&package).unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_str(include_str!("../../bot-plugins/buy-remaining/bot.json")).unwrap();
    manifest["timeout_ms"] = serde_json::json!(100);
    std::fs::write(
        package.join("bot.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    std::fs::write(package.join("bot.py"), "import time; time.sleep(5)").unwrap();
    let (shared, _, _) = setup("process-timeout").await;
    shared.app.lock().await.bot_registry = bot_plugins::load_bot_plugins(&root.0).unwrap();
    let mut slow = match template("process-timeout", "slow") {
        AgentTemplate::Plugin(config) => config,
        _ => unreachable!(),
    };
    slow.plugin_id = "example.buy-remaining".into();
    slow.plugin_version = "1.0.0".into();
    slow.config = serde_json::json!({"target_qty":1});
    let fast = AgentTemplate::DcaTrader(exchange_core::DcaTraderConfig {
        participant: ParticipantConfig {
            participant_id: "fast".into(),
            ..slow.participant.clone()
        },
        interval_steps: 1,
        order_qty: 1,
        use_market_order: false,
        limit_offset_ticks: 0,
        fallback_price_tick: 90,
        side: Side::Buy,
    });
    let _ = start_agents(
        State(shared.clone()),
        HeaderMap::new(),
        Path("process-timeout".into()),
        Json(StartAgentsRequest {
            agents: vec![AgentTemplate::Plugin(slow), fast],
            interval_ms: Some(10),
        }),
    )
    .await
    .unwrap();
    eventually(&shared, |app| {
        agent_status_for_room(app, "process-timeout")
            .bot_errors
            .contains_key("slow")
    })
    .await;
    let step = shared
        .app
        .lock()
        .await
        .rooms
        .clock("process-timeout")
        .unwrap()
        .step();
    eventually(&shared, |app| {
        app.rooms.clock("process-timeout").unwrap().step() >= step + 2
    })
    .await;
    let app = shared.app.lock().await;
    let status = agent_status_for_room(&app, "process-timeout");
    assert!(status.market_running && status.running);
    assert!(status.bot_errors["slow"].contains("timed out"));
    assert!(!status.bot_errors.contains_key("fast"));
    assert!(
        app.rooms
            .execution_history("process-timeout")
            .unwrap()
            .len()
            > 2
    );
    drop(app);
    shutdown(&shared).await;
}

async fn prepare(shared: &SharedState, room: &str) -> (realtime::Control, PersistedAgent) {
    // Paused worker allows deterministic control over commits without wall-clock races.
    let _ = pause_room(State(shared.clone()), HeaderMap::new(), Path(room.into()))
        .await
        .unwrap();
    start(shared, room, &["fast"], 60_000).await;
    eventually(shared, |app| {
        app.agent_workers[room].status(room.into()).lifecycle == "paused"
    })
    .await;
    let app = shared.app.lock().await;
    (
        app.agent_workers[room].control.clone(),
        app.schedulers[room].agents[0].clone(),
    )
}

fn work(
    control: &realtime::Control,
    prior: &PersistedAgent,
    epoch: u64,
    actions: Vec<OrderAction>,
) -> realtime::Work {
    let mut next = prior.kind_state.clone();
    if let PersistedAgentKindState::Plugin { data, .. } = &mut next {
        *data = serde_json::json!(1);
    }
    realtime::Work::Bot {
        control: control.clone(),
        epoch,
        decision: Box::new(realtime::Decision {
            prior: prior.clone(),
            next,
            actions,
        }),
    }
}

async fn commit(
    shared: &SharedState,
    room: &str,
    work: realtime::Work,
) -> Result<SchedulerState, ApiError> {
    commit_scheduler_work(shared.clone(), room.into(), None, false, None, Some(work)).await
}

async fn prepare_wave(
    shared: &SharedState,
    room: &str,
) -> (realtime::Control, Vec<PersistedAgent>) {
    let _ = pause_room(State(shared.clone()), HeaderMap::new(), Path(room.into()))
        .await
        .unwrap();
    start(shared, room, &["fast-a", "fast-b"], 60_000).await;
    eventually(shared, |app| {
        app.agent_workers[room].status(room.into()).lifecycle == "paused"
    })
    .await;
    let _ = resume_room(State(shared.clone()), HeaderMap::new(), Path(room.into()))
        .await
        .unwrap();
    let app = shared.app.lock().await;
    (
        app.agent_workers[room].control.clone(),
        app.schedulers[room].agents.clone(),
    )
}

fn ready_wave(
    control: &realtime::Control,
    priors: &[PersistedAgent],
    bad_second: bool,
) -> realtime::Work {
    let epoch = control.epoch.load(Ordering::Acquire);
    let decisions = priors
        .iter()
        .enumerate()
        .map(|(i, prior)| {
            let actions = vec![
                OrderAction::PlaceLimit {
                    side: Side::Buy,
                    price_tick: 90,
                    qty: 1
                };
                if bad_second && i == 1 {
                    exchange_core::MAX_BOT_ACTIONS + 1
                } else {
                    1
                }
            ];
            let realtime::Work::Bot { decision, .. } = work(control, prior, epoch, actions) else {
                unreachable!()
            };
            *decision
        })
        .collect();
    realtime::Work::Bots {
        control: control.clone(),
        epoch,
        decisions,
    }
}

#[tokio::test]
async fn realtime_ready_wave_has_one_mutation_and_recovers_all_bot_states_and_orders() {
    use journal::JournalStore;
    let (shared, _, mut store) = setup("wave").await;
    let (control, priors) = prepare_wave(&shared, "wave").await;
    let before = store.load_recovery().unwrap();
    let clock = shared.app.lock().await.rooms.clock("wave").unwrap();
    commit(&shared, "wave", ready_wave(&control, &priors, false))
        .await
        .unwrap();
    let after = store.load_recovery().unwrap();
    assert_eq!(after.mutations.len(), before.mutations.len() + 1);
    assert_eq!(after.executions.len(), before.executions.len() + 2);
    let recovered = recover_rooms(&after).unwrap();
    let recovered_schedulers = scheduler_states_from_recovery(&after).unwrap();
    let app = shared.app.lock().await;
    assert_eq!(app.rooms.clock("wave").unwrap(), clock);
    assert_eq!(count(&app, "wave", "fast-a"), 1);
    assert_eq!(count(&app, "wave", "fast-b"), 1);
    assert_eq!(
        recovered.book_snapshot("wave").unwrap(),
        app.rooms.book_snapshot("wave").unwrap()
    );
    assert_eq!(recovered_schedulers["wave"], app.schedulers["wave"]);
    drop(app);
    shutdown(&shared).await;
}

#[tokio::test]
async fn realtime_periodic_checkpoint_replays_clock_and_order_tail() {
    use journal::JournalStore;
    let (shared, _, mut store) = setup("checkpoint-tail").await;
    let (control, priors) = prepare_wave(&shared, "checkpoint-tail").await;
    let initial_checkpoints = shared.lifecycle.metrics_snapshot().checkpoint_writes_total;
    for _ in 0..99 {
        commit(
            &shared,
            "checkpoint-tail",
            realtime::Work::Clock(control.clone()),
        )
        .await
        .unwrap();
    }
    assert_eq!(
        shared.lifecycle.metrics_snapshot().checkpoint_writes_total,
        initial_checkpoints
    );
    for expected in [99, 100, 101] {
        if expected > 99 {
            commit(
                &shared,
                "checkpoint-tail",
                realtime::Work::Clock(control.clone()),
            )
            .await
            .unwrap();
        }
        if expected == 101 {
            commit(
                &shared,
                "checkpoint-tail",
                ready_wave(&control, &priors, false),
            )
            .await
            .unwrap();
        }
        let recovery = store.load_recovery().unwrap();
        let recovered = recover_rooms(&recovery).unwrap();
        let app = shared.app.lock().await;
        assert_eq!(recovered.clock("checkpoint-tail").unwrap().step(), expected);
        for account in [10, 20] {
            assert_eq!(
                recovered
                    .participant_observation("checkpoint-tail", "V-BTC-SPOT", account)
                    .unwrap(),
                app.rooms
                    .participant_observation("checkpoint-tail", "V-BTC-SPOT", account)
                    .unwrap()
            );
        }
        assert_eq!(
            scheduler_states_from_recovery(&recovery).unwrap()["checkpoint-tail"],
            app.schedulers["checkpoint-tail"]
        );
    }
    assert_eq!(
        shared.lifecycle.metrics_snapshot().checkpoint_writes_total,
        initial_checkpoints + 1
    );
    shutdown(&shared).await;
}

#[tokio::test]
async fn realtime_bad_ready_wave_rolls_back_before_individual_retry() {
    use journal::JournalStore;
    let (shared, _, mut store) = setup("wave-bad").await;
    let (control, priors) = prepare_wave(&shared, "wave-bad").await;
    let before = store.load_recovery().unwrap();
    let order_id = shared.app.lock().await.next_order_id;
    let error = commit(&shared, "wave-bad", ready_wave(&control, &priors, true))
        .await
        .unwrap_err();
    assert_eq!(error.0, StatusCode::BAD_REQUEST);
    let after = store.load_recovery().unwrap();
    assert_eq!(after.mutations.len(), before.mutations.len());
    assert_eq!(after.executions.len(), before.executions.len());
    assert_eq!(shared.app.lock().await.next_order_id, order_id);
    let epoch = control.epoch.load(Ordering::Acquire);
    commit(
        &shared,
        "wave-bad",
        work(
            &control,
            &priors[0],
            epoch,
            vec![OrderAction::PlaceLimit {
                side: Side::Buy,
                price_tick: 90,
                qty: 1,
            }],
        ),
    )
    .await
    .unwrap();
    let app = shared.app.lock().await;
    assert_eq!(count(&app, "wave-bad", "fast-a"), 1);
    assert_eq!(count(&app, "wave-bad", "fast-b"), 0);
    assert_eq!(app.next_order_id, order_id + 1);
    drop(app);
    shutdown(&shared).await;
}

#[tokio::test]
async fn realtime_ack_preserves_roster_and_untouched_state_and_recovers_empty_actions() {
    use journal::JournalStore;
    let room = "sparse-ack";
    let (shared, _, mut store) = setup(room).await;
    let (control, priors) = prepare_wave(&shared, room).await;
    let (roster, untouched, history_len, revision) = {
        let mut app = shared.app.lock().await;
        let mut scheduler = app.schedulers[room].clone();
        if let PersistedAgentKindState::Plugin { data, .. } = &mut scheduler.agents[1].kind_state {
            *data = serde_json::json!({"untouched": "x".repeat(10000)});
        }
        install_scheduler(&mut app, scheduler).await.unwrap();
        let scheduler = &app.schedulers[room];
        let untouched = match &scheduler.agents[1].kind_state {
            PersistedAgentKindState::Plugin { data, .. } => {
                data["untouched"].as_str().unwrap().as_ptr() as usize
            }
            _ => unreachable!(),
        };
        (
            scheduler.agents.as_ptr() as usize,
            untouched,
            app.rooms.execution_history_len(room).unwrap(),
            scheduler.revision,
        )
    };
    let epoch = control.epoch.load(Ordering::Acquire);
    commit_realtime_work(
        shared.clone(),
        room.into(),
        work(&control, &priors[0], epoch, vec![]),
    )
    .await
    .unwrap();
    // Empty order actions still durably advance this bot's state.
    {
        let app = shared.app.lock().await;
        assert_eq!(count(&app, room, "fast-a"), 1);
        assert_eq!(app.schedulers[room].revision, revision + 1);
        assert_eq!(app.rooms.execution_history_len(room).unwrap(), history_len);
        assert_eq!(app.schedulers[room].agents.as_ptr() as usize, roster);
        match &app.schedulers[room].agents[1].kind_state {
            PersistedAgentKindState::Plugin { data, .. } => assert_eq!(
                data["untouched"].as_str().unwrap().as_ptr() as usize,
                untouched
            ),
            _ => unreachable!(),
        }
    }
    // A clock-only commit preserves the same allocations as well.
    commit_realtime_work(
        shared.clone(),
        room.into(),
        realtime::Work::Clock(control.clone()),
    )
    .await
    .unwrap();
    let recovery = store.load_recovery().unwrap();
    let recovered = scheduler_states_from_recovery(&recovery).unwrap();
    {
        let app = shared.app.lock().await;
        assert_eq!(recovered[room], app.schedulers[room]);
        assert_eq!(app.schedulers[room].agents.as_ptr() as usize, roster);
    }
    assert!(matches!(&recovery.mutations.last().unwrap().mutation,
        RoomMutation::SchedulerDelta { delta, clock_steps: 1, .. } if delta.changes.is_empty()));
    shutdown(&shared).await;
}

#[tokio::test]
async fn realtime_ready_wave_skips_stale_members_and_fences_pause_epoch() {
    let (shared, _, _) = setup("wave-fence").await;
    let (control, priors) = prepare_wave(&shared, "wave-fence").await;
    let epoch = control.epoch.load(Ordering::Acquire);
    commit(
        &shared,
        "wave-fence",
        work(
            &control,
            &priors[0],
            epoch,
            vec![OrderAction::PlaceLimit {
                side: Side::Buy,
                price_tick: 90,
                qty: 1,
            }],
        ),
    )
    .await
    .unwrap();
    commit(&shared, "wave-fence", ready_wave(&control, &priors, false))
        .await
        .unwrap();
    let app = shared.app.lock().await;
    assert_eq!(count(&app, "wave-fence", "fast-a"), 1);
    assert_eq!(count(&app, "wave-fence", "fast-b"), 1);
    let current = app.schedulers["wave-fence"].agents.clone();
    let order_id = app.next_order_id;
    drop(app);
    let stale = ready_wave(&control, &current, false);
    let _ = pause_room(
        State(shared.clone()),
        HeaderMap::new(),
        Path("wave-fence".into()),
    )
    .await
    .unwrap();
    let _ = resume_room(
        State(shared.clone()),
        HeaderMap::new(),
        Path("wave-fence".into()),
    )
    .await
    .unwrap();
    commit(&shared, "wave-fence", stale).await.unwrap();
    assert_eq!(shared.app.lock().await.next_order_id, order_id);
    shutdown(&shared).await;
}

#[tokio::test]
async fn realtime_pause_resume_and_replacement_fence_old_decisions() {
    let (shared, _, _) = setup("fence").await;
    let (control, prior) = prepare(&shared, "fence").await;
    let epoch = control.epoch.load(Ordering::Acquire);
    let _ = resume_room(
        State(shared.clone()),
        HeaderMap::new(),
        Path("fence".into()),
    )
    .await
    .unwrap();
    let before = shared
        .app
        .lock()
        .await
        .rooms
        .execution_history("fence")
        .unwrap()
        .len();
    let action = vec![OrderAction::PlaceMarket {
        side: Side::Buy,
        qty: 1,
    }];
    commit(
        &shared,
        "fence",
        work(&control, &prior, epoch, action.clone()),
    )
    .await
    .unwrap();
    assert_eq!(
        shared
            .app
            .lock()
            .await
            .rooms
            .execution_history("fence")
            .unwrap()
            .len(),
        before
    );
    let current_epoch = control.epoch.load(Ordering::Acquire);
    start(&shared, "fence", &[], 60_000).await;
    commit(
        &shared,
        "fence",
        work(&control, &prior, current_epoch, action),
    )
    .await
    .unwrap();
    assert_eq!(
        shared
            .app
            .lock()
            .await
            .rooms
            .execution_history("fence")
            .unwrap()
            .len(),
        before
    );
    shutdown(&shared).await;
}

#[tokio::test]
async fn realtime_decision_uses_current_book_and_state_recovers_without_duplicate_orders() {
    let (shared, _, mut store) = setup("receipt").await;
    let (control, prior) = prepare(&shared, "receipt").await;
    // Another bot owns the same account and sorts first. Receipts must identify
    // the actual submitting bot, not infer identity from account ownership.
    {
        let mut app = shared.app.lock().await;
        let mut scheduler = app.schedulers["receipt"].clone();
        scheduler
            .agents
            .insert(0, PersistedAgent::from_template(template("receipt", "aaa")));
        install_scheduler(&mut app, scheduler).await.unwrap();
    }
    let _ = resume_room(
        State(shared.clone()),
        HeaderMap::new(),
        Path("receipt".into()),
    )
    .await
    .unwrap();
    let epoch = control.epoch.load(Ordering::Acquire);
    // Replace the ask after the bot's observation was taken.
    let _ = submit_order(
        State(shared.clone()),
        HeaderMap::new(),
        Path("receipt".into()),
        Json(SubmitOrderRequest {
            participant_id: "human".into(),
            instrument_id: None,
            account_id: 10,
            action: OrderAction::Amend {
                order_id: 2,
                price_tick: Some(105),
                qty: None,
            },
        }),
    )
    .await
    .unwrap();
    let action = vec![OrderAction::PlaceMarket {
        side: Side::Buy,
        qty: 1,
    }];
    commit(
        &shared,
        "receipt",
        work(&control, &prior, epoch, action.clone()),
    )
    .await
    .unwrap();
    let expected = shared
        .app
        .lock()
        .await
        .rooms
        .book_snapshot("receipt")
        .unwrap();
    let len = shared
        .app
        .lock()
        .await
        .rooms
        .execution_history("receipt")
        .unwrap()
        .len();
    commit(&shared, "receipt", work(&control, &prior, epoch, action))
        .await
        .unwrap();
    assert_eq!(
        shared
            .app
            .lock()
            .await
            .rooms
            .execution_history("receipt")
            .unwrap()
            .len(),
        len
    );
    shutdown(&shared).await;
    let recovery = store.load_recovery().unwrap();
    assert_eq!(
        recovery
            .executions
            .last()
            .unwrap()
            .participant_id
            .as_deref(),
        Some("fast")
    );
    assert_eq!(recovery.executions.last().unwrap().account_id, Some(20));
    let recovered = recover_rooms(&recovery).unwrap();
    assert_eq!(recovered.book_snapshot("receipt").unwrap(), expected);
    let replayed = recover_rooms_for_full_replay(&recovery).unwrap();
    assert_eq!(replayed.book_snapshot("receipt").unwrap(), expected);
    assert_eq!(replayed.execution_history("receipt").unwrap().len(), len);
    assert_eq!(
        scheduler_states_from_recovery(&recovery).unwrap()["receipt"].agents[1].kind_state,
        shared.app.lock().await.schedulers["receipt"].agents[1].kind_state
    );
    let observed = recovered
        .participant_observation("receipt", "V-BTC-SPOT", 20)
        .unwrap();
    assert_eq!(observed.public_trades.last().unwrap().price_tick, 105);
}

#[tokio::test]
async fn realtime_training_deadline_advances_only_on_clock_and_late_orders_are_rejected() {
    let (shared, _, mut store) = setup("training-live").await;
    let (control, prior) = prepare(&shared, "training-live").await;
    let spec = exchange_core::TrainingSpec::low_slippage_buy(
        "training-live",
        scenario("training-live"),
        vec![],
        20,
        5,
        2,
        101,
    )
    .unwrap();
    let mut run = exchange_core::TrainingRun::new(spec);
    run.start().unwrap();
    shared
        .app
        .lock()
        .await
        .training_runs
        .insert("training-live".into(), run);
    let _ = resume_room(
        State(shared.clone()),
        HeaderMap::new(),
        Path("training-live".into()),
    )
    .await
    .unwrap();
    let epoch = control.epoch.load(Ordering::Acquire);
    commit(
        &shared,
        "training-live",
        work(&control, &prior, epoch, vec![]),
    )
    .await
    .unwrap();
    assert_eq!(
        shared.app.lock().await.training_runs["training-live"].steps_elapsed,
        0
    );
    commit(
        &shared,
        "training-live",
        realtime::Work::Clock(control.clone()),
    )
    .await
    .unwrap();
    commit(
        &shared,
        "training-live",
        realtime::Work::Clock(control.clone()),
    )
    .await
    .unwrap();
    assert!(shared.app.lock().await.training_runs["training-live"].is_finished());
    let prior = shared.app.lock().await.schedulers["training-live"].agents[0].clone();
    let error = commit(
        &shared,
        "training-live",
        work(
            &control,
            &prior,
            epoch,
            vec![OrderAction::PlaceMarket {
                side: Side::Buy,
                qty: 1,
            }],
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        shared.app.lock().await.training_runs["training-live"].filled_qty,
        0
    );
    shutdown(&shared).await;
    let recovery = store.load_recovery().unwrap();
    let restored = training_runs_from_recovery(&recovery);
    assert_eq!(
        restored["training-live"],
        shared.app.lock().await.training_runs["training-live"]
    );
    assert!(recovery.mutations.iter().any(|m| matches!(&m.mutation,
        RoomMutation::SchedulerProgress { clock_steps: 1, training: Some(run), .. }
        | RoomMutation::SchedulerDelta { clock_steps: 1, training: Some(run), .. }
        if run.steps_elapsed == 2)));
}
