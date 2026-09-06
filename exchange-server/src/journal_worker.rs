use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use tokio::sync::{mpsc, oneshot};

use exchange_core::{RoomBootstrap, ScenarioConfig, VenueTransfer, model::AccountId};

use crate::journal::{
    AccountLedgerProjection, ExecutionPage, JournalError, JournalExecution, JournalSnapshot,
    JournalStore, JournalTransfer, MarketTickProjection, OrderProjection, PendingJournalMutation,
    PositionSnapshotProjection, RoomLeaseClaim, RoomRoutingRecord, RoomWriterLease,
    TradeProjection,
};

pub const DEFAULT_JOURNAL_QUEUE_CAPACITY: usize = 64;

type JournalJob = Box<dyn FnOnce(&mut dyn JournalStore) + Send + 'static>;

/// Serializes all synchronous journal work onto one bounded, dedicated worker.
///
/// The bounded Tokio channel provides asynchronous backpressure to request
/// handlers. The journal itself never runs on a Tokio runtime thread, and a
/// non-`Sync` backend (notably `postgres::Client`) remains owned by exactly one
/// OS thread for its entire lifetime.
#[derive(Clone)]
pub(crate) struct JournalCoordinator {
    writer: JournalWorker,
    readers: Arc<Vec<JournalWorker>>,
    next_reader: Arc<AtomicUsize>,
}

#[derive(Clone)]
struct JournalWorker {
    sender: mpsc::Sender<JournalJob>,
    metrics: Arc<JournalCoordinatorMetrics>,
}

#[derive(Default)]
struct JournalCoordinatorMetrics {
    operations_started: AtomicU64,
    operations_completed: AtomicU64,
    operation_errors: AtomicU64,
    operations_in_flight: AtomicUsize,
    worker_active: AtomicUsize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct JournalCoordinatorMetricsSnapshot {
    pub(crate) write_workers: usize,
    pub(crate) read_workers: usize,
    pub(crate) queue_capacity: usize,
    pub(crate) queue_depth: usize,
    pub(crate) channel_open: bool,
    pub(crate) operations_started: u64,
    pub(crate) operations_completed: u64,
    pub(crate) operation_errors: u64,
    pub(crate) operations_in_flight: usize,
    pub(crate) worker_active: usize,
}

impl JournalCoordinator {
    pub(crate) fn new(store: Box<dyn JournalStore>) -> Self {
        Self::with_capacity_and_read_stores(store, Vec::new(), DEFAULT_JOURNAL_QUEUE_CAPACITY)
    }

    pub(crate) fn with_read_stores(
        writer: Box<dyn JournalStore>,
        readers: Vec<Box<dyn JournalStore>>,
    ) -> Self {
        Self::with_capacity_and_read_stores(writer, readers, DEFAULT_JOURNAL_QUEUE_CAPACITY)
    }

    #[cfg(test)]
    fn with_capacity(store: Box<dyn JournalStore>, capacity: usize) -> Self {
        Self::with_capacity_and_read_stores(store, Vec::new(), capacity)
    }

    fn with_capacity_and_read_stores(
        writer: Box<dyn JournalStore>,
        readers: Vec<Box<dyn JournalStore>>,
        capacity: usize,
    ) -> Self {
        assert!(capacity > 0, "journal queue capacity must be positive");
        let writer =
            JournalWorker::spawn(writer, capacity, "marketforge-journal-write".to_string());
        let readers = readers
            .into_iter()
            .enumerate()
            .map(|(index, store)| {
                JournalWorker::spawn(store, capacity, format!("marketforge-journal-read-{index}"))
            })
            .collect();
        Self {
            writer,
            readers: Arc::new(readers),
            next_reader: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn read_worker(&self) -> &JournalWorker {
        if self.readers.is_empty() {
            return &self.writer;
        }
        let index = self.next_reader.fetch_add(1, Ordering::Relaxed) % self.readers.len();
        &self.readers[index]
    }

    pub(crate) fn metrics_snapshot(&self) -> JournalCoordinatorMetricsSnapshot {
        let mut snapshot = self.writer.metrics_snapshot();
        snapshot.write_workers = 1;
        snapshot.read_workers = self.readers.len();
        for reader in self.readers.iter() {
            snapshot.merge(reader.metrics_snapshot());
        }
        snapshot
    }

    pub(crate) async fn execute<T, F>(&self, operation: F) -> Result<T, JournalError>
    where
        T: Send + 'static,
        F: FnOnce(&mut dyn JournalStore) -> Result<T, JournalError> + Send + 'static,
    {
        self.writer.execute(operation).await
    }

    async fn execute_read<T, F>(&self, operation: F) -> Result<T, JournalError>
    where
        T: Send + 'static,
        F: FnOnce(&mut dyn JournalStore) -> Result<T, JournalError> + Send + 'static,
    {
        self.read_worker().execute(operation).await
    }
}

impl JournalWorker {
    fn spawn(mut store: Box<dyn JournalStore>, capacity: usize, thread_name: String) -> Self {
        let (sender, mut receiver) = mpsc::channel::<JournalJob>(capacity);
        let metrics = Arc::new(JournalCoordinatorMetrics::default());
        thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                while let Some(job) = receiver.blocking_recv() {
                    job(store.as_mut());
                }
            })
            .expect("failed to spawn journal worker");
        Self { sender, metrics }
    }

    fn metrics_snapshot(&self) -> JournalCoordinatorMetricsSnapshot {
        let queue_capacity = self.sender.max_capacity();
        JournalCoordinatorMetricsSnapshot {
            write_workers: 0,
            read_workers: 0,
            queue_capacity,
            queue_depth: queue_capacity.saturating_sub(self.sender.capacity()),
            channel_open: !self.sender.is_closed(),
            operations_started: self.metrics.operations_started.load(Ordering::Relaxed),
            operations_completed: self.metrics.operations_completed.load(Ordering::Relaxed),
            operation_errors: self.metrics.operation_errors.load(Ordering::Relaxed),
            operations_in_flight: self.metrics.operations_in_flight.load(Ordering::Relaxed),
            worker_active: self.metrics.worker_active.load(Ordering::Relaxed),
        }
    }

    async fn execute<T, F>(&self, operation: F) -> Result<T, JournalError>
    where
        T: Send + 'static,
        F: FnOnce(&mut dyn JournalStore) -> Result<T, JournalError> + Send + 'static,
    {
        self.metrics
            .operations_started
            .fetch_add(1, Ordering::Relaxed);
        self.metrics
            .operations_in_flight
            .fetch_add(1, Ordering::Relaxed);
        let (result_sender, result_receiver) = oneshot::channel();
        let metrics = Arc::clone(&self.metrics);
        if self
            .sender
            .send(Box::new(move |store| {
                metrics.worker_active.fetch_add(1, Ordering::Relaxed);
                let result = operation(store);
                metrics.worker_active.fetch_sub(1, Ordering::Relaxed);
                metrics.operations_completed.fetch_add(1, Ordering::Relaxed);
                metrics.operations_in_flight.fetch_sub(1, Ordering::Relaxed);
                if result.is_err() {
                    metrics.operation_errors.fetch_add(1, Ordering::Relaxed);
                }
                let _ = result_sender.send(result);
            }))
            .await
            .is_err()
        {
            self.metrics
                .operations_completed
                .fetch_add(1, Ordering::Relaxed);
            self.metrics
                .operation_errors
                .fetch_add(1, Ordering::Relaxed);
            self.metrics
                .operations_in_flight
                .fetch_sub(1, Ordering::Relaxed);
            return Err(journal_worker_stopped());
        }
        result_receiver
            .await
            .map_err(|_| journal_worker_stopped())?
    }
}

impl JournalCoordinatorMetricsSnapshot {
    fn merge(&mut self, other: Self) {
        self.queue_capacity = self.queue_capacity.saturating_add(other.queue_capacity);
        self.queue_depth = self.queue_depth.saturating_add(other.queue_depth);
        self.channel_open &= other.channel_open;
        self.operations_started = self
            .operations_started
            .saturating_add(other.operations_started);
        self.operations_completed = self
            .operations_completed
            .saturating_add(other.operations_completed);
        self.operation_errors = self.operation_errors.saturating_add(other.operation_errors);
        self.operations_in_flight = self
            .operations_in_flight
            .saturating_add(other.operations_in_flight);
        self.worker_active = self.worker_active.saturating_add(other.worker_active);
    }
}

impl JournalCoordinator {
    pub(crate) async fn create_room(
        &self,
        owner_user_id: &str,
        scenario: &ScenarioConfig,
        bootstrap: &RoomBootstrap,
        account_ids: &[AccountId],
        seed_records: &[JournalExecution],
        initial_snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        let owner_user_id = owner_user_id.to_string();
        let scenario = scenario.clone();
        let bootstrap = bootstrap.clone();
        let account_ids = account_ids.to_vec();
        let seed_records = seed_records.to_vec();
        let initial_snapshot = initial_snapshot.cloned();
        self.execute(move |store| {
            store.create_room(
                &owner_user_id,
                &scenario,
                &bootstrap,
                &account_ids,
                &seed_records,
                initial_snapshot.as_ref(),
            )
        })
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn create_room_with_writer_lease(
        &self,
        owner_user_id: &str,
        scenario: &ScenarioConfig,
        bootstrap: &RoomBootstrap,
        account_ids: &[AccountId],
        seed_records: &[JournalExecution],
        initial_snapshot: Option<&JournalSnapshot>,
        writer_owner_id: &str,
        writer_owner_url: Option<&str>,
        lease_duration: Duration,
    ) -> Result<RoomWriterLease, JournalError> {
        let owner_user_id = owner_user_id.to_string();
        let scenario = scenario.clone();
        let bootstrap = bootstrap.clone();
        let account_ids = account_ids.to_vec();
        let seed_records = seed_records.to_vec();
        let initial_snapshot = initial_snapshot.cloned();
        let writer_owner_id = writer_owner_id.to_string();
        let writer_owner_url = writer_owner_url.map(str::to_string);
        self.execute(move |store| {
            store.create_room_with_writer_lease(
                &owner_user_id,
                &scenario,
                &bootstrap,
                &account_ids,
                &seed_records,
                initial_snapshot.as_ref(),
                &writer_owner_id,
                writer_owner_url.as_deref(),
                lease_duration,
            )
        })
        .await
    }

    pub(crate) async fn renew_room_writer_lease(
        &self,
        claim: &RoomLeaseClaim,
        duration: Duration,
    ) -> Result<Option<RoomWriterLease>, JournalError> {
        let claim = claim.clone();
        self.execute(move |store| store.renew_room_writer_lease(&claim, duration))
            .await
    }

    pub(crate) async fn release_room_writer_lease(
        &self,
        claim: &RoomLeaseClaim,
    ) -> Result<bool, JournalError> {
        let claim = claim.clone();
        self.execute(move |store| store.release_room_writer_lease(&claim))
            .await
    }

    pub(crate) async fn health_check(&self) -> Result<(), JournalError> {
        self.writer.execute(|store| store.health_check()).await?;
        for reader in self.readers.iter() {
            reader.execute(|store| store.health_check()).await?;
        }
        Ok(())
    }

    pub(crate) async fn load_room_recovery(
        &self,
        room_id: &str,
    ) -> Result<crate::journal::JournalRecovery, JournalError> {
        let room_id = room_id.to_string();
        self.execute(move |store| store.load_room_recovery(&room_id))
            .await
    }

    pub(crate) async fn acquire_room_writer_lease(
        &self,
        room_id: &str,
        owner_id: &str,
        owner_url: Option<&str>,
        duration: Duration,
    ) -> Result<Option<RoomWriterLease>, JournalError> {
        let room_id = room_id.to_string();
        let owner_id = owner_id.to_string();
        let owner_url = owner_url.map(str::to_string);
        self.execute(move |store| {
            store.acquire_room_writer_lease(&room_id, &owner_id, owner_url.as_deref(), duration)
        })
        .await
    }

    pub(crate) async fn current_room_writer_lease(
        &self,
        room_id: &str,
    ) -> Result<Option<RoomWriterLease>, JournalError> {
        let room_id = room_id.to_string();
        self.execute_read(move |store| store.current_room_writer_lease(&room_id))
            .await
    }

    pub(crate) async fn query_room_routes(
        &self,
        user_id: &str,
        after_room_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RoomRoutingRecord>, JournalError> {
        let user_id = user_id.to_string();
        let after_room_id = after_room_id.map(str::to_string);
        self.execute_read(move |store| {
            store.query_room_routes(&user_id, after_room_id.as_deref(), limit)
        })
        .await
    }

    pub(crate) async fn find_idempotent_execution(
        &self,
        user_id: &str,
        room_id: &str,
        idempotency_key: &str,
    ) -> Result<Option<JournalExecution>, JournalError> {
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        let idempotency_key = idempotency_key.to_string();
        self.execute(move |store| {
            store.find_idempotent_execution(&user_id, &room_id, &idempotency_key)
        })
        .await
    }

    pub(crate) async fn query_executions(
        &self,
        room_id: &str,
        after_command_seq: Option<u64>,
        from_start: bool,
        limit: usize,
    ) -> Result<ExecutionPage, JournalError> {
        let room_id = room_id.to_string();
        self.execute_read(move |store| {
            store.query_executions(&room_id, after_command_seq, from_start, limit)
        })
        .await
    }

    pub(crate) async fn append_executions(
        &self,
        records: &[JournalExecution],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        let records = records.to_vec();
        let snapshot = snapshot.cloned();
        self.execute(move |store| store.append_executions(&records, snapshot.as_ref()))
            .await
    }

    pub(crate) async fn append_executions_fenced(
        &self,
        claim: &RoomLeaseClaim,
        records: &[JournalExecution],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        let claim = claim.clone();
        let records = records.to_vec();
        let snapshot = snapshot.cloned();
        self.execute(move |store| {
            store.append_executions_fenced(&claim, &records, snapshot.as_ref())
        })
        .await
    }

    pub(crate) async fn append_room_mutation(
        &self,
        mutation: &PendingJournalMutation,
        execution_records: &[JournalExecution],
        transfer_records: &[JournalTransfer],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        let mutation = mutation.clone();
        let execution_records = execution_records.to_vec();
        let transfer_records = transfer_records.to_vec();
        let snapshot = snapshot.cloned();
        self.execute(move |store| {
            store.append_room_mutation(
                &mutation,
                &execution_records,
                &transfer_records,
                snapshot.as_ref(),
            )
        })
        .await
    }

    pub(crate) async fn append_room_mutation_fenced(
        &self,
        claim: &RoomLeaseClaim,
        mutation: &PendingJournalMutation,
        execution_records: &[JournalExecution],
        transfer_records: &[JournalTransfer],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        let claim = claim.clone();
        let mutation = mutation.clone();
        let execution_records = execution_records.to_vec();
        let transfer_records = transfer_records.to_vec();
        let snapshot = snapshot.cloned();
        self.execute(move |store| {
            store.append_room_mutation_fenced(
                &claim,
                &mutation,
                &execution_records,
                &transfer_records,
                snapshot.as_ref(),
            )
        })
        .await
    }

    pub(crate) async fn upsert_room_member(
        &self,
        room_id: &str,
        user_id: &str,
        role: &str,
    ) -> Result<(), JournalError> {
        let room_id = room_id.to_string();
        let user_id = user_id.to_string();
        let role = role.to_string();
        self.execute(move |store| store.upsert_room_member(&room_id, &user_id, &role))
            .await
    }

    pub(crate) async fn remove_room_member(
        &self,
        room_id: &str,
        user_id: &str,
    ) -> Result<(), JournalError> {
        let room_id = room_id.to_string();
        let user_id = user_id.to_string();
        self.execute(move |store| store.remove_room_member(&room_id, &user_id))
            .await
    }

    pub(crate) async fn assign_account_owner(
        &self,
        room_id: &str,
        account_id: AccountId,
        user_id: &str,
    ) -> Result<(), JournalError> {
        let room_id = room_id.to_string();
        let user_id = user_id.to_string();
        self.execute(move |store| store.assign_account_owner(&room_id, account_id, &user_id))
            .await
    }

    pub(crate) async fn user_room_role(
        &self,
        user_id: &str,
        room_id: &str,
    ) -> Result<Option<String>, JournalError> {
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        self.execute_read(move |store| store.user_room_role(&user_id, &room_id))
            .await
    }

    pub(crate) async fn user_can_access_room(
        &self,
        user_id: &str,
        room_id: &str,
    ) -> Result<bool, JournalError> {
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        self.execute_read(move |store| store.user_can_access_room(&user_id, &room_id))
            .await
    }

    pub(crate) async fn user_can_administer_room(
        &self,
        user_id: &str,
        room_id: &str,
    ) -> Result<bool, JournalError> {
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        self.execute_read(move |store| store.user_can_administer_room(&user_id, &room_id))
            .await
    }

    pub(crate) async fn user_can_access_account(
        &self,
        user_id: &str,
        room_id: &str,
        account_id: AccountId,
    ) -> Result<bool, JournalError> {
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        self.execute_read(move |store| {
            store.user_can_access_account(&user_id, &room_id, account_id)
        })
        .await
    }

    pub(crate) async fn query_orders(
        &self,
        user_id: &str,
        room_id: &str,
        instrument_id: Option<&str>,
        account_id: Option<AccountId>,
        limit: usize,
    ) -> Result<Vec<OrderProjection>, JournalError> {
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        let instrument_id = instrument_id.map(str::to_string);
        self.execute_read(move |store| {
            store.query_orders(
                &user_id,
                &room_id,
                instrument_id.as_deref(),
                account_id,
                limit,
            )
        })
        .await
    }

    pub(crate) async fn query_trades(
        &self,
        user_id: &str,
        room_id: &str,
        instrument_id: Option<&str>,
        account_id: Option<AccountId>,
        limit: usize,
    ) -> Result<Vec<TradeProjection>, JournalError> {
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        let instrument_id = instrument_id.map(str::to_string);
        self.execute_read(move |store| {
            store.query_trades(
                &user_id,
                &room_id,
                instrument_id.as_deref(),
                account_id,
                limit,
            )
        })
        .await
    }

    pub(crate) async fn query_market_ticks(
        &self,
        user_id: &str,
        room_id: &str,
        instrument_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MarketTickProjection>, JournalError> {
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        let instrument_id = instrument_id.map(str::to_string);
        self.execute_read(move |store| {
            store.query_market_ticks(&user_id, &room_id, instrument_id.as_deref(), limit)
        })
        .await
    }

    pub(crate) async fn query_account_ledger(
        &self,
        user_id: &str,
        room_id: &str,
        instrument_id: Option<&str>,
        account_id: Option<AccountId>,
        limit: usize,
    ) -> Result<Vec<AccountLedgerProjection>, JournalError> {
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        let instrument_id = instrument_id.map(str::to_string);
        self.execute_read(move |store| {
            store.query_account_ledger(
                &user_id,
                &room_id,
                instrument_id.as_deref(),
                account_id,
                limit,
            )
        })
        .await
    }

    pub(crate) async fn query_position_snapshots(
        &self,
        user_id: &str,
        room_id: &str,
        instrument_id: Option<&str>,
        account_id: Option<AccountId>,
        limit: usize,
    ) -> Result<Vec<PositionSnapshotProjection>, JournalError> {
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        let instrument_id = instrument_id.map(str::to_string);
        self.execute_read(move |store| {
            store.query_position_snapshots(
                &user_id,
                &room_id,
                instrument_id.as_deref(),
                account_id,
                limit,
            )
        })
        .await
    }

    pub(crate) async fn query_transfers(
        &self,
        user_id: &str,
        room_id: &str,
        account_id: Option<AccountId>,
        limit: usize,
    ) -> Result<Vec<VenueTransfer>, JournalError> {
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        self.execute_read(move |store| store.query_transfers(&user_id, &room_id, account_id, limit))
            .await
    }
}

fn journal_worker_stopped() -> JournalError {
    JournalError::Recovery("journal worker stopped before returning a result".to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Condvar, Mutex},
        time::Duration,
    };

    use exchange_core::{MarketStatus, RoomBootstrap, ScenarioConfig, model::AccountId};

    use super::*;
    use crate::journal::{
        InMemoryJournalStore, JournalExecution, JournalRecovery, JournalSnapshot,
    };

    struct SlowRecoveryStore {
        inner: InMemoryJournalStore,
        visited: Arc<Mutex<bool>>,
        started: Option<oneshot::Sender<()>>,
    }

    struct BlockingReadStore {
        started: std::sync::mpsc::Sender<()>,
        release: Arc<(Mutex<bool>, Condvar)>,
    }

    impl JournalStore for BlockingReadStore {
        fn load_recovery(&mut self) -> Result<JournalRecovery, JournalError> {
            Ok(JournalRecovery::default())
        }

        fn create_room(
            &mut self,
            _owner_user_id: &str,
            _scenario: &ScenarioConfig,
            _bootstrap: &RoomBootstrap,
            _account_ids: &[AccountId],
            _seed_records: &[JournalExecution],
            _initial_snapshot: Option<&JournalSnapshot>,
        ) -> Result<(), JournalError> {
            Ok(())
        }

        fn append_executions(
            &mut self,
            _records: &[JournalExecution],
            _snapshot: Option<&JournalSnapshot>,
        ) -> Result<(), JournalError> {
            Ok(())
        }

        fn update_room_status(
            &mut self,
            _room_id: &str,
            _status: MarketStatus,
        ) -> Result<(), JournalError> {
            Ok(())
        }

        fn user_can_access_room(
            &mut self,
            _user_id: &str,
            _room_id: &str,
        ) -> Result<bool, JournalError> {
            let _ = self.started.send(());
            let (released, released_changed) = &*self.release;
            let mut released = released.lock().unwrap();
            while !*released {
                released = released_changed.wait(released).unwrap();
            }
            Ok(true)
        }
    }

    impl JournalStore for SlowRecoveryStore {
        fn load_recovery(&mut self) -> Result<JournalRecovery, JournalError> {
            if let Some(started) = self.started.take() {
                let _ = started.send(());
            }
            thread::sleep(Duration::from_millis(100));
            *self.visited.lock().unwrap() = true;
            self.inner.load_recovery()
        }

        fn create_room(
            &mut self,
            owner_user_id: &str,
            scenario: &ScenarioConfig,
            bootstrap: &RoomBootstrap,
            account_ids: &[AccountId],
            seed_records: &[JournalExecution],
            initial_snapshot: Option<&JournalSnapshot>,
        ) -> Result<(), JournalError> {
            self.inner.create_room(
                owner_user_id,
                scenario,
                bootstrap,
                account_ids,
                seed_records,
                initial_snapshot,
            )
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
            room_id: &str,
            status: MarketStatus,
        ) -> Result<(), JournalError> {
            self.inner.update_room_status(room_id, status)
        }
    }

    #[tokio::test]
    async fn slow_journal_work_does_not_block_the_runtime() {
        let visited = Arc::new(Mutex::new(false));
        let (started_sender, started_receiver) = oneshot::channel();
        let coordinator = JournalCoordinator::with_capacity(
            Box::new(SlowRecoveryStore {
                inner: InMemoryJournalStore::new(),
                visited: Arc::clone(&visited),
                started: Some(started_sender),
            }),
            1,
        );
        let metrics_coordinator = coordinator.clone();
        let slow_call = tokio::spawn(async move {
            coordinator
                .execute(|store| store.load_recovery())
                .await
                .unwrap();
        });

        started_receiver.await.unwrap();
        let active = metrics_coordinator.metrics_snapshot();
        assert_eq!(active.queue_capacity, 1);
        assert_eq!(active.operations_started, 1);
        assert_eq!(active.operations_completed, 0);
        assert_eq!(active.operations_in_flight, 1);
        assert_eq!(active.worker_active, 1);
        assert_eq!(active.write_workers, 1);
        assert_eq!(active.read_workers, 0);
        tokio::time::timeout(
            Duration::from_millis(30),
            tokio::time::sleep(Duration::from_millis(1)),
        )
        .await
        .expect("Tokio runtime was blocked by the synchronous journal");
        slow_call.await.unwrap();
        assert!(*visited.lock().unwrap());
        let completed = metrics_coordinator.metrics_snapshot();
        assert_eq!(completed.operations_started, 1);
        assert_eq!(completed.operations_completed, 1);
        assert_eq!(completed.operation_errors, 0);
        assert_eq!(completed.operations_in_flight, 0);
        assert_eq!(completed.worker_active, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn read_workers_run_in_parallel_without_blocking_the_writer() {
        let (started_sender, started_receiver) = std::sync::mpsc::channel();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let readers = (0..2)
            .map(|_| {
                Box::new(BlockingReadStore {
                    started: started_sender.clone(),
                    release: Arc::clone(&release),
                }) as Box<dyn JournalStore>
            })
            .collect();
        let coordinator = JournalCoordinator::with_capacity_and_read_stores(
            Box::new(InMemoryJournalStore::new()),
            readers,
            1,
        );

        let first_read = {
            let coordinator = coordinator.clone();
            tokio::spawn(
                async move { coordinator.user_can_access_room("reader-1", "room-1").await },
            )
        };
        let second_read = {
            let coordinator = coordinator.clone();
            tokio::spawn(
                async move { coordinator.user_can_access_room("reader-2", "room-1").await },
            )
        };

        let both_started = tokio::task::spawn_blocking(move || {
            (0..2).all(|_| {
                started_receiver
                    .recv_timeout(Duration::from_secs(1))
                    .is_ok()
            })
        })
        .await
        .unwrap();
        let writer_result = if both_started {
            Some(
                tokio::time::timeout(Duration::from_millis(200), coordinator.execute(|_| Ok(7)))
                    .await,
            )
        } else {
            None
        };

        let (released, released_changed) = &*release;
        *released.lock().unwrap() = true;
        released_changed.notify_all();

        assert!(
            both_started,
            "both reader workers should accept work concurrently"
        );
        assert_eq!(writer_result.unwrap().unwrap().unwrap(), 7);
        assert!(first_read.await.unwrap().unwrap());
        assert!(second_read.await.unwrap().unwrap());

        let metrics = coordinator.metrics_snapshot();
        assert_eq!(metrics.write_workers, 1);
        assert_eq!(metrics.read_workers, 2);
        assert_eq!(metrics.queue_capacity, 3);
        assert_eq!(metrics.operations_started, 3);
        assert_eq!(metrics.operations_completed, 3);
        assert_eq!(metrics.operations_in_flight, 0);
        assert_eq!(metrics.worker_active, 0);
    }
}
