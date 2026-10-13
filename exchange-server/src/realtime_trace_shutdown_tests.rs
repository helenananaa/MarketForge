//! Run EACH ignored case in a fresh test process with a fresh trace path and
//! MARKETFORGE_BOT_TRACE_SHUTDOWN=1. No test mutates process environment.
use super::*;
use crate::journal::{JournalRecovery, JournalStore};

fn trace_paths() -> (std::path::PathBuf, std::path::PathBuf) {
    assert!(bot_runtime_diagnostics::shutdown_requested());
    let path = std::path::PathBuf::from(
        std::env::var_os("MARKETFORGE_BOT_TRACE").expect("fresh trace path required"),
    );
    assert!(path.is_absolute() && !path.exists());
    let ack = std::path::PathBuf::from(format!("{}.complete.json", path.display()));
    assert!(!ack.exists());
    (path, ack)
}

fn rows(path: &std::path::Path) -> Vec<serde_json::Value> {
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.ends_with('\n'));
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn traced_work(control: &realtime::Control, prior: &PersistedAgent) -> (realtime::Work, u64) {
    // This test controls transaction admission using a fixed valid decision,
    // not a synthetic timing sample. Normal worker compute is exercised below.
    let id = bot_runtime_diagnostics::next_decision_id().unwrap();
    let mut pending = work(
        control,
        prior,
        control.epoch.load(Ordering::Acquire),
        vec![OrderAction::PlaceLimit {
            side: Side::Buy,
            price_tick: 90,
            qty: 1,
        }],
    );
    let realtime::Work::Bot { decision, .. } = &mut pending else {
        unreachable!()
    };
    decision.trace_id = Some(id);
    bot_runtime_diagnostics::trace_event(serde_json::json!({
        "stage":"decision", "decision_id":id, "room_id":prior.template.config().room_id,
        "participant_id":prior.template.participant_id(), "instrument_id":"V-BTC-SPOT",
        "observation_market_ms":0, "bar_close_ms":null, "action_count":1,
    }));
    (pending, id)
}

async fn manual_scheduler(shared: &SharedState, room: &str) -> (realtime::Control, PersistedAgent) {
    let mut app = shared.app.lock_room(room).await;
    assert_eq!(app.rooms.status(room).unwrap(), MarketStatus::Running);
    let scheduler = SchedulerState::new(
        room,
        vec![template(room, "fast")],
        exchange_core::SchedulerMode::Auto {
            interval_ms: 60_000,
        },
    );
    install_scheduler(&mut app, scheduler).await.unwrap();
    (
        realtime::Control::default(),
        app.schedulers[room].agents[0].clone(),
    )
}

struct TraceGateJournal {
    inner: journal::SharedInMemoryJournalStore,
    gate: Arc<Gate>,
    fail: bool,
}

impl JournalStore for TraceGateJournal {
    fn load_recovery(&mut self) -> Result<JournalRecovery, JournalError> {
        self.inner.load_recovery()
    }
    fn create_room(
        &mut self,
        owner: &str,
        scenario: &ScenarioConfig,
        bootstrap: &exchange_core::RoomBootstrap,
        accounts: &[AccountId],
        records: &[JournalExecution],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        self.inner
            .create_room(owner, scenario, bootstrap, accounts, records, snapshot)
    }
    fn append_executions(
        &mut self,
        records: &[JournalExecution],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        self.inner.append_executions(records, snapshot)
    }
    fn update_room_status(&mut self, room: &str, status: MarketStatus) -> Result<(), JournalError> {
        self.inner.update_room_status(room, status)
    }
    fn append_room_mutation(
        &mut self,
        mutation: &PendingJournalMutation,
        records: &[JournalExecution],
        transfers: &[JournalTransfer],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        if !records.is_empty()
            && matches!(&mutation.mutation,
            RoomMutation::SchedulerDelta { clock_steps: 0, delta, .. } if !delta.changes.is_empty())
        {
            self.gate.entered.fetch_add(1, Ordering::SeqCst);
            self.gate.started.notify_one();
            let mut released = self.gate.released.lock().unwrap();
            while !*released {
                released = self.gate.wake.wait(released).unwrap();
            }
            if self.fail {
                return Err(JournalError::Recovery(
                    "injected trace shutdown append failure".into(),
                ));
            }
        }
        self.inner
            .append_room_mutation(mutation, records, transfers, snapshot)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "isolated process: fresh MARKETFORGE_BOT_TRACE and MARKETFORGE_BOT_TRACE_SHUTDOWN=1"]
async fn trace_shutdown_before_durable_guard_discards_without_mutation() {
    let (path, ack) = trace_paths();
    let room = "trace-before-guard";
    let (shared, _, mut store) = setup(room).await;
    let (control, prior) = manual_scheduler(&shared, room).await;
    let (pending, id) = traced_work(&control, &prior);
    let before = serde_json::to_value(store.load_recovery().unwrap()).unwrap();
    let metrics = shared.lifecycle.metrics_snapshot();
    shared.lifecycle.begin_shutdown();
    commit_realtime_work(shared.clone(), room.into(), pending)
        .await
        .unwrap();
    shared.lifecycle.wait_for_durable_writes().await;
    let after = shared.lifecycle.metrics_snapshot();
    assert_eq!(after.durable_writes_started, metrics.durable_writes_started);
    assert_eq!(
        after.durable_writes_rejected,
        metrics.durable_writes_rejected + 1
    );
    assert_eq!(
        serde_json::to_value(store.load_recovery().unwrap()).unwrap(),
        before
    );
    assert_eq!(
        shared.app.lock_room(room).await.schedulers[room].agents[0],
        prior
    );
    bot_runtime_diagnostics::finish_trace().unwrap();
    assert!(ack.exists());
    let events: Vec<_> = rows(&path)
        .into_iter()
        .filter(|event| event["decision_id"] == id)
        .collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1]["stage"], "decision_discarded");
    assert_eq!(events[1]["reason"], "worker_shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "isolated process: fresh MARKETFORGE_BOT_TRACE and MARKETFORGE_BOT_TRACE_SHUTDOWN=1"]
async fn trace_shutdown_after_durable_guard_waits_for_real_append_and_receipt() {
    let (path, ack) = trace_paths();
    let room = "trace-after-guard";
    let (shared, _, mut store) = setup(room).await;
    let (control, prior) = manual_scheduler(&shared, room).await;
    let (pending, id) = traced_work(&control, &prior);
    let before = store.load_recovery().unwrap();
    let gate = Arc::new(Gate::default());
    let _release = ReleaseOnDrop(gate.clone());
    shared.app.lock_room(room).await.journal =
        JournalCoordinator::new(Box::new(TraceGateJournal {
            inner: store.clone(),
            gate: gate.clone(),
            fail: false,
        }));
    let transaction = tokio::spawn(commit_realtime_work(shared.clone(), room.into(), pending));
    tokio::time::timeout(Duration::from_secs(3), gate.started.notified())
        .await
        .unwrap();
    assert_eq!(shared.lifecycle.metrics_snapshot().active_durable_writes, 1);
    assert_eq!(
        store.load_recovery().unwrap().mutations.len(),
        before.mutations.len()
    );
    shared.lifecycle.begin_shutdown();
    let lifecycle = shared.lifecycle.clone();
    let (entered_send, entered_wait) = tokio::sync::oneshot::channel();
    let closeout = tokio::spawn(async move {
        entered_send.send(()).unwrap();
        // Mirror the server ordering: writer return, then durable drain, seal.
        transaction.await.unwrap().unwrap();
        lifecycle.wait_for_durable_writes().await;
        tokio::task::spawn_blocking(bot_runtime_diagnostics::finish_trace)
            .await
            .unwrap()
            .unwrap();
    });
    entered_wait.await.unwrap();
    assert!(!closeout.is_finished());
    assert!(!ack.exists());
    gate.release();
    tokio::time::timeout(Duration::from_secs(3), closeout)
        .await
        .unwrap()
        .unwrap();
    let recovery = store.load_recovery().unwrap();
    assert_eq!(recovery.mutations.len(), before.mutations.len() + 1);
    assert_eq!(recovery.executions.len(), before.executions.len() + 1);
    let app = shared.app.lock_room(room).await;
    assert_eq!(count(&app, room, "fast"), 1);
    assert_eq!(
        scheduler_states_from_recovery(&recovery).unwrap()[room],
        app.schedulers[room]
    );
    assert_eq!(
        recover_rooms(&recovery)
            .unwrap()
            .book_snapshot(room)
            .unwrap(),
        app.rooms.book_snapshot(room).unwrap()
    );
    let events: Vec<_> = rows(&path)
        .into_iter()
        .filter(|event| event["decision_id"] == id)
        .collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1]["stage"], "action_durable");
    assert_eq!(events[1]["outcome"], "accepted");
    assert!(ack.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "isolated process: fresh MARKETFORGE_BOT_TRACE and MARKETFORGE_BOT_TRACE_SHUTDOWN=1"]
async fn trace_shutdown_real_worker_append_failure_prevents_success_ack() {
    let (path, ack) = trace_paths();
    let room = "trace-failed-append";
    let (shared, _, mut store) = setup(room).await;
    let gate = Arc::new(Gate::default());
    let _release = ReleaseOnDrop(gate.clone());
    shared.app.lock_room(room).await.journal =
        JournalCoordinator::new(Box::new(TraceGateJournal {
            inner: store.clone(),
            gate: gate.clone(),
            fail: true,
        }));
    start(&shared, room, &["fast"], 60_000).await;
    tokio::time::timeout(Duration::from_secs(3), gate.started.notified())
        .await
        .unwrap();
    let before = serde_json::to_value(store.load_recovery().unwrap()).unwrap();
    assert_eq!(shared.lifecycle.metrics_snapshot().active_durable_writes, 1);
    shared.lifecycle.begin_shutdown();
    gate.release();
    tokio::time::timeout(Duration::from_secs(3), shutdown(&shared))
        .await
        .unwrap();
    shared.lifecycle.wait_for_durable_writes().await;
    assert_eq!(gate.entered.load(Ordering::SeqCst), 1);
    assert_eq!(
        serde_json::to_value(store.load_recovery().unwrap()).unwrap(),
        before
    );
    assert_eq!(count(&*shared.app.lock().await, room, "fast"), 0);
    assert_eq!(shared.lifecycle.metrics_snapshot().agent_errors_total, 1);
    let error = bot_runtime_diagnostics::finish_trace().unwrap_err();
    assert!(error.to_string().contains("no completion acknowledgement"));
    assert!(!ack.exists());
    let events = rows(&path);
    let decisions: Vec<_> = events
        .iter()
        .filter(|event| event["stage"] == "decision")
        .collect();
    assert_eq!(decisions.len(), 1);
    let id = &decisions[0]["decision_id"];
    assert!(
        events
            .iter()
            .any(|event| &event["decision_id"] == id && event["stage"] == "decision_failed")
    );
    assert!(
        events
            .iter()
            .any(|event| &event["decision_id"] == id
                && event["stage"] == "transaction_attempt_failed")
    );
    assert!(!events.iter().any(|event| matches!(
        event["stage"].as_str(),
        Some("action_durable" | "decision_discarded" | "trace_complete")
    )));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "isolated process: fresh MARKETFORGE_BOT_TRACE and MARKETFORGE_BOT_TRACE_SHUTDOWN=1"]
async fn trace_shutdown_closed_market_waits_for_started_native_job() {
    let (path, ack) = trace_paths();
    let room = "trace-close-native";
    let (shared, gate, _) = setup(room).await;
    gate.native.store(true, Ordering::Release);
    let _release = ReleaseOnDrop(gate.clone());
    start(&shared, room, &["slow"], 10).await;
    tokio::time::timeout(Duration::from_secs(3), gate.started.notified())
        .await
        .unwrap();
    let control = {
        let mut app = shared.app.lock_room(room).await;
        // Publish the closed market without stop/epoch changes so this targets
        // the MarketStatus::Closed loop exit, not the ordinary stop branch.
        app.rooms.close_room(room).unwrap();
        app.agent_workers[room].control.clone()
    };
    assert!(!control.stop.load(Ordering::Acquire));
    assert_eq!(
        shared.app.read_room(room).await.rooms.status(room).unwrap(),
        MarketStatus::Closed
    );
    let mut closeout = tokio::task::spawn_blocking(bot_runtime_diagnostics::finish_trace);
    assert!(
        tokio::time::timeout(Duration::from_millis(250), &mut closeout)
            .await
            .is_err(),
        "a closed market must not seal while its started native job is gated"
    );
    assert!(!ack.exists());
    gate.release();
    tokio::time::timeout(Duration::from_secs(3), closeout)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(control.stop.load(Ordering::Acquire));
    shutdown(&shared).await;
    let events = rows(&path);
    assert_eq!(
        events
            .iter()
            .filter(|event| event["stage"] == "decision")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["stage"] == "decision_discarded")
            .count(),
        1
    );
    assert!(
        !events
            .iter()
            .any(|event| event["stage"] == "action_durable")
    );
    assert_eq!(events.last().unwrap()["stage"], "trace_complete");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "isolated process: fresh MARKETFORGE_BOT_TRACE and MARKETFORGE_BOT_TRACE_SHUTDOWN=1"]
async fn trace_shutdown_caught_native_panic_after_epoch_stop_has_no_ack() {
    let (path, ack) = trace_paths();
    let room = "trace-stop-panic";
    let (shared, gate, _) = setup(room).await;
    gate.native.store(true, Ordering::Release);
    let _release = ReleaseOnDrop(gate.clone());
    start(&shared, room, &["slow"], 60_000).await;
    tokio::time::timeout(Duration::from_secs(3), gate.started.notified())
        .await
        .unwrap();
    // The bot panics before next_decision_id, so receipt completeness alone
    // cannot reveal this failure. Both stale-ready and drain paths must retain it.
    gate.panic_after_gate.store(true, Ordering::Release);
    {
        let app = shared.app.lock().await;
        app.agent_workers[room]
            .control
            .epoch
            .fetch_add(1, Ordering::AcqRel);
        app.agent_workers[room].request_stop();
    }
    shared.lifecycle.begin_shutdown();
    gate.release();
    tokio::time::timeout(Duration::from_secs(3), shutdown(&shared))
        .await
        .unwrap();
    shared.lifecycle.wait_for_durable_writes().await;
    assert!(bot_runtime_diagnostics::finish_trace().is_err());
    assert!(!ack.exists());
    let events = rows(&path);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event["stage"].as_str(), Some("decision" | "trace_complete")))
    );
}
