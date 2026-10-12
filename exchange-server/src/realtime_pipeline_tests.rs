#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn realtime_pipeline_refills_during_durable_write_without_speculative_publication() {
    use journal::{JournalRecovery, JournalStore};
    struct GatedJournal {
        inner: journal::SharedInMemoryJournalStore,
        gate: Arc<Gate>,
    }
    impl JournalStore for GatedJournal {
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
        fn update_room_status(
            &mut self,
            room: &str,
            status: MarketStatus,
        ) -> Result<(), JournalError> {
            self.inner.update_room_status(room, status)
        }
        fn append_room_mutation(
            &mut self,
            mutation: &PendingJournalMutation,
            records: &[JournalExecution],
            transfers: &[JournalTransfer],
            snapshot: Option<&JournalSnapshot>,
        ) -> Result<(), JournalError> {
            if matches!(&mutation.mutation, RoomMutation::SchedulerDelta { clock_steps: 0, delta, .. } if !delta.changes.is_empty())
            {
                self.gate.started.notify_one();
                let mut released = self.gate.released.lock().unwrap();
                while !*released {
                    released = self.gate.wake.wait(released).unwrap();
                }
            }
            self.inner
                .append_room_mutation(mutation, records, transfers, snapshot)
        }
    }
    let room = "pipeline-durable-gate";
    let (shared, bots, mut store) = setup(room).await;
    let gate = Arc::new(Gate::default());
    let _release = ReleaseOnDrop(gate.clone());
    shared.app.lock_room(room).await.journal = JournalCoordinator::new(Box::new(GatedJournal {
        inner: store.clone(),
        gate: gate.clone(),
    }));
    let ids: Vec<_> = (0..48).map(|i| format!("pipeline-{i}")).collect();
    start(
        &shared,
        room,
        &ids.iter().map(String::as_str).collect::<Vec<_>>(),
        10_000,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(3), gate.started.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if bots.decided.lock().unwrap().len() == 48 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("independent calculations must refill past 16 permits during the gated commit");
    assert!(
        bots.decided
            .lock()
            .unwrap()
            .values()
            .all(|calls| *calls == 1)
    );
    let pending = tokio::time::timeout(Duration::from_secs(1), shared.app.read_dispatch(room))
        .await
        .unwrap()
        .unwrap();
    let pending_state = pending.dispatch.unwrap();
    assert!(
        pending_state
            .agents
            .iter()
            .all(|agent| matches!(&agent.kind_state,
        PersistedAgentKindState::Plugin { data, .. } if data.as_u64().unwrap_or(0) == 0))
    );
    gate.release();
    eventually(&shared, |app| {
        ids.iter().all(|id| count(app, room, id) == 1)
    })
    .await;
    shutdown(&shared).await;
    let restored = scheduler_states_from_recovery(&store.load_recovery().unwrap()).unwrap();
    assert_eq!(
        restored[room],
        shared.app.lock_room(room).await.schedulers[room]
    );
    assert!(
        pending_state
            .agents
            .iter()
            .all(|agent| matches!(&agent.kind_state,
        PersistedAgentKindState::Plugin { data, .. } if data.as_u64().unwrap_or(0) == 0)),
        "published predecessor stays immutable"
    );
}
