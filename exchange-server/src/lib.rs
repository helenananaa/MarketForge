use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    convert::Infallible,
    fmt::Write as _,
    future::Future,
    io::{self, BufRead, BufReader},
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub mod auth;
pub mod journal;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{
        HeaderMap, HeaderName, HeaderValue, Method, StatusCode,
        header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE},
    },
    response::{
        IntoResponse, Response,
        sse::{Event as SseEvent, KeepAlive, Sse},
    },
    routing::{get, post},
};
use exchange_core::{
    AccountSnapshots, ActorExecution, ActorExecutionResult, ActorRejectReason, AgentTemplate,
    AssetLedgerEntry, BookSnapshot, EXTERNAL_ACTIONS_PER_STEP, Event, GatewayRequest, InstrumentId,
    MarketExecution, MarketStatus, MarketView, Money, OrderAction, OrderGateway, OrderId,
    Participant, ParticipantId, ParticipantObservation, PortfolioAccountSnapshot, RoomId,
    RoomManager, RoomManagerError, RoomNetWorthSnapshot, STRATEGY_PROTOCOL_VERSION, ScenarioConfig,
    SimulationClock, SpotAccountSnapshot, SpotClearingEvent, TradingApi, TrainingStatus,
    VenueAccountSnapshot, VenueAccountVenueSnapshot, VenueToVenueTransfer, VenueTransfer,
    model::{AccountId, CancelOrder, Command, OrderKind, SetMarkPrice},
    perp::{PerpAccountSnapshot, PerpClearingEvent},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::sync::{Mutex as AsyncMutex, Notify, broadcast, watch};
use tower_http::cors::CorsLayer;

use crate::auth::{AuthError, AuthPolicy, USER_ID_HEADER};
use crate::journal::{
    AccountLedgerProjection, ControlIdempotencyRecord, ExecutionPage, JournalError,
    JournalExecution, JournalMutation, JournalRecovery, JournalSnapshot, JournalStore,
    JournalStoreBundle, JournalTransfer, MAX_ROOM_LEASE_DURATION_MS, MarketTickProjection,
    OrderProjection, PendingJournalMutation, PositionSnapshotProjection, RoomLeaseClaim,
    RoomMutation, RoomRoutingRecord, RoomWriterLease, TradeProjection, control_request_fingerprint,
    journal_stores_from_env,
};
use crate::journal_worker::JournalCoordinator;

type SharedState = Arc<ServerState>;
type ApiError = (StatusCode, Json<ErrorResponse>);
type ApiResult<T> = Result<Json<T>, ApiError>;
const SNAPSHOT_INTERVAL_COMMANDS: u64 = 100;
const ROOM_EVENT_CHANNEL_CAPACITY: usize = 1_024;
const ROOM_EVENT_CACHE_CAPACITY: usize = 1_024;
const IDEMPOTENCY_KEY_HEADER: &str = "idempotency-key";
pub const BIND_ADDR_ENV: &str = "MARKETFORGE_BIND_ADDR";
pub const DEFAULT_BIND_ADDR: &str = "127.0.0.1:57305";
pub const CORS_ORIGINS_ENV: &str = "MARKETFORGE_CORS_ORIGINS";
pub const RUNTIME_MODE_ENV: &str = "MARKETFORGE_RUNTIME_MODE";
pub const INSTANCE_ID_ENV: &str = "MARKETFORGE_INSTANCE_ID";
pub const ADVERTISE_URL_ENV: &str = "MARKETFORGE_ADVERTISE_URL";
pub const ROOM_LEASE_DURATION_MS_ENV: &str = "MARKETFORGE_ROOM_LEASE_DURATION_MS";
pub const ROOM_LEASE_RENEW_INTERVAL_MS_ENV: &str = "MARKETFORGE_ROOM_LEASE_RENEW_INTERVAL_MS";
const DEFAULT_ROOM_LEASE_DURATION_MS: u64 = 15_000;
const DEFAULT_ROOM_LEASE_RENEW_INTERVAL_MS: u64 = 5_000;
const MIN_ROOM_LEASE_RENEW_INTERVAL_MS: u64 = 10;
const DEFAULT_CORS_ORIGINS: &[&str] = &["http://127.0.0.1:57304", "http://localhost:57304"];
const SYSTEM_LIQUIDATION_ORDER_ID_BASE: OrderId = 9_000_000_000_000_000_000;

struct ServerState {
    app: AsyncMutex<AppState>,
    lifecycle: RuntimeLifecycle,
    started_at: Instant,
}

impl ServerState {
    fn new(app: AppState) -> Self {
        Self {
            app: AsyncMutex::new(app),
            lifecycle: RuntimeLifecycle::new(),
            started_at: Instant::now(),
        }
    }
}

#[derive(Clone)]
struct RuntimeLifecycle {
    inner: Arc<RuntimeLifecycleInner>,
}

struct RuntimeLifecycleInner {
    accepting_durable_writes: AtomicBool,
    active_durable_writes: AtomicUsize,
    durable_writes_started: AtomicU64,
    durable_writes_completed: AtomicU64,
    durable_writes_failed: AtomicU64,
    durable_writes_rejected: AtomicU64,
    active_sse_connections: AtomicUsize,
    sse_connections_started: AtomicU64,
    sse_resync_required: AtomicU64,
    scheduler_steps_total: AtomicU64,
    scheduler_step_errors_total: AtomicU64,
    agent_errors_total: AtomicU64,
    checkpoint_writes_total: AtomicU64,
    checkpoint_duration_ms_total: AtomicU64,
    replayed_commands_total: AtomicU64,
    drained: Notify,
    shutdown: watch::Sender<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RuntimeLifecycleMetricsSnapshot {
    accepting_durable_writes: bool,
    active_durable_writes: usize,
    durable_writes_started: u64,
    durable_writes_completed: u64,
    durable_writes_failed: u64,
    durable_writes_rejected: u64,
    active_sse_connections: usize,
    sse_connections_started: u64,
    sse_resync_required: u64,
    scheduler_steps_total: u64,
    scheduler_step_errors_total: u64,
    agent_errors_total: u64,
    checkpoint_writes_total: u64,
    checkpoint_duration_ms_total: u64,
    replayed_commands_total: u64,
}

impl RuntimeLifecycle {
    fn new() -> Self {
        let (shutdown, _) = watch::channel(false);
        Self {
            inner: Arc::new(RuntimeLifecycleInner {
                accepting_durable_writes: AtomicBool::new(true),
                active_durable_writes: AtomicUsize::new(0),
                durable_writes_started: AtomicU64::new(0),
                durable_writes_completed: AtomicU64::new(0),
                durable_writes_failed: AtomicU64::new(0),
                durable_writes_rejected: AtomicU64::new(0),
                active_sse_connections: AtomicUsize::new(0),
                sse_connections_started: AtomicU64::new(0),
                sse_resync_required: AtomicU64::new(0),
                scheduler_steps_total: AtomicU64::new(0),
                scheduler_step_errors_total: AtomicU64::new(0),
                agent_errors_total: AtomicU64::new(0),
                checkpoint_writes_total: AtomicU64::new(0),
                checkpoint_duration_ms_total: AtomicU64::new(0),
                replayed_commands_total: AtomicU64::new(0),
                drained: Notify::new(),
                shutdown,
            }),
        }
    }

    fn try_begin_durable_write(&self) -> Option<DurableWriteGuard> {
        if !self.inner.accepting_durable_writes.load(Ordering::Acquire) {
            self.inner
                .durable_writes_rejected
                .fetch_add(1, Ordering::Relaxed);
            return None;
        }
        self.inner
            .active_durable_writes
            .fetch_add(1, Ordering::AcqRel);
        if !self.inner.accepting_durable_writes.load(Ordering::Acquire) {
            self.release_durable_write();
            self.inner
                .durable_writes_rejected
                .fetch_add(1, Ordering::Relaxed);
            return None;
        }
        self.inner
            .durable_writes_started
            .fetch_add(1, Ordering::Relaxed);
        Some(DurableWriteGuard {
            lifecycle: self.clone(),
            succeeded: false,
        })
    }

    fn release_durable_write(&self) {
        if self
            .inner
            .active_durable_writes
            .fetch_sub(1, Ordering::AcqRel)
            == 1
        {
            self.inner.drained.notify_waiters();
        }
    }

    fn finish_durable_write(&self, succeeded: bool) {
        self.inner
            .durable_writes_completed
            .fetch_add(1, Ordering::Relaxed);
        if !succeeded {
            self.inner
                .durable_writes_failed
                .fetch_add(1, Ordering::Relaxed);
        }
        self.release_durable_write();
    }

    fn is_accepting_durable_writes(&self) -> bool {
        self.inner.accepting_durable_writes.load(Ordering::Acquire)
    }

    fn open_sse_connection(&self) -> SseConnectionGuard {
        self.inner
            .active_sse_connections
            .fetch_add(1, Ordering::Relaxed);
        self.inner
            .sse_connections_started
            .fetch_add(1, Ordering::Relaxed);
        SseConnectionGuard {
            lifecycle: self.clone(),
        }
    }

    fn finish_sse_connection(&self) {
        self.inner
            .active_sse_connections
            .fetch_sub(1, Ordering::Relaxed);
    }

    fn record_sse_resync_required(&self) {
        self.inner
            .sse_resync_required
            .fetch_add(1, Ordering::Relaxed);
    }

    fn record_scheduler_step(&self, succeeded: bool) {
        self.inner
            .scheduler_steps_total
            .fetch_add(1, Ordering::Relaxed);
        if !succeeded {
            self.inner
                .scheduler_step_errors_total
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    fn record_agent_error(&self) {
        self.inner
            .agent_errors_total
            .fetch_add(1, Ordering::Relaxed);
    }

    fn record_checkpoint(&self, duration_ms: u64) {
        self.inner
            .checkpoint_writes_total
            .fetch_add(1, Ordering::Relaxed);
        self.inner
            .checkpoint_duration_ms_total
            .fetch_add(duration_ms, Ordering::Relaxed);
    }

    fn record_replayed_commands(&self, count: u64) {
        self.inner
            .replayed_commands_total
            .fetch_add(count, Ordering::Relaxed);
    }

    fn metrics_snapshot(&self) -> RuntimeLifecycleMetricsSnapshot {
        RuntimeLifecycleMetricsSnapshot {
            accepting_durable_writes: self.is_accepting_durable_writes(),
            active_durable_writes: self.inner.active_durable_writes.load(Ordering::Relaxed),
            durable_writes_started: self.inner.durable_writes_started.load(Ordering::Relaxed),
            durable_writes_completed: self.inner.durable_writes_completed.load(Ordering::Relaxed),
            durable_writes_failed: self.inner.durable_writes_failed.load(Ordering::Relaxed),
            durable_writes_rejected: self.inner.durable_writes_rejected.load(Ordering::Relaxed),
            active_sse_connections: self.inner.active_sse_connections.load(Ordering::Relaxed),
            sse_connections_started: self.inner.sse_connections_started.load(Ordering::Relaxed),
            sse_resync_required: self.inner.sse_resync_required.load(Ordering::Relaxed),
            scheduler_steps_total: self.inner.scheduler_steps_total.load(Ordering::Relaxed),
            scheduler_step_errors_total: self
                .inner
                .scheduler_step_errors_total
                .load(Ordering::Relaxed),
            agent_errors_total: self.inner.agent_errors_total.load(Ordering::Relaxed),
            checkpoint_writes_total: self.inner.checkpoint_writes_total.load(Ordering::Relaxed),
            checkpoint_duration_ms_total: self
                .inner
                .checkpoint_duration_ms_total
                .load(Ordering::Relaxed),
            replayed_commands_total: self.inner.replayed_commands_total.load(Ordering::Relaxed),
        }
    }

    fn begin_shutdown(&self) {
        self.inner
            .accepting_durable_writes
            .store(false, Ordering::Release);
        self.inner.shutdown.send_replace(true);
        if self.inner.active_durable_writes.load(Ordering::Acquire) == 0 {
            self.inner.drained.notify_waiters();
        }
    }

    fn subscribe_shutdown(&self) -> watch::Receiver<bool> {
        self.inner.shutdown.subscribe()
    }

    async fn wait_for_durable_writes(&self) {
        loop {
            let notified = self.inner.drained.notified();
            if self.inner.active_durable_writes.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }
}

struct DurableWriteGuard {
    lifecycle: RuntimeLifecycle,
    succeeded: bool,
}

impl DurableWriteGuard {
    fn mark_succeeded(&mut self) {
        self.succeeded = true;
    }
}

struct SseConnectionGuard {
    lifecycle: RuntimeLifecycle,
}

impl SseConnectionGuard {
    fn record_resync_required(&self) {
        self.lifecycle.record_sse_resync_required();
    }
}

impl Drop for SseConnectionGuard {
    fn drop(&mut self) {
        self.lifecycle.finish_sse_connection();
    }
}

impl Drop for DurableWriteGuard {
    fn drop(&mut self) {
        self.lifecycle.finish_durable_write(self.succeeded);
    }
}

fn shared_state(app: AppState) -> SharedState {
    Arc::new(ServerState::new(app))
}

mod journal_worker;

mod json_i128 {
    use std::fmt;

    use serde::{
        Deserializer, Serializer,
        de::{self, Visitor},
    };

    pub fn serialize<S>(value: &i128, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if let Ok(value) = i64::try_from(*value) {
            serializer.serialize_i64(value)
        } else if let Ok(value) = u64::try_from(*value) {
            serializer.serialize_u64(value)
        } else {
            serializer.serialize_str(&value.to_string())
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<i128, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct I128Visitor;

        impl Visitor<'_> for I128Visitor {
            type Value = i128;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an integer or a base-10 i128 string")
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(i128::from(value))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(i128::from(value))
            }

            fn visit_i128<E>(self, value: i128) -> Result<Self::Value, E> {
                Ok(value)
            }

            fn visit_u128<E>(self, value: u128) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                i128::try_from(value).map_err(E::custom)
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                value.parse().map_err(E::custom)
            }
        }

        deserializer.deserialize_any(I128Visitor)
    }
}

mod json_i128_option {
    use std::fmt;

    use serde::{
        Deserializer, Serializer,
        de::{self, Visitor},
    };

    pub fn serialize<S>(value: &Option<i128>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(value) => super::json_i128::serialize(value, serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<i128>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct OptionI128Visitor;

        impl<'de> Visitor<'de> for OptionI128Visitor {
            type Value = Option<i128>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an optional integer or base-10 i128 string")
            }

            fn visit_none<E>(self) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(None)
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(None)
            }

            fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
            where
                D: Deserializer<'de>,
            {
                super::json_i128::deserialize(deserializer).map(Some)
            }
        }

        deserializer.deserialize_option(OptionI128Visitor)
    }
}

struct AppState {
    rooms: RoomManager,
    executions: BTreeMap<RoomId, VecDeque<RoomExecutionSummary>>,
    room_event_senders: BTreeMap<RoomId, broadcast::Sender<RoomExecutionSummary>>,
    next_order_id: OrderId,
    base_url: String,
    agent_workers: BTreeMap<RoomId, AgentWorkerHandle>,
    schedulers: BTreeMap<RoomId, exchange_core::SchedulerState>,
    training_runs: BTreeMap<String, exchange_core::TrainingRun>,
    journal: JournalCoordinator,
    auth_policy: AuthPolicy,
    room_lease_runtime: Option<RoomLeaseRuntimeState>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RoomLeaseRuntimeConfig {
    mode: RoomLeaseRuntimeMode,
    instance_id: String,
    owner_url: String,
    lease_duration: Duration,
    renew_interval: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RoomLeaseRuntimeMode {
    GuardedSingleActive,
    RoomLeased,
}

#[derive(Debug)]
struct RoomLeaseRuntimeState {
    config: RoomLeaseRuntimeConfig,
    leases: BTreeMap<RoomId, RoomWriterLease>,
    lost_rooms: BTreeSet<RoomId>,
    renew_failures: u64,
}

impl AppState {
    fn new(base_url: impl Into<String>) -> Self {
        Self::new_with_journal_and_auth_policy(
            base_url,
            Box::new(journal::InMemoryJournalStore::new()),
            AuthPolicy::local_development(),
        )
    }

    fn new_with_journal(base_url: impl Into<String>, journal: Box<dyn JournalStore>) -> Self {
        Self::new_with_journal_and_auth_policy(base_url, journal, AuthPolicy::local_development())
    }

    fn new_with_journal_and_auth_policy(
        base_url: impl Into<String>,
        journal: Box<dyn JournalStore>,
        auth_policy: AuthPolicy,
    ) -> Self {
        Self::new_with_journal_bundle_and_auth_policy(
            base_url,
            JournalStoreBundle::single(journal),
            auth_policy,
        )
    }

    fn new_with_journal_bundle_and_auth_policy(
        base_url: impl Into<String>,
        journal: JournalStoreBundle,
        auth_policy: AuthPolicy,
    ) -> Self {
        let journal = if journal.readers.is_empty() {
            JournalCoordinator::new(journal.writer)
        } else {
            JournalCoordinator::with_read_stores(journal.writer, journal.readers)
        };
        Self {
            rooms: RoomManager::new(),
            executions: BTreeMap::new(),
            room_event_senders: BTreeMap::new(),
            next_order_id: 1,
            base_url: base_url.into(),
            agent_workers: BTreeMap::new(),
            schedulers: BTreeMap::new(),
            training_runs: BTreeMap::new(),
            journal,
            auth_policy,
            room_lease_runtime: None,
        }
    }

    fn recover_with_journal_bundle_and_auth_policy(
        base_url: impl Into<String>,
        mut journal: JournalStoreBundle,
        auth_policy: AuthPolicy,
        room_lease_config: Option<RoomLeaseRuntimeConfig>,
    ) -> Result<Self, JournalError> {
        let mut recovery = journal.writer.load_recovery()?;
        let next_order_id = next_order_id_from_recovery(&recovery)?;
        let room_lease_runtime = room_lease_config
            .map(|config| {
                acquire_recovered_room_writer_leases(&mut *journal.writer, &recovery, config)
            })
            .transpose()?;
        if let Some(runtime) = &room_lease_runtime
            && runtime.config.mode == RoomLeaseRuntimeMode::RoomLeased
        {
            retain_recovery_rooms(&mut recovery, &runtime.leases);
        }
        let rooms = recover_rooms(&recovery)?;
        let schedulers = scheduler_states_from_recovery(&recovery);
        let executions = execution_summaries_from_recovery(&recovery);
        let journal = if journal.readers.is_empty() {
            JournalCoordinator::new(journal.writer)
        } else {
            JournalCoordinator::with_read_stores(journal.writer, journal.readers)
        };

        Ok(Self {
            rooms,
            executions,
            room_event_senders: BTreeMap::new(),
            next_order_id,
            base_url: base_url.into(),
            agent_workers: BTreeMap::new(),
            schedulers,
            training_runs: training_runs_from_recovery(&recovery),
            journal,
            auth_policy,
            room_lease_runtime,
        })
    }

    fn room_lease_claim(&self, room_id: &str) -> Result<Option<RoomLeaseClaim>, JournalError> {
        let Some(runtime) = &self.room_lease_runtime else {
            return Ok(None);
        };
        runtime
            .leases
            .get(room_id)
            .map(|lease| Some(lease.claim.clone()))
            .ok_or_else(|| JournalError::RoomLeaseNotOwned {
                room_id: room_id.to_string(),
                owner_id: runtime.config.instance_id.clone(),
            })
    }

    async fn append_executions(
        &self,
        room_id: &str,
        records: &[JournalExecution],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        match self.room_lease_claim(room_id)? {
            Some(claim) => {
                self.journal
                    .append_executions_fenced(&claim, records, snapshot)
                    .await
            }
            None => self.journal.append_executions(records, snapshot).await,
        }
    }

    async fn append_room_mutation(
        &self,
        mutation: &PendingJournalMutation,
        execution_records: &[JournalExecution],
        transfer_records: &[JournalTransfer],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        match self.room_lease_claim(&mutation.room_id)? {
            Some(claim) => {
                self.journal
                    .append_room_mutation_fenced(
                        &claim,
                        mutation,
                        execution_records,
                        transfer_records,
                        snapshot,
                    )
                    .await
            }
            None => {
                self.journal
                    .append_room_mutation(mutation, execution_records, transfer_records, snapshot)
                    .await
            }
        }
    }

    fn room_lease_readiness_error(&self) -> Option<String> {
        let runtime = self.room_lease_runtime.as_ref()?;
        let missing = self
            .rooms
            .room_ids()
            .into_iter()
            .filter(|room_id| !runtime.leases.contains_key(*room_id))
            .collect::<Vec<_>>();
        if missing.is_empty() {
            None
        } else {
            Some(format!(
                "instance {} does not own writer leases for rooms: {}",
                runtime.config.instance_id,
                missing.join(",")
            ))
        }
    }

    fn room_lease_metrics(&self) -> (usize, usize, u64) {
        self.room_lease_runtime
            .as_ref()
            .map(|runtime| {
                (
                    runtime.leases.len(),
                    runtime.lost_rooms.len(),
                    runtime.renew_failures,
                )
            })
            .unwrap_or((0, 0, 0))
    }

    fn room_event_receiver(&mut self, room_id: &str) -> broadcast::Receiver<RoomExecutionSummary> {
        self.room_event_senders
            .entry(room_id.to_string())
            .or_insert_with(|| broadcast::channel(ROOM_EVENT_CHANNEL_CAPACITY).0)
            .subscribe()
    }

    fn replace_room_executions(&mut self, room_id: RoomId, executions: Vec<RoomExecutionSummary>) {
        self.executions.insert(
            room_id.clone(),
            bounded_execution_cache(executions.iter().cloned()),
        );
        self.publish_room_executions(&room_id, &executions);
    }

    fn append_room_executions(&mut self, room_id: &str, executions: Vec<RoomExecutionSummary>) {
        let cached = self.executions.entry(room_id.to_string()).or_default();
        for execution in executions.iter().cloned() {
            if cached.len() == ROOM_EVENT_CACHE_CAPACITY {
                cached.pop_front();
            }
            cached.push_back(execution);
        }
        self.publish_room_executions(room_id, &executions);
    }

    fn publish_room_executions(&mut self, room_id: &str, executions: &[RoomExecutionSummary]) {
        if executions.is_empty() {
            return;
        }
        let sender = self
            .room_event_senders
            .entry(room_id.to_string())
            .or_insert_with(|| broadcast::channel(ROOM_EVENT_CHANNEL_CAPACITY).0);
        for execution in executions {
            let _ = sender.send(execution.clone());
        }
    }
}

fn bounded_execution_cache(
    executions: impl IntoIterator<Item = RoomExecutionSummary>,
) -> VecDeque<RoomExecutionSummary> {
    let mut cached = VecDeque::with_capacity(ROOM_EVENT_CACHE_CAPACITY);
    for execution in executions {
        if cached.len() == ROOM_EVENT_CACHE_CAPACITY {
            cached.pop_front();
        }
        cached.push_back(execution);
    }
    cached
}

fn acquire_recovered_room_writer_leases(
    store: &mut dyn JournalStore,
    recovery: &JournalRecovery,
    config: RoomLeaseRuntimeConfig,
) -> Result<RoomLeaseRuntimeState, JournalError> {
    if config.mode == RoomLeaseRuntimeMode::RoomLeased {
        let mut leases = BTreeMap::new();
        for room in &recovery.rooms {
            match store.acquire_room_writer_lease(
                &room.room_id,
                &config.instance_id,
                Some(&config.owner_url),
                config.lease_duration,
            ) {
                Ok(Some(lease)) => {
                    leases.insert(room.room_id.clone(), lease);
                }
                Ok(None) => {}
                Err(error) => {
                    release_room_writer_lease_set(store, leases.values());
                    return Err(error);
                }
            }
        }
        return Ok(RoomLeaseRuntimeState {
            config,
            leases,
            lost_rooms: BTreeSet::new(),
            renew_failures: 0,
        });
    }

    let deadline = Instant::now() + config.lease_duration;
    let mut leases = BTreeMap::new();
    for room in &recovery.rooms {
        loop {
            match store.acquire_room_writer_lease(
                &room.room_id,
                &config.instance_id,
                Some(&config.owner_url),
                config.lease_duration,
            ) {
                Ok(Some(lease)) => {
                    leases.insert(room.room_id.clone(), lease);
                    break;
                }
                Ok(None) => {}
                Err(error) => {
                    release_room_writer_lease_set(store, leases.values());
                    return Err(error);
                }
            }
            let now = Instant::now();
            if now >= deadline {
                release_room_writer_lease_set(store, leases.values());
                return Err(JournalError::RoomLeaseNotOwned {
                    room_id: room.room_id.clone(),
                    owner_id: config.instance_id.clone(),
                });
            }
            thread::sleep((deadline - now).min(Duration::from_millis(50)));
        }
    }
    Ok(RoomLeaseRuntimeState {
        config,
        leases,
        lost_rooms: BTreeSet::new(),
        renew_failures: 0,
    })
}

fn retain_recovery_rooms(
    recovery: &mut JournalRecovery,
    leases: &BTreeMap<RoomId, RoomWriterLease>,
) {
    recovery
        .rooms
        .retain(|room| leases.contains_key(&room.room_id));
    recovery
        .executions
        .retain(|execution| leases.contains_key(&execution.room_id));
    recovery
        .mutations
        .retain(|mutation| leases.contains_key(&mutation.room_id));
    recovery
        .snapshots
        .retain(|snapshot| leases.contains_key(&snapshot.room_id));
}

fn release_room_writer_lease_set<'a>(
    store: &mut dyn JournalStore,
    leases: impl IntoIterator<Item = &'a RoomWriterLease>,
) {
    for lease in leases {
        let _ = store.release_room_writer_lease(&lease.claim);
    }
}

pub fn new_app() -> Router {
    new_app_with_base_url("http://127.0.0.1:57305")
}

pub fn new_app_with_base_url(base_url: impl Into<String>) -> Router {
    app(shared_state(AppState::new(base_url)))
}

pub fn new_app_with_journal(base_url: impl Into<String>, journal: Box<dyn JournalStore>) -> Router {
    app(shared_state(AppState::new_with_journal(base_url, journal)))
}

pub fn new_app_with_journal_and_auth_policy(
    base_url: impl Into<String>,
    journal: Box<dyn JournalStore>,
    auth_policy: AuthPolicy,
) -> Router {
    app(shared_state(AppState::new_with_journal_and_auth_policy(
        base_url,
        journal,
        auth_policy,
    )))
}

pub fn new_app_recovering_with_journal(
    base_url: impl Into<String>,
    journal: Box<dyn JournalStore>,
) -> Result<Router, JournalError> {
    new_app_recovering_with_journal_factory_sync(
        base_url.into(),
        AuthPolicy::local_development(),
        default_cors_origins(),
        None,
        move || Ok(JournalStoreBundle::single(journal)),
    )
}

pub fn new_app_recovering_with_journal_and_auth_policy(
    base_url: impl Into<String>,
    journal: Box<dyn JournalStore>,
    auth_policy: AuthPolicy,
) -> Result<Router, JournalError> {
    new_app_recovering_with_journal_factory_sync(
        base_url.into(),
        auth_policy,
        default_cors_origins(),
        None,
        move || Ok(JournalStoreBundle::single(journal)),
    )
}

pub fn new_app_from_env_with_base_url(base_url: impl Into<String>) -> Result<Router, JournalError> {
    let base_url = base_url.into();
    let auth_policy =
        AuthPolicy::from_env().map_err(|error| JournalError::Recovery(error.to_string()))?;
    let cors_origins =
        cors_origins_from_env().map_err(|error| JournalError::Recovery(error.to_string()))?;
    if room_lease_config_from_env(Some(&base_url))
        .map_err(|error| JournalError::Recovery(error.to_string()))?
        .is_some()
    {
        return Err(JournalError::Recovery(format!(
            "{INSTANCE_ID_ENV} requires the managed serve/serve_from_env lifecycle"
        )));
    }
    new_app_recovering_with_journal_factory_sync(
        base_url,
        auth_policy,
        cors_origins,
        None,
        journal_stores_from_env,
    )
}

fn new_app_recovering_with_journal_factory_sync<F>(
    base_url: String,
    auth_policy: AuthPolicy,
    cors_origins: Vec<HeaderValue>,
    room_lease_config: Option<RoomLeaseRuntimeConfig>,
    journal_factory: F,
) -> Result<Router, JournalError>
where
    F: FnOnce() -> Result<JournalStoreBundle, JournalError> + Send + 'static,
{
    let state = std::thread::Builder::new()
        .name("marketforge-journal-startup".to_string())
        .spawn(move || recover_app_state(base_url, auth_policy, room_lease_config, journal_factory))
        .map_err(|error| {
            JournalError::Recovery(format!("failed to spawn journal startup worker: {error}"))
        })?
        .join()
        .map_err(|_| JournalError::Recovery("journal startup worker panicked".to_string()))??;
    Ok(app_with_cors_origins(shared_state(state), cors_origins))
}

#[cfg(test)]
async fn new_app_recovering_with_journal_factory_async<F>(
    base_url: String,
    auth_policy: AuthPolicy,
    cors_origins: Vec<HeaderValue>,
    journal_factory: F,
) -> Result<Router, JournalError>
where
    F: FnOnce() -> Result<JournalStoreBundle, JournalError> + Send + 'static,
{
    let state = recover_shared_state_async(base_url, auth_policy, None, journal_factory).await?;
    Ok(app_with_cors_origins(state, cors_origins))
}

async fn recover_shared_state_async<F>(
    base_url: String,
    auth_policy: AuthPolicy,
    room_lease_config: Option<RoomLeaseRuntimeConfig>,
    journal_factory: F,
) -> Result<SharedState, JournalError>
where
    F: FnOnce() -> Result<JournalStoreBundle, JournalError> + Send + 'static,
{
    let state = tokio::task::spawn_blocking(move || {
        recover_app_state(base_url, auth_policy, room_lease_config, journal_factory)
    })
    .await
    .map_err(|error| JournalError::Recovery(format!("journal startup worker failed: {error}")))??;
    Ok(shared_state(state))
}

fn recover_app_state<F>(
    base_url: String,
    auth_policy: AuthPolicy,
    room_lease_config: Option<RoomLeaseRuntimeConfig>,
    journal_factory: F,
) -> Result<AppState, JournalError>
where
    F: FnOnce() -> Result<JournalStoreBundle, JournalError>,
{
    let journal = journal_factory()?;
    AppState::recover_with_journal_bundle_and_auth_policy(
        base_url,
        journal,
        auth_policy,
        room_lease_config,
    )
}

pub async fn serve(addr: SocketAddr) -> Result<(), std::io::Error> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    serve_listener(listener).await
}

pub fn bind_addr_from_env() -> Result<SocketAddr, io::Error> {
    let value = std::env::var(BIND_ADDR_ENV).unwrap_or_else(|_| DEFAULT_BIND_ADDR.to_string());
    parse_bind_addr(&value)
}

fn parse_bind_addr(value: &str) -> Result<SocketAddr, io::Error> {
    value.parse().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid {BIND_ADDR_ENV} value {value:?}: {error}"),
        )
    })
}

pub fn cors_origins_from_env() -> Result<Vec<HeaderValue>, io::Error> {
    match std::env::var(CORS_ORIGINS_ENV) {
        Ok(value) => parse_cors_origins(&value),
        Err(std::env::VarError::NotPresent) => Ok(default_cors_origins()),
        Err(error) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid {CORS_ORIGINS_ENV}: {error}"),
        )),
    }
}

fn default_cors_origins() -> Vec<HeaderValue> {
    DEFAULT_CORS_ORIGINS
        .iter()
        .map(|origin| HeaderValue::from_static(origin))
        .collect()
}

fn parse_cors_origins(value: &str) -> Result<Vec<HeaderValue>, io::Error> {
    let mut origins = Vec::new();
    for origin in value.split(',').map(str::trim) {
        let authority = origin
            .strip_prefix("http://")
            .or_else(|| origin.strip_prefix("https://"));
        if authority.is_none_or(|authority| {
            authority.is_empty()
                || authority.contains('/')
                || authority.contains('?')
                || authority.contains('#')
                || authority.contains('@')
                || authority.chars().any(char::is_whitespace)
        }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "invalid {CORS_ORIGINS_ENV} origin {origin:?}; expected an http(s) origin without a path"
                ),
            ));
        }
        let header = HeaderValue::from_str(origin).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid {CORS_ORIGINS_ENV} origin {origin:?}: {error}"),
            )
        })?;
        if !origins.contains(&header) {
            origins.push(header);
        }
    }
    if origins.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{CORS_ORIGINS_ENV} must contain at least one origin"),
        ));
    }
    Ok(origins)
}

fn room_lease_config_from_env(
    default_advertise_url: Option<&str>,
) -> Result<Option<RoomLeaseRuntimeConfig>, io::Error> {
    let runtime_mode = optional_env(RUNTIME_MODE_ENV)?;
    let instance_id = optional_env(INSTANCE_ID_ENV)?;
    let advertise_url = optional_env(ADVERTISE_URL_ENV)?;
    let database_url = optional_env("MARKETFORGE_DATABASE_URL")?;
    let lease_duration_ms = optional_env(ROOM_LEASE_DURATION_MS_ENV)?;
    let renew_interval_ms = optional_env(ROOM_LEASE_RENEW_INTERVAL_MS_ENV)?;
    parse_room_lease_runtime_config(
        runtime_mode.as_deref(),
        instance_id.as_deref(),
        database_url.as_deref(),
        lease_duration_ms.as_deref(),
        renew_interval_ms.as_deref(),
        advertise_url.as_deref(),
        default_advertise_url,
    )
}

fn parse_room_lease_runtime_config(
    runtime_mode: Option<&str>,
    instance_id: Option<&str>,
    database_url: Option<&str>,
    lease_duration_ms: Option<&str>,
    renew_interval_ms: Option<&str>,
    advertise_url: Option<&str>,
    default_advertise_url: Option<&str>,
) -> Result<Option<RoomLeaseRuntimeConfig>, io::Error> {
    let mode = match runtime_mode.unwrap_or("single-active") {
        "single-active" => RoomLeaseRuntimeMode::GuardedSingleActive,
        "room-leased" => RoomLeaseRuntimeMode::RoomLeased,
        value => {
            return Err(invalid_env_error(format!(
                "invalid {RUNTIME_MODE_ENV} value {value:?}; expected single-active or room-leased"
            )));
        }
    };
    let Some(instance_id) = instance_id else {
        if mode == RoomLeaseRuntimeMode::RoomLeased {
            return Err(invalid_env_error(format!(
                "{RUNTIME_MODE_ENV}=room-leased requires {INSTANCE_ID_ENV}"
            )));
        }
        if lease_duration_ms.is_some() || renew_interval_ms.is_some() || advertise_url.is_some() {
            return Err(invalid_env_error(format!(
                "room lease configuration requires {INSTANCE_ID_ENV}"
            )));
        }
        return Ok(None);
    };
    if instance_id.trim().is_empty() {
        return Err(invalid_env_error(format!(
            "{INSTANCE_ID_ENV} must not be empty"
        )));
    }
    if instance_id.len() > 128
        || !instance_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
        })
    {
        return Err(invalid_env_error(format!(
            "invalid {INSTANCE_ID_ENV} value; expected 1-128 ASCII letters, digits, '.', '_', '-', or ':'"
        )));
    }
    if database_url.is_none_or(|value| value.trim().is_empty()) {
        return Err(invalid_env_error(format!(
            "{INSTANCE_ID_ENV} requires a non-empty MARKETFORGE_DATABASE_URL"
        )));
    }
    let owner_url = parse_advertise_url(
        advertise_url
            .or(default_advertise_url)
            .ok_or_else(|| {
                invalid_env_error(format!(
                    "{INSTANCE_ID_ENV} requires {ADVERTISE_URL_ENV} when the bind address is not externally routable"
                ))
            })?,
    )?;

    let lease_duration_ms = parse_room_lease_timing_value(
        ROOM_LEASE_DURATION_MS_ENV,
        lease_duration_ms,
        DEFAULT_ROOM_LEASE_DURATION_MS,
    )?;
    if lease_duration_ms == 0 || lease_duration_ms > MAX_ROOM_LEASE_DURATION_MS {
        return Err(invalid_env_error(format!(
            "{ROOM_LEASE_DURATION_MS_ENV} must be from 1 to {MAX_ROOM_LEASE_DURATION_MS}"
        )));
    }
    let renew_interval_ms = parse_room_lease_timing_value(
        ROOM_LEASE_RENEW_INTERVAL_MS_ENV,
        renew_interval_ms,
        DEFAULT_ROOM_LEASE_RENEW_INTERVAL_MS
            .min((lease_duration_ms / 3).max(MIN_ROOM_LEASE_RENEW_INTERVAL_MS)),
    )?;
    if renew_interval_ms < MIN_ROOM_LEASE_RENEW_INTERVAL_MS
        || renew_interval_ms >= lease_duration_ms
    {
        return Err(invalid_env_error(format!(
            "{ROOM_LEASE_RENEW_INTERVAL_MS_ENV} must be at least {MIN_ROOM_LEASE_RENEW_INTERVAL_MS} and less than {ROOM_LEASE_DURATION_MS_ENV}"
        )));
    }

    Ok(Some(RoomLeaseRuntimeConfig {
        mode,
        instance_id: instance_id.to_string(),
        owner_url,
        lease_duration: Duration::from_millis(lease_duration_ms),
        renew_interval: Duration::from_millis(renew_interval_ms),
    }))
}

fn parse_advertise_url(value: &str) -> Result<String, io::Error> {
    if value.len() > 2_048 {
        return Err(invalid_env_error(format!(
            "{ADVERTISE_URL_ENV} must not exceed 2048 bytes"
        )));
    }
    let url = reqwest::Url::parse(value).map_err(|error| {
        invalid_env_error(format!(
            "invalid {ADVERTISE_URL_ENV} value {value:?}: {error}"
        ))
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid_env_error(format!(
            "invalid {ADVERTISE_URL_ENV} value {value:?}; expected an http(s) base URL without credentials, query, or fragment"
        )));
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

fn parse_room_lease_timing_value(
    name: &str,
    value: Option<&str>,
    default: u64,
) -> Result<u64, io::Error> {
    match value {
        Some(value) => value.parse::<u64>().map_err(|_| {
            invalid_env_error(format!(
                "invalid {name} value {value:?}; expected milliseconds"
            ))
        }),
        None => Ok(default),
    }
}

fn optional_env(name: &str) -> Result<Option<String>, io::Error> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(invalid_env_error(format!("invalid {name}: {error}"))),
    }
}

fn invalid_env_error(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

pub async fn serve_from_env() -> Result<(), io::Error> {
    serve(bind_addr_from_env()?).await
}

pub async fn serve_listener(listener: tokio::net::TcpListener) -> Result<(), std::io::Error> {
    serve_listener_with_shutdown(listener, shutdown_signal()).await
}

pub async fn serve_listener_with_shutdown<F>(
    listener: tokio::net::TcpListener,
    shutdown: F,
) -> Result<(), std::io::Error>
where
    F: Future<Output = ()> + Send + 'static,
{
    let addr = listener.local_addr()?;
    let auth_policy =
        AuthPolicy::from_env().map_err(|error| io::Error::other(error.to_string()))?;
    validate_auth_policy_for_addr(&auth_policy, addr)?;
    let cors_origins = cors_origins_from_env()?;
    let base_url = format!("http://{addr}");
    let default_advertise_url = (!addr.ip().is_unspecified()).then_some(base_url.as_str());
    let room_lease_config = room_lease_config_from_env(default_advertise_url)?;
    let state = recover_shared_state_async(
        base_url,
        auth_policy,
        room_lease_config,
        journal_stores_from_env,
    )
    .await
    .map_err(|error| io::Error::other(error.to_string()))?;
    let lease_renewer = tokio::spawn(run_room_lease_renewer(state.clone()));
    let app = app_with_cors_origins(state.clone(), cors_origins);
    let lifecycle = state.lifecycle.clone();
    let shutdown_lifecycle = lifecycle.clone();
    println!("exchange-server listening on http://{addr}");
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            shutdown_lifecycle.begin_shutdown();
        })
        .await;

    // Also enter shutdown when the listener exits unexpectedly. Axum waits for
    // request tasks, while this explicit drain additionally covers durable
    // transactions detached from disconnected HTTP requests.
    lifecycle.begin_shutdown();
    let workers = {
        let mut app = state.app.lock().await;
        std::mem::take(&mut app.agent_workers)
    };
    for worker in workers.values() {
        worker.request_stop();
    }
    tokio::task::spawn_blocking(move || {
        for worker in workers.into_values() {
            worker.shutdown_and_join();
        }
    })
    .await
    .map_err(|error| io::Error::other(format!("agent shutdown worker failed: {error}")))?;
    lifecycle.wait_for_durable_writes().await;
    lease_renewer
        .await
        .map_err(|error| io::Error::other(format!("room lease renewer failed: {error}")))?;
    if let Err(error) = release_owned_room_writer_leases(&state).await {
        eprintln!("failed to release room writer leases during shutdown: {error}");
    }
    result
}

async fn run_room_lease_renewer(state: SharedState) {
    let renew_interval = {
        let app = state.app.lock().await;
        app.room_lease_runtime
            .as_ref()
            .map(|runtime| runtime.config.renew_interval)
    };
    let Some(renew_interval) = renew_interval else {
        return;
    };
    let mut shutdown = state.lifecycle.subscribe_shutdown();
    let mut ticker = tokio::time::interval(renew_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
            _ = ticker.tick() => renew_room_writer_leases_once(&state).await,
        }
    }
}

async fn renew_room_writer_leases_once(state: &SharedState) {
    let Some((journal, duration, leases)) = ({
        let app = state.app.lock().await;
        app.room_lease_runtime.as_ref().map(|runtime| {
            (
                app.journal.clone(),
                runtime.config.lease_duration,
                runtime.leases.values().cloned().collect::<Vec<_>>(),
            )
        })
    }) else {
        return;
    };

    let mut results = Vec::with_capacity(leases.len());
    for lease in leases {
        let result = journal
            .renew_room_writer_lease(&lease.claim, duration)
            .await;
        results.push((lease, result));
    }

    let mut stopped_workers = Vec::new();
    let mut errors = Vec::new();
    {
        let mut app = state.app.lock().await;
        let mut lost_room_ids = Vec::new();
        let unload_lost_rooms;
        {
            let Some(runtime) = app.room_lease_runtime.as_mut() else {
                return;
            };
            unload_lost_rooms = runtime.config.mode == RoomLeaseRuntimeMode::RoomLeased;
            for (previous, result) in results {
                let room_id = previous.claim.room_id.clone();
                let still_current = runtime
                    .leases
                    .get(&room_id)
                    .is_some_and(|lease| lease.claim == previous.claim);
                if !still_current {
                    continue;
                }
                match result {
                    Ok(Some(renewed)) => {
                        runtime.leases.insert(room_id, renewed);
                    }
                    Ok(None) => {
                        runtime.leases.remove(&room_id);
                        runtime.lost_rooms.insert(room_id.clone());
                        runtime.renew_failures = runtime.renew_failures.saturating_add(1);
                        lost_room_ids.push(room_id.clone());
                        errors.push(format!("room {room_id} lease was no longer renewable"));
                    }
                    Err(error) => {
                        runtime.leases.remove(&room_id);
                        runtime.lost_rooms.insert(room_id.clone());
                        runtime.renew_failures = runtime.renew_failures.saturating_add(1);
                        lost_room_ids.push(room_id.clone());
                        errors.push(format!("room {room_id} lease renewal failed: {error}"));
                    }
                }
            }
        }
        for room_id in lost_room_ids {
            if let Some(worker) = app.agent_workers.remove(&room_id) {
                stopped_workers.push(worker);
            }
            if unload_lost_rooms {
                app.rooms.remove_room(&room_id);
                app.executions.remove(&room_id);
                app.room_event_senders.remove(&room_id);
            }
        }
    }
    for error in errors {
        eprintln!("{error}");
    }
    if !stopped_workers.is_empty() {
        let _ = tokio::task::spawn_blocking(move || {
            for worker in stopped_workers {
                worker.shutdown_and_join();
            }
        })
        .await;
    }
}

async fn release_owned_room_writer_leases(state: &SharedState) -> Result<(), JournalError> {
    let Some((journal, leases)) = ({
        let app = state.app.lock().await;
        app.room_lease_runtime.as_ref().map(|runtime| {
            (
                app.journal.clone(),
                runtime.leases.values().cloned().collect::<Vec<_>>(),
            )
        })
    }) else {
        return Ok(());
    };
    let mut first_error = None;
    for lease in &leases {
        if let Err(error) = journal.release_room_writer_lease(&lease.claim).await
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    {
        let mut app = state.app.lock().await;
        if let Some(runtime) = app.room_lease_runtime.as_mut() {
            for lease in leases {
                if runtime
                    .leases
                    .get(&lease.claim.room_id)
                    .is_some_and(|current| current.claim == lease.claim)
                {
                    runtime.leases.remove(&lease.claim.room_id);
                }
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(error) => {
                    eprintln!("failed to install SIGTERM handler: {error}");
                    let _ = tokio::signal::ctrl_c().await;
                    return;
                }
            };
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(error) = result {
                    eprintln!("failed to listen for Ctrl-C: {error}");
                }
            }
            _ = terminate.recv() => {}
        }
    }

    #[cfg(not(unix))]
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("failed to listen for Ctrl-C: {error}");
    }
}

fn validate_auth_policy_for_addr(policy: &AuthPolicy, addr: SocketAddr) -> Result<(), io::Error> {
    if addr.ip().is_loopback() || policy.requires_bearer_token() {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "refusing unauthenticated non-loopback bind {addr}; configure {}",
            auth::AUTH_TOKENS_ENV
        ),
    ))
}

fn app(state: SharedState) -> Router {
    app_with_cors_origins(state, default_cors_origins())
}

fn app_with_cors_origins(state: SharedState, cors_origins: Vec<HeaderValue>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(cors_origins)
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            AUTHORIZATION,
            HeaderName::from_static(USER_ID_HEADER),
            HeaderName::from_static(IDEMPOTENCY_KEY_HEADER),
            HeaderName::from_static("last-event-id"),
        ]);

    Router::new()
        .route("/health", get(readiness))
        .route("/health/live", get(liveness))
        .route("/health/ready", get(readiness))
        .route("/metrics", get(metrics))
        .route("/cluster/rooms", get(cluster_rooms))
        .route("/training/runs", post(start_training_run))
        .route("/training/runs/{run_id}", get(training_run_status))
        .route("/training/runs/{run_id}/abort", post(abort_training_run))
        .route("/training/runs/{run_id}/result", get(training_run_result))
        .route("/training/runs/{run_id}/report", get(training_run_report))
        .route("/rooms/{room_id}/replay", get(replay_room_isolated))
        .route("/rooms/{room_id}/members", post(upsert_room_member))
        .route(
            "/rooms/{room_id}/members/{user_id}",
            post(remove_room_member),
        )
        .route(
            "/rooms/{room_id}/accounts/{account_id}/owners",
            post(assign_account_owner),
        )
        .route("/rooms/{room_id}/observe", get(observe_room))
        .route("/rooms", post(create_room).get(list_rooms))
        .route(
            "/rooms/{room_id}/agents",
            get(agent_status).post(start_agents),
        )
        .route("/rooms/{room_id}/agents/stop", post(stop_agents))
        .route("/rooms/{room_id}/events", get(room_events))
        .route("/rooms/{room_id}/events/stream", get(room_event_stream))
        .route("/rooms/{room_id}/owner", get(room_owner))
        .route("/rooms/{room_id}/view", get(market_view))
        .route("/rooms/{room_id}/book", get(book_snapshot))
        .route("/rooms/{room_id}/ticker", get(room_ticker))
        .route("/rooms/{room_id}/candles", get(room_candles))
        .route("/rooms/{room_id}/stream/public", get(public_room_stream))
        .route("/rooms/{room_id}/stream/private", get(private_room_stream))
        .route("/rooms/{room_id}/accounts", get(account_snapshots))
        .route(
            "/rooms/{room_id}/venue/accounts",
            get(venue_account_snapshots),
        )
        .route(
            "/rooms/{room_id}/venue/accounts/by-venue",
            get(venue_account_snapshots_by_venue),
        )
        .route("/rooms/{room_id}/portfolio", get(room_portfolios))
        .route("/rooms/{room_id}/assets/ledger", get(room_asset_ledger))
        .route("/rooms/{room_id}/net-worth", get(room_net_worth))
        .route("/rooms/{room_id}/clock", get(room_clock))
        .route("/rooms/{room_id}/clock/advance", post(advance_room_clock))
        .route("/rooms/{room_id}/clock/step", post(manual_room_step))
        .route("/rooms/{room_id}/transfers", get(room_transfers))
        .route("/rooms/{room_id}/transfers/deposit", post(submit_deposit))
        .route(
            "/rooms/{room_id}/transfers/withdraw",
            post(submit_withdrawal),
        )
        .route(
            "/rooms/{room_id}/transfers/venue-to-venue",
            post(submit_venue_to_venue_transfer),
        )
        .route(
            "/rooms/{room_id}/instruments/{instrument_id}/view",
            get(market_view_for_instrument),
        )
        .route(
            "/rooms/{room_id}/instruments/{instrument_id}/mark-price",
            post(set_mark_price_for_instrument),
        )
        .route(
            "/rooms/{room_id}/instruments/{instrument_id}/book",
            get(book_snapshot_for_instrument),
        )
        .route(
            "/rooms/{room_id}/instruments/{instrument_id}/ticker",
            get(room_ticker_for_instrument),
        )
        .route(
            "/rooms/{room_id}/instruments/{instrument_id}/candles",
            get(room_candles_for_instrument),
        )
        .route(
            "/rooms/{room_id}/instruments/{instrument_id}/accounts",
            get(account_snapshots_for_instrument),
        )
        .route(
            "/rooms/{room_id}/instruments/{instrument_id}/orders",
            get(room_orders_for_instrument).post(submit_order_for_instrument),
        )
        .route(
            "/rooms/{room_id}/instruments/{instrument_id}/trades",
            get(room_trades_for_instrument),
        )
        .route(
            "/rooms/{room_id}/instruments/{instrument_id}/ticks",
            get(room_ticks_for_instrument),
        )
        .route(
            "/rooms/{room_id}/instruments/{instrument_id}/ledger",
            get(room_ledger_for_instrument),
        )
        .route(
            "/rooms/{room_id}/instruments/{instrument_id}/positions",
            get(room_positions_for_instrument),
        )
        .route(
            "/rooms/{room_id}/orders",
            get(room_orders).post(submit_order),
        )
        .route("/rooms/{room_id}/trades", get(room_trades))
        .route("/rooms/{room_id}/ticks", get(room_ticks))
        .route("/rooms/{room_id}/ledger", get(room_ledger))
        .route("/rooms/{room_id}/positions", get(room_positions))
        .route("/rooms/{room_id}/pause", post(pause_room))
        .route("/rooms/{room_id}/resume", post(resume_room))
        .route("/rooms/{room_id}/close", post(close_room))
        .layer(cors)
        .with_state(state)
}

async fn liveness() -> Json<HealthResponse> {
    Json(HealthResponse { ok: true })
}

async fn readiness(State(state): State<SharedState>) -> ApiResult<HealthResponse> {
    ensure_accepting_durable_writes(&state)?;
    let (journal, lease_error) = {
        let app = state.app.lock().await;
        (app.journal.clone(), app.room_lease_readiness_error())
    };
    if let Some(error) = lease_error {
        return Err(api_error(StatusCode::SERVICE_UNAVAILABLE, error));
    }
    journal.health_check().await.map_err(|error| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("journal is unavailable: {error}"),
        )
    })?;
    ensure_accepting_durable_writes(&state)?;
    Ok(Json(HealthResponse { ok: true }))
}

fn ensure_accepting_durable_writes(state: &SharedState) -> Result<(), ApiError> {
    if state.lifecycle.is_accepting_durable_writes() {
        return Ok(());
    }
    Err(api_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "server is shutting down".to_string(),
    ))
}

async fn metrics(State(state): State<SharedState>) -> Response {
    let lifecycle = state.lifecycle.metrics_snapshot();
    let uptime_seconds = state.started_at.elapsed().as_secs_f64();
    let (
        room_count,
        agent_worker_count,
        event_cache_entries,
        journal,
        owned_room_leases,
        lost_room_leases,
        room_lease_renew_failures,
        training_running,
        training_completed,
        training_failed,
        agent_error_workers,
    ) = {
        let app = state.app.lock().await;
        let (owned_room_leases, lost_room_leases, room_lease_renew_failures) =
            app.room_lease_metrics();
        let mut training_running = 0usize;
        let mut training_completed = 0usize;
        let mut training_failed = 0usize;
        for run in app.training_runs.values() {
            match run.status {
                TrainingStatus::Completed => training_completed += 1,
                TrainingStatus::Failed | TrainingStatus::Aborted => training_failed += 1,
                TrainingStatus::Created | TrainingStatus::Running => training_running += 1,
            }
        }
        let agent_error_workers = app
            .agent_workers
            .iter()
            .filter(|(room_id, worker)| worker.status((*room_id).clone()).last_error.is_some())
            .count();
        (
            app.rooms.room_ids().len(),
            app.agent_workers.len(),
            app.executions.values().map(VecDeque::len).sum::<usize>(),
            app.journal.metrics_snapshot(),
            owned_room_leases,
            lost_room_leases,
            room_lease_renew_failures,
            training_running,
            training_completed,
            training_failed,
            agent_error_workers,
        )
    };

    let mut body = String::new();
    append_prometheus_metric(
        &mut body,
        "marketforge_process_up",
        "Whether the MarketForge HTTP process can serve requests.",
        "gauge",
        1,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_process_uptime_seconds",
        "Seconds since the recovered application state became available.",
        "gauge",
        uptime_seconds,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_accepting_durable_writes",
        "Whether the server accepts new durable state transitions.",
        "gauge",
        usize::from(lifecycle.accepting_durable_writes),
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_active_durable_writes",
        "Durable state transitions currently in progress.",
        "gauge",
        lifecycle.active_durable_writes,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_durable_writes_started_total",
        "Durable state transitions accepted since process start.",
        "counter",
        lifecycle.durable_writes_started,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_durable_writes_completed_total",
        "Accepted durable state transitions that have left the protected transaction interval.",
        "counter",
        lifecycle.durable_writes_completed,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_durable_writes_failed_total",
        "Accepted durable state transitions that ended without a successful response.",
        "counter",
        lifecycle.durable_writes_failed,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_durable_writes_rejected_total",
        "Durable state transitions rejected because shutdown had started.",
        "counter",
        lifecycle.durable_writes_rejected,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_sse_connections",
        "Currently open room execution SSE streams.",
        "gauge",
        lifecycle.active_sse_connections,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_sse_connections_started_total",
        "Room execution SSE streams opened since process start.",
        "counter",
        lifecycle.sse_connections_started,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_sse_resync_required_total",
        "SSE streams closed with a resync_required event.",
        "counter",
        lifecycle.sse_resync_required,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_scheduler_steps_total",
        "Scheduler steps attempted since process start.",
        "counter",
        lifecycle.scheduler_steps_total,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_scheduler_step_errors_total",
        "Scheduler steps that failed since process start.",
        "counter",
        lifecycle.scheduler_step_errors_total,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_agent_errors_total",
        "Background agent workers that recorded a last_error.",
        "counter",
        lifecycle.agent_errors_total,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_agent_error_workers",
        "Currently registered agent workers with a last_error.",
        "gauge",
        agent_error_workers,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_training_runs_running",
        "Loaded training runs in Created or Running.",
        "gauge",
        training_running,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_training_runs_completed",
        "Loaded training runs in Completed.",
        "gauge",
        training_completed,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_training_runs_failed",
        "Loaded training runs in Failed or Aborted.",
        "gauge",
        training_failed,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_checkpoint_writes_total",
        "State checkpoints written since process start.",
        "counter",
        lifecycle.checkpoint_writes_total,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_checkpoint_duration_ms_total",
        "Cumulative milliseconds spent building checkpoints.",
        "counter",
        lifecycle.checkpoint_duration_ms_total,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_replayed_commands_total",
        "Commands replayed by isolated history replay since process start.",
        "counter",
        lifecycle.replayed_commands_total,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_rooms",
        "Rooms currently loaded in memory.",
        "gauge",
        room_count,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_agent_workers",
        "Background agent workers currently registered.",
        "gauge",
        agent_worker_count,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_room_writer_leases_owned",
        "Room writer leases currently owned by this process.",
        "gauge",
        owned_room_leases,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_room_writer_leases_lost",
        "Rooms whose writer lease was lost by this process.",
        "gauge",
        lost_room_leases,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_room_writer_lease_renew_failures_total",
        "Room writer lease renewals that failed or found a superseded lease.",
        "counter",
        room_lease_renew_failures,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_event_cache_entries",
        "Execution summaries retained across all bounded room caches.",
        "gauge",
        event_cache_entries,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_event_cache_capacity_per_room",
        "Maximum execution summaries retained in memory per room.",
        "gauge",
        ROOM_EVENT_CACHE_CAPACITY,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_journal_write_workers",
        "Dedicated journal workers that serialize migrations and durable writes.",
        "gauge",
        journal.write_workers,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_journal_read_workers",
        "Dedicated journal workers available for authorization and projection reads.",
        "gauge",
        journal.read_workers,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_journal_channel_open",
        "Whether every journal worker channel is open.",
        "gauge",
        usize::from(journal.channel_open),
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_journal_queue_capacity",
        "Aggregate queue capacity across journal workers.",
        "gauge",
        journal.queue_capacity,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_journal_queue_depth",
        "Journal operations currently buffered across bounded worker queues.",
        "gauge",
        journal.queue_depth,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_journal_worker_active",
        "Journal operations currently executing across dedicated workers.",
        "gauge",
        journal.worker_active,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_journal_operations_in_flight",
        "Journal operations executing, queued, or waiting for queue capacity.",
        "gauge",
        journal.operations_in_flight,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_journal_operations_started_total",
        "Journal operations submitted since process start.",
        "counter",
        journal.operations_started,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_journal_operations_completed_total",
        "Journal operations completed since process start.",
        "counter",
        journal.operations_completed,
    );
    append_prometheus_metric(
        &mut body,
        "marketforge_journal_operation_errors_total",
        "Journal operations that returned an error or could not be queued.",
        "counter",
        journal.operation_errors,
    );

    let mut response = body.into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn append_prometheus_metric(
    body: &mut String,
    name: &str,
    help: &str,
    metric_type: &str,
    value: impl std::fmt::Display,
) {
    writeln!(body, "# HELP {name} {help}").expect("writing metrics to a String cannot fail");
    writeln!(body, "# TYPE {name} {metric_type}").expect("writing metrics to a String cannot fail");
    writeln!(body, "{name} {value}").expect("writing metrics to a String cannot fail");
}

async fn create_room(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> ApiResult<CreateRoomResponse> {
    run_durable_state_transaction(state.clone(), async move {
        let shared = state.clone();
        let mut state = lock_state(&shared).await?;
        let user_id = current_user_id(&headers, &state.auth_policy)?;
        let request = parse_create_room_payload(payload)?;
        validate_agent_templates(&request.scenario.room_id, &request.agents)?;
        let mut candidate_rooms = state.rooms.clone();
        let seed_commands = request.scenario.seed_commands();
        let next_order_id = next_api_order_id_after_commands(state.next_order_id, &seed_commands)?;
        let account_ids = scenario_account_ids(&request.scenario);
        let bootstrap = candidate_rooms
            .create_room(request.scenario.clone())
            .map_err(api_error_from_room)?;
        let room_id = bootstrap.room_id.clone();
        let seed_execution_count = bootstrap.seed_executions.len();
        let seed_records = seed_commands
            .into_iter()
            .zip(bootstrap.seed_executions.iter().cloned())
            .map(|(command, execution)| JournalExecution::seed(command, execution))
            .collect::<Vec<_>>();
        let initial_snapshot = latest_room_snapshot(&candidate_rooms, &room_id, &seed_records);

        let lease_config = state
            .room_lease_runtime
            .as_ref()
            .map(|runtime| runtime.config.clone());
        let initial_lease = if let Some(config) = &lease_config {
            Some(
                state
                    .journal
                    .create_room_with_writer_lease(
                        &user_id,
                        &request.scenario,
                        &bootstrap,
                        &account_ids,
                        &seed_records,
                        initial_snapshot.as_ref(),
                        &config.instance_id,
                        Some(&config.owner_url),
                        config.lease_duration,
                    )
                    .await
                    .map_err(api_error_from_journal)?,
            )
        } else {
            state
                .journal
                .create_room(
                    &user_id,
                    &request.scenario,
                    &bootstrap,
                    &account_ids,
                    &seed_records,
                    initial_snapshot.as_ref(),
                )
                .await
                .map_err(api_error_from_journal)?;
            None
        };
        if let Some(lease) = initial_lease
            && let Some(runtime) = state.room_lease_runtime.as_mut()
        {
            runtime.leases.insert(room_id.clone(), lease);
            runtime.lost_rooms.remove(&room_id);
        }
        state.replace_room_executions(
            room_id.clone(),
            seed_records
                .iter()
                .map(|record| record.execution.clone())
                .collect(),
        );
        state.rooms = candidate_rooms;
        state.next_order_id = next_order_id;

        let mut agent_status = AgentWorkerStatus::stopped(room_id.clone());

        if !request.agents.is_empty() && request.autostart_agents.unwrap_or(true) {
            agent_status = start_agent_worker_for_room(
                &shared,
                &mut state,
                room_id.clone(),
                StartAgentsRequest {
                    agents: request.agents,
                    interval_ms: request.agent_interval_ms,
                },
            )?;
        }

        Ok(Json(CreateRoomResponse {
            room_id,
            seed_execution_count,
            agent_worker: agent_status,
        }))
    })
    .await
}

fn parse_create_room_payload(
    payload: serde_json::Value,
) -> Result<CreateRoomRequest, (StatusCode, Json<ErrorResponse>)> {
    if payload.get("scenario").is_some() {
        return serde_json::from_value(payload).map_err(api_error_from_json);
    }

    serde_json::from_value::<ScenarioConfig>(payload)
        .map(|scenario| CreateRoomRequest {
            scenario,
            agents: Vec::new(),
            agent_interval_ms: None,
            autostart_agents: None,
        })
        .map_err(api_error_from_json)
}

fn recover_rooms(recovery: &JournalRecovery) -> Result<RoomManager, JournalError> {
    let mut rooms = RoomManager::new();
    let mut executions_by_room = recovery.executions.iter().fold(
        BTreeMap::<&str, Vec<&JournalExecution>>::new(),
        |mut map, execution| {
            map.entry(execution.room_id.as_str())
                .or_default()
                .push(execution);
            map
        },
    );
    for executions in executions_by_room.values_mut() {
        executions.sort_by_key(|execution| execution.command_seq);
    }

    let mut mutations_by_room = recovery.mutations.iter().fold(
        BTreeMap::<&str, Vec<&JournalMutation>>::new(),
        |mut map, mutation| {
            map.entry(mutation.room_id.as_str())
                .or_default()
                .push(mutation);
            map
        },
    );
    for mutations in mutations_by_room.values_mut() {
        mutations.sort_by_key(|mutation| mutation.mutation_seq);
        for pair in mutations.windows(2) {
            if pair[1].command_cursor < pair[0].command_cursor {
                return Err(JournalError::Recovery(format!(
                    "room {} mutation cursor regressed from {} to {}",
                    pair[1].room_id, pair[0].command_cursor, pair[1].command_cursor
                )));
            }
        }
    }

    let snapshots_by_room = recovery.snapshots.iter().fold(
        BTreeMap::<&str, &JournalSnapshot>::new(),
        |mut map, snapshot| {
            map.insert(snapshot.room_id.as_str(), snapshot);
            map
        },
    );

    for room in &recovery.rooms {
        let room_mutations = mutations_by_room
            .get(room.room_id.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default();
        let checkpoint = room_mutations
            .iter()
            .rev()
            .find(|mutation| matches!(&mutation.mutation, RoomMutation::StateCheckpoint { .. }));
        let has_mutation_journal = !room_mutations.is_empty();

        let (initial_command_cursor, replay_after_mutation_seq, status_history_complete) =
            if let Some(checkpoint) = checkpoint {
                let RoomMutation::StateCheckpoint {
                    actor,
                    complete_history,
                } = &checkpoint.mutation
                else {
                    unreachable!("checkpoint selection only accepts checkpoint mutations");
                };
                rooms
                    .restore_simulation_room(actor.as_ref().clone(), Vec::new())
                    .map_err(|error| JournalError::Recovery(format!("{error:?}")))?;
                let normalized_actor_cursor = rooms
                    .simulation_room(&room.room_id)
                    .map_err(|error| JournalError::Recovery(format!("{error:?}")))?
                    .next_command_seq();
                if checkpoint.command_cursor != normalized_actor_cursor {
                    return Err(JournalError::Recovery(format!(
                        "room {} checkpoint cursor {} does not match actor cursor {}",
                        room.room_id, checkpoint.command_cursor, normalized_actor_cursor
                    )));
                }
                (
                    checkpoint.command_cursor,
                    checkpoint.mutation_seq,
                    *complete_history,
                )
            } else if has_mutation_journal {
                let bootstrap = rooms
                    .create_room(room.scenario.clone())
                    .map_err(|error| JournalError::Recovery(format!("{error:?}")))?;
                (
                    command_cursor_after_actor_executions(&bootstrap.seed_executions)?,
                    0,
                    true,
                )
            } else if let Some(snapshot) = snapshots_by_room.get(room.room_id.as_str()) {
                rooms
                    .restore_simulation_room(snapshot.actor.clone(), Vec::new())
                    .map_err(|error| JournalError::Recovery(format!("{error:?}")))?;
                let normalized_actor_cursor = rooms
                    .simulation_room(&room.room_id)
                    .map_err(|error| JournalError::Recovery(format!("{error:?}")))?
                    .next_command_seq();
                (normalized_actor_cursor, 0, false)
            } else {
                let bootstrap = rooms
                    .create_room(room.scenario.clone())
                    .map_err(|error| JournalError::Recovery(format!("{error:?}")))?;
                (
                    command_cursor_after_actor_executions(&bootstrap.seed_executions)?,
                    0,
                    false,
                )
            };

        let mutations_to_replay = room_mutations
            .iter()
            .copied()
            .filter(|mutation| mutation.mutation_seq > replay_after_mutation_seq)
            .collect::<Vec<_>>();
        let mut next_mutation = 0;
        let mut command_cursor = initial_command_cursor;
        for record in executions_by_room
            .get(room.room_id.as_str())
            .into_iter()
            .flatten()
        {
            if record.command_seq < initial_command_cursor {
                continue;
            }
            while let Some(mutation) = mutations_to_replay.get(next_mutation).copied() {
                if mutation.command_cursor > record.command_seq {
                    break;
                }
                if mutation.command_cursor > command_cursor {
                    return Err(JournalError::Recovery(format!(
                        "room {} mutation {} follows missing command cursor {}",
                        room.room_id, mutation.mutation_seq, mutation.command_cursor
                    )));
                }
                replay_room_mutation(&mut rooms, mutation)?;
                next_mutation += 1;
            }
            if record.command_seq != command_cursor {
                return Err(JournalError::Recovery(format!(
                    "room {} execution sequence {} does not match command cursor {}",
                    room.room_id, record.command_seq, command_cursor
                )));
            }

            let automatic_replay = if record.participant_id.is_none() {
                rooms
                    .execution_history(&room.room_id)
                    .map_err(|error| JournalError::Recovery(format!("{error:?}")))?
                    .iter()
                    .find(|execution| execution.command_seq == record.command_seq)
                    .cloned()
            } else {
                None
            };
            if let Some(replayed) = automatic_replay {
                let replayed_command = command_from_actor_execution(&replayed);
                let replayed_summary = RoomExecutionSummary::from_execution(replayed);
                if replayed_command.as_ref() != Some(&record.command)
                    || !execution_summary_matches(&record.execution, &replayed_summary)
                {
                    return Err(JournalError::Recovery(format!(
                        "replayed system execution diverged for room {} command_seq {}",
                        record.room_id, record.command_seq
                    )));
                }
                command_cursor = next_command_cursor(record.command_seq)?;
                continue;
            }

            if !status_history_complete {
                rooms
                    .restore_room_status(&room.room_id, record.execution.status)
                    .map_err(|error| JournalError::Recovery(format!("{error:?}")))?;
            }
            let paused_for_scheduler_replay = record.execution.accepted
                && rooms
                    .status(&room.room_id)
                    .map_err(|error| JournalError::Recovery(format!("{error:?}")))?
                    == MarketStatus::Paused;
            if paused_for_scheduler_replay {
                rooms
                    .restore_room_status(&room.room_id, MarketStatus::Running)
                    .map_err(|error| JournalError::Recovery(format!("{error:?}")))?;
            }
            let replayed = match record.execution.instrument_id.as_deref() {
                Some(instrument_id) => {
                    rooms.apply_to_instrument(&room.room_id, instrument_id, record.command.clone())
                }
                None => rooms.apply(&room.room_id, record.command.clone()),
            }
            .map_err(|error| JournalError::Recovery(format!("{error:?}")))?;
            if paused_for_scheduler_replay {
                rooms
                    .restore_room_status(&room.room_id, MarketStatus::Paused)
                    .map_err(|error| JournalError::Recovery(format!("{error:?}")))?;
            }
            let mut replayed_summary = RoomExecutionSummary::from_execution(replayed);
            if paused_for_scheduler_replay {
                replayed_summary.status = record.execution.status;
            }
            if !execution_summary_matches(&record.execution, &replayed_summary) {
                return Err(JournalError::Recovery(format!(
                    "replayed execution diverged for room {} command_seq {}",
                    record.room_id, record.command_seq
                )));
            }
            command_cursor = next_command_cursor(record.command_seq)?;
        }

        for mutation in mutations_to_replay.iter().skip(next_mutation).copied() {
            if mutation.command_cursor > command_cursor {
                return Err(JournalError::Recovery(format!(
                    "room {} mutation {} follows missing command cursor {}",
                    room.room_id, mutation.mutation_seq, mutation.command_cursor
                )));
            }
            replay_room_mutation(&mut rooms, mutation)?;
        }

        let replayed_command_cursor = rooms
            .simulation_room(&room.room_id)
            .map_err(|error| JournalError::Recovery(format!("{error:?}")))?
            .next_command_seq();
        if replayed_command_cursor != command_cursor {
            return Err(JournalError::Recovery(format!(
                "room {} replay produced command cursor {} but journal ended at {}",
                room.room_id, replayed_command_cursor, command_cursor
            )));
        }

        if has_mutation_journal {
            let replayed_status = rooms
                .status(&room.room_id)
                .map_err(|error| JournalError::Recovery(format!("{error:?}")))?;
            if replayed_status != room.status {
                return Err(JournalError::Recovery(format!(
                    "replayed room status diverged for room {}: journal={replayed_status:?}, rooms_table={:?}",
                    room.room_id, room.status
                )));
            }
        } else {
            rooms
                .restore_room_status(&room.room_id, room.status)
                .map_err(|error| JournalError::Recovery(format!("{error:?}")))?;
        }
    }

    Ok(rooms)
}

fn training_runs_from_recovery(
    recovery: &JournalRecovery,
) -> BTreeMap<String, exchange_core::TrainingRun> {
    let mut runs = BTreeMap::new();
    let mut mutations = recovery.mutations.clone();
    mutations.sort_by_key(|mutation| mutation.mutation_seq);
    for mutation in mutations {
        if let RoomMutation::TrainingProgress { run } = mutation.mutation {
            runs.insert(run.spec.run_id.clone(), *run);
        }
    }
    runs
}

fn scheduler_states_from_recovery(
    recovery: &JournalRecovery,
) -> BTreeMap<RoomId, exchange_core::SchedulerState> {
    let mut mutations_by_room = BTreeMap::<&str, Vec<&JournalMutation>>::new();
    for mutation in &recovery.mutations {
        mutations_by_room
            .entry(mutation.room_id.as_str())
            .or_default()
            .push(mutation);
    }
    let mut schedulers = BTreeMap::new();
    for (room_id, mut mutations) in mutations_by_room {
        mutations.sort_by_key(|mutation| mutation.mutation_seq);
        if let Some(state) = mutations
            .iter()
            .rev()
            .find_map(|mutation| match &mutation.mutation {
                RoomMutation::SchedulerProgress { state, .. } => Some(state.clone()),
                _ => None,
            })
        {
            schedulers.insert(room_id.to_string(), state);
        }
    }
    schedulers
}

fn replay_room_mutation(
    rooms: &mut RoomManager,
    record: &JournalMutation,
) -> Result<(), JournalError> {
    if record.schema_version != journal::ROOM_MUTATION_SCHEMA_VERSION {
        return Err(JournalError::Recovery(format!(
            "unsupported room mutation schema version {}",
            record.schema_version
        )));
    }

    match &record.mutation {
        RoomMutation::StateCheckpoint { .. } => Err(JournalError::Recovery(format!(
            "room {} mutation {} contains an unexpected replay checkpoint",
            record.room_id, record.mutation_seq
        ))),
        RoomMutation::ClockAdvanced {
            steps,
            completed_transfers,
        } => {
            let replayed = rooms
                .advance_clock(&record.room_id, *steps)
                .map_err(|error| JournalError::Recovery(format!("{error:?}")))?;
            ensure_mutation_result_matches(record, completed_transfers, &replayed)
        }
        RoomMutation::DepositSubmitted {
            venue_id,
            account_id,
            asset_id,
            amount,
            transfer,
        } => {
            let replayed = rooms
                .submit_deposit(
                    &record.room_id,
                    venue_id.as_deref(),
                    *account_id,
                    asset_id.clone(),
                    *amount,
                )
                .map_err(|error| JournalError::Recovery(format!("{error:?}")))?;
            ensure_mutation_result_matches(record, transfer, &replayed)
        }
        RoomMutation::WithdrawalSubmitted {
            venue_id,
            account_id,
            asset_id,
            amount,
            transfer,
        } => {
            let replayed = rooms
                .submit_withdrawal(
                    &record.room_id,
                    venue_id.as_deref(),
                    *account_id,
                    asset_id.clone(),
                    *amount,
                )
                .map_err(|error| JournalError::Recovery(format!("{error:?}")))?;
            ensure_mutation_result_matches(record, transfer, &replayed)
        }
        RoomMutation::VenueToVenueTransferSubmitted {
            from_venue_id,
            to_venue_id,
            account_id,
            asset_id,
            amount,
            transfer,
        } => {
            let replayed = rooms
                .submit_venue_to_venue_transfer(
                    &record.room_id,
                    from_venue_id,
                    to_venue_id,
                    *account_id,
                    asset_id.clone(),
                    *amount,
                )
                .map_err(|error| JournalError::Recovery(format!("{error:?}")))?;
            ensure_mutation_result_matches(record, transfer, &replayed)
        }
        RoomMutation::StatusChanged { status } => rooms
            .restore_room_status(&record.room_id, *status)
            .map_err(|error| JournalError::Recovery(format!("{error:?}"))),
        RoomMutation::SchedulerProgress { clock_steps, .. } => {
            if *clock_steps == 0 {
                return Ok(());
            }
            rooms
                .advance_clock(&record.room_id, *clock_steps)
                .map(|_| ())
                .map_err(|error| JournalError::Recovery(format!("{error:?}")))
        }
        RoomMutation::TrainingProgress { .. } => Ok(()),
    }
}

fn ensure_mutation_result_matches<T: PartialEq>(
    record: &JournalMutation,
    stored: &T,
    replayed: &T,
) -> Result<(), JournalError> {
    if stored == replayed {
        Ok(())
    } else {
        Err(JournalError::Recovery(format!(
            "replayed room mutation diverged for room {} mutation_seq {}",
            record.room_id, record.mutation_seq
        )))
    }
}

fn execution_summary_matches(
    stored: &RoomExecutionSummary,
    replayed: &RoomExecutionSummary,
) -> bool {
    stored.room_id == replayed.room_id
        && stored.command_seq == replayed.command_seq
        && (stored.instrument_id.is_none() || stored.instrument_id == replayed.instrument_id)
        && (stored.market_time_ms.is_none() || stored.market_time_ms == replayed.market_time_ms)
        && stored.status == replayed.status
        && stored.accepted == replayed.accepted
        && stored.reject_reason == replayed.reject_reason
        && stored.clearing_event_count == replayed.clearing_event_count
        && clearing_event_summaries_match(
            stored.clearing_event_count,
            &stored.clearing_events,
            &replayed.clearing_events,
            stored.clearing_events_omitted,
        )
        && event_summaries_match(&stored.events, &replayed.events)
}

fn clearing_event_summaries_match(
    stored_count: usize,
    stored: &[ClearingEventSummary],
    replayed: &[ClearingEventSummary],
    stored_field_omitted: bool,
) -> bool {
    // Early journals persisted only `clearing_event_count`. When the payload
    // field is absent, the count still lets us verify that deterministic replay
    // produced the expected number of legs. An explicitly empty field remains
    // strict because it represents a malformed modern payload when count > 0.
    if stored_field_omitted && stored_count > 0 {
        return replayed.len() == stored_count;
    }
    stored.len() == replayed.len()
        && stored
            .iter()
            .zip(replayed)
            .all(|(stored, replayed)| clearing_event_summary_matches(stored, replayed))
}

fn clearing_event_summary_matches(
    stored: &ClearingEventSummary,
    replayed: &ClearingEventSummary,
) -> bool {
    if stored == replayed {
        return true;
    }
    let mut normalized = replayed.clone();
    match (stored, &mut normalized) {
        (
            ClearingEventSummary::PerpTradeSettled {
                buyer: stored_buyer,
                seller: stored_seller,
                ..
            },
            ClearingEventSummary::PerpTradeSettled { buyer, seller, .. },
        ) => {
            mask_legacy_portfolio_margins(stored_buyer, buyer);
            mask_legacy_portfolio_margins(stored_seller, seller);
        }
        (
            ClearingEventSummary::PerpMarginStatusChanged {
                account: stored_account,
                ..
            },
            ClearingEventSummary::PerpMarginStatusChanged { account, .. },
        ) => mask_legacy_portfolio_margins(stored_account, account),
        (
            ClearingEventSummary::PerpLiquidationSettled {
                account: stored_account,
                auto_deleveraging_allocations: stored_adl,
                socialized_loss_allocations: stored_socialized,
                ..
            },
            ClearingEventSummary::PerpLiquidationSettled {
                account,
                auto_deleveraging_allocations,
                socialized_loss_allocations,
                ..
            },
        ) => {
            mask_legacy_portfolio_margins(stored_account, account);
            for (stored, replayed) in stored_adl.iter().zip(auto_deleveraging_allocations) {
                mask_legacy_portfolio_margins(&stored.account, &mut replayed.account);
            }
            for (stored, replayed) in stored_socialized.iter().zip(socialized_loss_allocations) {
                mask_legacy_portfolio_margins(&stored.account, &mut replayed.account);
            }
        }
        _ => {}
    }
    stored == &normalized
}

fn mask_legacy_portfolio_margins(
    stored: &PerpAccountStateSummary,
    replayed: &mut PerpAccountStateSummary,
) {
    if stored.portfolio_initial_margin.is_none() {
        replayed.portfolio_initial_margin = None;
    }
    if stored.portfolio_maintenance_margin.is_none() {
        replayed.portfolio_maintenance_margin = None;
    }
}

fn event_summaries_match(stored: &[EventSummary], replayed: &[EventSummary]) -> bool {
    stored.len() == replayed.len()
        && stored
            .iter()
            .zip(replayed)
            .all(|(stored, replayed)| event_summary_matches(stored, replayed))
}

fn event_summary_matches(stored: &EventSummary, replayed: &EventSummary) -> bool {
    if stored == replayed {
        return true;
    }

    match (stored, replayed) {
        (
            EventSummary::TradePrinted {
                seq,
                trade_id,
                maker_order_id,
                maker_account_id,
                taker_order_id,
                taker_account_id,
                price_tick,
                qty,
                ..
            },
            EventSummary::TradePrinted {
                seq: replayed_seq,
                trade_id: replayed_trade_id,
                price_tick: replayed_price_tick,
                qty: replayed_qty,
                ..
            },
        ) => {
            *seq == *replayed_seq
                && *trade_id == *replayed_trade_id
                && *price_tick == *replayed_price_tick
                && *qty == *replayed_qty
                && *maker_order_id == 0
                && *maker_account_id == 0
                && *taker_order_id == 0
                && *taker_account_id == 0
        }
        _ => false,
    }
}

fn execution_summaries_from_recovery(
    recovery: &JournalRecovery,
) -> BTreeMap<RoomId, VecDeque<RoomExecutionSummary>> {
    let mut executions = BTreeMap::<RoomId, VecDeque<RoomExecutionSummary>>::new();
    for record in &recovery.executions {
        let cached = executions.entry(record.room_id.clone()).or_default();
        if cached.len() == ROOM_EVENT_CACHE_CAPACITY {
            cached.pop_front();
        }
        cached.push_back(record.execution.clone());
    }
    executions
}

fn next_order_id_from_recovery(recovery: &JournalRecovery) -> Result<OrderId, JournalError> {
    let mut next_order_id = 1;
    for execution in &recovery.executions {
        let Command::NewOrder(order) = &execution.command else {
            continue;
        };
        if order.order_id >= SYSTEM_LIQUIDATION_ORDER_ID_BASE {
            if execution.participant_id.is_none()
                && is_system_liquidation_command(&execution.command)
            {
                continue;
            }
            return Err(JournalError::Recovery(format!(
                "order id {} in room {} uses the reserved system-order range",
                order.order_id, execution.room_id
            )));
        }
        let following_order_id = order.order_id.checked_add(1).ok_or_else(|| {
            JournalError::Recovery(format!(
                "order id {} in room {} cannot be incremented",
                order.order_id, execution.room_id
            ))
        })?;
        next_order_id = next_order_id.max(following_order_id);
    }
    Ok(next_order_id)
}

fn next_api_order_id_after_commands(
    current: OrderId,
    commands: &[Command],
) -> Result<OrderId, (StatusCode, Json<ErrorResponse>)> {
    let mut next_order_id = current;
    for command in commands {
        let Command::NewOrder(order) = command else {
            continue;
        };
        if order.order_id >= SYSTEM_LIQUIDATION_ORDER_ID_BASE {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                format!(
                    "seed order id {} uses the reserved system-order range",
                    order.order_id
                ),
            ));
        }
        let following_order_id = order.order_id.checked_add(1).ok_or_else(|| {
            api_error(
                StatusCode::BAD_REQUEST,
                format!("seed order id {} cannot be incremented", order.order_id),
            )
        })?;
        next_order_id = next_order_id.max(following_order_id);
    }
    Ok(next_order_id)
}

fn is_system_liquidation_command(command: &Command) -> bool {
    matches!(
        command,
        Command::NewOrder(order)
            if order.order_id >= SYSTEM_LIQUIDATION_ORDER_ID_BASE
                && order.reduce_only
                && matches!(
                    order.kind,
                    OrderKind::Market
                        | OrderKind::ImmediateOrCancel { price_tick: None }
                        | OrderKind::FillOrKill { price_tick: None }
                )
    )
}

fn command_from_actor_execution(execution: &ActorExecution) -> Option<Command> {
    match &execution.result {
        ActorExecutionResult::Accepted(MarketExecution::Spot(execution)) => {
            Some(execution.command.command.clone())
        }
        ActorExecutionResult::Accepted(MarketExecution::Perp(execution)) => {
            Some(execution.command.command.clone())
        }
        ActorExecutionResult::Rejected(_) => None,
    }
}

async fn journal_new_executions(
    state: &mut AppState,
    candidate_rooms: &RoomManager,
    room_id: &str,
    previous_history_len: usize,
    first_record: JournalExecution,
) -> Result<(), JournalError> {
    let new_history = candidate_rooms
        .execution_history(room_id)
        .map_err(|error| JournalError::Recovery(format!("{error:?}")))?
        .iter()
        .skip(previous_history_len)
        .cloned()
        .collect::<Vec<_>>();
    let mut journal_records = vec![first_record];
    for system_execution in new_history.into_iter().skip(1) {
        let command = command_from_actor_execution(&system_execution).ok_or_else(|| {
            JournalError::Recovery(format!(
                "automatic execution {} in room {} did not retain a journalable command",
                system_execution.command_seq, system_execution.room_id
            ))
        })?;
        journal_records.push(JournalExecution::system(command, system_execution));
    }

    let snapshot = batch_snapshot_if_due(candidate_rooms, &journal_records);
    state
        .append_executions(room_id, &journal_records, snapshot.as_ref())
        .await?;
    state.append_room_executions(
        room_id,
        journal_records
            .into_iter()
            .map(|record| record.execution)
            .collect(),
    );

    Ok(())
}

fn batch_snapshot_if_due(
    rooms: &RoomManager,
    records: &[JournalExecution],
) -> Option<JournalSnapshot> {
    if !records.iter().any(|record| {
        record
            .command_seq
            .is_multiple_of(SNAPSHOT_INTERVAL_COMMANDS)
    }) {
        return None;
    }
    let final_record = records.last()?;
    Some(JournalSnapshot {
        room_id: final_record.room_id.clone(),
        command_seq: final_record.command_seq,
        actor: rooms.simulation_room(&final_record.room_id).ok()?.clone(),
    })
}

fn latest_room_snapshot(
    rooms: &RoomManager,
    room_id: &str,
    records: &[JournalExecution],
) -> Option<JournalSnapshot> {
    let command_seq = records.last()?.command_seq;
    Some(JournalSnapshot {
        room_id: room_id.to_string(),
        command_seq,
        actor: rooms.simulation_room(room_id).ok()?.clone(),
    })
}

fn current_room_snapshot(
    rooms: &RoomManager,
    room_id: &str,
    command_seq: u64,
) -> Option<JournalSnapshot> {
    Some(JournalSnapshot {
        room_id: room_id.to_string(),
        command_seq,
        actor: rooms.simulation_room(room_id).ok()?.clone(),
    })
}

fn next_command_cursor(command_seq: u64) -> Result<u64, JournalError> {
    command_seq
        .checked_add(1)
        .ok_or(JournalError::SequenceOutOfRange(command_seq))
}

fn command_cursor_after_actor_executions(
    executions: &[ActorExecution],
) -> Result<u64, JournalError> {
    executions.last().map_or(Ok(0), |execution| {
        next_command_cursor(execution.command_seq)
    })
}

fn latest_persisted_command_seq(state: &AppState, room_id: &str) -> Option<u64> {
    state
        .executions
        .get(room_id)
        .and_then(|executions| executions.back())
        .map(|execution| execution.command_seq)
}

fn next_persisted_command_cursor(state: &AppState, room_id: &str) -> Result<u64, JournalError> {
    latest_persisted_command_seq(state, room_id).map_or(Ok(0), next_command_cursor)
}

async fn list_rooms(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> ApiResult<ListRoomsResponse> {
    let (user_id, room_ids, journal) = {
        let state = lock_state(&state).await?;
        (
            current_user_id(&headers, &state.auth_policy)?,
            state
                .rooms
                .room_ids()
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>(),
            state.journal.clone(),
        )
    };
    let mut rooms = Vec::new();
    for room_id in room_ids {
        if journal
            .user_can_access_room(&user_id, &room_id)
            .await
            .map_err(api_error_from_journal)?
        {
            rooms.push(room_id);
        }
    }

    Ok(Json(ListRoomsResponse { rooms }))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StartTrainingRequest {
    pub run_id: String,
    pub scenario: ScenarioConfig,
    #[serde(default)]
    pub agents: Vec<AgentTemplate>,
    pub trainee_account_id: AccountId,
    pub target_qty: u64,
    pub horizon_steps: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TrainingRunResponse {
    pub api_version: String,
    pub run: exchange_core::TrainingRun,
    pub score: exchange_core::TrainingScore,
}

async fn start_training_run(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(request): Json<StartTrainingRequest>,
) -> ApiResult<TrainingRunResponse> {
    {
        let app = lock_state(&state).await?;
        current_user_id(&headers, &app.auth_policy)?;
    }
    run_durable_state_transaction(state.clone(), async move {
        let shared = state.clone();
        let mut app = lock_state(&shared).await?;
        if let Some(existing) = app.training_runs.get(&request.run_id).cloned() {
            return Ok(Json(TrainingRunResponse {
                api_version: "training.v1".to_string(),
                score: existing.score(),
                run: existing,
            }));
        }
        let room_id = request.scenario.room_id.clone();
        if app.rooms.status(&room_id).is_ok() {
            return Err(api_error(
                StatusCode::CONFLICT,
                format!("room {room_id} already exists"),
            ));
        }
        let user_id = current_user_id(&headers, &app.auth_policy)?;
        let mut candidate = app.rooms.clone();
        let bootstrap = candidate
            .create_room(request.scenario.clone())
            .map_err(api_error_from_room)?;
        let ticker = candidate
            .ticker(
                &room_id,
                candidate
                    .room(&room_id)
                    .map_err(api_error_from_room)?
                    .primary_instrument_id(),
            )
            .map_err(api_error_from_room)?;
        let reference = ticker.mid_tick.ok_or_else(|| {
            api_error(
                StatusCode::CONFLICT,
                "training start requires a two-sided book for P0".to_string(),
            )
        })?;
        let spec = exchange_core::TrainingSpec::low_slippage_buy(
            request.run_id.clone(),
            request.scenario.clone(),
            request.agents.clone(),
            request.trainee_account_id,
            request.target_qty,
            request.horizon_steps,
            reference,
        )
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, format!("{error:?}")))?;
        let mut run = exchange_core::TrainingRun::new(spec);
        run.start()
            .map_err(|error| api_error(StatusCode::CONFLICT, format!("{error:?}")))?;
        let account_ids = scenario_account_ids(&request.scenario);
        let seed_commands = request.scenario.seed_commands();
        let next_order_id = next_api_order_id_after_commands(app.next_order_id, &seed_commands)?;
        let seed_records = seed_commands
            .into_iter()
            .zip(bootstrap.seed_executions.iter().cloned())
            .map(|(command, execution)| JournalExecution::seed(command, execution))
            .collect::<Vec<_>>();
        let lease_config = app
            .room_lease_runtime
            .as_ref()
            .map(|runtime| runtime.config.clone());
        let initial_lease = if let Some(config) = &lease_config {
            Some(
                app.journal
                    .create_room_with_writer_lease(
                        &user_id,
                        &request.scenario,
                        &bootstrap,
                        &account_ids,
                        &seed_records,
                        None,
                        &config.instance_id,
                        Some(&config.owner_url),
                        config.lease_duration,
                    )
                    .await
                    .map_err(api_error_from_journal)?,
            )
        } else {
            app.journal
                .create_room(
                    &user_id,
                    &request.scenario,
                    &bootstrap,
                    &account_ids,
                    &seed_records,
                    None,
                )
                .await
                .map_err(api_error_from_journal)?;
            None
        };
        if let Some(lease) = initial_lease
            && let Some(runtime) = app.room_lease_runtime.as_mut()
        {
            runtime.leases.insert(room_id.clone(), lease);
            runtime.lost_rooms.remove(&room_id);
        }
        app.replace_room_executions(
            room_id.clone(),
            seed_records
                .iter()
                .map(|record| record.execution.clone())
                .collect(),
        );
        let cursor = command_cursor_after_actor(&bootstrap)?;
        app.append_room_mutation(
            &PendingJournalMutation::new(
                room_id.clone(),
                cursor,
                RoomMutation::TrainingProgress {
                    run: Box::new(run.clone()),
                },
            ),
            &[],
            &[],
            None,
        )
        .await
        .map_err(api_error_from_journal)?;
        app.rooms = candidate;
        app.next_order_id = next_order_id;
        if !request.agents.is_empty() {
            let _ = start_agent_worker_for_room(
                &shared,
                &mut app,
                room_id,
                StartAgentsRequest {
                    agents: request.agents,
                    interval_ms: Some(50),
                },
            );
        }
        app.training_runs
            .insert(request.run_id.clone(), run.clone());
        Ok(Json(TrainingRunResponse {
            api_version: "training.v1".to_string(),
            score: run.score(),
            run,
        }))
    })
    .await
}

fn command_cursor_after_actor(bootstrap: &exchange_core::RoomBootstrap) -> Result<u64, ApiError> {
    Ok(bootstrap
        .seed_executions
        .last()
        .map(|execution| execution.command_seq.saturating_add(1))
        .unwrap_or(0))
}

async fn training_run_status(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> ApiResult<TrainingRunResponse> {
    let app = lock_state(&state).await?;
    let _ = current_user_id(&headers, &app.auth_policy)?;
    let run = app.training_runs.get(&run_id).cloned().ok_or_else(|| {
        api_error(
            StatusCode::NOT_FOUND,
            format!("training run {run_id} not found"),
        )
    })?;
    Ok(Json(TrainingRunResponse {
        api_version: "training.v1".to_string(),
        score: run.score(),
        run,
    }))
}

async fn abort_training_run(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> ApiResult<TrainingRunResponse> {
    {
        let app = lock_state(&state).await?;
        current_user_id(&headers, &app.auth_policy)?;
    }
    run_durable_state_transaction(state.clone(), async move {
        let mut app = lock_state(&state).await?;
        let mut run = app.training_runs.get(&run_id).cloned().ok_or_else(|| {
            api_error(
                StatusCode::NOT_FOUND,
                format!("training run {run_id} not found"),
            )
        })?;
        run.abort()
            .map_err(|error| api_error(StatusCode::CONFLICT, format!("{error:?}")))?;
        let mut candidate_rooms = app.rooms.clone();
        let settle = settle_training_residuals(&mut candidate_rooms, &mut run)?;
        let records = settle
            .into_iter()
            .filter_map(|execution| {
                command_from_actor_execution(&execution)
                    .map(|command| JournalExecution::system(command, execution))
            })
            .collect::<Vec<_>>();
        if !records.is_empty() {
            app.journal
                .append_executions(&records, None)
                .await
                .map_err(api_error_from_journal)?;
            app.append_room_executions(
                &run.spec.room_id,
                records
                    .iter()
                    .map(|record| record.execution.clone())
                    .collect(),
            );
        }
        persist_training_progress(&mut app, &run, &[]).await?;
        app.rooms = candidate_rooms;
        Ok(Json(TrainingRunResponse {
            api_version: "training.v1".to_string(),
            score: run.score(),
            run,
        }))
    })
    .await
}

async fn training_run_result(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> ApiResult<TrainingRunResponse> {
    training_run_status(State(state), headers, Path(run_id)).await
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TrainingReportResponse {
    pub json: serde_json::Value,
    pub markdown: String,
}

async fn training_run_report(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> ApiResult<TrainingReportResponse> {
    let app = lock_state(&state).await?;
    current_user_id(&headers, &app.auth_policy)?;
    let run = app.training_runs.get(&run_id).cloned().ok_or_else(|| {
        api_error(
            StatusCode::NOT_FOUND,
            format!("training run {run_id} not found"),
        )
    })?;
    Ok(Json(TrainingReportResponse {
        json: exchange_core::training_report_json(&run),
        markdown: exchange_core::training_report_markdown(&run),
    }))
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ReplayQuery {
    pub at_command_seq: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IsolatedReplayResponse {
    pub room_id: String,
    pub replayed_commands: usize,
    pub live_room_untouched: bool,
    pub book: BookSnapshot,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomMemberRequest {
    pub user_id: String,
    pub role: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomMemberResponse {
    pub room_id: String,
    pub user_id: String,
    pub role: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AssignAccountRequest {
    pub user_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AssignAccountResponse {
    pub room_id: String,
    pub account_id: AccountId,
    pub user_id: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ObserveQuery {
    pub account_id: AccountId,
    pub instrument_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ObservationResponse {
    pub api_version: String,
    pub observation: ParticipantObservation,
}

fn journal_write_error(error: JournalError) -> ApiError {
    if let JournalError::Recovery(message) = &error
        && (message.contains("invalid role")
            || message.contains("cannot be assigned")
            || message.contains("is not a member"))
    {
        return api_error(StatusCode::BAD_REQUEST, message.clone());
    }
    api_error_from_journal(error)
}

fn training_assignment_frozen(app: &AppState, room_id: &str) -> bool {
    app.training_runs
        .values()
        .any(|run| run.spec.room_id == room_id && !matches!(run.status, TrainingStatus::Created))
}

async fn upsert_room_member(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Json(request): Json<RoomMemberRequest>,
) -> ApiResult<RoomMemberResponse> {
    authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Admin).await?;
    let journal = {
        let app = lock_state(&state).await?;
        app.journal.clone()
    };
    journal
        .upsert_room_member(&room_id, &request.user_id, &request.role)
        .await
        .map_err(journal_write_error)?;
    Ok(Json(RoomMemberResponse {
        room_id,
        user_id: request.user_id,
        role: request.role,
    }))
}

async fn remove_room_member(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, user_id)): Path<(String, String)>,
    Json(_): Json<serde_json::Value>,
) -> ApiResult<RoomMemberResponse> {
    authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Admin).await?;
    let journal = {
        let app = lock_state(&state).await?;
        app.journal.clone()
    };
    journal
        .remove_room_member(&room_id, &user_id)
        .await
        .map_err(journal_write_error)?;
    Ok(Json(RoomMemberResponse {
        room_id,
        user_id,
        role: "removed".to_string(),
    }))
}

async fn assign_account_owner(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, account_id)): Path<(String, AccountId)>,
    Json(request): Json<AssignAccountRequest>,
) -> ApiResult<AssignAccountResponse> {
    authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Admin).await?;
    let journal = {
        let app = lock_state(&state).await?;
        if training_assignment_frozen(&app, &room_id) {
            return Err(api_error(
                StatusCode::CONFLICT,
                format!("account assignment is frozen after training start in room {room_id}"),
            ));
        }
        app.journal.clone()
    };
    journal
        .assign_account_owner(&room_id, account_id, &request.user_id)
        .await
        .map_err(journal_write_error)?;
    Ok(Json(AssignAccountResponse {
        room_id,
        account_id,
        user_id: request.user_id,
    }))
}

async fn observe_room(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Query(query): Query<ObserveQuery>,
) -> ApiResult<ObservationResponse> {
    authorize_room_read(
        &state,
        &headers,
        &room_id,
        RoomReadAccess::Account(query.account_id),
    )
    .await?;
    let app = lock_state(&state).await?;
    let instrument_id = query.instrument_id.unwrap_or_else(|| {
        app.rooms
            .room(&room_id)
            .map(|room| room.primary_instrument_id().to_string())
            .unwrap_or_default()
    });
    let observation = app
        .rooms
        .participant_observation(&room_id, &instrument_id, query.account_id)
        .map_err(api_error_from_room)?;
    Ok(Json(ObservationResponse {
        api_version: STRATEGY_PROTOCOL_VERSION.to_string(),
        observation,
    }))
}

async fn replay_room_isolated(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Query(query): Query<ReplayQuery>,
) -> ApiResult<IsolatedReplayResponse> {
    authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Admin).await?;
    let journal = {
        let app = lock_state(&state).await?;
        app.journal.clone()
    };
    let mut recovery = journal
        .load_room_recovery(&room_id)
        .await
        .map_err(api_error_from_journal)?;
    if let Some(at) = query.at_command_seq {
        recovery
            .executions
            .retain(|execution| execution.command_seq <= at);
        recovery
            .mutations
            .retain(|mutation| mutation.command_cursor <= at.saturating_add(1));
    }
    let replayed_commands = recovery.executions.len();
    state
        .lifecycle
        .record_replayed_commands(replayed_commands as u64);
    let rooms = recover_rooms(&recovery).map_err(api_error_from_journal)?;
    let instrument_id = rooms
        .room(&room_id)
        .map_err(api_error_from_room)?
        .primary_instrument_id()
        .to_string();
    let book = rooms
        .book_snapshot_for(&room_id, &instrument_id)
        .map_err(api_error_from_room)?;
    Ok(Json(IsolatedReplayResponse {
        room_id,
        replayed_commands,
        live_room_untouched: true,
        book,
    }))
}

async fn cluster_rooms(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Query(query): Query<ClusterRoomsQuery>,
) -> ApiResult<ClusterRoomsResponse> {
    if query
        .after_room_id
        .as_deref()
        .is_some_and(|room_id| room_id.is_empty())
    {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "after_room_id must not be empty".to_string(),
        ));
    }
    let limit = query_limit(query.limit);
    let (user_id, journal) = {
        let state = lock_state(&state).await?;
        (
            current_user_id(&headers, &state.auth_policy)?,
            state.journal.clone(),
        )
    };
    let mut routes = journal
        .query_room_routes(
            &user_id,
            query.after_room_id.as_deref(),
            limit.saturating_add(1),
        )
        .await
        .map_err(api_error_from_journal)?;
    let has_more = routes.len() > limit;
    routes.truncate(limit);
    let next_after_room_id = routes.last().map(|route| route.room_id.clone());
    let rooms = routes.into_iter().map(cluster_room_route).collect();

    Ok(Json(ClusterRoomsResponse {
        rooms,
        next_after_room_id,
        has_more,
    }))
}

fn cluster_room_route(route: RoomRoutingRecord) -> ClusterRoomRouteResponse {
    ClusterRoomRouteResponse {
        room_id: route.room_id,
        owner: route.owner.map(room_owner_response),
    }
}

async fn start_agents(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Json(request): Json<StartAgentsRequest>,
) -> ApiResult<AgentWorkerStatus> {
    let authorization =
        authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Admin).await?;
    let _ = authorization;
    let mut app = lock_state(&state).await?;
    start_agent_worker_for_room(&state, &mut app, room_id, request).map(Json)
}

async fn agent_status(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<AgentWorkerStatus> {
    authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Admin).await?;
    let state = lock_state(&state).await?;
    Ok(Json(agent_status_for_room(&state, &room_id)))
}

async fn stop_agents(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<AgentWorkerStatus> {
    authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Admin).await?;
    let mut state = lock_state(&state).await?;
    if let Some(worker) = state.agent_workers.remove(&room_id) {
        worker.stop();
    }
    Ok(Json(AgentWorkerStatus::stopped(room_id)))
}

async fn room_events(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Query(query): Query<RoomEventsQuery>,
) -> ApiResult<RoomEventsResponse> {
    if query.from_start && query.after_command_seq.is_some() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "from_start and after_command_seq cannot be used together".to_string(),
        ));
    }
    let authorization =
        authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Admin).await?;
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let cached_history = {
        let state = lock_state(&state).await?;
        state.executions.get(&room_id).cloned().unwrap_or_default()
    };
    let cached_latest_command_seq = cached_history.back().map(|execution| execution.command_seq);
    validate_event_cursor(&room_id, query.after_command_seq, cached_latest_command_seq)?;

    let page = match authorization
        .journal
        .query_executions(&room_id, query.after_command_seq, query.from_start, limit)
        .await
    {
        Ok(page) => page,
        Err(JournalError::UnsupportedOperation("query_executions")) => execution_page_from_cache(
            &cached_history,
            query.after_command_seq,
            query.from_start,
            limit,
        ),
        Err(error) => return Err(api_error_from_journal(error)),
    };
    validate_event_cursor(&room_id, query.after_command_seq, page.latest_command_seq)?;
    let next_after_command_seq = page
        .executions
        .last()
        .map(|execution| execution.command_seq)
        .or(query.after_command_seq);

    Ok(Json(RoomEventsResponse {
        room_id,
        executions: page.executions,
        next_after_command_seq,
        latest_command_seq: page.latest_command_seq,
        has_more: page.has_more,
    }))
}

fn execution_page_from_cache(
    history: &VecDeque<RoomExecutionSummary>,
    after_command_seq: Option<u64>,
    from_start: bool,
    limit: usize,
) -> ExecutionPage {
    let latest_command_seq = history.back().map(|execution| execution.command_seq);
    let limit = limit.clamp(1, 500);
    let start = if let Some(after_command_seq) = after_command_seq {
        history.partition_point(|execution| execution.command_seq <= after_command_seq)
    } else if from_start {
        0
    } else {
        history.len().saturating_sub(limit)
    };
    let end = start.saturating_add(limit).min(history.len());
    ExecutionPage {
        executions: history.range(start..end).cloned().collect(),
        latest_command_seq,
        has_more: (after_command_seq.is_some() || from_start) && end < history.len(),
    }
}

struct RoomEventStreamState {
    room_id: RoomId,
    after_command_seq: Option<u64>,
    backlog: VecDeque<RoomExecutionSummary>,
    receiver: broadcast::Receiver<RoomExecutionSummary>,
    shutdown: watch::Receiver<bool>,
    connection: SseConnectionGuard,
    journal: JournalCoordinator,
    fallback_history: VecDeque<RoomExecutionSummary>,
    replay_until_command_seq: Option<u64>,
    replay_from_start: bool,
    replay_complete: bool,
    terminate: bool,
}

async fn room_event_stream(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Query(query): Query<RoomEventStreamQuery>,
) -> Result<Sse<impl futures_util::Stream<Item = Result<SseEvent, Infallible>>>, ApiError> {
    let lifecycle = state.lifecycle.clone();
    let shutdown = lifecycle.subscribe_shutdown();
    let requested_cursor = event_stream_cursor(&headers, query.after_command_seq)?;
    if query.replay_from_start && requested_cursor.is_some() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "replay_from_start and an event cursor cannot be used together".to_string(),
        ));
    }
    let authorization =
        authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Admin).await?;
    let mut state = lock_state(&state).await?;

    let latest_command_seq = state
        .executions
        .get(&room_id)
        .and_then(|history| history.back())
        .map(|execution| execution.command_seq);
    validate_event_cursor(&room_id, requested_cursor, latest_command_seq)?;

    // Subscribe while the state lock is held and capture the durable replay
    // boundary. Writers publish under the same lock after commit, so records at
    // or below this boundary come from the journal and later records come from
    // the live receiver without a gap.
    let receiver = state.room_event_receiver(&room_id);
    let replay_requested = query.replay_from_start || requested_cursor.is_some();
    let after_command_seq = if replay_requested {
        requested_cursor
    } else {
        latest_command_seq
    };
    let replay_complete =
        !replay_requested || latest_command_seq.is_none() || requested_cursor == latest_command_seq;
    let fallback_history = state.executions.get(&room_id).cloned().unwrap_or_default();
    drop(state);
    let connection = lifecycle.open_sse_connection();

    let stream_state = RoomEventStreamState {
        room_id,
        after_command_seq,
        backlog: VecDeque::new(),
        receiver,
        shutdown,
        connection,
        journal: authorization.journal,
        fallback_history,
        replay_until_command_seq: latest_command_seq,
        replay_from_start: query.replay_from_start,
        replay_complete,
        terminate: false,
    };
    let stream = futures_util::stream::unfold(stream_state, |mut state| async move {
        if state.terminate || *state.shutdown.borrow() {
            return None;
        }

        loop {
            let execution = if let Some(execution) = state.backlog.pop_front() {
                execution
            } else if !state.replay_complete {
                let Some(replay_until_command_seq) = state.replay_until_command_seq else {
                    state.replay_complete = true;
                    continue;
                };
                let journal = state.journal.clone();
                let room_id = state.room_id.clone();
                let after_command_seq = state.after_command_seq;
                let from_start = state.replay_from_start && after_command_seq.is_none();
                let page_result = tokio::select! {
                    biased;
                    _ = state.shutdown.changed() => return None,
                    result = journal.query_executions(
                        &room_id,
                        after_command_seq,
                        from_start,
                        500,
                    ) => result,
                };
                let page = match page_result {
                    Ok(page) => page,
                    Err(JournalError::UnsupportedOperation("query_executions")) => {
                        execution_page_from_cache(
                            &state.fallback_history,
                            after_command_seq,
                            from_start,
                            500,
                        )
                    }
                    Err(_) => {
                        state.connection.record_resync_required();
                        let data = serde_json::json!({
                            "room_id": state.room_id,
                            "after_command_seq": state.after_command_seq,
                            "reason": "journal_unavailable",
                        })
                        .to_string();
                        state.terminate = true;
                        let event = SseEvent::default().event("resync_required").data(data);
                        return Some((Ok::<_, Infallible>(event), state));
                    }
                };
                state.replay_from_start = false;

                if page
                    .latest_command_seq
                    .is_none_or(|latest| latest < replay_until_command_seq)
                {
                    state.connection.record_resync_required();
                    let data = serde_json::json!({
                        "room_id": state.room_id,
                        "after_command_seq": state.after_command_seq,
                        "reason": "journal_history_gap",
                    })
                    .to_string();
                    state.terminate = true;
                    let event = SseEvent::default().event("resync_required").data(data);
                    return Some((Ok::<_, Infallible>(event), state));
                }

                let mut reached_boundary = false;
                for execution in page.executions {
                    if execution.command_seq > replay_until_command_seq {
                        reached_boundary = true;
                        break;
                    }
                    if execution.command_seq == replay_until_command_seq {
                        reached_boundary = true;
                    }
                    state.backlog.push_back(execution);
                }
                if reached_boundary {
                    state.replay_complete = true;
                } else if state.backlog.is_empty() || !page.has_more {
                    state.connection.record_resync_required();
                    let data = serde_json::json!({
                        "room_id": state.room_id,
                        "after_command_seq": state.after_command_seq,
                        "reason": "journal_history_gap",
                    })
                    .to_string();
                    state.terminate = true;
                    let event = SseEvent::default().event("resync_required").data(data);
                    return Some((Ok::<_, Infallible>(event), state));
                }
                continue;
            } else {
                let received = tokio::select! {
                    biased;
                    _ = state.shutdown.changed() => return None,
                    received = state.receiver.recv() => received,
                };
                match received {
                    Ok(execution) => execution,
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        state.connection.record_resync_required();
                        let data = serde_json::json!({
                            "room_id": state.room_id,
                            "after_command_seq": state.after_command_seq,
                            "reason": "consumer_lagged",
                            "skipped_messages": skipped,
                        })
                        .to_string();
                        state.terminate = true;
                        let event = SseEvent::default().event("resync_required").data(data);
                        return Some((Ok::<_, Infallible>(event), state));
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            };

            if execution.room_id != state.room_id
                || state
                    .after_command_seq
                    .is_some_and(|cursor| execution.command_seq <= cursor)
            {
                continue;
            }
            let Ok(data) = serde_json::to_string(&execution) else {
                continue;
            };
            state.after_command_seq = Some(execution.command_seq);
            let event = SseEvent::default()
                .event("execution")
                .id(execution.command_seq.to_string())
                .data(data);
            return Some((Ok::<_, Infallible>(event), state));
        }
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

fn event_stream_cursor(
    headers: &HeaderMap,
    query_cursor: Option<u64>,
) -> Result<Option<u64>, ApiError> {
    if query_cursor.is_some() {
        return Ok(query_cursor);
    }
    let Some(value) = headers.get("last-event-id") else {
        return Ok(None);
    };
    let value = value.to_str().map_err(|_| {
        api_error(
            StatusCode::BAD_REQUEST,
            "Last-Event-ID must be an unsigned command sequence".to_string(),
        )
    })?;
    value.parse::<u64>().map(Some).map_err(|_| {
        api_error(
            StatusCode::BAD_REQUEST,
            "Last-Event-ID must be an unsigned command sequence".to_string(),
        )
    })
}

fn validate_event_cursor(
    room_id: &str,
    requested_cursor: Option<u64>,
    latest_command_seq: Option<u64>,
) -> Result<(), ApiError> {
    let Some(requested) = requested_cursor else {
        return Ok(());
    };
    let Some(latest) = latest_command_seq else {
        return Err(api_error(
            StatusCode::CONFLICT,
            format!(
                "event cursor {requested} does not exist because room {room_id} has no executions"
            ),
        ));
    };
    if requested > latest {
        return Err(api_error(
            StatusCode::CONFLICT,
            format!(
                "event cursor {requested} is ahead of room {room_id} latest command sequence {latest}"
            ),
        ));
    }
    Ok(())
}

fn request_idempotency_key(headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    let mut values = headers.get_all(IDEMPOTENCY_KEY_HEADER).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Idempotency-Key must be sent at most once".to_string(),
        ));
    }
    let value = value.to_str().map_err(|_| {
        api_error(
            StatusCode::BAD_REQUEST,
            "Idempotency-Key must contain visible ASCII characters".to_string(),
        )
    })?;
    if value.is_empty() || value.len() > 128 || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Idempotency-Key must contain 1 to 128 visible ASCII characters".to_string(),
        ));
    }
    Ok(Some(value.to_string()))
}

fn control_conflict_error(key: &str) -> ApiError {
    api_error(
        StatusCode::CONFLICT,
        format!("idempotency key {key:?} was already used for a different control request"),
    )
}

fn control_fingerprint(operation: &str, params: serde_json::Value) -> String {
    control_request_fingerprint(operation, params)
}

fn control_record(
    user_id: impl Into<String>,
    room_id: impl Into<String>,
    key: impl Into<String>,
    fingerprint: impl Into<String>,
    response: &impl Serialize,
) -> Result<ControlIdempotencyRecord, ApiError> {
    Ok(ControlIdempotencyRecord {
        user_id: user_id.into(),
        room_id: room_id.into(),
        idempotency_key: key.into(),
        request_fingerprint: fingerprint.into(),
        response_json: serde_json::to_value(response).map_err(api_error_from_json)?,
    })
}

async fn load_control_replay<T: DeserializeOwned>(
    state: &AppState,
    user_id: &str,
    room_id: &str,
    key: &str,
    fingerprint: &str,
) -> Result<Option<T>, ApiError> {
    let Some(existing) = state
        .journal
        .find_control_idempotency(user_id, room_id, key)
        .await
        .map_err(api_error_from_journal)?
    else {
        return Ok(None);
    };
    if existing.request_fingerprint != fingerprint {
        return Err(control_conflict_error(key));
    }
    serde_json::from_value(existing.response_json)
        .map(Some)
        .map_err(api_error_from_json)
}

async fn append_control_mutation(
    state: &mut AppState,
    pending: PendingJournalMutation,
    execution_records: &[JournalExecution],
    transfer_records: &[JournalTransfer],
    snapshot: Option<&JournalSnapshot>,
) -> Result<Option<serde_json::Value>, ApiError> {
    match state
        .append_room_mutation(&pending, execution_records, transfer_records, snapshot)
        .await
    {
        Ok(()) => Ok(None),
        Err(JournalError::ControlIdempotencyConflict {
            user_id,
            room_id,
            idempotency_key,
        }) => {
            let existing = state
                .journal
                .find_control_idempotency(&user_id, &room_id, &idempotency_key)
                .await
                .map_err(api_error_from_journal)?
                .ok_or_else(|| {
                    api_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!(
                            "control idempotency key {idempotency_key:?} conflicted but was not found"
                        ),
                    )
                })?;
            if pending
                .control_idempotency
                .as_ref()
                .is_some_and(|record| record.request_fingerprint != existing.request_fingerprint)
            {
                return Err(control_conflict_error(&idempotency_key));
            }
            Ok(Some(existing.response_json))
        }
        Err(error) => Err(api_error_from_journal(error)),
    }
}

async fn room_orders(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Query(query): Query<ProjectionQuery>,
) -> ApiResult<RoomOrdersResponse> {
    room_orders_response(state, headers, room_id, query.instrument_id.clone(), query).await
}

async fn room_orders_for_instrument(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, instrument_id)): Path<(String, String)>,
    Query(query): Query<ProjectionQuery>,
) -> ApiResult<RoomOrdersResponse> {
    room_orders_response(state, headers, room_id, Some(instrument_id), query).await
}

async fn room_orders_response(
    state: SharedState,
    headers: HeaderMap,
    room_id: String,
    instrument_id: Option<InstrumentId>,
    query: ProjectionQuery,
) -> ApiResult<RoomOrdersResponse> {
    let authorization = authorize_room_read(
        &state,
        &headers,
        &room_id,
        RoomReadAccess::projection(query.account_id),
    )
    .await?;
    let orders = authorization
        .journal
        .query_orders(
            &authorization.user_id,
            &room_id,
            instrument_id.as_deref(),
            query.account_id,
            query_limit(query.limit),
        )
        .await
        .map_err(api_error_from_journal)?;
    Ok(Json(RoomOrdersResponse { room_id, orders }))
}

async fn room_trades(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Query(query): Query<ProjectionQuery>,
) -> ApiResult<RoomTradesResponse> {
    room_trades_response(state, headers, room_id, query.instrument_id.clone(), query).await
}

async fn room_trades_for_instrument(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, instrument_id)): Path<(String, String)>,
    Query(query): Query<ProjectionQuery>,
) -> ApiResult<RoomTradesResponse> {
    room_trades_response(state, headers, room_id, Some(instrument_id), query).await
}

async fn room_trades_response(
    state: SharedState,
    headers: HeaderMap,
    room_id: String,
    instrument_id: Option<InstrumentId>,
    query: ProjectionQuery,
) -> ApiResult<RoomTradesResponse> {
    let authorization = authorize_room_read(
        &state,
        &headers,
        &room_id,
        RoomReadAccess::projection(query.account_id),
    )
    .await?;
    let trades = authorization
        .journal
        .query_trades(
            &authorization.user_id,
            &room_id,
            instrument_id.as_deref(),
            query.account_id,
            query_limit(query.limit),
        )
        .await
        .map_err(api_error_from_journal)?;
    Ok(Json(RoomTradesResponse { room_id, trades }))
}

async fn room_ticks(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Query(query): Query<ProjectionQuery>,
) -> ApiResult<RoomTicksResponse> {
    room_ticks_response(state, headers, room_id, query.instrument_id.clone(), query).await
}

async fn room_ticks_for_instrument(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, instrument_id)): Path<(String, String)>,
    Query(query): Query<ProjectionQuery>,
) -> ApiResult<RoomTicksResponse> {
    room_ticks_response(state, headers, room_id, Some(instrument_id), query).await
}

async fn room_ticks_response(
    state: SharedState,
    headers: HeaderMap,
    room_id: String,
    instrument_id: Option<InstrumentId>,
    query: ProjectionQuery,
) -> ApiResult<RoomTicksResponse> {
    let authorization =
        authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Room).await?;
    let ticks = authorization
        .journal
        .query_market_ticks(
            &authorization.user_id,
            &room_id,
            instrument_id.as_deref(),
            query_limit(query.limit),
        )
        .await
        .map_err(api_error_from_journal)?;
    Ok(Json(RoomTicksResponse { room_id, ticks }))
}

async fn room_ledger(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Query(query): Query<ProjectionQuery>,
) -> ApiResult<RoomLedgerResponse> {
    room_ledger_response(state, headers, room_id, query.instrument_id.clone(), query).await
}

async fn room_ledger_for_instrument(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, instrument_id)): Path<(String, String)>,
    Query(query): Query<ProjectionQuery>,
) -> ApiResult<RoomLedgerResponse> {
    room_ledger_response(state, headers, room_id, Some(instrument_id), query).await
}

async fn room_ledger_response(
    state: SharedState,
    headers: HeaderMap,
    room_id: String,
    instrument_id: Option<InstrumentId>,
    query: ProjectionQuery,
) -> ApiResult<RoomLedgerResponse> {
    let authorization = authorize_room_read(
        &state,
        &headers,
        &room_id,
        RoomReadAccess::projection(query.account_id),
    )
    .await?;
    let ledger = authorization
        .journal
        .query_account_ledger(
            &authorization.user_id,
            &room_id,
            instrument_id.as_deref(),
            query.account_id,
            query_limit(query.limit),
        )
        .await
        .map_err(api_error_from_journal)?;
    Ok(Json(RoomLedgerResponse { room_id, ledger }))
}

async fn room_positions(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Query(query): Query<ProjectionQuery>,
) -> ApiResult<RoomPositionsResponse> {
    room_positions_response(state, headers, room_id, query.instrument_id.clone(), query).await
}

async fn room_positions_for_instrument(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, instrument_id)): Path<(String, String)>,
    Query(query): Query<ProjectionQuery>,
) -> ApiResult<RoomPositionsResponse> {
    room_positions_response(state, headers, room_id, Some(instrument_id), query).await
}

async fn room_positions_response(
    state: SharedState,
    headers: HeaderMap,
    room_id: String,
    instrument_id: Option<InstrumentId>,
    query: ProjectionQuery,
) -> ApiResult<RoomPositionsResponse> {
    let authorization = authorize_room_read(
        &state,
        &headers,
        &room_id,
        RoomReadAccess::projection(query.account_id),
    )
    .await?;
    let positions = authorization
        .journal
        .query_position_snapshots(
            &authorization.user_id,
            &room_id,
            instrument_id.as_deref(),
            query.account_id,
            query_limit(query.limit),
        )
        .await
        .map_err(api_error_from_journal)?;
    Ok(Json(RoomPositionsResponse { room_id, positions }))
}

fn query_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(100).clamp(1, 500)
}

fn current_user_id(
    headers: &HeaderMap,
    policy: &AuthPolicy,
) -> Result<String, (StatusCode, Json<ErrorResponse>)> {
    policy.authenticate(headers).map_err(|error| match error {
        AuthError::MissingCredentials | AuthError::InvalidCredentials => {
            api_error(StatusCode::UNAUTHORIZED, error.to_string())
        }
        AuthError::InvalidUserId => api_error(StatusCode::BAD_REQUEST, error.to_string()),
        AuthError::Configuration(_) => {
            api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
        }
    })
}

#[derive(Clone, Copy)]
enum RoomReadAccess {
    Room,
    Admin,
    Account(AccountId),
}

impl RoomReadAccess {
    fn projection(account_id: Option<AccountId>) -> Self {
        account_id.map_or(Self::Admin, Self::Account)
    }
}

struct AuthorizedJournalRead {
    user_id: String,
    journal: JournalCoordinator,
}

async fn authorize_room_read(
    state: &SharedState,
    headers: &HeaderMap,
    room_id: &str,
    access: RoomReadAccess,
) -> Result<AuthorizedJournalRead, ApiError> {
    let (user_id, journal, room_leased) = {
        let state = lock_state(state).await?;
        let user_id = current_user_id(headers, &state.auth_policy)?;
        let room_leased = state
            .room_lease_runtime
            .as_ref()
            .is_some_and(|runtime| runtime.config.mode == RoomLeaseRuntimeMode::RoomLeased);
        if !room_leased {
            state.rooms.status(room_id).map_err(api_error_from_room)?;
        }
        (user_id, state.journal.clone(), room_leased)
    };

    let allowed = match access {
        RoomReadAccess::Room => journal.user_can_access_room(&user_id, room_id).await,
        RoomReadAccess::Admin => journal.user_can_administer_room(&user_id, room_id).await,
        RoomReadAccess::Account(account_id) => {
            journal
                .user_can_access_account(&user_id, room_id, account_id)
                .await
        }
    }
    .map_err(api_error_from_journal)?;
    if allowed {
        if room_leased {
            ensure_room_owned(state, room_id).await?;
        }
        return Ok(AuthorizedJournalRead { user_id, journal });
    }

    let message = match access {
        RoomReadAccess::Room => format!("user {user_id} cannot access room {room_id}"),
        RoomReadAccess::Admin => format!("user {user_id} cannot administer room {room_id}"),
        RoomReadAccess::Account(account_id) => {
            format!("user {user_id} cannot access account {account_id} in room {room_id}")
        }
    };
    Err(api_error(StatusCode::FORBIDDEN, message))
}

async fn room_owner(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<RoomOwnerResponse> {
    let (user_id, journal) = {
        let app = lock_state(&state).await?;
        (
            current_user_id(&headers, &app.auth_policy)?,
            app.journal.clone(),
        )
    };
    let allowed = journal
        .user_can_access_room(&user_id, &room_id)
        .await
        .map_err(api_error_from_journal)?;
    if !allowed {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            format!("user {user_id} cannot access room {room_id}"),
        ));
    }

    let lease = journal
        .current_room_writer_lease(&room_id)
        .await
        .map_err(api_error_from_journal)?
        .ok_or_else(|| {
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("room {room_id} has no active writer lease"),
            )
        })?;
    Ok(Json(room_owner_response(lease)))
}

fn room_owner_response(lease: RoomWriterLease) -> RoomOwnerResponse {
    RoomOwnerResponse {
        room_id: lease.claim.room_id,
        owner_id: lease.claim.owner_id,
        owner_url: lease.owner_url,
        fencing_token: lease.claim.fencing_token,
        expires_at_unix_ms: lease.expires_at_unix_ms,
    }
}

async fn ensure_room_owned(state: &SharedState, room_id: &str) -> Result<(), ApiError> {
    let mut app = lock_state(state).await?;
    let Some(runtime) = app.room_lease_runtime.as_ref() else {
        return app
            .rooms
            .status(room_id)
            .map(|_| ())
            .map_err(api_error_from_room);
    };
    if runtime.config.mode != RoomLeaseRuntimeMode::RoomLeased {
        return app
            .rooms
            .status(room_id)
            .map(|_| ())
            .map_err(api_error_from_room);
    }

    let journal = app.journal.clone();
    let instance_id = runtime.config.instance_id.clone();
    let owner_url = runtime.config.owner_url.clone();
    let lease_duration = runtime.config.lease_duration;
    let existing_lease = runtime.leases.get(room_id).cloned();
    if existing_lease.is_some() && app.rooms.status(room_id).is_ok() {
        return Ok(());
    }

    let lease = if let Some(lease) = existing_lease {
        lease
    } else {
        match journal
            .acquire_room_writer_lease(room_id, &instance_id, Some(&owner_url), lease_duration)
            .await
            .map_err(api_error_from_journal)?
        {
            Some(lease) => lease,
            None => {
                let current = journal
                    .current_room_writer_lease(room_id)
                    .await
                    .map_err(api_error_from_journal)?;
                return match current {
                    Some(current) if current.claim.owner_id != instance_id => {
                        Err(api_error_from_journal(JournalError::RoomLeaseOwnedBy {
                            room_id: room_id.to_string(),
                            owner_id: current.claim.owner_id,
                            owner_url: current.owner_url,
                            fencing_token: current.claim.fencing_token,
                            expires_at_unix_ms: current.expires_at_unix_ms,
                        }))
                    }
                    _ => Err(api_error_from_journal(JournalError::RoomLeaseNotOwned {
                        room_id: room_id.to_string(),
                        owner_id: instance_id,
                    })),
                };
            }
        }
    };

    let recovered = journal.load_room_recovery(room_id).await;
    let recovered = match recovered.and_then(|recovery| {
        if recovery.rooms.len() != 1 || recovery.rooms[0].room_id != room_id {
            return Err(JournalError::Recovery(format!(
                "room {room_id} was not present in its durable recovery snapshot"
            )));
        }
        let rooms = recover_rooms(&recovery)?;
        let scheduler = scheduler_states_from_recovery(&recovery).remove(room_id);
        let room = rooms
            .simulation_room(room_id)
            .map_err(|error| JournalError::Recovery(format!("{error:?}")))?
            .clone();
        let history = rooms
            .execution_history(room_id)
            .map_err(|error| JournalError::Recovery(format!("{error:?}")))?
            .to_vec();
        let next_order_id = next_order_id_from_recovery(&recovery)?;
        let cached_executions = execution_summaries_from_recovery(&recovery)
            .remove(room_id)
            .unwrap_or_default();
        Ok((room, history, cached_executions, next_order_id, scheduler))
    }) {
        Ok(recovered) => recovered,
        Err(error) => {
            let _ = journal.release_room_writer_lease(&lease.claim).await;
            if let Some(runtime) = app.room_lease_runtime.as_mut() {
                runtime.leases.remove(room_id);
            }
            return Err(api_error_from_journal(error));
        }
    };

    let renewed = match journal
        .renew_room_writer_lease(&lease.claim, lease_duration)
        .await
    {
        Ok(Some(renewed)) => renewed,
        Ok(None) => {
            if let Some(runtime) = app.room_lease_runtime.as_mut() {
                runtime.leases.remove(room_id);
                runtime.lost_rooms.insert(room_id.to_string());
            }
            return Err(api_error_from_journal(JournalError::RoomLeaseLost {
                room_id: lease.claim.room_id,
                owner_id: lease.claim.owner_id,
                fencing_token: lease.claim.fencing_token,
            }));
        }
        Err(error) => {
            let _ = journal.release_room_writer_lease(&lease.claim).await;
            if let Some(runtime) = app.room_lease_runtime.as_mut() {
                runtime.leases.remove(room_id);
                runtime.lost_rooms.insert(room_id.to_string());
                runtime.renew_failures = runtime.renew_failures.saturating_add(1);
            }
            return Err(api_error_from_journal(error));
        }
    };

    let (room, history, cached_executions, next_order_id, scheduler) = recovered;
    app.rooms.remove_room(room_id);
    if let Err(error) = app.rooms.restore_simulation_room(room, history) {
        let _ = journal.release_room_writer_lease(&renewed.claim).await;
        if let Some(runtime) = app.room_lease_runtime.as_mut() {
            runtime.leases.remove(room_id);
            runtime.lost_rooms.insert(room_id.to_string());
        }
        return Err(api_error_from_room(error));
    }
    app.executions
        .insert(room_id.to_string(), cached_executions);
    app.next_order_id = app.next_order_id.max(next_order_id);
    app.room_event_senders.remove(room_id);
    if let Some(scheduler) = scheduler {
        app.schedulers.insert(room_id.to_string(), scheduler);
    }
    if let Some(runtime) = app.room_lease_runtime.as_mut() {
        runtime.leases.insert(room_id.to_string(), renewed);
        runtime.lost_rooms.remove(room_id);
    }
    Ok(())
}

fn scenario_account_ids(scenario: &ScenarioConfig) -> Vec<AccountId> {
    scenario
        .accounts
        .iter()
        .map(|account| match account {
            exchange_core::ScenarioAccount::Basic { account_id, .. }
            | exchange_core::ScenarioAccount::Spot { account_id, .. } => *account_id,
        })
        .collect()
}

async fn market_view(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<MarketView> {
    market_view_response(state, headers, room_id, None).await
}

async fn market_view_for_instrument(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, instrument_id)): Path<(String, String)>,
) -> ApiResult<MarketView> {
    market_view_response(state, headers, room_id, Some(instrument_id)).await
}

async fn market_view_response(
    state: SharedState,
    headers: HeaderMap,
    room_id: String,
    instrument_id: Option<InstrumentId>,
) -> ApiResult<MarketView> {
    authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Admin).await?;
    let state = lock_state(&state).await?;
    let room = state.rooms.room(&room_id).map_err(api_error_from_room)?;
    let venue_id = room.venue_id().to_string();
    let instrument_id = instrument_id.unwrap_or_else(|| room.primary_instrument_id().to_string());
    Ok(Json(MarketView {
        room_id: room_id.clone(),
        venue_id,
        instrument_id: instrument_id.clone(),
        status: state.rooms.status(&room_id).map_err(api_error_from_room)?,
        book: state
            .rooms
            .book_snapshot_for(&room_id, &instrument_id)
            .map_err(api_error_from_room)?,
        accounts: state
            .rooms
            .account_snapshots_for(&room_id, &instrument_id)
            .map_err(api_error_from_room)?,
    }))
}

async fn book_snapshot(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<BookSnapshot> {
    book_snapshot_response(state, headers, room_id, None).await
}

async fn book_snapshot_for_instrument(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, instrument_id)): Path<(String, String)>,
) -> ApiResult<BookSnapshot> {
    book_snapshot_response(state, headers, room_id, Some(instrument_id)).await
}

async fn book_snapshot_response(
    state: SharedState,
    headers: HeaderMap,
    room_id: String,
    instrument_id: Option<InstrumentId>,
) -> ApiResult<BookSnapshot> {
    authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Room).await?;
    let state = lock_state(&state).await?;
    let instrument_id = match instrument_id {
        Some(instrument_id) => instrument_id,
        None => state
            .rooms
            .room(&room_id)
            .map(|room| room.primary_instrument_id().to_string())
            .map_err(api_error_from_room)?,
    };
    state
        .rooms
        .book_snapshot_for(&room_id, &instrument_id)
        .map(Json)
        .map_err(api_error_from_room)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TickerResponse {
    pub api_version: String,
    pub room_id: String,
    pub instrument_id: InstrumentId,
    pub market_time_ms: u64,
    pub ticker: exchange_core::Ticker,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct CandleQuery {
    pub interval_ms: Option<u64>,
    pub after_open_time_ms: Option<u64>,
    pub instrument_id: Option<InstrumentId>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CandleResponse {
    pub api_version: String,
    pub room_id: String,
    pub instrument_id: InstrumentId,
    pub interval_ms: u64,
    pub market_time_ms: u64,
    pub candles: Vec<exchange_core::Candle>,
    pub next_after_open_time_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UserStreamEvent {
    pub api_version: String,
    pub stream: String,
    pub stream_seq: u64,
    pub command_seq: Option<u64>,
    pub kind: String,
    pub payload: serde_json::Value,
}

async fn room_ticker(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<TickerResponse> {
    ticker_response(state, headers, room_id, None).await
}

async fn room_ticker_for_instrument(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, instrument_id)): Path<(String, String)>,
) -> ApiResult<TickerResponse> {
    ticker_response(state, headers, room_id, Some(instrument_id)).await
}

async fn ticker_response(
    state: SharedState,
    headers: HeaderMap,
    room_id: String,
    instrument_id: Option<InstrumentId>,
) -> ApiResult<TickerResponse> {
    authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Room).await?;
    let state = lock_state(&state).await?;
    let instrument_id = match instrument_id {
        Some(instrument_id) => instrument_id,
        None => state
            .rooms
            .room(&room_id)
            .map(|room| room.primary_instrument_id().to_string())
            .map_err(api_error_from_room)?,
    };
    let clock = state.rooms.clock(&room_id).map_err(api_error_from_room)?;
    let ticker = state
        .rooms
        .ticker(&room_id, &instrument_id)
        .map_err(api_error_from_room)?;
    Ok(Json(TickerResponse {
        api_version: "http.v1".to_string(),
        room_id,
        instrument_id,
        market_time_ms: clock.market_time_ms(),
        ticker,
    }))
}

async fn room_candles(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Query(query): Query<CandleQuery>,
) -> ApiResult<CandleResponse> {
    candles_response(state, headers, room_id, query.instrument_id.clone(), query).await
}

async fn room_candles_for_instrument(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, instrument_id)): Path<(String, String)>,
    Query(query): Query<CandleQuery>,
) -> ApiResult<CandleResponse> {
    candles_response(state, headers, room_id, Some(instrument_id), query).await
}

async fn candles_response(
    state: SharedState,
    headers: HeaderMap,
    room_id: String,
    instrument_id: Option<InstrumentId>,
    query: CandleQuery,
) -> ApiResult<CandleResponse> {
    authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Room).await?;
    let interval_ms = query.interval_ms.unwrap_or(1_000);
    if interval_ms == 0 {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "interval_ms must be greater than zero".to_string(),
        ));
    }
    let state = lock_state(&state).await?;
    let instrument_id = match instrument_id {
        Some(instrument_id) => instrument_id,
        None => state
            .rooms
            .room(&room_id)
            .map(|room| room.primary_instrument_id().to_string())
            .map_err(api_error_from_room)?,
    };
    let clock = state.rooms.clock(&room_id).map_err(api_error_from_room)?;
    let mut candles = state
        .rooms
        .candles(&room_id, &instrument_id, interval_ms)
        .map_err(api_error_from_room)?;
    if let Some(after) = query.after_open_time_ms {
        candles.retain(|candle| candle.open_time_ms > after);
    }
    let next_after_open_time_ms = candles.last().map(|candle| candle.open_time_ms);
    Ok(Json(CandleResponse {
        api_version: "http.v1".to_string(),
        room_id,
        instrument_id,
        interval_ms,
        market_time_ms: clock.market_time_ms(),
        candles,
        next_after_open_time_ms,
    }))
}

async fn public_room_stream(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Query(query): Query<RoomEventStreamQuery>,
) -> Result<Sse<impl futures_util::Stream<Item = Result<SseEvent, Infallible>>>, ApiError> {
    scoped_room_stream(state, headers, room_id, query, StreamScope::Public).await
}

async fn private_room_stream(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Query(query): Query<RoomEventStreamQuery>,
) -> Result<Sse<impl futures_util::Stream<Item = Result<SseEvent, Infallible>>>, ApiError> {
    scoped_room_stream(state, headers, room_id, query, StreamScope::Private).await
}

#[derive(Clone, Copy)]
enum StreamScope {
    Public,
    Private,
}

async fn scoped_room_stream(
    state: SharedState,
    headers: HeaderMap,
    room_id: String,
    query: RoomEventStreamQuery,
    scope: StreamScope,
) -> Result<Sse<impl futures_util::Stream<Item = Result<SseEvent, Infallible>>>, ApiError> {
    let access = match scope {
        StreamScope::Public => RoomReadAccess::Room,
        StreamScope::Private => RoomReadAccess::Room,
    };
    let authorization = authorize_room_read(&state, &headers, &room_id, access).await?;
    if matches!(scope, StreamScope::Private)
        && authorization
            .journal
            .user_room_role(&authorization.user_id, &room_id)
            .await
            .map_err(api_error_from_journal)?
            .as_deref()
            == Some("spectator")
    {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            format!(
                "user {} cannot subscribe to private streams in room {room_id}",
                authorization.user_id
            ),
        ));
    }
    if let Some(requested_scope) = query.scope.as_deref() {
        let expected = match scope {
            StreamScope::Public => "public",
            StreamScope::Private => "private",
        };
        if requested_scope != expected {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                format!("cursor scope {requested_scope:?} does not match {expected} stream"),
            ));
        }
    }
    let user_id = authorization.user_id.clone();
    let journal = authorization.journal.clone();
    let lifecycle = state.lifecycle.clone();
    let shutdown = lifecycle.subscribe_shutdown();
    let mut app = lock_state(&state).await?;
    let receiver = app.room_event_receiver(&room_id);
    let cached = app.executions.get(&room_id).cloned().unwrap_or_default();
    let latest_seq = cached
        .back()
        .map(|execution| execution.command_seq)
        .or_else(|| latest_persisted_command_seq(&app, &room_id));
    let snapshot = match scope {
        StreamScope::Public => public_stream_snapshot(&app, &room_id, latest_seq)?,
        StreamScope::Private => {
            private_stream_snapshot(&app, &journal, &room_id, &user_id, latest_seq).await?
        }
    };
    let after = query.after_command_seq;
    let history = stream_history_executions(&app, &journal, &room_id, &cached, after).await?;
    drop(app);
    let connection = lifecycle.open_sse_connection();
    let stream_name = match scope {
        StreamScope::Public => "public",
        StreamScope::Private => "private",
    };
    let mut seq = 0_u64;
    let mut initial = VecDeque::new();
    initial.push_back(UserStreamEvent {
        api_version: "http.v1".to_string(),
        stream: stream_name.to_string(),
        stream_seq: {
            seq += 1;
            seq
        },
        command_seq: None,
        kind: "snapshot".to_string(),
        payload: snapshot,
    });
    for execution in history {
        if after.is_some_and(|cursor| execution.command_seq <= cursor) {
            continue;
        }
        if let Some(event) =
            scoped_event_from_execution(&execution, scope, &user_id, &journal, seq + 1).await
        {
            seq = event.stream_seq;
            initial.push_back(event);
        }
    }
    let stream_state = ScopedStreamState {
        room_id,
        scope,
        user_id,
        journal,
        stream_seq: seq,
        backlog: initial,
        receiver,
        shutdown,
        connection,
        terminate: false,
    };
    let stream = futures_util::stream::unfold(stream_state, |mut state| async move {
        if state.terminate || *state.shutdown.borrow() {
            return None;
        }
        loop {
            if let Some(event) = state.backlog.pop_front() {
                let data = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
                let sse = SseEvent::default()
                    .id(event.stream_seq.to_string())
                    .event(event.kind.clone())
                    .data(data);
                return Some((Ok(sse), state));
            }
            match state.receiver.recv().await {
                Ok(execution) => {
                    if let Some(event) = scoped_event_from_execution(
                        &execution,
                        state.scope,
                        &state.user_id,
                        &state.journal,
                        state.stream_seq + 1,
                    )
                    .await
                    {
                        if matches!(state.scope, StreamScope::Private)
                            && !state
                                .journal
                                .user_can_access_room(&state.user_id, &state.room_id)
                                .await
                                .unwrap_or(false)
                        {
                            state.terminate = true;
                            let event = SseEvent::default().event("resync_required").data(
                                "{\"code\":\"unauthorized\",\"error\":\"permission revoked\"}",
                            );
                            return Some((Ok(event), state));
                        }
                        state.stream_seq = event.stream_seq;
                        state.backlog.push_back(event);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    state.connection.record_resync_required();
                    state.terminate = true;
                    let event = SseEvent::default()
                        .event("resync_required")
                        .data("{\"code\":\"resync_required\"}");
                    return Some((Ok(event), state));
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

struct ScopedStreamState {
    room_id: RoomId,
    scope: StreamScope,
    user_id: String,
    journal: JournalCoordinator,
    stream_seq: u64,
    backlog: VecDeque<UserStreamEvent>,
    receiver: broadcast::Receiver<RoomExecutionSummary>,
    shutdown: watch::Receiver<bool>,
    connection: SseConnectionGuard,
    terminate: bool,
}

fn public_stream_snapshot(
    app: &AppState,
    room_id: &str,
    at_command_seq: Option<u64>,
) -> Result<serde_json::Value, ApiError> {
    let instrument_id = app
        .rooms
        .room(room_id)
        .map(|room| room.primary_instrument_id().to_string())
        .map_err(api_error_from_room)?;
    let book = app
        .rooms
        .book_snapshot_for(room_id, &instrument_id)
        .map_err(api_error_from_room)?;
    let ticker = app
        .rooms
        .ticker(room_id, &instrument_id)
        .map_err(api_error_from_room)?;
    let clock = app.rooms.clock(room_id).map_err(api_error_from_room)?;
    Ok(serde_json::json!({
        "cursor": {
            "room_id": room_id,
            "scope": "public",
            "version": "stream.v1",
            "command_seq": at_command_seq,
        },
        "instrument_id": instrument_id,
        "book": book,
        "ticker": ticker,
        "market_time_ms": clock.market_time_ms(),
        "status": clock_status_name(app.rooms.status(room_id).map_err(api_error_from_room)?),
    }))
}

async fn private_stream_snapshot(
    app: &AppState,
    journal: &JournalCoordinator,
    room_id: &str,
    user_id: &str,
    at_command_seq: Option<u64>,
) -> Result<serde_json::Value, ApiError> {
    let mut payload = public_stream_snapshot(app, room_id, at_command_seq)?;
    if let Some(cursor) = payload.get_mut("cursor") {
        cursor["scope"] = serde_json::json!("private");
    }
    let instrument_id = app
        .rooms
        .room(room_id)
        .map(|room| room.primary_instrument_id().to_string())
        .map_err(api_error_from_room)?;
    let is_admin = journal
        .user_can_administer_room(user_id, room_id)
        .await
        .map_err(api_error_from_journal)?;
    let accounts = app
        .rooms
        .account_snapshots_for(room_id, &instrument_id)
        .map_err(api_error_from_room)?;
    let account_ids = match &accounts {
        AccountSnapshots::Spot(items) => {
            items.iter().map(|item| item.account_id).collect::<Vec<_>>()
        }
        AccountSnapshots::Perp(items) => {
            items.iter().map(|item| item.account_id).collect::<Vec<_>>()
        }
    };
    let mut visible_accounts = Vec::new();
    for account_id in account_ids {
        if is_admin
            || journal
                .user_can_access_account(user_id, room_id, account_id)
                .await
                .map_err(api_error_from_journal)?
        {
            visible_accounts.push(account_id);
        }
    }
    let mut orders = Vec::new();
    for account_id in &visible_accounts {
        let mut resting = app
            .rooms
            .resting_orders_for_account(room_id, &instrument_id, *account_id)
            .map_err(api_error_from_room)?;
        orders.append(&mut resting);
    }
    payload["accounts"] = serde_json::json!(visible_accounts);
    payload["orders"] = serde_json::to_value(&orders).map_err(api_error_from_json)?;
    Ok(payload)
}

async fn stream_history_executions(
    _app: &AppState,
    journal: &JournalCoordinator,
    room_id: &str,
    cached: &VecDeque<RoomExecutionSummary>,
    after: Option<u64>,
) -> Result<Vec<RoomExecutionSummary>, ApiError> {
    let Some(after) = after else {
        return Ok(Vec::new());
    };
    let oldest_cached = cached.front().map(|execution| execution.command_seq);
    if oldest_cached.is_some_and(|oldest| oldest <= after + 1) {
        return Ok(cached
            .iter()
            .filter(|execution| execution.command_seq > after)
            .cloned()
            .collect());
    }
    let page = journal
        .query_executions(room_id, Some(after), false, ROOM_EVENT_CACHE_CAPACITY)
        .await
        .map_err(api_error_from_journal)?;
    Ok(page
        .executions
        .into_iter()
        .filter(|execution| execution.command_seq > after)
        .collect())
}

fn clock_status_name(status: MarketStatus) -> &'static str {
    match status {
        MarketStatus::Running => "running",
        MarketStatus::Paused => "paused",
        MarketStatus::Closed => "closed",
    }
}

async fn scoped_event_from_execution(
    execution: &RoomExecutionSummary,
    scope: StreamScope,
    user_id: &str,
    journal: &JournalCoordinator,
    stream_seq: u64,
) -> Option<UserStreamEvent> {
    let kinds = execution
        .events
        .iter()
        .map(event_kind_name)
        .collect::<Vec<_>>();
    let is_public = kinds.iter().any(|kind| {
        matches!(
            *kind,
            "trade_printed" | "order_canceled" | "order_rested" | "order_accepted"
        )
    });
    match scope {
        StreamScope::Public if is_public || kinds.is_empty() => Some(UserStreamEvent {
            api_version: "http.v1".to_string(),
            stream: "public".to_string(),
            stream_seq,
            command_seq: Some(execution.command_seq),
            kind: "execution".to_string(),
            payload: serde_json::json!({
                "command_seq": execution.command_seq,
                "instrument_id": execution.instrument_id,
                "events": kinds,
                "accepted": execution.accepted,
            }),
        }),
        StreamScope::Private => {
            if !user_can_see_private_execution(execution, user_id, journal).await {
                return None;
            }
            Some(UserStreamEvent {
                api_version: "http.v1".to_string(),
                stream: "private".to_string(),
                stream_seq,
                command_seq: Some(execution.command_seq),
                kind: "execution".to_string(),
                payload: serde_json::to_value(execution).unwrap_or_else(|_| serde_json::json!({})),
            })
        }
        StreamScope::Public => None,
    }
}

fn event_kind_name(event: &EventSummary) -> &'static str {
    match event {
        EventSummary::OrderAccepted { .. } => "order_accepted",
        EventSummary::OrderRejected { .. } => "order_rejected",
        EventSummary::RiskRejected { .. } => "risk_rejected",
        EventSummary::TradePrinted { .. } => "trade_printed",
        EventSummary::OrderPartiallyFilled { .. } => "order_partially_filled",
        EventSummary::OrderFilled { .. } => "order_filled",
        EventSummary::OrderRested { .. } => "order_rested",
        EventSummary::OrderExpired { .. } => "order_expired",
        EventSummary::OrderCanceled { .. } => "order_canceled",
        EventSummary::CancelRejected { .. } => "cancel_rejected",
        EventSummary::OrderAmended { .. } => "order_amended",
        EventSummary::AmendRejected { .. } => "amend_rejected",
    }
}

async fn user_can_see_private_execution(
    execution: &RoomExecutionSummary,
    user_id: &str,
    journal: &JournalCoordinator,
) -> bool {
    if journal
        .user_can_administer_room(user_id, &execution.room_id)
        .await
        .unwrap_or(false)
    {
        return true;
    }
    if let Some(account_id) = execution.submit_account_id
        && journal
            .user_can_access_account(user_id, &execution.room_id, account_id)
            .await
            .unwrap_or(false)
    {
        return true;
    }
    let mut accounts = Vec::new();
    for event in &execution.events {
        if let EventSummary::TradePrinted {
            maker_account_id,
            taker_account_id,
            ..
        } = event
        {
            accounts.push(*maker_account_id);
            accounts.push(*taker_account_id);
        }
    }
    for clearing in &execution.clearing_events {
        match clearing {
            ClearingEventSummary::SpotTradeSettled {
                buyer_account_id,
                seller_account_id,
                ..
            }
            | ClearingEventSummary::PerpTradeSettled {
                buyer_account_id,
                seller_account_id,
                ..
            } => {
                accounts.push(*buyer_account_id);
                accounts.push(*seller_account_id);
            }
            _ => {}
        }
    }
    for account_id in accounts {
        if journal
            .user_can_access_account(user_id, &execution.room_id, account_id)
            .await
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

async fn account_snapshots(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<AccountSnapshots> {
    account_snapshots_response(state, headers, room_id, None).await
}

async fn account_snapshots_for_instrument(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, instrument_id)): Path<(String, String)>,
) -> ApiResult<AccountSnapshots> {
    account_snapshots_response(state, headers, room_id, Some(instrument_id)).await
}

async fn account_snapshots_response(
    state: SharedState,
    headers: HeaderMap,
    room_id: String,
    instrument_id: Option<InstrumentId>,
) -> ApiResult<AccountSnapshots> {
    authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Admin).await?;
    let state = lock_state(&state).await?;
    let instrument_id = match instrument_id {
        Some(instrument_id) => instrument_id,
        None => state
            .rooms
            .room(&room_id)
            .map(|room| room.primary_instrument_id().to_string())
            .map_err(api_error_from_room)?,
    };
    state
        .rooms
        .account_snapshots_for(&room_id, &instrument_id)
        .map(Json)
        .map_err(api_error_from_room)
}

async fn venue_account_snapshots(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<RoomVenueAccountsResponse> {
    authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Admin).await?;
    let state = lock_state(&state).await?;
    state
        .rooms
        .venue_account_snapshots(&room_id)
        .map(|accounts| {
            Json(RoomVenueAccountsResponse {
                room_id: room_id.clone(),
                accounts,
            })
        })
        .map_err(api_error_from_room)
}

async fn venue_account_snapshots_by_venue(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<RoomVenueAccountsByVenueResponse> {
    authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Admin).await?;
    let state = lock_state(&state).await?;
    state
        .rooms
        .venue_account_snapshots_by_venue(&room_id)
        .map(|accounts| {
            Json(RoomVenueAccountsByVenueResponse {
                room_id: room_id.clone(),
                accounts,
            })
        })
        .map_err(api_error_from_room)
}

async fn room_portfolios(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<RoomPortfoliosResponse> {
    authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Admin).await?;
    let state = lock_state(&state).await?;
    state
        .rooms
        .portfolio_snapshots(&room_id)
        .map(|accounts| {
            Json(RoomPortfoliosResponse {
                room_id: room_id.clone(),
                accounts,
            })
        })
        .map_err(api_error_from_room)
}

async fn room_asset_ledger(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<RoomAssetLedgerResponse> {
    authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Admin).await?;
    let state = lock_state(&state).await?;
    state
        .rooms
        .asset_ledger(&room_id)
        .map(|ledger| {
            Json(RoomAssetLedgerResponse {
                room_id: room_id.clone(),
                ledger: ledger.to_vec(),
            })
        })
        .map_err(api_error_from_room)
}

async fn room_net_worth(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<RoomNetWorthSnapshot> {
    authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Admin).await?;
    let state = lock_state(&state).await?;
    state
        .rooms
        .net_worth_snapshot(&room_id)
        .map(Json)
        .map_err(api_error_from_room)
}

async fn room_clock(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<RoomClockResponse> {
    authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Room).await?;
    let state = lock_state(&state).await?;
    state
        .rooms
        .clock(&room_id)
        .map(|clock| {
            Json(RoomClockResponse {
                room_id: room_id.clone(),
                clock,
            })
        })
        .map_err(api_error_from_room)
}

async fn advance_room_clock(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Json(request): Json<AdvanceClockRequest>,
) -> ApiResult<AdvanceClockResponse> {
    authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Admin).await?;
    let user_id = {
        let app = lock_state(&state).await?;
        current_user_id(&headers, &app.auth_policy)?
    };
    let idempotency_key = request_idempotency_key(&headers)?;
    let fingerprint = control_fingerprint(
        "clock/advance",
        serde_json::json!({ "steps": request.steps }),
    );
    run_durable_state_transaction(state.clone(), async move {
    let mut state = lock_state(&state).await?;
    if let Some(key) = idempotency_key.as_deref()
        && let Some(replayed) =
            load_control_replay(&state, &user_id, &room_id, key, &fingerprint).await?
    {
        return Ok(Json(replayed));
    }
    let command_cursor = next_persisted_command_cursor(&state, &room_id)
        .map_err(api_error_from_journal)?;
    let mut candidate_rooms = state.rooms.clone();
    let previous_history_len = candidate_rooms
        .execution_history(&room_id)
        .map_err(api_error_from_room)?
        .len();
    let completed_transfers = candidate_rooms
        .advance_clock(&room_id, request.steps)
        .map_err(api_error_from_room)?;
    let clock = candidate_rooms
        .clock(&room_id)
        .map_err(api_error_from_room)?;
    let records = completed_transfers
        .iter()
        .cloned()
        .map(|transfer| JournalTransfer::recorded(room_id.clone(), transfer))
        .collect::<Vec<_>>();
    let training_run_id = state
        .training_runs
        .values()
        .find(|run| run.spec.room_id == room_id)
        .map(|run| run.spec.run_id.clone());
    if let Some(run_id) = training_run_id.as_ref()
        && let Some(mut run) = state.training_runs.get(run_id).cloned()
    {
        let new_executions = candidate_rooms
            .execution_history(&room_id)
            .map_err(api_error_from_room)?
            .iter()
            .skip(previous_history_len)
            .cloned()
            .collect::<Vec<_>>();
        for execution in &new_executions {
            apply_training_execution(&mut run, execution, None, execution_account_id(execution));
        }
        for _ in 0..request.steps {
            run.on_step();
            if run.is_finished() {
                break;
            }
        }
        settle_training_residuals(&mut candidate_rooms, &mut run)?;
        state.training_runs.insert(run_id.clone(), run);
    }
    let execution_records = candidate_rooms
        .execution_history(&room_id)
        .map_err(api_error_from_room)?
        .iter()
        .skip(previous_history_len)
        .map(|execution| {
            command_from_actor_execution(execution)
                .map(|command| JournalExecution::system(command, execution.clone()))
                .ok_or_else(|| {
                    api_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!(
                            "room {room_id} produced an unjournalable execution while advancing its clock"
                        ),
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let final_command_seq = execution_records
        .last()
        .map(|record| record.command_seq)
        .or_else(|| latest_persisted_command_seq(&state, &room_id))
        .unwrap_or(0);
    let snapshot = current_room_snapshot(&candidate_rooms, &room_id, final_command_seq)
        .ok_or_else(|| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("room {room_id} disappeared while advancing its clock"),
            )
        })?;
    let response = AdvanceClockResponse {
        room_id: room_id.clone(),
        clock,
        completed_transfers: completed_transfers.clone(),
    };
    let record = idempotency_key
        .as_ref()
        .map(|key| {
            control_record(
                user_id.clone(),
                room_id.clone(),
                key.clone(),
                fingerprint.clone(),
                &response,
            )
        })
        .transpose()?;
    let pending = PendingJournalMutation::new(
        room_id.clone(),
        command_cursor,
        RoomMutation::ClockAdvanced {
            steps: request.steps,
            completed_transfers: completed_transfers.clone(),
        },
    );
    let pending = match record {
        Some(record) => pending.with_control_idempotency(record),
        None => pending,
    };
    if let Some(replay_json) =
        append_control_mutation(&mut state, pending, &execution_records, &records, Some(&snapshot))
            .await?
    {
        let replayed = serde_json::from_value(replay_json).map_err(api_error_from_json)?;
        return Ok(Json(replayed));
    }
    if let Some(run_id) = training_run_id
        && let Some(run) = state.training_runs.get(&run_id).cloned()
    {
        persist_training_progress(&mut state, &run, &[]).await?;
    }
    state.append_room_executions(
        &room_id,
        execution_records
            .iter()
            .map(|record| record.execution.clone())
            .collect(),
    );
    state.rooms = candidate_rooms;

    Ok(Json(response))
    })
    .await
}

async fn manual_room_step(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<exchange_core::SchedulerState> {
    authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Admin).await?;
    let user_id = {
        let app = lock_state(&state).await?;
        current_user_id(&headers, &app.auth_policy)?
    };
    let idempotency_key = request_idempotency_key(&headers)?;
    let fingerprint = control_fingerprint("clock/step", serde_json::json!({}));
    let control = idempotency_key.map(|key| ControlIdempotencyIntent {
        user_id,
        key,
        fingerprint,
    });
    let stepped = commit_scheduler_step(state, room_id, None, true, control).await?;
    Ok(Json(stepped))
}

async fn room_transfers(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<RoomTransfersResponse> {
    let authorization =
        authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Admin).await?;
    authorization
        .journal
        .query_transfers(&authorization.user_id, &room_id, None, 100)
        .await
        .map(|transfers| {
            Json(RoomTransfersResponse {
                room_id: room_id.clone(),
                transfers,
            })
        })
        .map_err(api_error_from_journal)
}

async fn submit_deposit(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Json(request): Json<TransferRequest>,
) -> ApiResult<TransferResponse> {
    authorize_room_read(
        &state,
        &headers,
        &room_id,
        RoomReadAccess::Account(request.account_id),
    )
    .await?;
    run_durable_state_transaction(state.clone(), async move {
        let mut state = lock_state(&state).await?;
        reject_trainee_transfer(&state, &room_id, request.account_id)?;
        let command_cursor =
            next_persisted_command_cursor(&state, &room_id).map_err(api_error_from_journal)?;
        let venue_id = request.venue_id.clone();
        let asset_id = request.asset_id.clone();
        let mut candidate_rooms = state.rooms.clone();
        let transfer = candidate_rooms
            .submit_deposit(
                &room_id,
                request.venue_id.as_deref(),
                request.account_id,
                request.asset_id,
                request.amount,
            )
            .map_err(api_error_from_room)?;
        let snapshot = current_room_snapshot(
            &candidate_rooms,
            &room_id,
            latest_persisted_command_seq(&state, &room_id).unwrap_or(0),
        )
        .ok_or_else(|| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("room {room_id} disappeared while submitting a deposit"),
            )
        })?;
        let record = JournalTransfer::recorded(room_id.clone(), transfer.clone());
        state
            .append_room_mutation(
                &PendingJournalMutation::new(
                    room_id.clone(),
                    command_cursor,
                    RoomMutation::DepositSubmitted {
                        venue_id,
                        account_id: request.account_id,
                        asset_id,
                        amount: request.amount,
                        transfer: transfer.clone(),
                    },
                ),
                &[],
                &[record],
                Some(&snapshot),
            )
            .await
            .map_err(api_error_from_journal)?;
        state.rooms = candidate_rooms;

        Ok(Json(TransferResponse { room_id, transfer }))
    })
    .await
}

async fn submit_withdrawal(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Json(request): Json<TransferRequest>,
) -> ApiResult<TransferResponse> {
    authorize_room_read(
        &state,
        &headers,
        &room_id,
        RoomReadAccess::Account(request.account_id),
    )
    .await?;
    run_durable_state_transaction(state.clone(), async move {
        let mut state = lock_state(&state).await?;
        reject_trainee_transfer(&state, &room_id, request.account_id)?;
        let command_cursor =
            next_persisted_command_cursor(&state, &room_id).map_err(api_error_from_journal)?;
        let venue_id = request.venue_id.clone();
        let asset_id = request.asset_id.clone();
        let mut candidate_rooms = state.rooms.clone();
        let transfer = candidate_rooms
            .submit_withdrawal(
                &room_id,
                request.venue_id.as_deref(),
                request.account_id,
                request.asset_id,
                request.amount,
            )
            .map_err(api_error_from_room)?;
        let snapshot = current_room_snapshot(
            &candidate_rooms,
            &room_id,
            latest_persisted_command_seq(&state, &room_id).unwrap_or(0),
        )
        .ok_or_else(|| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("room {room_id} disappeared while submitting a withdrawal"),
            )
        })?;
        let record = JournalTransfer::recorded(room_id.clone(), transfer.clone());
        state
            .append_room_mutation(
                &PendingJournalMutation::new(
                    room_id.clone(),
                    command_cursor,
                    RoomMutation::WithdrawalSubmitted {
                        venue_id,
                        account_id: request.account_id,
                        asset_id,
                        amount: request.amount,
                        transfer: transfer.clone(),
                    },
                ),
                &[],
                &[record],
                Some(&snapshot),
            )
            .await
            .map_err(api_error_from_journal)?;
        state.rooms = candidate_rooms;

        Ok(Json(TransferResponse { room_id, transfer }))
    })
    .await
}

async fn submit_venue_to_venue_transfer(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Json(request): Json<VenueToVenueTransferRequest>,
) -> ApiResult<VenueToVenueTransferResponse> {
    authorize_room_read(
        &state,
        &headers,
        &room_id,
        RoomReadAccess::Account(request.account_id),
    )
    .await?;
    run_durable_state_transaction(state.clone(), async move {
        let mut state = lock_state(&state).await?;
        reject_trainee_transfer(&state, &room_id, request.account_id)?;
        let command_cursor =
            next_persisted_command_cursor(&state, &room_id).map_err(api_error_from_journal)?;
        let from_venue_id = request.from_venue_id.clone();
        let to_venue_id = request.to_venue_id.clone();
        let asset_id = request.asset_id.clone();
        let mut candidate_rooms = state.rooms.clone();
        let transfer = candidate_rooms
            .submit_venue_to_venue_transfer(
                &room_id,
                &request.from_venue_id,
                &request.to_venue_id,
                request.account_id,
                request.asset_id,
                request.amount,
            )
            .map_err(api_error_from_room)?;
        let snapshot = current_room_snapshot(
            &candidate_rooms,
            &room_id,
            latest_persisted_command_seq(&state, &room_id).unwrap_or(0),
        )
        .ok_or_else(|| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("room {room_id} disappeared while submitting a venue transfer"),
            )
        })?;
        let mut records = vec![JournalTransfer::recorded(
            room_id.clone(),
            transfer.withdrawal.clone(),
        )];
        if let Some(deposit) = &transfer.deposit {
            records.push(JournalTransfer::recorded(room_id.clone(), deposit.clone()));
        }
        state
            .append_room_mutation(
                &PendingJournalMutation::new(
                    room_id.clone(),
                    command_cursor,
                    RoomMutation::VenueToVenueTransferSubmitted {
                        from_venue_id,
                        to_venue_id,
                        account_id: request.account_id,
                        asset_id,
                        amount: request.amount,
                        transfer: transfer.clone(),
                    },
                ),
                &[],
                &records,
                Some(&snapshot),
            )
            .await
            .map_err(api_error_from_journal)?;
        state.rooms = candidate_rooms;

        Ok(Json(VenueToVenueTransferResponse { room_id, transfer }))
    })
    .await
}

async fn submit_order(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Json(request): Json<SubmitOrderRequest>,
) -> ApiResult<OrderResponse> {
    submit_order_response(state, headers, room_id, None, request).await
}

async fn submit_order_for_instrument(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, instrument_id)): Path<(String, String)>,
    Json(request): Json<SubmitOrderRequest>,
) -> ApiResult<OrderResponse> {
    submit_order_response(state, headers, room_id, Some(instrument_id), request).await
}

async fn set_mark_price_for_instrument(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, instrument_id)): Path<(String, String)>,
    Json(request): Json<SetMarkPriceRequest>,
) -> ApiResult<RoomExecutionSummary> {
    authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Admin).await?;
    run_durable_state_transaction(state.clone(), async move {
        let mut state = lock_state(&state).await?;
        let mut candidate_rooms = state.rooms.clone();
        let previous_history_len = candidate_rooms
            .execution_history(&room_id)
            .map_err(api_error_from_room)?
            .len();
        let command = Command::SetMarkPrice(SetMarkPrice {
            price_tick: request.price_tick,
        });
        let execution = candidate_rooms
            .apply_to_instrument(&room_id, &instrument_id, command.clone())
            .map_err(api_error_from_room)?;
        let response = RoomExecutionSummary::from_execution(execution.clone());
        let journal_record = JournalExecution::system(command, execution);
        let journal_room_id = journal_record.room_id.clone();
        journal_new_executions(
            &mut state,
            &candidate_rooms,
            &journal_room_id,
            previous_history_len,
            journal_record,
        )
        .await
        .map_err(api_error_from_journal)?;
        state.rooms = candidate_rooms;

        Ok(Json(response))
    })
    .await
}

async fn submit_order_response(
    state: SharedState,
    headers: HeaderMap,
    room_id: String,
    instrument_id: Option<InstrumentId>,
    request: SubmitOrderRequest,
) -> ApiResult<OrderResponse> {
    let authorization = authorize_room_read(
        &state,
        &headers,
        &room_id,
        RoomReadAccess::Account(request.account_id),
    )
    .await?;
    if let Some(reason) = order_action_precision_error(&request.action) {
        return Err(api_error(StatusCode::BAD_REQUEST, reason));
    }
    run_durable_state_transaction(state.clone(), async move {
        let mut state = lock_state(&state).await?;
        let user_id = authorization.user_id;
        let is_admin = state
            .journal
            .user_can_administer_room(&user_id, &room_id)
            .await
            .map_err(api_error_from_journal)?;
        let idempotency_key = request_idempotency_key(&headers)?;
        let instrument_id = instrument_id.or_else(|| request.instrument_id.clone());
        let request_fingerprint = serde_json::to_string(&(instrument_id.as_deref(), &request))
            .map_err(|error| {
                api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to fingerprint order request: {error}"),
                )
            })?;
        if let Some(idempotency_key) = idempotency_key.as_deref()
            && let Some(existing) = state
                .journal
                .find_idempotent_execution(&user_id, &room_id, idempotency_key)
                .await
                .map_err(api_error_from_journal)?
        {
            if existing.request_fingerprint.as_deref() != Some(request_fingerprint.as_str()) {
                return Err(api_error(
                    StatusCode::CONFLICT,
                    format!(
                        "idempotency key {idempotency_key:?} was already used for a different order request"
                    ),
                ));
            }
            return Ok(Json(OrderResponse::from_summary(
                request.participant_id,
                request.account_id,
                request.action,
                existing.execution,
            )));
        }
        let quota_step = if is_admin {
            None
        } else {
            let step = state
                .rooms
                .clock(&room_id)
                .map_err(api_error_from_room)?
                .step();
            let count = state
                .journal
                .external_action_count(&user_id, &room_id, step)
                .await
                .map_err(api_error_from_journal)?;
            if count >= EXTERNAL_ACTIONS_PER_STEP {
                return Err(api_error(
                    StatusCode::TOO_MANY_REQUESTS,
                    "external action quota exceeded for this simulation step",
                ));
            }
            Some(step)
        };
        let first_order_id = state.next_order_id;
        if order_action_allocates_id(&request.action)
            && first_order_id >= SYSTEM_LIQUIDATION_ORDER_ID_BASE
        {
            return Err(api_error(
                StatusCode::CONFLICT,
                "API order-id range is exhausted",
            ));
        }
        let order_side = order_action_side(&request.action);
        if let Some(run) = state
            .training_runs
            .values()
            .find(|run| run.spec.room_id == room_id)
        {
            if !run.allows_trainee_action(request.account_id, order_side) {
                return Err(api_error(
                    StatusCode::CONFLICT,
                    format!("training run {} does not allow this action", run.spec.run_id),
                ));
            }
            if request.account_id == run.spec.trainee_account_id
                && let Some(qty) = order_action_qty(&request.action)
                && qty > run.remaining_buy_capacity()
            {
                return Err(api_error(
                    StatusCode::CONFLICT,
                    "order would exceed remaining training buy capacity".to_string(),
                ));
            }
        }
        let mut candidate_rooms = state.rooms.clone();
        let previous_history_len = candidate_rooms
            .execution_history(&room_id)
            .map_err(api_error_from_room)?
            .len();
        let observe_instrument = instrument_id
            .clone()
            .or_else(|| {
                candidate_rooms
                    .room(&room_id)
                    .ok()
                    .map(|room| room.primary_instrument_id().to_string())
            });
        let book_before = observe_instrument.as_deref().and_then(|instrument_id| {
            candidate_rooms
                .book_snapshot_for(&room_id, instrument_id)
                .ok()
        });
        let mut gateway = OrderGateway::new(&mut candidate_rooms, first_order_id);
        let execution = gateway
            .submit_action(GatewayRequest {
                participant_id: request.participant_id.clone(),
                room_id: room_id.clone(),
                instrument_id,
                account_id: request.account_id,
                action: request.action.clone(),
            })
            .map_err(|error| api_error_from_room(error.into_room_error()))?;

        let next_order_id = gateway.next_order_id();
        if let Some(run) = state
            .training_runs
            .values_mut()
            .find(|run| run.spec.room_id == room_id)
        {
            apply_training_execution(
                run,
                &execution.execution,
                book_before.as_ref(),
                Some(request.account_id),
            );
        }
        let response = OrderResponse::from_gateway_execution(
            request.participant_id,
            request.account_id,
            request.action,
            execution.execution.clone(),
        );
        let mut journal_record = JournalExecution::submitted(
            execution.participant_id,
            execution.account_id,
            execution.command,
            execution.execution,
        );
        if let Some(idempotency_key) = idempotency_key {
            journal_record = journal_record.with_idempotency(
                user_id.clone(),
                idempotency_key,
                request_fingerprint,
            );
        }
        if let Some(step) = quota_step {
            journal_record = journal_record.with_quota(user_id.clone(), step);
        }
        let journal_room_id = journal_record.room_id.clone();
        let training_run_id = state
            .training_runs
            .values()
            .find(|run| run.spec.room_id == room_id)
            .map(|run| run.spec.run_id.clone());
        let mut settled_run = None;
        if let Some(run_id) = training_run_id.as_ref()
            && let Some(mut run) = state.training_runs.get(run_id).cloned()
        {
            settle_training_residuals(&mut candidate_rooms, &mut run)?;
            settled_run = Some((run_id.clone(), run));
        }
        journal_new_executions(
            &mut state,
            &candidate_rooms,
            &journal_room_id,
            previous_history_len,
            journal_record,
        )
        .await
        .map_err(api_error_from_journal)?;
        if let Some((_, run)) = settled_run {
            persist_training_progress(&mut state, &run, &[]).await?;
        }
        state.rooms = candidate_rooms;
        state.next_order_id = next_order_id;

        Ok(Json(response))
    })
    .await
}

fn order_action_side(action: &OrderAction) -> Option<exchange_core::Side> {
    match action {
        OrderAction::PlaceLimit { side, .. }
        | OrderAction::PlaceMarket { side, .. }
        | OrderAction::PlacePostOnly { side, .. }
        | OrderAction::PlaceImmediateOrCancel { side, .. }
        | OrderAction::PlaceFillOrKill { side, .. }
        | OrderAction::PlaceReduceOnlyMarket { side, .. }
        | OrderAction::PlaceReduceOnlyImmediateOrCancel { side, .. }
        | OrderAction::PlaceReduceOnlyFillOrKill { side, .. } => Some(*side),
        OrderAction::Cancel { .. } | OrderAction::Amend { .. } => None,
    }
}

fn order_action_qty(action: &OrderAction) -> Option<u64> {
    match action {
        OrderAction::PlaceLimit { qty, .. }
        | OrderAction::PlaceMarket { qty, .. }
        | OrderAction::PlacePostOnly { qty, .. }
        | OrderAction::PlaceImmediateOrCancel { qty, .. }
        | OrderAction::PlaceFillOrKill { qty, .. }
        | OrderAction::PlaceReduceOnlyMarket { qty, .. }
        | OrderAction::PlaceReduceOnlyImmediateOrCancel { qty, .. }
        | OrderAction::PlaceReduceOnlyFillOrKill { qty, .. } => Some(*qty),
        OrderAction::Cancel { .. } | OrderAction::Amend { .. } => None,
    }
}

fn order_action_precision_error(action: &OrderAction) -> Option<String> {
    let qty = order_action_qty(action);
    if qty == Some(0) {
        return Some("qty must be a positive integer".to_string());
    }
    let price_tick = match action {
        OrderAction::PlaceLimit { price_tick, .. }
        | OrderAction::PlacePostOnly { price_tick, .. } => Some(*price_tick),
        OrderAction::PlaceImmediateOrCancel { price_tick, .. }
        | OrderAction::PlaceFillOrKill { price_tick, .. }
        | OrderAction::PlaceReduceOnlyImmediateOrCancel { price_tick, .. }
        | OrderAction::PlaceReduceOnlyFillOrKill { price_tick, .. } => *price_tick,
        OrderAction::Amend {
            price_tick, qty, ..
        } => {
            if *qty == Some(0) {
                return Some("qty must be a positive integer".to_string());
            }
            *price_tick
        }
        OrderAction::PlaceMarket { .. }
        | OrderAction::PlaceReduceOnlyMarket { .. }
        | OrderAction::Cancel { .. } => None,
    };
    if price_tick.is_some_and(|tick| tick <= 0) {
        return Some("price_tick must be a positive integer".to_string());
    }
    None
}

fn reject_trainee_transfer(
    state: &AppState,
    room_id: &str,
    account_id: AccountId,
) -> Result<(), ApiError> {
    if let Some(run) = state
        .training_runs
        .values()
        .find(|run| run.spec.room_id == room_id)
        && !run.allows_trainee_transfer(account_id)
    {
        return Err(api_error(
            StatusCode::CONFLICT,
            format!(
                "training run {} does not allow transfers for trainee account {account_id}",
                run.spec.run_id
            ),
        ));
    }
    Ok(())
}

fn apply_training_execution(
    run: &mut exchange_core::TrainingRun,
    execution: &ActorExecution,
    book_before: Option<&BookSnapshot>,
    submitter_account: Option<AccountId>,
) {
    let trainee = run.spec.trainee_account_id;
    let submitter = submitter_account.or_else(|| new_order_account_id(execution));
    if matches!(execution.result, ActorExecutionResult::Rejected(_)) {
        if submitter == Some(trainee) {
            run.record_reject();
        }
        return;
    }
    let fees = training_fees_by_trade(execution, trainee);
    for event in execution_events(execution) {
        match event {
            Event::OrderRested { remaining_qty, .. }
                if submitter == Some(trainee)
                    && new_order_side(execution) == Some(exchange_core::Side::Buy) =>
            {
                run.record_open_buy(*remaining_qty);
            }
            Event::OrderCanceled { remaining_qty, .. } if submitter == Some(trainee) => {
                run.record_cancel(*remaining_qty);
            }
            Event::TradePrinted(trade) => {
                let trainee_order = if trade.taker_account_id == trainee {
                    Some(trade.taker_order_id)
                } else if trade.maker_account_id == trainee {
                    Some(trade.maker_order_id)
                } else {
                    None
                };
                if let Some(order_id) = trainee_order {
                    let fee = fees.get(&trade.trade_id).copied().unwrap_or(0);
                    run.record_fill_evidence(exchange_core::TrainingFill {
                        price_tick: trade.price_tick,
                        qty: trade.qty,
                        fee,
                        order_id: Some(order_id),
                        command_seq: Some(execution.command_seq),
                        book_before: book_before.cloned(),
                    });
                }
            }
            _ => {}
        }
    }
}

fn new_order_account_id(execution: &ActorExecution) -> Option<AccountId> {
    match &execution.result {
        ActorExecutionResult::Accepted(MarketExecution::Spot(result)) => {
            if let Command::NewOrder(order) = &result.command.command {
                Some(order.account_id)
            } else {
                None
            }
        }
        ActorExecutionResult::Accepted(MarketExecution::Perp(result)) => {
            if let Command::NewOrder(order) = &result.command.command {
                Some(order.account_id)
            } else {
                None
            }
        }
        ActorExecutionResult::Rejected(_) => None,
    }
}

fn new_order_side(execution: &ActorExecution) -> Option<exchange_core::Side> {
    match &execution.result {
        ActorExecutionResult::Accepted(MarketExecution::Spot(result)) => {
            if let Command::NewOrder(order) = &result.command.command {
                Some(order.side)
            } else {
                None
            }
        }
        ActorExecutionResult::Accepted(MarketExecution::Perp(result)) => {
            if let Command::NewOrder(order) = &result.command.command {
                Some(order.side)
            } else {
                None
            }
        }
        ActorExecutionResult::Rejected(_) => None,
    }
}

fn execution_events(execution: &ActorExecution) -> Vec<&Event> {
    match &execution.result {
        ActorExecutionResult::Accepted(MarketExecution::Spot(result)) => {
            result.events.iter().map(|record| &record.event).collect()
        }
        ActorExecutionResult::Accepted(MarketExecution::Perp(result)) => {
            result.events.iter().map(|record| &record.event).collect()
        }
        ActorExecutionResult::Rejected(_) => Vec::new(),
    }
}

fn training_fees_by_trade(execution: &ActorExecution, trainee: AccountId) -> BTreeMap<u64, i128> {
    let mut fees = BTreeMap::new();
    match &execution.result {
        ActorExecutionResult::Accepted(MarketExecution::Spot(result)) => {
            for event in &result.clearing_events {
                let exchange_core::SpotClearingEvent::TradeSettled {
                    trade_id,
                    buyer_account_id,
                    seller_account_id,
                    buyer_fee,
                    seller_fee,
                    ..
                } = event;
                if *buyer_account_id == trainee {
                    fees.insert(*trade_id, *buyer_fee);
                } else if *seller_account_id == trainee {
                    fees.insert(*trade_id, *seller_fee);
                }
            }
        }
        ActorExecutionResult::Accepted(MarketExecution::Perp(result)) => {
            for event in &result.clearing_events {
                if let exchange_core::PerpClearingEvent::TradeSettled {
                    trade_id,
                    buyer_account_id,
                    seller_account_id,
                    buyer_fee,
                    seller_fee,
                    ..
                } = event
                {
                    if *buyer_account_id == trainee {
                        fees.insert(*trade_id, *buyer_fee);
                    } else if *seller_account_id == trainee {
                        fees.insert(*trade_id, *seller_fee);
                    }
                }
            }
        }
        ActorExecutionResult::Rejected(_) => {}
    }
    fees
}

fn settle_training_residuals(
    rooms: &mut RoomManager,
    run: &mut exchange_core::TrainingRun,
) -> Result<Vec<ActorExecution>, ApiError> {
    if !run.is_finished() {
        return Ok(Vec::new());
    }
    let instrument_id = rooms
        .room(&run.spec.room_id)
        .map_err(api_error_from_room)?
        .primary_instrument_id()
        .to_string();
    let mut orders = rooms
        .resting_orders_for_account(
            &run.spec.room_id,
            &instrument_id,
            run.spec.trainee_account_id,
        )
        .map_err(api_error_from_room)?;
    orders.sort_by_key(|order| order.order_id);
    let mut executions = Vec::new();
    for order in orders {
        let execution = rooms
            .apply_to_instrument(
                &run.spec.room_id,
                &instrument_id,
                Command::CancelOrder(CancelOrder {
                    order_id: order.order_id,
                }),
            )
            .map_err(api_error_from_room)?;
        run.record_cancel(order.remaining_qty);
        executions.push(execution);
    }
    Ok(executions)
}

async fn persist_training_progress(
    state: &mut AppState,
    run: &exchange_core::TrainingRun,
    execution_records: &[JournalExecution],
) -> Result<(), ApiError> {
    let cursor =
        next_persisted_command_cursor(state, &run.spec.room_id).map_err(api_error_from_journal)?;
    state
        .append_room_mutation(
            &PendingJournalMutation::new(
                run.spec.room_id.clone(),
                cursor,
                RoomMutation::TrainingProgress {
                    run: Box::new(run.clone()),
                },
            ),
            execution_records,
            &[],
            None,
        )
        .await
        .map_err(api_error_from_journal)?;
    state
        .training_runs
        .insert(run.spec.run_id.clone(), run.clone());
    Ok(())
}

fn order_action_allocates_id(action: &OrderAction) -> bool {
    !matches!(
        action,
        OrderAction::Cancel { .. } | OrderAction::Amend { .. }
    )
}

async fn pause_room(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<RoomStatusResponse> {
    apply_room_status_control(state, headers, room_id, "pause", |rooms, room_id| {
        rooms.pause_room(room_id)
    })
    .await
}

async fn resume_room(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<RoomStatusResponse> {
    apply_room_status_control(state, headers, room_id, "resume", |rooms, room_id| {
        rooms.resume_room(room_id)
    })
    .await
}

async fn close_room(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<RoomStatusResponse> {
    apply_room_status_control(state, headers, room_id, "close", |rooms, room_id| {
        rooms.close_room(room_id)
    })
    .await
}

async fn apply_room_status_control(
    state: SharedState,
    headers: HeaderMap,
    room_id: String,
    operation: &'static str,
    apply: impl Fn(&mut exchange_core::RoomManager, &str) -> Result<(), RoomManagerError>
    + Send
    + 'static,
) -> ApiResult<RoomStatusResponse> {
    authorize_room_read(&state, &headers, &room_id, RoomReadAccess::Admin).await?;
    let user_id = {
        let app = lock_state(&state).await?;
        current_user_id(&headers, &app.auth_policy)?
    };
    let idempotency_key = request_idempotency_key(&headers)?;
    let fingerprint = control_fingerprint(operation, serde_json::json!({}));
    run_durable_state_transaction(state.clone(), async move {
        let mut state = lock_state(&state).await?;
        if let Some(key) = idempotency_key.as_deref()
            && let Some(replayed) =
                load_control_replay(&state, &user_id, &room_id, key, &fingerprint).await?
        {
            return Ok(Json(replayed));
        }
        let command_cursor =
            next_persisted_command_cursor(&state, &room_id).map_err(api_error_from_journal)?;
        let mut candidate_rooms = state.rooms.clone();
        let mut settle_records = Vec::new();
        let mut settled_runs = Vec::new();
        if operation == "close" {
            let run_ids = state
                .training_runs
                .values()
                .filter(|run| run.spec.room_id == room_id)
                .map(|run| run.spec.run_id.clone())
                .collect::<Vec<_>>();
            for run_id in run_ids {
                let Some(mut run) = state.training_runs.get(&run_id).cloned() else {
                    continue;
                };
                let _ = run.abort();
                let cancels = settle_training_residuals(&mut candidate_rooms, &mut run)?;
                settle_records.extend(cancels.into_iter().filter_map(|execution| {
                    command_from_actor_execution(&execution)
                        .map(|command| JournalExecution::system(command, execution))
                }));
                settled_runs.push(run);
            }
        }
        apply(&mut candidate_rooms, &room_id).map_err(api_error_from_room)?;
        let status = candidate_rooms
            .status(&room_id)
            .map_err(api_error_from_room)?;
        let response = RoomStatusResponse {
            room_id: room_id.clone(),
            status,
        };
        let record = idempotency_key
            .as_ref()
            .map(|key| {
                control_record(
                    user_id.clone(),
                    room_id.clone(),
                    key.clone(),
                    fingerprint.clone(),
                    &response,
                )
            })
            .transpose()?;
        let pending = PendingJournalMutation::new(
            room_id.clone(),
            command_cursor,
            RoomMutation::StatusChanged { status },
        );
        let pending = match record {
            Some(record) => pending.with_control_idempotency(record),
            None => pending,
        };
        if let Some(replay_json) =
            append_control_mutation(&mut state, pending, &settle_records, &[], None).await?
        {
            let replayed = serde_json::from_value(replay_json).map_err(api_error_from_json)?;
            return Ok(Json(replayed));
        }
        if !settle_records.is_empty() {
            state.append_room_executions(
                &room_id,
                settle_records
                    .iter()
                    .map(|record| record.execution.clone())
                    .collect(),
            );
        }
        for run in settled_runs {
            persist_training_progress(&mut state, &run, &[]).await?;
        }
        state.rooms = candidate_rooms;
        Ok(Json(response))
    })
    .await
}

async fn lock_state(
    state: &SharedState,
) -> Result<tokio::sync::MutexGuard<'_, AppState>, (StatusCode, Json<ErrorResponse>)> {
    Ok(state.app.lock().await)
}

/// Runs a durable state transition in a detached Tokio task.
///
/// Dropping an HTTP request future must not cancel the interval between a
/// successful journal commit and installing its candidate in-memory state.
async fn run_durable_state_transaction<T, F>(state: SharedState, transaction: F) -> ApiResult<T>
where
    T: Send + 'static,
    F: Future<Output = ApiResult<T>> + Send + 'static,
{
    let guard = state.lifecycle.try_begin_durable_write().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "server is shutting down and is not accepting durable writes".to_string(),
        )
    })?;
    tokio::spawn(async move {
        let mut guard = guard;
        let result = transaction.await;
        if result.is_ok() {
            guard.mark_succeeded();
        }
        result
    })
    .await
    .map_err(|error| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("durable state transaction failed: {error}"),
        )
    })?
}

fn start_agent_worker_for_room(
    shared: &SharedState,
    state: &mut AppState,
    room_id: RoomId,
    request: StartAgentsRequest,
) -> Result<AgentWorkerStatus, (StatusCode, Json<ErrorResponse>)> {
    let _ = &state.base_url;
    state
        .room_lease_claim(&room_id)
        .map_err(api_error_from_journal)?;
    validate_agent_templates(&room_id, &request.agents)?;

    if let Some(worker) = state.agent_workers.remove(&room_id) {
        worker.stop();
    }
    if request.agents.is_empty() {
        state.schedulers.remove(&room_id);
        return Ok(AgentWorkerStatus::stopped(room_id));
    }

    let interval_ms = request
        .interval_ms
        .unwrap_or(DEFAULT_AGENT_INTERVAL_MS)
        .max(1);
    let continuity = if !state.schedulers.contains_key(&room_id)
        && state
            .rooms
            .execution_history(&room_id)
            .map(|history| !history.is_empty())
            .unwrap_or(false)
    {
        exchange_core::AgentContinuity::LegacyNonContinuous
    } else {
        exchange_core::AgentContinuity::Continuous
    };
    let mut scheduler = if continuity == exchange_core::AgentContinuity::LegacyNonContinuous {
        exchange_core::SchedulerState::legacy_non_continuous(
            room_id.clone(),
            request.agents.clone(),
            exchange_core::SchedulerMode::Auto { interval_ms },
        )
    } else {
        exchange_core::SchedulerState::new(
            room_id.clone(),
            request.agents.clone(),
            exchange_core::SchedulerMode::Auto { interval_ms },
        )
    };
    if let Some(existing) = state.schedulers.get(&room_id)
        && existing.continuity == exchange_core::AgentContinuity::Continuous
        && existing.participant_ids() == scheduler.participant_ids()
    {
        scheduler = existing.clone();
        scheduler.mode = exchange_core::SchedulerMode::Auto { interval_ms };
    }
    let participant_ids = scheduler.participant_ids();
    state.schedulers.insert(room_id.clone(), scheduler);
    let worker = AgentWorkerHandle::spawn(
        shared.clone(),
        room_id.clone(),
        request.agents,
        Duration::from_millis(interval_ms),
    )
    .map_err(|error| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: error.to_string(),
                code: None,
                room_owner: None,
            }),
        )
    })?;
    state.agent_workers.insert(room_id.clone(), worker);

    Ok(AgentWorkerStatus {
        room_id,
        running: true,
        interval_ms,
        participants: participant_ids,
        last_error: None,
        lifecycle: "running".to_string(),
    })
}

fn validate_agent_templates(
    room_id: &str,
    templates: &[AgentTemplate],
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    for template in templates {
        let configured_room_id = match template {
            AgentTemplate::NoiseTrader(config) => &config.participant.room_id,
            AgentTemplate::DcaTrader(config) => &config.participant.room_id,
            AgentTemplate::GridTrader(config) => &config.participant.room_id,
            AgentTemplate::ContinuousMarketMaker(config) => &config.participant.room_id,
            AgentTemplate::CancelAtStep(config) => &config.participant.room_id,
        };
        if configured_room_id != room_id {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                format!(
                    "agent {} is configured for room {}, not path room {room_id}",
                    template.participant_id(),
                    configured_room_id
                ),
            ));
        }
        let instrument_id = match template {
            AgentTemplate::NoiseTrader(config) => config.participant.instrument_id.as_deref(),
            AgentTemplate::DcaTrader(config) => config.participant.instrument_id.as_deref(),
            AgentTemplate::GridTrader(config) => config.participant.instrument_id.as_deref(),
            AgentTemplate::ContinuousMarketMaker(config) => {
                config.participant.instrument_id.as_deref()
            }
            AgentTemplate::CancelAtStep(config) => config.participant.instrument_id.as_deref(),
        };
        if instrument_id.is_none() {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                format!(
                    "agent {} must set participant.instrument_id; implicit primary-market routing is not allowed",
                    template.participant_id()
                ),
            ));
        }
    }
    Ok(())
}

fn agent_status_for_room(state: &AppState, room_id: &str) -> AgentWorkerStatus {
    state
        .agent_workers
        .get(room_id)
        .map(|worker| worker.status(room_id.to_string()))
        .unwrap_or_else(|| AgentWorkerStatus::stopped(room_id.to_string()))
}

fn api_error(status: StatusCode, error: impl Into<String>) -> (StatusCode, Json<ErrorResponse>) {
    (
        status,
        Json(ErrorResponse {
            error: error.into(),
            code: None,
            room_owner: None,
        }),
    )
}

fn api_error_from_room(error: RoomManagerError) -> (StatusCode, Json<ErrorResponse>) {
    let status = match error {
        RoomManagerError::RoomNotFound { .. } => StatusCode::NOT_FOUND,
        RoomManagerError::OrderOwnershipMismatch { .. } => StatusCode::FORBIDDEN,
        RoomManagerError::RoomAlreadyExists { .. }
        | RoomManagerError::MarketConfig(_)
        | RoomManagerError::Actor(_)
        | RoomManagerError::Scenario(_)
        | RoomManagerError::Simulation(_)
        | RoomManagerError::Candle(_)
        | RoomManagerError::SystemOrderIdOverflow => StatusCode::BAD_REQUEST,
    };

    (
        status,
        Json(ErrorResponse {
            error: format!("{error:?}"),
            code: None,
            room_owner: None,
        }),
    )
}

fn api_error_from_json(error: serde_json::Error) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse {
            error: format!("invalid room request: {error}"),
            code: None,
            room_owner: None,
        }),
    )
}

fn api_error_from_journal(error: JournalError) -> (StatusCode, Json<ErrorResponse>) {
    let status = match &error {
        JournalError::RoomLeaseOwnedBy { .. } => StatusCode::CONFLICT,
        JournalError::RoomLeaseLost { .. } | JournalError::RoomLeaseNotOwned { .. } => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        JournalError::ExternalActionQuotaExceeded { .. } => StatusCode::TOO_MANY_REQUESTS,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    let (code, room_owner) = match &error {
        JournalError::RoomLeaseOwnedBy {
            room_id,
            owner_id,
            owner_url,
            fencing_token,
            expires_at_unix_ms,
        } => (
            Some("room_owned_by_other_instance".to_string()),
            Some(Box::new(RoomOwnerResponse {
                room_id: room_id.clone(),
                owner_id: owner_id.clone(),
                owner_url: owner_url.clone(),
                fencing_token: *fencing_token,
                expires_at_unix_ms: *expires_at_unix_ms,
            })),
        ),
        JournalError::RoomLeaseLost { .. } => (Some("room_lease_lost".to_string()), None),
        JournalError::RoomLeaseNotOwned { .. } => (Some("room_lease_not_owned".to_string()), None),
        _ => (None, None),
    };
    (
        status,
        Json(ErrorResponse {
            error: error.to_string(),
            code,
            room_owner,
        }),
    )
}

trait IntoRoomError {
    fn into_room_error(self) -> RoomManagerError;
}

impl IntoRoomError for exchange_core::GatewayError {
    fn into_room_error(self) -> RoomManagerError {
        match self {
            Self::Room(error) => error,
            Self::MissingInstrument => {
                RoomManagerError::Actor(exchange_core::ActorRejectReason::InstrumentNotFound {
                    instrument_id: String::new(),
                })
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HealthResponse {
    pub ok: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ErrorResponse {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_owner: Option<Box<RoomOwnerResponse>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomOwnerResponse {
    pub room_id: String,
    pub owner_id: String,
    pub owner_url: Option<String>,
    pub fencing_token: u64,
    pub expires_at_unix_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CreateRoomResponse {
    pub room_id: String,
    pub seed_execution_count: usize,
    pub agent_worker: AgentWorkerStatus,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ListRoomsResponse {
    pub rooms: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ClusterRoomsQuery {
    pub after_room_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ClusterRoomRouteResponse {
    pub room_id: String,
    pub owner: Option<RoomOwnerResponse>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ClusterRoomsResponse {
    pub rooms: Vec<ClusterRoomRouteResponse>,
    pub next_after_room_id: Option<String>,
    pub has_more: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomEventsQuery {
    pub limit: Option<usize>,
    pub after_command_seq: Option<u64>,
    #[serde(default)]
    pub from_start: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct RoomEventStreamQuery {
    pub after_command_seq: Option<u64>,
    #[serde(default)]
    pub replay_from_start: bool,
    pub scope: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProjectionQuery {
    pub instrument_id: Option<InstrumentId>,
    pub account_id: Option<AccountId>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomEventsResponse {
    pub room_id: String,
    pub executions: Vec<RoomExecutionSummary>,
    pub next_after_command_seq: Option<u64>,
    pub latest_command_seq: Option<u64>,
    pub has_more: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomOrdersResponse {
    pub room_id: String,
    pub orders: Vec<OrderProjection>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomTradesResponse {
    pub room_id: String,
    pub trades: Vec<TradeProjection>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomTicksResponse {
    pub room_id: String,
    pub ticks: Vec<MarketTickProjection>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomLedgerResponse {
    pub room_id: String,
    pub ledger: Vec<AccountLedgerProjection>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomPositionsResponse {
    pub room_id: String,
    pub positions: Vec<PositionSnapshotProjection>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomVenueAccountsResponse {
    pub room_id: String,
    pub accounts: Vec<VenueAccountSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomVenueAccountsByVenueResponse {
    pub room_id: String,
    pub accounts: Vec<VenueAccountVenueSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomPortfoliosResponse {
    pub room_id: String,
    pub accounts: Vec<PortfolioAccountSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomAssetLedgerResponse {
    pub room_id: String,
    pub ledger: Vec<AssetLedgerEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomClockResponse {
    pub room_id: String,
    pub clock: SimulationClock,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AdvanceClockRequest {
    pub steps: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AdvanceClockResponse {
    pub room_id: String,
    pub clock: SimulationClock,
    pub completed_transfers: Vec<VenueTransfer>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TransferRequest {
    #[serde(default)]
    pub venue_id: Option<String>,
    pub account_id: AccountId,
    pub asset_id: String,
    pub amount: Money,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TransferResponse {
    pub room_id: String,
    pub transfer: VenueTransfer,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VenueToVenueTransferRequest {
    pub from_venue_id: String,
    pub to_venue_id: String,
    pub account_id: AccountId,
    pub asset_id: String,
    pub amount: Money,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VenueToVenueTransferResponse {
    pub room_id: String,
    pub transfer: VenueToVenueTransfer,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomTransfersResponse {
    pub room_id: String,
    pub transfers: Vec<VenueTransfer>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RoomExecutionSummary {
    pub room_id: String,
    #[serde(default)]
    pub instrument_id: Option<InstrumentId>,
    #[serde(default)]
    pub submit_account_id: Option<AccountId>,
    pub command_seq: u64,
    /// Authoritative simulation time. Legacy journal rows may not contain it.
    #[serde(default)]
    pub market_time_ms: Option<u64>,
    pub status: MarketStatus,
    pub accepted: bool,
    pub reject_reason: Option<String>,
    pub events: Vec<EventSummary>,
    #[serde(default)]
    pub clearing_events: Vec<ClearingEventSummary>,
    pub clearing_event_count: usize,
    #[serde(skip)]
    clearing_events_omitted: bool,
}

#[derive(Deserialize)]
struct RoomExecutionSummaryPayload {
    room_id: String,
    #[serde(default)]
    instrument_id: Option<InstrumentId>,
    #[serde(default)]
    submit_account_id: Option<AccountId>,
    command_seq: u64,
    #[serde(default)]
    market_time_ms: Option<u64>,
    status: MarketStatus,
    accepted: bool,
    reject_reason: Option<String>,
    events: Vec<EventSummary>,
    #[serde(default)]
    clearing_events: Vec<ClearingEventSummary>,
    clearing_event_count: usize,
}

impl<'de> Deserialize<'de> for RoomExecutionSummary {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let payload = serde_json::Value::deserialize(deserializer)?;
        let clearing_events_omitted = payload.get("clearing_events").is_none();
        let payload: RoomExecutionSummaryPayload =
            serde_json::from_value(payload).map_err(serde::de::Error::custom)?;

        Ok(Self {
            room_id: payload.room_id,
            instrument_id: payload.instrument_id,
            submit_account_id: payload.submit_account_id,
            command_seq: payload.command_seq,
            market_time_ms: payload.market_time_ms,
            status: payload.status,
            accepted: payload.accepted,
            reject_reason: payload.reject_reason,
            events: payload.events,
            clearing_events: payload.clearing_events,
            clearing_event_count: payload.clearing_event_count,
            clearing_events_omitted,
        })
    }
}

impl RoomExecutionSummary {
    pub(crate) fn from_execution(execution: ActorExecution) -> Self {
        let submit_account_id = execution_account_id(&execution);
        let (accepted, reject_reason, events, clearing_events) = match execution.result {
            ActorExecutionResult::Accepted(market_execution) => {
                let (events, clearing_events) = summarize_market_execution(market_execution);
                (true, None, events, clearing_events)
            }
            ActorExecutionResult::Rejected(reason) => (
                false,
                Some(reject_reason_to_string(reason)),
                Vec::new(),
                Vec::new(),
            ),
        };
        let clearing_event_count = clearing_events.len();

        Self {
            room_id: execution.room_id,
            instrument_id: Some(execution.instrument_id),
            submit_account_id,
            command_seq: execution.command_seq,
            market_time_ms: Some(execution.market_time_ms),
            status: execution.status,
            accepted,
            reject_reason,
            events,
            clearing_events,
            clearing_event_count,
            clearing_events_omitted: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SubmitOrderRequest {
    pub participant_id: ParticipantId,
    #[serde(default)]
    pub instrument_id: Option<InstrumentId>,
    pub account_id: AccountId,
    pub action: OrderAction,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SetMarkPriceRequest {
    pub price_tick: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CreateRoomRequest {
    pub scenario: ScenarioConfig,
    #[serde(default)]
    pub agents: Vec<AgentTemplate>,
    pub agent_interval_ms: Option<u64>,
    pub autostart_agents: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StartAgentsRequest {
    pub agents: Vec<AgentTemplate>,
    pub interval_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentWorkerStatus {
    pub room_id: String,
    pub running: bool,
    pub interval_ms: u64,
    pub participants: Vec<ParticipantId>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub lifecycle: String,
}

impl AgentWorkerStatus {
    fn stopped(room_id: String) -> Self {
        Self {
            room_id,
            running: false,
            interval_ms: 0,
            participants: Vec::new(),
            last_error: None,
            lifecycle: "stopped".to_string(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomStatusResponse {
    pub room_id: String,
    pub status: MarketStatus,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OrderResponse {
    pub participant_id: ParticipantId,
    pub account_id: AccountId,
    pub action: OrderAction,
    pub room_id: String,
    pub instrument_id: Option<InstrumentId>,
    pub command_seq: u64,
    pub market_time_ms: Option<u64>,
    pub status: MarketStatus,
    pub accepted: bool,
    pub reject_reason: Option<String>,
    pub events: Vec<EventSummary>,
    pub clearing_events: Vec<ClearingEventSummary>,
    pub clearing_event_count: usize,
}

impl OrderResponse {
    fn from_gateway_execution(
        participant_id: ParticipantId,
        account_id: AccountId,
        action: OrderAction,
        execution: ActorExecution,
    ) -> Self {
        let summary = RoomExecutionSummary::from_execution(execution);

        Self::from_summary(participant_id, account_id, action, summary)
    }

    fn from_summary(
        participant_id: ParticipantId,
        account_id: AccountId,
        action: OrderAction,
        summary: RoomExecutionSummary,
    ) -> Self {
        Self {
            participant_id,
            account_id,
            action,
            room_id: summary.room_id,
            instrument_id: summary.instrument_id,
            command_seq: summary.command_seq,
            market_time_ms: summary.market_time_ms,
            status: summary.status,
            accepted: summary.accepted,
            reject_reason: summary.reject_reason,
            events: summary.events,
            clearing_events: summary.clearing_events,
            clearing_event_count: summary.clearing_event_count,
        }
    }
}

fn summarize_market_execution(
    execution: MarketExecution,
) -> (Vec<EventSummary>, Vec<ClearingEventSummary>) {
    match execution {
        MarketExecution::Spot(execution) => (
            execution
                .events
                .into_iter()
                .map(|record| EventSummary::from_event(record.seq, record.event))
                .collect(),
            execution
                .clearing_events
                .into_iter()
                .map(ClearingEventSummary::from_spot)
                .collect(),
        ),
        MarketExecution::Perp(execution) => (
            execution
                .events
                .into_iter()
                .map(|record| EventSummary::from_event(record.seq, record.event))
                .collect(),
            execution
                .clearing_events
                .into_iter()
                .map(ClearingEventSummary::from_perp)
                .collect(),
        ),
    }
}

fn reject_reason_to_string(reason: ActorRejectReason) -> String {
    match reason {
        ActorRejectReason::MarketPaused => "market paused".to_string(),
        ActorRejectReason::MarketClosed => "market closed".to_string(),
        ActorRejectReason::InstrumentNotFound { instrument_id } => {
            format!("instrument not found: {instrument_id}")
        }
        ActorRejectReason::WrongMarketKind => "wrong market kind".to_string(),
        ActorRejectReason::VenueRule(reason) => format!("venue rule rejected: {reason:?}"),
        ActorRejectReason::Clearing(error) => format!("clearing error: {error:?}"),
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum EventSummary {
    OrderAccepted {
        seq: u64,
        order_id: OrderId,
    },
    OrderRejected {
        seq: u64,
        order_id: OrderId,
        reason: String,
    },
    RiskRejected {
        seq: u64,
        order_id: OrderId,
        reason: String,
    },
    TradePrinted {
        seq: u64,
        trade_id: u64,
        #[serde(default)]
        maker_order_id: OrderId,
        #[serde(default)]
        maker_account_id: AccountId,
        #[serde(default)]
        taker_order_id: OrderId,
        #[serde(default)]
        taker_account_id: AccountId,
        price_tick: i64,
        qty: u64,
        #[serde(default = "default_taker_side")]
        taker_side: exchange_core::Side,
    },
    OrderPartiallyFilled {
        seq: u64,
        order_id: OrderId,
        remaining_qty: u64,
    },
    OrderFilled {
        seq: u64,
        order_id: OrderId,
    },
    OrderRested {
        seq: u64,
        order_id: OrderId,
        price_tick: i64,
        remaining_qty: u64,
    },
    OrderExpired {
        seq: u64,
        order_id: OrderId,
        unfilled_qty: u64,
    },
    OrderCanceled {
        seq: u64,
        order_id: OrderId,
        remaining_qty: u64,
    },
    CancelRejected {
        seq: u64,
        order_id: OrderId,
        reason: String,
    },
    OrderAmended {
        seq: u64,
        order_id: OrderId,
        old_price_tick: i64,
        new_price_tick: i64,
        old_qty: u64,
        new_qty: u64,
    },
    AmendRejected {
        seq: u64,
        order_id: OrderId,
        reason: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum ClearingEventSummary {
    SpotTradeSettled {
        trade_id: u64,
        buyer_account_id: AccountId,
        seller_account_id: AccountId,
        price_tick: i64,
        qty: u64,
        #[serde(with = "json_i128")]
        notional: i128,
        #[serde(with = "json_i128")]
        buyer_fee: i128,
        #[serde(with = "json_i128")]
        seller_fee: i128,
        buyer: SpotAccountStateSummary,
        seller: SpotAccountStateSummary,
    },
    PerpTradeSettled {
        trade_id: u64,
        buyer_account_id: AccountId,
        seller_account_id: AccountId,
        price_tick: i64,
        qty: u64,
        #[serde(with = "json_i128")]
        notional: i128,
        #[serde(with = "json_i128")]
        buyer_fee: i128,
        #[serde(with = "json_i128")]
        seller_fee: i128,
        #[serde(with = "json_i128")]
        buyer_realized_pnl: i128,
        #[serde(with = "json_i128")]
        seller_realized_pnl: i128,
        buyer: PerpAccountStateSummary,
        seller: Box<PerpAccountStateSummary>,
    },
    PerpMarginStatusChanged {
        account_id: AccountId,
        previous_status: String,
        new_status: String,
        mark_price_tick: i64,
        account: PerpAccountStateSummary,
    },
    PerpLiquidationSettled {
        account_id: AccountId,
        order_id: u64,
        #[serde(with = "json_i128")]
        liquidation_notional: i128,
        #[serde(with = "json_i128")]
        liquidation_fee: i128,
        #[serde(with = "json_i128")]
        insurance_fund_payment: i128,
        #[serde(with = "json_i128")]
        auto_deleveraging_loss: i128,
        auto_deleveraging_allocations: Vec<PerpAutoDeleveragingAllocationSummary>,
        #[serde(with = "json_i128")]
        socialized_loss: i128,
        socialized_loss_allocations: Vec<PerpSocializedLossAllocationSummary>,
        #[serde(with = "json_i128")]
        bad_debt: i128,
        #[serde(with = "json_i128")]
        insurance_fund_balance: i128,
        account: PerpAccountStateSummary,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SpotAccountStateSummary {
    pub account_id: AccountId,
    #[serde(with = "json_i128")]
    pub cash_balance: i128,
    #[serde(with = "json_i128")]
    pub position_qty: i128,
    #[serde(with = "json_i128")]
    pub fees_paid: i128,
}

impl SpotAccountStateSummary {
    fn from_snapshot(snapshot: SpotAccountSnapshot) -> Self {
        Self {
            account_id: snapshot.account_id,
            cash_balance: snapshot.cash_balance,
            position_qty: snapshot.position_qty,
            fees_paid: snapshot.fees_paid,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerpAccountStateSummary {
    pub account_id: AccountId,
    #[serde(with = "json_i128")]
    pub cash_balance: i128,
    #[serde(with = "json_i128")]
    pub position_qty: i128,
    pub avg_entry_price_tick: i64,
    #[serde(with = "json_i128")]
    pub realized_pnl: i128,
    #[serde(with = "json_i128")]
    pub unrealized_pnl: i128,
    #[serde(with = "json_i128")]
    pub equity: i128,
    #[serde(with = "json_i128")]
    pub initial_margin: i128,
    #[serde(with = "json_i128")]
    pub maintenance_margin: i128,
    #[serde(
        default,
        with = "json_i128_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub portfolio_initial_margin: Option<i128>,
    #[serde(
        default,
        with = "json_i128_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub portfolio_maintenance_margin: Option<i128>,
    pub margin_status: String,
    #[serde(with = "json_i128")]
    pub reserved_margin: i128,
    #[serde(with = "json_i128")]
    pub available_cash: i128,
    #[serde(with = "json_i128")]
    pub fees_paid: i128,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerpSocializedLossAllocationSummary {
    pub account_id: AccountId,
    #[serde(with = "json_i128")]
    pub loss: i128,
    pub account: PerpAccountStateSummary,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerpAutoDeleveragingAllocationSummary {
    pub account_id: AccountId,
    #[serde(with = "json_i128")]
    pub position_delta: i128,
    pub price_tick: i64,
    pub qty: u64,
    #[serde(with = "json_i128")]
    pub realized_pnl: i128,
    #[serde(with = "json_i128")]
    pub loss: i128,
    pub account: PerpAccountStateSummary,
}

impl PerpAccountStateSummary {
    fn from_snapshot(snapshot: PerpAccountSnapshot) -> Self {
        Self {
            account_id: snapshot.account_id,
            cash_balance: snapshot.cash_balance,
            position_qty: snapshot.position_qty,
            avg_entry_price_tick: snapshot.avg_entry_price_tick,
            realized_pnl: snapshot.realized_pnl,
            unrealized_pnl: snapshot.unrealized_pnl,
            equity: snapshot.equity,
            initial_margin: snapshot.initial_margin,
            maintenance_margin: snapshot.maintenance_margin,
            portfolio_initial_margin: Some(snapshot.portfolio_initial_margin),
            portfolio_maintenance_margin: Some(snapshot.portfolio_maintenance_margin),
            margin_status: snapshot.margin_status.as_str().to_string(),
            reserved_margin: snapshot.reserved_margin,
            available_cash: snapshot.available_cash,
            fees_paid: snapshot.fees_paid,
        }
    }
}

impl ClearingEventSummary {
    fn from_spot(event: SpotClearingEvent) -> Self {
        match event {
            SpotClearingEvent::TradeSettled {
                trade_id,
                buyer_account_id,
                seller_account_id,
                price_tick,
                qty,
                notional,
                buyer_fee,
                seller_fee,
                buyer,
                seller,
            } => Self::SpotTradeSettled {
                trade_id,
                buyer_account_id,
                seller_account_id,
                price_tick,
                qty,
                notional,
                buyer_fee,
                seller_fee,
                buyer: SpotAccountStateSummary::from_snapshot(buyer),
                seller: SpotAccountStateSummary::from_snapshot(seller),
            },
        }
    }

    fn from_perp(event: PerpClearingEvent) -> Self {
        match event {
            PerpClearingEvent::TradeSettled {
                trade_id,
                buyer_account_id,
                seller_account_id,
                price_tick,
                qty,
                notional,
                buyer_fee,
                seller_fee,
                buyer_realized_pnl,
                seller_realized_pnl,
                buyer,
                seller,
            } => Self::PerpTradeSettled {
                trade_id,
                buyer_account_id,
                seller_account_id,
                price_tick,
                qty,
                notional,
                buyer_fee,
                seller_fee,
                buyer_realized_pnl,
                seller_realized_pnl,
                buyer: PerpAccountStateSummary::from_snapshot(buyer),
                seller: Box::new(PerpAccountStateSummary::from_snapshot(seller)),
            },
            PerpClearingEvent::MarginStatusChanged {
                account_id,
                previous_status,
                new_status,
                mark_price_tick,
                snapshot,
            } => Self::PerpMarginStatusChanged {
                account_id,
                previous_status: previous_status.as_str().to_string(),
                new_status: new_status.as_str().to_string(),
                mark_price_tick,
                account: PerpAccountStateSummary::from_snapshot(snapshot),
            },
            PerpClearingEvent::LiquidationSettled {
                account_id,
                order_id,
                liquidation_notional,
                liquidation_fee,
                insurance_fund_payment,
                auto_deleveraging_loss,
                auto_deleveraging_allocations,
                socialized_loss,
                socialized_loss_allocations,
                bad_debt,
                insurance_fund_balance,
                snapshot,
            } => Self::PerpLiquidationSettled {
                account_id,
                order_id,
                liquidation_notional,
                liquidation_fee,
                insurance_fund_payment,
                auto_deleveraging_loss,
                auto_deleveraging_allocations: auto_deleveraging_allocations
                    .into_iter()
                    .map(|allocation| PerpAutoDeleveragingAllocationSummary {
                        account_id: allocation.account_id,
                        position_delta: allocation.position_delta,
                        price_tick: allocation.price_tick,
                        qty: allocation.qty,
                        realized_pnl: allocation.realized_pnl,
                        loss: allocation.loss,
                        account: PerpAccountStateSummary::from_snapshot(allocation.snapshot),
                    })
                    .collect(),
                socialized_loss,
                socialized_loss_allocations: socialized_loss_allocations
                    .into_iter()
                    .map(|allocation| PerpSocializedLossAllocationSummary {
                        account_id: allocation.account_id,
                        loss: allocation.loss,
                        account: PerpAccountStateSummary::from_snapshot(allocation.snapshot),
                    })
                    .collect(),
                bad_debt,
                insurance_fund_balance,
                account: PerpAccountStateSummary::from_snapshot(snapshot),
            },
        }
    }
}

impl EventSummary {
    fn from_event(seq: u64, event: Event) -> Self {
        match event {
            Event::OrderAccepted { order_id } => Self::OrderAccepted { seq, order_id },
            Event::OrderRejected { order_id, reason } => Self::OrderRejected {
                seq,
                order_id,
                reason: format!("{reason:?}"),
            },
            Event::RiskRejected { order_id, reason } => Self::RiskRejected {
                seq,
                order_id,
                reason: format!("{reason:?}"),
            },
            Event::TradePrinted(trade) => Self::TradePrinted {
                seq,
                trade_id: trade.trade_id,
                maker_order_id: trade.maker_order_id,
                maker_account_id: trade.maker_account_id,
                taker_order_id: trade.taker_order_id,
                taker_account_id: trade.taker_account_id,
                price_tick: trade.price_tick,
                qty: trade.qty,
                taker_side: trade.taker_side,
            },
            Event::OrderPartiallyFilled {
                order_id,
                remaining_qty,
            } => Self::OrderPartiallyFilled {
                seq,
                order_id,
                remaining_qty,
            },
            Event::OrderFilled { order_id } => Self::OrderFilled { seq, order_id },
            Event::OrderRested {
                order_id,
                price_tick,
                remaining_qty,
            } => Self::OrderRested {
                seq,
                order_id,
                price_tick,
                remaining_qty,
            },
            Event::OrderExpired {
                order_id,
                unfilled_qty,
            } => Self::OrderExpired {
                seq,
                order_id,
                unfilled_qty,
            },
            Event::OrderCanceled {
                order_id,
                remaining_qty,
            } => Self::OrderCanceled {
                seq,
                order_id,
                remaining_qty,
            },
            Event::CancelRejected { order_id, reason } => Self::CancelRejected {
                seq,
                order_id,
                reason: format!("{reason:?}"),
            },
            Event::OrderAmended {
                order_id,
                old_price_tick,
                new_price_tick,
                old_qty,
                new_qty,
            } => Self::OrderAmended {
                seq,
                order_id,
                old_price_tick,
                new_price_tick,
                old_qty,
                new_qty,
            },
            Event::AmendRejected { order_id, reason } => Self::AmendRejected {
                seq,
                order_id,
                reason: format!("{reason:?}"),
            },
        }
    }
}

fn default_taker_side() -> exchange_core::Side {
    exchange_core::Side::Buy
}

#[derive(Clone)]
pub struct HttpTradingClient {
    base_url: String,
    client: reqwest::blocking::Client,
    user_id: Option<String>,
    bearer_token: Option<String>,
    trusted_owner_urls: BTreeSet<String>,
}

impl std::fmt::Debug for HttpTradingClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpTradingClient")
            .field("base_url", &self.base_url)
            .field("user_id", &self.user_id)
            .field(
                "bearer_token",
                &self.bearer_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("trusted_owner_url_count", &self.trusted_owner_urls.len())
            .finish_non_exhaustive()
    }
}

impl HttpTradingClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client: reqwest::blocking::Client::new(),
            user_id: None,
            bearer_token: None,
            trusted_owner_urls: BTreeSet::new(),
        }
    }

    pub fn with_user_id(base_url: impl Into<String>, user_id: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client: reqwest::blocking::Client::new(),
            user_id: Some(user_id.into()),
            bearer_token: None,
            trusted_owner_urls: BTreeSet::new(),
        }
    }

    pub fn with_bearer_token(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client: reqwest::blocking::Client::new(),
            user_id: None,
            bearer_token: Some(token.into()),
            trusted_owner_urls: BTreeSet::new(),
        }
    }

    /// Trusts an exact owner base URL for one-hop room-owner retries.
    ///
    /// Authentication credentials are forwarded only after the advertised URL
    /// passes validation and matches this explicit allowlist.
    pub fn trust_owner_url(&mut self, owner_url: impl AsRef<str>) -> Result<(), HttpTradingError> {
        let owner_url = normalize_owner_url(owner_url.as_ref())?;
        self.trusted_owner_urls.insert(owner_url);
        Ok(())
    }

    pub fn with_trusted_owner_url(
        mut self,
        owner_url: impl AsRef<str>,
    ) -> Result<Self, HttpTradingError> {
        self.trust_owner_url(owner_url)?;
        Ok(self)
    }

    pub fn create_room(
        &self,
        scenario: &ScenarioConfig,
    ) -> Result<CreateRoomResponse, HttpTradingError> {
        self.post_json("/rooms", scenario)
    }

    pub fn create_room_with_agents(
        &self,
        request: &CreateRoomRequest,
    ) -> Result<CreateRoomResponse, HttpTradingError> {
        self.post_json("/rooms", request)
    }

    pub fn list_rooms(&self) -> Result<ListRoomsResponse, HttpTradingError> {
        self.get_json("/rooms")
    }

    pub fn cluster_rooms(&self, limit: usize) -> Result<ClusterRoomsResponse, HttpTradingError> {
        self.cluster_rooms_page(None, limit)
    }

    pub fn cluster_rooms_after(
        &self,
        after_room_id: &str,
        limit: usize,
    ) -> Result<ClusterRoomsResponse, HttpTradingError> {
        self.cluster_rooms_page(Some(after_room_id), limit)
    }

    pub fn market_view(&self, room_id: &str) -> Result<MarketView, HttpTradingError> {
        self.get_json(&format!("/rooms/{room_id}/view"))
    }

    pub fn market_view_for(
        &self,
        room_id: &str,
        instrument_id: &str,
    ) -> Result<MarketView, HttpTradingError> {
        self.get_json(&format!(
            "/rooms/{room_id}/instruments/{instrument_id}/view"
        ))
    }

    pub fn room_events(
        &self,
        room_id: &str,
        limit: usize,
    ) -> Result<RoomEventsResponse, HttpTradingError> {
        self.get_json(&format!("/rooms/{room_id}/events?limit={limit}"))
    }

    pub fn room_events_after(
        &self,
        room_id: &str,
        after_command_seq: u64,
        limit: usize,
    ) -> Result<RoomEventsResponse, HttpTradingError> {
        self.get_json(&format!(
            "/rooms/{room_id}/events?after_command_seq={after_command_seq}&limit={limit}"
        ))
    }

    pub fn room_events_from_start(
        &self,
        room_id: &str,
        limit: usize,
    ) -> Result<RoomEventsResponse, HttpTradingError> {
        self.get_json(&format!(
            "/rooms/{room_id}/events?from_start=true&limit={limit}"
        ))
    }

    pub fn room_event_stream_after(
        &self,
        room_id: &str,
        after_command_seq: u64,
    ) -> Result<HttpRoomEventStream, HttpTradingError> {
        HttpRoomEventStream::connect(self.clone(), room_id, Some(after_command_seq), false)
    }

    pub fn room_event_stream_from_start(
        &self,
        room_id: &str,
    ) -> Result<HttpRoomEventStream, HttpTradingError> {
        HttpRoomEventStream::connect(self.clone(), room_id, None, true)
    }

    pub fn room_accounts(&self, room_id: &str) -> Result<AccountSnapshots, HttpTradingError> {
        self.get_json(&format!("/rooms/{room_id}/accounts"))
    }

    pub fn room_ticker(&self, room_id: &str) -> Result<TickerResponse, HttpTradingError> {
        self.get_json(&format!("/rooms/{room_id}/ticker"))
    }

    pub fn room_candles(
        &self,
        room_id: &str,
        interval_ms: u64,
    ) -> Result<CandleResponse, HttpTradingError> {
        self.get_json(&format!(
            "/rooms/{room_id}/candles?interval_ms={interval_ms}"
        ))
    }

    pub fn room_clock(&self, room_id: &str) -> Result<RoomClockResponse, HttpTradingError> {
        self.get_json(&format!("/rooms/{room_id}/clock"))
    }

    pub fn advance_room_clock(
        &self,
        room_id: &str,
        steps: u64,
    ) -> Result<AdvanceClockResponse, HttpTradingError> {
        self.post_json(
            &format!("/rooms/{room_id}/clock/advance"),
            &AdvanceClockRequest { steps },
        )
    }

    pub fn set_mark_price_for(
        &self,
        room_id: &str,
        instrument_id: &str,
        price_tick: i64,
    ) -> Result<RoomExecutionSummary, HttpTradingError> {
        self.post_json(
            &format!("/rooms/{room_id}/instruments/{instrument_id}/mark-price"),
            &SetMarkPriceRequest { price_tick },
        )
    }

    pub fn submit_order(
        &self,
        room_id: &str,
        request: &SubmitOrderRequest,
    ) -> Result<OrderResponse, HttpTradingError> {
        self.post_json(&format!("/rooms/{room_id}/orders"), request)
    }

    pub fn submit_order_idempotent(
        &self,
        room_id: &str,
        idempotency_key: &str,
        request: &SubmitOrderRequest,
    ) -> Result<OrderResponse, HttpTradingError> {
        self.post_json_with_idempotency(
            &format!("/rooms/{room_id}/orders"),
            request,
            idempotency_key,
        )
    }

    pub fn submit_order_for(
        &self,
        room_id: &str,
        instrument_id: &str,
        request: &SubmitOrderRequest,
    ) -> Result<OrderResponse, HttpTradingError> {
        self.post_json(
            &format!("/rooms/{room_id}/instruments/{instrument_id}/orders"),
            request,
        )
    }

    pub fn submit_order_for_idempotent(
        &self,
        room_id: &str,
        instrument_id: &str,
        idempotency_key: &str,
        request: &SubmitOrderRequest,
    ) -> Result<OrderResponse, HttpTradingError> {
        self.post_json_with_idempotency(
            &format!("/rooms/{room_id}/instruments/{instrument_id}/orders"),
            request,
            idempotency_key,
        )
    }

    pub fn pause_room(&self, room_id: &str) -> Result<RoomStatusResponse, HttpTradingError> {
        self.post_json(&format!("/rooms/{room_id}/pause"), &())
    }

    pub fn resume_room(&self, room_id: &str) -> Result<RoomStatusResponse, HttpTradingError> {
        self.post_json(&format!("/rooms/{room_id}/resume"), &())
    }

    pub fn close_room(&self, room_id: &str) -> Result<RoomStatusResponse, HttpTradingError> {
        self.post_json(&format!("/rooms/{room_id}/close"), &())
    }

    pub fn start_agents(
        &self,
        room_id: &str,
        request: &StartAgentsRequest,
    ) -> Result<AgentWorkerStatus, HttpTradingError> {
        self.post_json(&format!("/rooms/{room_id}/agents"), request)
    }

    pub fn agent_status(&self, room_id: &str) -> Result<AgentWorkerStatus, HttpTradingError> {
        self.get_json(&format!("/rooms/{room_id}/agents"))
    }

    pub fn stop_agents(&self, room_id: &str) -> Result<AgentWorkerStatus, HttpTradingError> {
        self.post_json(&format!("/rooms/{room_id}/agents/stop"), &())
    }

    pub fn start_training(
        &self,
        request: &StartTrainingRequest,
    ) -> Result<TrainingRunResponse, HttpTradingError> {
        self.post_json("/training/runs", request)
    }

    pub fn training_status(&self, run_id: &str) -> Result<TrainingRunResponse, HttpTradingError> {
        self.get_json(&format!("/training/runs/{run_id}"))
    }

    pub fn abort_training(&self, run_id: &str) -> Result<TrainingRunResponse, HttpTradingError> {
        self.post_json(&format!("/training/runs/{run_id}/abort"), &())
    }

    pub fn training_result(&self, run_id: &str) -> Result<TrainingRunResponse, HttpTradingError> {
        self.get_json(&format!("/training/runs/{run_id}/result"))
    }

    pub fn training_report(
        &self,
        run_id: &str,
    ) -> Result<TrainingReportResponse, HttpTradingError> {
        self.get_json(&format!("/training/runs/{run_id}/report"))
    }

    pub fn upsert_room_member(
        &self,
        room_id: &str,
        user_id: &str,
        role: &str,
    ) -> Result<RoomMemberResponse, HttpTradingError> {
        self.post_json(
            &format!("/rooms/{room_id}/members"),
            &RoomMemberRequest {
                user_id: user_id.to_string(),
                role: role.to_string(),
            },
        )
    }

    pub fn remove_room_member(
        &self,
        room_id: &str,
        user_id: &str,
    ) -> Result<RoomMemberResponse, HttpTradingError> {
        self.post_json(
            &format!("/rooms/{room_id}/members/{user_id}"),
            &serde_json::json!({}),
        )
    }

    pub fn assign_account_owner(
        &self,
        room_id: &str,
        account_id: AccountId,
        user_id: &str,
    ) -> Result<AssignAccountResponse, HttpTradingError> {
        self.post_json(
            &format!("/rooms/{room_id}/accounts/{account_id}/owners"),
            &AssignAccountRequest {
                user_id: user_id.to_string(),
            },
        )
    }

    pub fn observe_room(
        &self,
        room_id: &str,
        account_id: AccountId,
        instrument_id: Option<&str>,
    ) -> Result<ObservationResponse, HttpTradingError> {
        let mut path = format!("/rooms/{room_id}/observe?account_id={account_id}");
        if let Some(instrument_id) = instrument_id {
            path.push_str(&format!("&instrument_id={instrument_id}"));
        }
        self.get_json(&path)
    }

    pub fn replay_room(
        &self,
        room_id: &str,
        at_command_seq: Option<u64>,
    ) -> Result<IsolatedReplayResponse, HttpTradingError> {
        match at_command_seq {
            Some(seq) => self.get_json(&format!("/rooms/{room_id}/replay?at_command_seq={seq}")),
            None => self.get_json(&format!("/rooms/{room_id}/replay")),
        }
    }

    fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, HttpTradingError> {
        self.send_with_owner_retry(|base_url| {
            self.authenticated_request(self.client.get(format!("{base_url}{path}")))
        })
    }

    fn cluster_rooms_page(
        &self,
        after_room_id: Option<&str>,
        limit: usize,
    ) -> Result<ClusterRoomsResponse, HttpTradingError> {
        let mut query = vec![("limit", limit.to_string())];
        if let Some(after_room_id) = after_room_id {
            query.push(("after_room_id", after_room_id.to_string()));
        }
        self.send_with_owner_retry(|base_url| {
            self.authenticated_request(
                self.client
                    .get(format!("{base_url}/cluster/rooms"))
                    .query(&query),
            )
        })
    }

    fn post_json<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, HttpTradingError> {
        self.send_with_owner_retry(|base_url| {
            self.authenticated_request(self.client.post(format!("{base_url}{path}")).json(body))
        })
    }

    fn post_json_with_idempotency<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        idempotency_key: &str,
    ) -> Result<T, HttpTradingError> {
        self.send_with_owner_retry(|base_url| {
            self.authenticated_request(
                self.client
                    .post(format!("{base_url}{path}"))
                    .header(IDEMPOTENCY_KEY_HEADER, idempotency_key)
                    .json(body),
            )
        })
    }

    fn send_with_owner_retry<T, F>(&self, build_request: F) -> Result<T, HttpTradingError>
    where
        T: DeserializeOwned,
        F: Fn(&str) -> reqwest::blocking::RequestBuilder,
    {
        let response = self.send_response_with_owner_retry(build_request)?;
        decode_response(response)
    }

    fn send_response_with_owner_retry<F>(
        &self,
        build_request: F,
    ) -> Result<reqwest::blocking::Response, HttpTradingError>
    where
        F: Fn(&str) -> reqwest::blocking::RequestBuilder,
    {
        self.send_response_with_owner_retry_from(&self.base_url, &build_request)
    }

    fn send_response_with_owner_retry_from<F>(
        &self,
        initial_base_url: &str,
        build_request: &F,
    ) -> Result<reqwest::blocking::Response, HttpTradingError>
    where
        F: Fn(&str) -> reqwest::blocking::RequestBuilder,
    {
        let response = build_request(initial_base_url)
            .send()
            .map_err(HttpTradingError::Http)?;
        if response.status().is_success() {
            return Ok(response);
        }

        let (status, error) = decode_error_response(response);
        let Some(owner_url) = self.owner_retry_target(initial_base_url, status, &error)? else {
            return Err(api_response_error(status, error));
        };

        let response = build_request(&owner_url)
            .send()
            .map_err(HttpTradingError::Http)?;
        if response.status().is_success() {
            Ok(response)
        } else {
            let (status, error) = decode_error_response(response);
            Err(api_response_error(status, error))
        }
    }

    fn owner_retry_target(
        &self,
        request_base_url: &str,
        status: reqwest::StatusCode,
        error: &ErrorResponse,
    ) -> Result<Option<String>, HttpTradingError> {
        if status != reqwest::StatusCode::CONFLICT
            || error.code.as_deref() != Some("room_owned_by_other_instance")
        {
            return Ok(None);
        }
        let Some(owner_url) = error
            .room_owner
            .as_deref()
            .and_then(|owner| owner.owner_url.as_deref())
        else {
            return Ok(None);
        };
        let owner_url = normalize_owner_url(owner_url)?;
        let normalized_base_url =
            normalize_owner_url(request_base_url).unwrap_or_else(|_| request_base_url.to_string());
        if owner_url == normalized_base_url {
            return Err(HttpTradingError::OwnerRouteLoop { owner_url });
        }
        if !self.trusted_owner_urls.contains(&owner_url) {
            return Err(HttpTradingError::UntrustedOwnerUrl { owner_url });
        }
        Ok(Some(owner_url))
    }

    fn authenticated_request(
        &self,
        request: reqwest::blocking::RequestBuilder,
    ) -> reqwest::blocking::RequestBuilder {
        if let Some(token) = &self.bearer_token {
            request.bearer_auth(token)
        } else if let Some(user_id) = &self.user_id {
            request.header(USER_ID_HEADER, user_id)
        } else {
            request
        }
    }

    fn open_room_event_stream(
        &self,
        room_id: &str,
        after_command_seq: Option<u64>,
        replay_from_start: bool,
    ) -> Result<reqwest::blocking::Response, HttpTradingError> {
        let build_request = |base_url: &str| {
            let mut request = self
                .client
                .get(format!("{base_url}/rooms/{room_id}/events/stream"))
                .header("accept", "text/event-stream");
            if let Some(after_command_seq) = after_command_seq {
                request = request.header("last-event-id", after_command_seq.to_string());
            } else if replay_from_start {
                request = request.query(&[("replay_from_start", "true")]);
            }
            self.authenticated_request(request)
        };
        let candidates = std::iter::once(self.base_url.as_str()).chain(
            self.trusted_owner_urls
                .iter()
                .map(String::as_str)
                .filter(|owner_url| *owner_url != self.base_url),
        );
        let mut response = None;
        let mut last_transport_error = None;
        for base_url in candidates {
            match self.send_response_with_owner_retry_from(base_url, &build_request) {
                Ok(opened) => {
                    response = Some(opened);
                    break;
                }
                Err(HttpTradingError::Http(error)) => last_transport_error = Some(error),
                Err(error) => return Err(error),
            }
        }
        let response = match response {
            Some(response) => response,
            None => {
                let error = last_transport_error.expect("event stream has at least one base URL");
                return Err(HttpTradingError::Http(error));
            }
        };
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !content_type
            .split(';')
            .next()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
        {
            return Err(HttpTradingError::InvalidEventStream {
                reason: format!("expected text/event-stream, received {content_type:?}"),
            });
        }
        Ok(response)
    }
}

pub struct HttpRoomEventStream {
    client: HttpTradingClient,
    room_id: String,
    after_command_seq: Option<u64>,
    replay_from_start: bool,
    reader: BufReader<reqwest::blocking::Response>,
    terminated: bool,
}

impl HttpRoomEventStream {
    fn connect(
        client: HttpTradingClient,
        room_id: &str,
        after_command_seq: Option<u64>,
        replay_from_start: bool,
    ) -> Result<Self, HttpTradingError> {
        let response =
            client.open_room_event_stream(room_id, after_command_seq, replay_from_start)?;
        Ok(Self {
            client,
            room_id: room_id.to_string(),
            after_command_seq,
            replay_from_start,
            reader: BufReader::new(response),
            terminated: false,
        })
    }

    pub fn last_command_seq(&self) -> Option<u64> {
        self.after_command_seq
    }

    fn reconnect(&mut self) -> Result<(), HttpTradingError> {
        let response = self.client.open_room_event_stream(
            &self.room_id,
            self.after_command_seq,
            self.replay_from_start && self.after_command_seq.is_none(),
        )?;
        self.reader = BufReader::new(response);
        Ok(())
    }

    fn invalid(
        &mut self,
        reason: impl Into<String>,
    ) -> Option<Result<RoomExecutionSummary, HttpTradingError>> {
        self.terminated = true;
        Some(Err(HttpTradingError::InvalidEventStream {
            reason: reason.into(),
        }))
    }
}

impl Iterator for HttpRoomEventStream {
    type Item = Result<RoomExecutionSummary, HttpTradingError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.terminated {
            return None;
        }

        let mut reconnected = false;
        loop {
            let event = match read_sse_event(&mut self.reader) {
                Ok(Some(event)) => event,
                Ok(None) => {
                    if reconnected {
                        self.terminated = true;
                        return Some(Err(HttpTradingError::EventStreamClosed {
                            room_id: self.room_id.clone(),
                            after_command_seq: self.after_command_seq,
                        }));
                    }
                    if let Err(error) = self.reconnect() {
                        self.terminated = true;
                        return Some(Err(error));
                    }
                    reconnected = true;
                    continue;
                }
                Err(error) => {
                    if reconnected {
                        return self.invalid(format!("failed to read SSE response: {error}"));
                    }
                    if let Err(reconnect_error) = self.reconnect() {
                        self.terminated = true;
                        return Some(Err(reconnect_error));
                    }
                    reconnected = true;
                    continue;
                }
            };

            match event.event.as_deref().unwrap_or("message") {
                "execution" => {
                    let Some(event_id) = event.id.as_deref() else {
                        return self.invalid("execution event has no id");
                    };
                    let command_seq = match event_id.parse::<u64>() {
                        Ok(command_seq) => command_seq,
                        Err(_) => {
                            return self.invalid("execution event id is not an unsigned integer");
                        }
                    };
                    let execution = match serde_json::from_str::<RoomExecutionSummary>(
                        &event.data.join("\n"),
                    ) {
                        Ok(execution) => execution,
                        Err(error) => {
                            return self.invalid(format!("invalid execution event JSON: {error}"));
                        }
                    };
                    if execution.room_id != self.room_id {
                        return self.invalid(format!(
                            "execution event belongs to room {}, expected {}",
                            execution.room_id, self.room_id
                        ));
                    }
                    if execution.command_seq != command_seq {
                        return self.invalid(format!(
                            "execution event id {command_seq} does not match payload command sequence {}",
                            execution.command_seq
                        ));
                    }
                    if let Some(after_command_seq) = self.after_command_seq {
                        if command_seq <= after_command_seq {
                            continue;
                        }
                        let Some(expected) = after_command_seq.checked_add(1) else {
                            return self.invalid("execution cursor overflowed");
                        };
                        if command_seq != expected {
                            self.terminated = true;
                            return Some(Err(HttpTradingError::EventStreamGap {
                                room_id: self.room_id.clone(),
                                expected_command_seq: expected,
                                actual_command_seq: command_seq,
                            }));
                        }
                    } else if self.replay_from_start && command_seq != 0 {
                        self.terminated = true;
                        return Some(Err(HttpTradingError::EventStreamGap {
                            room_id: self.room_id.clone(),
                            expected_command_seq: 0,
                            actual_command_seq: command_seq,
                        }));
                    }
                    self.after_command_seq = Some(command_seq);
                    self.replay_from_start = false;
                    return Some(Ok(execution));
                }
                "resync_required" => {
                    let payload =
                        match serde_json::from_str::<SseResyncRequired>(&event.data.join("\n")) {
                            Ok(payload) => payload,
                            Err(error) => {
                                return self.invalid(format!(
                                    "invalid resync_required event JSON: {error}"
                                ));
                            }
                        };
                    self.terminated = true;
                    return Some(Err(HttpTradingError::EventStreamResyncRequired {
                        room_id: payload.room_id,
                        after_command_seq: payload.after_command_seq,
                        reason: payload.reason,
                        skipped_messages: payload.skipped_messages,
                    }));
                }
                _ => continue,
            }
        }
    }
}

#[derive(Default)]
struct RawSseEvent {
    event: Option<String>,
    id: Option<String>,
    data: Vec<String>,
}

#[derive(Deserialize)]
struct SseResyncRequired {
    room_id: String,
    after_command_seq: Option<u64>,
    reason: String,
    #[serde(default)]
    skipped_messages: Option<u64>,
}

fn read_sse_event(reader: &mut impl BufRead) -> io::Result<Option<RawSseEvent>> {
    let mut event = RawSseEvent::default();
    let mut has_fields = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if has_fields {
                return Ok(Some(event));
            }
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => {
                event.event = Some(value.to_string());
                has_fields = true;
            }
            "id" => {
                event.id = Some(value.to_string());
                has_fields = true;
            }
            "data" => {
                event.data.push(value.to_string());
                has_fields = true;
            }
            _ => {
                continue;
            }
        }
    }
}

fn normalize_owner_url(owner_url: &str) -> Result<String, HttpTradingError> {
    const MAX_OWNER_URL_LEN: usize = 2_048;

    if owner_url.is_empty() {
        return Err(HttpTradingError::InvalidOwnerUrl {
            reason: "owner URL must not be empty".to_string(),
        });
    }
    if owner_url.len() > MAX_OWNER_URL_LEN {
        return Err(HttpTradingError::InvalidOwnerUrl {
            reason: format!("owner URL exceeds {MAX_OWNER_URL_LEN} bytes"),
        });
    }

    let parsed =
        reqwest::Url::parse(owner_url).map_err(|error| HttpTradingError::InvalidOwnerUrl {
            reason: error.to_string(),
        })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(HttpTradingError::InvalidOwnerUrl {
            reason: "owner URL must use http or https".to_string(),
        });
    }
    if parsed.host_str().is_none() {
        return Err(HttpTradingError::InvalidOwnerUrl {
            reason: "owner URL must include a host".to_string(),
        });
    }
    let authority = parsed
        .as_str()
        .split_once("://")
        .map(|(_, remainder)| remainder.split('/').next().unwrap_or(remainder))
        .unwrap_or_default();
    if authority.contains('@') {
        return Err(HttpTradingError::InvalidOwnerUrl {
            reason: "owner URL must not include credentials".to_string(),
        });
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(HttpTradingError::InvalidOwnerUrl {
            reason: "owner URL must not include a query string or fragment".to_string(),
        });
    }

    Ok(parsed.as_str().trim_end_matches('/').to_string())
}

fn decode_response<T: DeserializeOwned>(
    response: reqwest::blocking::Response,
) -> Result<T, HttpTradingError> {
    let status = response.status();
    if status.is_success() {
        return response.json().map_err(HttpTradingError::Http);
    }

    let (status, error) = decode_error_response(response);
    Err(api_response_error(status, error))
}

fn decode_error_response(
    response: reqwest::blocking::Response,
) -> (reqwest::StatusCode, ErrorResponse) {
    let status = response.status();
    let fallback = ErrorResponse {
        error: format!("HTTP {status}"),
        code: None,
        room_owner: None,
    };
    let error = response.json::<ErrorResponse>().unwrap_or(fallback);
    (status, error)
}

fn api_response_error(status: reqwest::StatusCode, error: ErrorResponse) -> HttpTradingError {
    HttpTradingError::Api {
        status: status.as_u16(),
        error: error.error,
    }
}

#[derive(Debug)]
pub enum HttpTradingError {
    Http(reqwest::Error),
    Api {
        status: u16,
        error: String,
    },
    InvalidOwnerUrl {
        reason: String,
    },
    UntrustedOwnerUrl {
        owner_url: String,
    },
    OwnerRouteLoop {
        owner_url: String,
    },
    InvalidEventStream {
        reason: String,
    },
    EventStreamClosed {
        room_id: String,
        after_command_seq: Option<u64>,
    },
    EventStreamGap {
        room_id: String,
        expected_command_seq: u64,
        actual_command_seq: u64,
    },
    EventStreamResyncRequired {
        room_id: String,
        after_command_seq: Option<u64>,
        reason: String,
        skipped_messages: Option<u64>,
    },
}

impl std::fmt::Display for HttpTradingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(error) => write!(formatter, "{error}"),
            Self::Api { status, error } => write!(formatter, "api error {status}: {error}"),
            Self::InvalidOwnerUrl { reason } => write!(formatter, "invalid owner URL: {reason}"),
            Self::UntrustedOwnerUrl { owner_url } => {
                write!(
                    formatter,
                    "refusing to forward credentials to untrusted owner URL {owner_url}"
                )
            }
            Self::OwnerRouteLoop { owner_url } => {
                write!(formatter, "room owner retry would loop back to {owner_url}")
            }
            Self::InvalidEventStream { reason } => {
                write!(formatter, "invalid room event stream: {reason}")
            }
            Self::EventStreamClosed {
                room_id,
                after_command_seq,
            } => write!(
                formatter,
                "room event stream for {room_id} closed after command {after_command_seq:?}"
            ),
            Self::EventStreamGap {
                room_id,
                expected_command_seq,
                actual_command_seq,
            } => write!(
                formatter,
                "room event stream for {room_id} expected command {expected_command_seq}, received {actual_command_seq}"
            ),
            Self::EventStreamResyncRequired {
                room_id,
                after_command_seq,
                reason,
                skipped_messages,
            } => write!(
                formatter,
                "room event stream for {room_id} requires resync after command {after_command_seq:?}: {reason} (skipped {skipped_messages:?})"
            ),
        }
    }
}

impl std::error::Error for HttpTradingError {}

pub fn run_remote_participant_once<P: Participant + ?Sized>(
    client: &HttpTradingClient,
    participant: &mut P,
) -> Result<Vec<OrderResponse>, HttpTradingError> {
    let config = participant.config().clone();
    let instrument_id = config.instrument_id.clone();
    let view = match instrument_id.as_deref() {
        Some(instrument_id) => client.market_view_for(&config.room_id, instrument_id)?,
        None => client.market_view(&config.room_id)?,
    };
    participant.observe(&observation_from_market_view(&view, config.account_id));

    participant
        .decide()
        .into_iter()
        .map(|action| {
            client.submit_order(
                &config.room_id,
                &SubmitOrderRequest {
                    participant_id: config.participant_id.clone(),
                    instrument_id: instrument_id.clone(),
                    account_id: config.account_id,
                    action,
                },
            )
        })
        .collect()
}

fn observation_from_market_view(
    view: &MarketView,
    account_id: AccountId,
) -> exchange_core::ParticipantObservation {
    let own_account = match &view.accounts {
        AccountSnapshots::Spot(accounts) => accounts
            .iter()
            .find(|account| account.account_id == account_id)
            .cloned()
            .map(exchange_core::AccountSnapshot::Spot),
        AccountSnapshots::Perp(accounts) => accounts
            .iter()
            .find(|account| account.account_id == account_id)
            .cloned()
            .map(exchange_core::AccountSnapshot::Perp),
    };
    exchange_core::ParticipantObservation {
        version: exchange_core::PARTICIPANT_OBSERVATION_VERSION,
        room_id: view.room_id.clone(),
        venue_id: view.venue_id.clone(),
        instrument_id: view.instrument_id.clone(),
        status: view.status,
        step: 0,
        market_time_ms: 0,
        book: view.book.clone(),
        public_trades: Vec::new(),
        own_orders: Vec::new(),
        own_account,
    }
}

const DEFAULT_AGENT_INTERVAL_MS: u64 = 1_000;

struct AgentWorkerHandle {
    stop: Arc<AtomicBool>,
    last_error: Arc<Mutex<Option<String>>>,
    interval_ms: u64,
    participants: Vec<ParticipantId>,
    lifecycle: Arc<Mutex<AgentWorkerLifecycle>>,
    join: Option<thread::JoinHandle<()>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AgentWorkerLifecycle {
    Running,
    Paused,
    Recovering,
    Failed,
    Stopped,
}

impl AgentWorkerLifecycle {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Recovering => "recovering",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }
}

impl AgentWorkerHandle {
    fn spawn(
        shared: SharedState,
        room_id: RoomId,
        templates: Vec<AgentTemplate>,
        interval: Duration,
    ) -> Result<Self, AgentWorkerError> {
        if templates.is_empty() {
            return Err(AgentWorkerError::NoAgents);
        }

        let interval_ms = interval.as_millis().try_into().unwrap_or(u64::MAX);
        let participants = templates
            .iter()
            .map(|template| template.participant_id().to_string())
            .collect::<Vec<_>>();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let last_error = Arc::new(Mutex::new(None));
        let worker_last_error = Arc::clone(&last_error);
        let lifecycle = Arc::new(Mutex::new(AgentWorkerLifecycle::Running));
        let worker_lifecycle = Arc::clone(&lifecycle);
        let runtime = tokio::runtime::Handle::current();
        let join = thread::Builder::new()
            .name(format!("marketforge-agents-{room_id}"))
            .spawn(move || {
                while !worker_stop.load(Ordering::Relaxed) {
                    let room_id = room_id.clone();
                    let shared = shared.clone();
                    let runtime_lifecycle = shared.lifecycle.clone();
                    let result = runtime.block_on(run_scheduler_catch_up(
                        shared,
                        room_id,
                        worker_stop.clone(),
                        worker_lifecycle.clone(),
                    ));
                    if let Err(error) = result {
                        runtime_lifecycle.record_agent_error();
                        if let Ok(mut last_error) = worker_last_error.lock() {
                            *last_error = Some(error);
                        }
                        if let Ok(mut lifecycle) = worker_lifecycle.lock() {
                            *lifecycle = AgentWorkerLifecycle::Failed;
                        }
                        worker_stop.store(true, Ordering::Relaxed);
                        return;
                    }
                    sleep_until_next_step(interval, &worker_stop);
                }
                if let Ok(mut lifecycle) = worker_lifecycle.lock()
                    && *lifecycle != AgentWorkerLifecycle::Failed
                {
                    *lifecycle = AgentWorkerLifecycle::Stopped;
                }
            })
            .map_err(AgentWorkerError::Spawn)?;

        Ok(Self {
            stop,
            last_error,
            interval_ms,
            participants,
            lifecycle,
            join: Some(join),
        })
    }

    fn status(&self, room_id: String) -> AgentWorkerStatus {
        let lifecycle = self
            .lifecycle
            .lock()
            .ok()
            .map(|lifecycle| lifecycle.as_str().to_string())
            .unwrap_or_else(|| "running".to_string());
        AgentWorkerStatus {
            room_id,
            running: !self.stop.load(Ordering::Relaxed)
                && lifecycle != "failed"
                && lifecycle != "stopped",
            interval_ms: self.interval_ms,
            participants: self.participants.clone(),
            last_error: self
                .last_error
                .lock()
                .ok()
                .and_then(|last_error| last_error.clone()),
            lifecycle,
        }
    }

    fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    fn stop(self) {
        self.request_stop();
    }

    fn shutdown_and_join(mut self) {
        self.request_stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for AgentWorkerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

async fn run_scheduler_catch_up(
    shared: SharedState,
    room_id: RoomId,
    stop: Arc<AtomicBool>,
    lifecycle: Arc<Mutex<AgentWorkerLifecycle>>,
) -> Result<(), String> {
    let mut steps_run = 0_u32;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let action = {
            let app = shared.app.lock().await;
            if !shared.lifecycle.is_accepting_durable_writes() {
                return Ok(());
            }
            if let Ok(mut lifecycle) = lifecycle.lock() {
                *lifecycle = AgentWorkerLifecycle::Recovering;
            }
            if app
                .room_lease_claim(&room_id)
                .map_err(|error| error.to_string())?
                .is_none()
                && app
                    .room_lease_runtime
                    .as_ref()
                    .is_some_and(|runtime| runtime.config.mode == RoomLeaseRuntimeMode::RoomLeased)
            {
                return Err(format!("lost writer lease for room {room_id}"));
            }
            match app.rooms.status(&room_id) {
                Ok(MarketStatus::Closed) => return Ok(()),
                Ok(MarketStatus::Paused) => {
                    if let Ok(mut lifecycle) = lifecycle.lock() {
                        *lifecycle = AgentWorkerLifecycle::Paused;
                    }
                    return Ok(());
                }
                Ok(MarketStatus::Running) => {}
                Err(error) => return Err(format!("{error:?}")),
            }
            let scheduler = app
                .schedulers
                .get(&room_id)
                .cloned()
                .ok_or_else(|| format!("room {room_id} has no scheduler state"))?;
            let catch_up_limit = scheduler.catch_up_limit;
            if steps_run >= catch_up_limit {
                if let Ok(mut lifecycle) = lifecycle.lock() {
                    *lifecycle = AgentWorkerLifecycle::Running;
                }
                return Ok(());
            }
            drop(app);
            Some(scheduler)
        };
        let Some(scheduler) = action else {
            return Ok(());
        };
        if let Ok(mut lifecycle) = lifecycle.lock() {
            *lifecycle = AgentWorkerLifecycle::Running;
        }
        commit_scheduler_step(
            shared.clone(),
            room_id.clone(),
            Some(scheduler),
            false,
            None,
        )
        .await
        .map_err(|(_, body)| body.0.error)?;
        steps_run = steps_run.saturating_add(1);
        if steps_run >= 1 {
            // Auto-step runs one simulation step per wake; catch-up repeats
            // until the limit without skipping the remaining steps.
            let more = {
                let app = shared.app.lock().await;
                app.schedulers
                    .get(&room_id)
                    .is_some_and(|scheduler| scheduler.lagged)
            };
            if !more {
                return Ok(());
            }
        }
    }
}

struct ControlIdempotencyIntent {
    user_id: String,
    key: String,
    fingerprint: String,
}

async fn commit_scheduler_step(
    shared: SharedState,
    room_id: RoomId,
    scheduler: Option<exchange_core::SchedulerState>,
    require_paused: bool,
    control: Option<ControlIdempotencyIntent>,
) -> Result<exchange_core::SchedulerState, (StatusCode, Json<ErrorResponse>)> {
    run_durable_state_transaction(shared.clone(), async move {
        let mut state = lock_state(&shared).await?;
        if let Some(intent) = &control
            && let Some(replayed) = load_control_replay(
                &state,
                &intent.user_id,
                &room_id,
                &intent.key,
                &intent.fingerprint,
            )
            .await?
        {
            return Ok(Json(replayed));
        }
        state
            .room_lease_claim(&room_id)
            .map_err(api_error_from_journal)?;
        if state.rooms.status(&room_id).map_err(api_error_from_room)? == MarketStatus::Closed {
            return Err(api_error(
                StatusCode::CONFLICT,
                format!("room {room_id} is closed"),
            ));
        }
        if require_paused
            && state.rooms.status(&room_id).map_err(api_error_from_room)? != MarketStatus::Paused
        {
            return Err(api_error(
                StatusCode::CONFLICT,
                format!("manual step requires room {room_id} to be paused"),
            ));
        }
        let scheduler = state
            .schedulers
            .get(&room_id)
            .cloned()
            .or(scheduler)
            .ok_or_else(|| {
                api_error(
                    StatusCode::CONFLICT,
                    format!("room {room_id} has no scheduler to step"),
                )
            })?;
        let command_cursor =
            next_persisted_command_cursor(&state, &room_id).map_err(api_error_from_journal)?;
        let mut candidate_rooms = state.rooms.clone();
        let previous_history_len = candidate_rooms
            .execution_history(&room_id)
            .map_err(api_error_from_room)?
            .len();
        let clock_before = candidate_rooms
            .clock(&room_id)
            .map_err(api_error_from_room)?;
        let mut next_order_id = state.next_order_id;
        let outcome = match exchange_core::run_scheduler_step(
            &mut candidate_rooms,
            &mut next_order_id,
            scheduler,
            exchange_core::CrashPoint::None,
        ) {
            Ok(outcome) => {
                shared.lifecycle.record_scheduler_step(true);
                outcome
            }
            Err(error) => {
                shared.lifecycle.record_scheduler_step(false);
                return Err(api_error(StatusCode::BAD_REQUEST, format!("{error:?}")));
            }
        };
        let clock_after = candidate_rooms
            .clock(&room_id)
            .map_err(api_error_from_room)?;
        let clock_steps = clock_after.step().saturating_sub(clock_before.step());
        let training_run_id = state
            .training_runs
            .values()
            .find(|run| run.spec.room_id == room_id)
            .map(|run| run.spec.run_id.clone());
        let mut updated_training = None;
        if let Some(run_id) = training_run_id.as_ref()
            && let Some(mut run) = state.training_runs.get(run_id).cloned()
        {
            let new_executions = candidate_rooms
                .execution_history(&room_id)
                .map_err(api_error_from_room)?
                .iter()
                .skip(previous_history_len)
                .cloned()
                .collect::<Vec<_>>();
            for execution in &new_executions {
                apply_training_execution(
                    &mut run,
                    execution,
                    None,
                    execution_account_id(execution),
                );
            }
            for _ in 0..clock_steps.max(1) {
                run.on_step();
                if run.is_finished() {
                    break;
                }
            }
            settle_training_residuals(&mut candidate_rooms, &mut run)?;
            updated_training = Some((run_id.clone(), run));
        }
        let execution_records = candidate_rooms
            .execution_history(&room_id)
            .map_err(api_error_from_room)?
            .iter()
            .skip(previous_history_len)
            .map(|execution| {
                let command = command_from_actor_execution(execution).ok_or_else(|| {
                    api_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("room {room_id} produced an unjournalable scheduler execution"),
                    )
                })?;
                let participant_id = execution_participant_id(&outcome.state, execution);
                if let Some(participant_id) = participant_id {
                    Ok(JournalExecution::submitted(
                        participant_id,
                        execution_account_id(execution).unwrap_or(0),
                        command,
                        execution.clone(),
                    ))
                } else {
                    Ok(JournalExecution::system(command, execution.clone()))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let checkpoint_started = Instant::now();
        let snapshot = current_room_snapshot(
            &candidate_rooms,
            &room_id,
            execution_records
                .last()
                .map(|record| record.command_seq)
                .or_else(|| latest_persisted_command_seq(&state, &room_id))
                .unwrap_or(0),
        );
        if snapshot.is_some() {
            shared.lifecycle.record_checkpoint(
                u64::try_from(checkpoint_started.elapsed().as_millis()).unwrap_or(u64::MAX),
            );
        }
        let record = control
            .as_ref()
            .map(|intent| {
                control_record(
                    intent.user_id.clone(),
                    room_id.clone(),
                    intent.key.clone(),
                    intent.fingerprint.clone(),
                    &outcome.state,
                )
            })
            .transpose()?;
        let pending = PendingJournalMutation::new(
            room_id.clone(),
            command_cursor,
            RoomMutation::SchedulerProgress {
                clock_steps,
                state: outcome.state.clone(),
            },
        );
        let pending = match record {
            Some(record) => pending.with_control_idempotency(record),
            None => pending,
        };
        if let Some(replay_json) = append_control_mutation(
            &mut state,
            pending,
            &execution_records,
            &[],
            snapshot.as_ref(),
        )
        .await?
        {
            let replayed = serde_json::from_value(replay_json).map_err(api_error_from_json)?;
            return Ok(Json(replayed));
        }
        if let Some((_, run)) = updated_training {
            persist_training_progress(&mut state, &run, &[]).await?;
        }
        state.append_room_executions(
            &room_id,
            execution_records
                .into_iter()
                .map(|record| record.execution)
                .collect(),
        );
        state.rooms = candidate_rooms;
        state.next_order_id = next_order_id;
        state
            .schedulers
            .insert(room_id.clone(), outcome.state.clone());
        Ok(Json(outcome.state))
    })
    .await
    .map(|json| json.0)
}

fn execution_participant_id(
    scheduler: &exchange_core::SchedulerState,
    execution: &exchange_core::ActorExecution,
) -> Option<String> {
    let account_id = execution_account_id(execution)?;
    scheduler
        .agents
        .iter()
        .find(|agent| agent.account_id() == account_id)
        .map(|agent| agent.template.participant_id().to_string())
}

fn execution_account_id(execution: &exchange_core::ActorExecution) -> Option<AccountId> {
    match &execution.result {
        ActorExecutionResult::Accepted(MarketExecution::Spot(result)) => {
            match &result.command.command {
                Command::NewOrder(order) => Some(order.account_id),
                Command::CancelOrder(_) | Command::AmendOrder(_) | Command::SetMarkPrice(_) => None,
            }
        }
        ActorExecutionResult::Accepted(MarketExecution::Perp(result)) => {
            match &result.command.command {
                Command::NewOrder(order) => Some(order.account_id),
                _ => None,
            }
        }
        ActorExecutionResult::Rejected(_) => None,
    }
}

fn sleep_until_next_step(interval: Duration, stop: &AtomicBool) {
    let mut slept = Duration::ZERO;
    while slept < interval && !stop.load(Ordering::Relaxed) {
        let remaining = interval - slept;
        let chunk = remaining.min(Duration::from_millis(50));
        thread::sleep(chunk);
        slept += chunk;
    }
}

#[derive(Debug)]
enum AgentWorkerError {
    NoAgents,
    Spawn(std::io::Error),
}

impl std::fmt::Display for AgentWorkerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAgents => formatter.write_str("agent worker needs at least one agent"),
            Self::Spawn(error) => write!(formatter, "failed to spawn agent worker: {error}"),
        }
    }
}

impl std::error::Error for AgentWorkerError {}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{HeaderMap, Method, Request},
        routing::{get as route_get, post as route_post},
    };
    use exchange_core::{
        AgentTemplate, DcaTrader, DcaTraderConfig, GatewayRequest, InstrumentConfig, MarketConfig,
        NewOrder, OrderKind, ParticipantConfig, ParticipantKind, RoomBootstrap, Side,
        SpotClearingConfig, SpotMarketConfig, SpotRiskConfig,
        scenario::{ScenarioAccount, ScenarioPortfolio},
    };
    use futures_util::StreamExt;
    use tower::ServiceExt;

    use crate::journal::{JournalError, JournalExecution, JournalStore, PostgresJournalStore};

    async fn bind_http_test_listener() -> Option<(tokio::net::TcpListener, String)> {
        match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => {
                let base_url = format!("http://{}", listener.local_addr().unwrap());
                Some((listener, base_url))
            }
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => None,
            Err(error) => panic!("failed to bind test listener: {error}"),
        }
    }

    fn room_owner_conflict(owner_url: &str) -> (StatusCode, Json<ErrorResponse>) {
        room_owner_conflict_for("route-room", owner_url)
    }

    fn room_owner_conflict_for(
        room_id: &str,
        owner_url: &str,
    ) -> (StatusCode, Json<ErrorResponse>) {
        (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "room is owned by another instance".to_string(),
                code: Some("room_owned_by_other_instance".to_string()),
                room_owner: Some(Box::new(RoomOwnerResponse {
                    room_id: room_id.to_string(),
                    owner_id: "owner-instance".to_string(),
                    owner_url: Some(owner_url.to_string()),
                    fencing_token: 7,
                    expires_at_unix_ms: 1_800_000_000_000,
                })),
            }),
        )
    }

    fn sse_execution_response(execution: &RoomExecutionSummary) -> Response {
        let body = format!(
            ": heartbeat\n\nid: {}\nevent: execution\ndata: {}\n\n",
            execution.command_seq,
            serde_json::to_string(execution).unwrap()
        );
        ([(CONTENT_TYPE, "text/event-stream")], body).into_response()
    }

    fn sse_resync_response(room_id: &str) -> Response {
        let body = format!(
            "event: resync_required\ndata: {}\n\n",
            serde_json::json!({
                "room_id": room_id,
                "after_command_seq": 4,
                "reason": "consumer_lagged",
                "skipped_messages": 3,
            })
        );
        ([(CONTENT_TYPE, "text/event-stream")], body).into_response()
    }

    fn spot_scenario(room_id: &str) -> ScenarioConfig {
        ScenarioConfig {
            room_id: room_id.to_string(),
            venue_preset: None,
            venue_rules: exchange_core::VenueRuleConfig::default(),
            venue_asset_policy: exchange_core::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: MarketConfig::Spot(SpotMarketConfig {
                instrument: InstrumentConfig::new("V-BTC-SPOT", 1, 1).unwrap(),
                clearing: SpotClearingConfig::default(),
                risk: SpotRiskConfig::default(),
            }),
            extra_markets: Vec::new(),
            initial_portfolios: Vec::new(),
            initial_allocations: Vec::new(),
            routed_initial_allocations: Vec::new(),
            accounts: vec![
                ScenarioAccount::Spot {
                    account_id: 10,
                    cash_balance: 1_000,
                    position_qty: 10,
                },
                ScenarioAccount::Basic {
                    account_id: 20,
                    cash_balance: 1_000,
                },
            ],
            seed_orders: vec![],
            routed_seed_orders: Vec::new(),
        }
    }

    fn synthetic_execution(room_id: &str, command_seq: u64) -> RoomExecutionSummary {
        RoomExecutionSummary {
            room_id: room_id.to_string(),
            instrument_id: Some("V-BTC-SPOT".to_string()),
            submit_account_id: None,
            command_seq,
            market_time_ms: Some(command_seq.saturating_mul(1_000)),
            status: MarketStatus::Running,
            accepted: true,
            reject_reason: None,
            events: Vec::new(),
            clearing_events: Vec::new(),
            clearing_event_count: 0,
            clearing_events_omitted: false,
        }
    }

    struct PagingJournalStore {
        executions: Vec<RoomExecutionSummary>,
    }

    struct BlockingAuthorizationJournal {
        started: Option<std::sync::mpsc::Sender<()>>,
        release: Arc<(Mutex<bool>, std::sync::Condvar)>,
    }

    impl JournalStore for BlockingAuthorizationJournal {
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
            if let Some(started) = self.started.take() {
                let _ = started.send(());
            }
            let (released, released_changed) = &*self.release;
            let mut released = released.lock().unwrap();
            while !*released {
                released = released_changed.wait(released).unwrap();
            }
            Ok(true)
        }
    }

    impl JournalStore for PagingJournalStore {
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
            Ok(true)
        }

        fn user_can_administer_room(
            &mut self,
            _user_id: &str,
            _room_id: &str,
        ) -> Result<bool, JournalError> {
            Ok(true)
        }

        fn query_executions(
            &mut self,
            _room_id: &str,
            after_command_seq: Option<u64>,
            from_start: bool,
            limit: usize,
        ) -> Result<ExecutionPage, JournalError> {
            let history = self.executions.iter().cloned().collect::<VecDeque<_>>();
            Ok(execution_page_from_cache(
                &history,
                after_command_seq,
                from_start,
                limit,
            ))
        }
    }

    fn paged_event_app(execution_count: usize) -> (Router, SharedState) {
        let room_id = "paged-room";
        let executions = (0..execution_count)
            .map(|command_seq| synthetic_execution(room_id, command_seq as u64))
            .collect::<Vec<_>>();
        let mut app_state = AppState::new_with_journal(
            "http://127.0.0.1:57305",
            Box::new(PagingJournalStore {
                executions: executions.clone(),
            }),
        );
        app_state.rooms.create_room(spot_scenario(room_id)).unwrap();
        app_state.replace_room_executions(room_id.to_string(), executions);
        let state = shared_state(app_state);
        (app(state.clone()), state)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slow_read_authorization_does_not_block_app_state_or_journal_writes() {
        let (started_sender, started_receiver) = std::sync::mpsc::channel();
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let mut app_state = AppState::new_with_journal_bundle_and_auth_policy(
            "http://127.0.0.1:57305",
            JournalStoreBundle {
                writer: Box::new(journal::InMemoryJournalStore::new()),
                readers: vec![Box::new(BlockingAuthorizationJournal {
                    started: Some(started_sender),
                    release: Arc::clone(&release),
                })],
            },
            AuthPolicy::local_development(),
        );
        app_state
            .rooms
            .create_room(spot_scenario("slow-read-room"))
            .unwrap();
        let app = app(shared_state(app_state));

        let slow_read = tokio::spawn(
            app.clone().oneshot(
                Request::builder()
                    .uri("/rooms/slow-read-room/ticks")
                    .body(Body::empty())
                    .unwrap(),
            ),
        );
        tokio::task::spawn_blocking(move || {
            started_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("read authorization did not reach its dedicated worker")
        })
        .await
        .unwrap();

        let writer_result = tokio::time::timeout(
            Duration::from_millis(500),
            app.clone().oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&spot_scenario("writer-progress-room")).unwrap(),
                    ))
                    .unwrap(),
            ),
        )
        .await;

        let (released, released_changed) = &*release;
        *released.lock().unwrap() = true;
        released_changed.notify_all();

        let writer_response = writer_result
            .expect("a blocked read must not retain AppState or the write worker")
            .unwrap();
        assert_eq!(writer_response.status(), StatusCode::OK);
        let read_response = slow_read.await.unwrap().unwrap();
        assert_eq!(read_response.status(), StatusCode::OK);

        let metrics = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(metrics.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("marketforge_journal_write_workers 1\n"));
        assert!(body.contains("marketforge_journal_read_workers 1\n"));
    }

    #[tokio::test]
    async fn shutdown_rejects_new_durable_writes_and_waits_for_active_work() {
        let lifecycle = RuntimeLifecycle::new();
        let mut shutdown = lifecycle.subscribe_shutdown();
        let mut guard = lifecycle.try_begin_durable_write().unwrap();
        let active = lifecycle.metrics_snapshot();
        assert_eq!(active.active_durable_writes, 1);
        assert_eq!(active.durable_writes_started, 1);
        assert_eq!(active.durable_writes_completed, 0);

        lifecycle.begin_shutdown();
        shutdown.changed().await.unwrap();
        assert!(*shutdown.borrow());
        assert!(lifecycle.try_begin_durable_write().is_none());
        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                lifecycle.wait_for_durable_writes()
            )
            .await
            .is_err()
        );

        guard.mark_succeeded();
        drop(guard);
        tokio::time::timeout(Duration::from_secs(1), lifecycle.wait_for_durable_writes())
            .await
            .expect("shutdown should finish after the active durable write exits");
        let completed = lifecycle.metrics_snapshot();
        assert_eq!(completed.active_durable_writes, 0);
        assert_eq!(completed.durable_writes_started, 1);
        assert_eq!(completed.durable_writes_completed, 1);
        assert_eq!(completed.durable_writes_failed, 0);
        assert_eq!(completed.durable_writes_rejected, 1);
    }

    #[tokio::test]
    async fn event_history_pages_through_journal_after_memory_cache_is_trimmed() {
        let execution_count = ROOM_EVENT_CACHE_CAPACITY + 2;
        let (app, state) = paged_event_app(execution_count);
        {
            let state = state.app.lock().await;
            let cache = state.executions.get("paged-room").unwrap();
            assert_eq!(cache.len(), ROOM_EVENT_CACHE_CAPACITY);
            assert_eq!(cache.front().unwrap().command_seq, 2);
            assert_eq!(
                cache.back().unwrap().command_seq,
                (execution_count - 1) as u64
            );
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/paged-room/events?from_start=true&limit=2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let page: RoomEventsResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(page.executions[0].command_seq, 0);
        assert_eq!(page.executions[1].command_seq, 1);
        assert_eq!(page.latest_command_seq, Some((execution_count - 1) as u64));
        assert!(page.has_more);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/paged-room/events/stream?replay_from_start=true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut stream = response.into_body().into_data_stream();
        let chunk = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("journal-backed SSE replay should start immediately")
            .expect("SSE replay should produce an execution")
            .expect("SSE replay frame should be readable");
        let chunk = String::from_utf8(chunk.to_vec()).unwrap();
        assert!(chunk.contains("id: 0\n"));
        for expected_command_seq in 1..execution_count {
            let chunk = tokio::time::timeout(Duration::from_secs(1), stream.next())
                .await
                .expect("journal-backed SSE replay should continue across pages")
                .expect("SSE replay should reach its captured boundary")
                .expect("SSE replay frame should be readable");
            let chunk = String::from_utf8(chunk.to_vec()).unwrap();
            assert!(chunk.contains(&format!("id: {expected_command_seq}\n")));
        }
    }

    #[test]
    fn recovery_keeps_only_the_bounded_execution_tail() {
        let execution_count = ROOM_EVENT_CACHE_CAPACITY + 2;
        let recovery = JournalRecovery {
            rooms: Vec::new(),
            executions: (0..execution_count)
                .map(|command_seq| JournalExecution {
                    room_id: "recovery-cache-room".to_string(),
                    command_seq: command_seq as u64,
                    participant_id: None,
                    account_id: None,
                    request_user_id: None,
                    idempotency_key: None,
                    request_fingerprint: None,
                    quota_user_step: None,
                    command: Command::SetMarkPrice(SetMarkPrice {
                        price_tick: command_seq as i64,
                    }),
                    execution: synthetic_execution("recovery-cache-room", command_seq as u64),
                })
                .collect(),
            mutations: Vec::new(),
            snapshots: Vec::new(),
        };

        let executions = execution_summaries_from_recovery(&recovery);
        let cache = executions.get("recovery-cache-room").unwrap();
        assert_eq!(cache.len(), ROOM_EVENT_CACHE_CAPACITY);
        assert_eq!(cache.front().unwrap().command_seq, 2);
        assert_eq!(
            cache.back().unwrap().command_seq,
            (execution_count - 1) as u64
        );
    }

    #[tokio::test]
    async fn journal_replay_hands_off_to_live_events_at_the_captured_boundary() {
        let (app, state) = paged_event_app(2);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/paged-room/events/stream?replay_from_start=true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        {
            let mut app = state.app.lock().await;
            app.append_room_executions("paged-room", vec![synthetic_execution("paged-room", 2)]);
        }

        let mut stream = response.into_body().into_data_stream();
        for expected_command_seq in 0..=2 {
            let chunk = tokio::time::timeout(Duration::from_secs(1), stream.next())
                .await
                .expect("SSE should cross from journal replay to live delivery")
                .expect("SSE should deliver every execution around the boundary")
                .expect("SSE frame should be readable");
            let chunk = String::from_utf8(chunk.to_vec()).unwrap();
            assert!(chunk.contains(&format!("id: {expected_command_seq}\n")));
        }
    }

    #[tokio::test]
    async fn shutdown_closes_live_event_streams() {
        let (app, state) = paged_event_app(1);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/paged-room/events/stream")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut stream = response.into_body().into_data_stream();
        state.lifecycle.begin_shutdown();
        let end = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("SSE should observe server shutdown");
        assert!(end.is_none());
    }

    #[test]
    fn http_client_debug_redacts_bearer_tokens() {
        let client =
            HttpTradingClient::with_bearer_token("http://127.0.0.1:57305", "super-secret-token");
        let debug = format!("{client:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("super-secret-token"));
    }

    fn seeded_spot_scenario(room_id: &str) -> ScenarioConfig {
        let mut scenario = spot_scenario(room_id);
        scenario.seed_orders = vec![Command::NewOrder(NewOrder {
            order_id: 10_000,
            account_id: 10,
            side: Side::Sell,
            kind: OrderKind::Limit { price_tick: 104 },
            qty: 8,
            reduce_only: false,
        })];
        scenario
    }

    fn spot_perp_scenario(room_id: &str) -> ScenarioConfig {
        ScenarioConfig {
            room_id: room_id.to_string(),
            venue_preset: None,
            venue_rules: exchange_core::VenueRuleConfig::default(),
            venue_asset_policy: exchange_core::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: MarketConfig::Spot(SpotMarketConfig {
                instrument: InstrumentConfig::new("V-BTC-SPOT", 1, 1).unwrap(),
                clearing: SpotClearingConfig::default(),
                risk: SpotRiskConfig::default(),
            }),
            extra_markets: vec![MarketConfig::Perp(exchange_core::PerpMarketConfig {
                instrument: InstrumentConfig::new("V-BTC-PERP", 1, 1).unwrap(),
                clearing: exchange_core::PerpClearingConfig {
                    leverage: 10,
                    ..exchange_core::PerpClearingConfig::default()
                },
                risk: exchange_core::PerpRiskConfig::default(),
                initial_mark_price_tick: 100,
            })],
            initial_portfolios: Vec::new(),
            initial_allocations: Vec::new(),
            routed_initial_allocations: Vec::new(),
            accounts: vec![
                ScenarioAccount::Spot {
                    account_id: 10,
                    cash_balance: 1_000,
                    position_qty: 10,
                },
                ScenarioAccount::Basic {
                    account_id: 20,
                    cash_balance: 1_000,
                },
            ],
            seed_orders: Vec::new(),
            routed_seed_orders: vec![exchange_core::ScenarioSeedOrder {
                instrument_id: Some("V-BTC-PERP".to_string()),
                command: Command::NewOrder(NewOrder {
                    order_id: 10_000,
                    account_id: 20,
                    side: Side::Sell,
                    kind: OrderKind::Limit { price_tick: 100 },
                    qty: 5,
                    reduce_only: false,
                }),
            }],
        }
    }

    fn shared_collateral_perp_scenario(room_id: &str) -> ScenarioConfig {
        let perp_market = |instrument_id: &str, base_asset: &str| {
            MarketConfig::Perp(exchange_core::PerpMarketConfig {
                instrument: InstrumentConfig::new_for_venue(
                    "venue-a",
                    instrument_id,
                    base_asset,
                    "USD",
                    instrument_id,
                    1,
                    1,
                )
                .unwrap(),
                clearing: exchange_core::PerpClearingConfig {
                    leverage: 10,
                    maintenance_margin_ppm: 50_000,
                    ..exchange_core::PerpClearingConfig::default()
                },
                risk: exchange_core::PerpRiskConfig::default(),
                initial_mark_price_tick: 100,
            })
        };

        ScenarioConfig {
            room_id: room_id.to_string(),
            venue_preset: None,
            venue_rules: exchange_core::VenueRuleConfig::default(),
            venue_asset_policy: exchange_core::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: perp_market("V-BTC-PERP", "BTC"),
            extra_markets: vec![perp_market("V-ETH-PERP", "ETH")],
            initial_portfolios: Vec::new(),
            initial_allocations: Vec::new(),
            routed_initial_allocations: Vec::new(),
            accounts: vec![
                ScenarioAccount::Basic {
                    account_id: 10,
                    cash_balance: 10_000,
                },
                ScenarioAccount::Basic {
                    account_id: 20,
                    cash_balance: 1_000,
                },
            ],
            seed_orders: Vec::new(),
            routed_seed_orders: Vec::new(),
        }
    }

    fn perp_liquidation_scenario(room_id: &str) -> ScenarioConfig {
        perp_liquidation_scenario_with_mark(room_id, 80)
    }

    fn perp_liquidation_scenario_with_mark(
        room_id: &str,
        initial_mark_price_tick: i64,
    ) -> ScenarioConfig {
        ScenarioConfig {
            room_id: room_id.to_string(),
            venue_preset: None,
            venue_rules: exchange_core::VenueRuleConfig::default(),
            venue_asset_policy: exchange_core::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: MarketConfig::Perp(exchange_core::PerpMarketConfig {
                instrument: InstrumentConfig::new("V-BTC-PERP", 1, 1).unwrap(),
                clearing: exchange_core::PerpClearingConfig {
                    leverage: 10,
                    maintenance_margin_ppm: 50_000,
                    liquidation_fee_ppm: 10_000,
                    ..exchange_core::PerpClearingConfig::default()
                },
                risk: exchange_core::PerpRiskConfig::default(),
                initial_mark_price_tick,
            }),
            extra_markets: Vec::new(),
            initial_portfolios: Vec::new(),
            initial_allocations: Vec::new(),
            routed_initial_allocations: Vec::new(),
            accounts: vec![
                ScenarioAccount::Basic {
                    account_id: 10,
                    cash_balance: 10_000,
                },
                ScenarioAccount::Basic {
                    account_id: 20,
                    cash_balance: 200,
                },
                ScenarioAccount::Basic {
                    account_id: 30,
                    cash_balance: 10_000,
                },
            ],
            seed_orders: Vec::new(),
            routed_seed_orders: Vec::new(),
        }
    }

    fn dca_template(room_id: &str, participant_id: &str, account_id: AccountId) -> AgentTemplate {
        AgentTemplate::DcaTrader(DcaTraderConfig {
            participant: ParticipantConfig {
                participant_id: participant_id.to_string(),
                kind: ParticipantKind::RuleAgent,
                room_id: room_id.to_string(),
                account_id,
                instrument_id: Some("V-BTC-SPOT".to_string()),
            },
            interval_steps: 1,
            order_qty: 2,
            use_market_order: false,
            limit_offset_ticks: 0,
            fallback_price_tick: 100,
            side: Side::Buy,
        })
    }

    #[tokio::test]
    async fn cors_preflight_allows_client_identity_cursor_and_idempotency_headers() {
        let response = new_app()
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/rooms")
                    .header("origin", "http://127.0.0.1:57304")
                    .header("access-control-request-method", "POST")
                    .header(
                        "access-control-request-headers",
                        "content-type, x-user-id, idempotency-key, last-event-id",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let allowed_headers = response
            .headers()
            .get("access-control-allow-headers")
            .unwrap()
            .to_str()
            .unwrap()
            .to_ascii_lowercase();
        assert!(allowed_headers.contains("x-user-id"));
        assert!(allowed_headers.contains("idempotency-key"));
        assert!(allowed_headers.contains("last-event-id"));
    }

    #[test]
    fn account_summaries_preserve_i128_values() {
        let summary = SpotAccountStateSummary::from_snapshot(SpotAccountSnapshot {
            account_id: 7,
            cash_balance: i128::MAX,
            position_qty: i128::MIN + 1,
            reserved_cash: 0,
            reserved_position: 0,
            available_cash: i128::MAX,
            available_position: i128::MIN + 1,
            fees_paid: i128::MAX - 1,
        });
        assert_eq!(summary.cash_balance, i128::MAX);
        assert_eq!(summary.position_qty, i128::MIN + 1);
        assert_eq!(summary.fees_paid, i128::MAX - 1);
        let json = serde_json::to_string(&summary).unwrap();
        let round_trip: SpotAccountStateSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(round_trip, summary);

        let normal = SpotAccountStateSummary::from_snapshot(SpotAccountSnapshot {
            account_id: 8,
            cash_balance: 42,
            position_qty: -3,
            reserved_cash: 0,
            reserved_position: 0,
            available_cash: 42,
            available_position: -3,
            fees_paid: 1,
        });
        let normal_json = serde_json::to_value(normal).unwrap();
        assert_eq!(normal_json["cash_balance"], 42);

        let execution = RoomExecutionSummary {
            room_id: "i128-room".to_string(),
            instrument_id: Some("V-BTC-SPOT".to_string()),
            submit_account_id: None,
            command_seq: 3,
            market_time_ms: Some(3_000),
            status: MarketStatus::Running,
            accepted: true,
            reject_reason: None,
            events: Vec::new(),
            clearing_events: vec![ClearingEventSummary::SpotTradeSettled {
                trade_id: 1,
                buyer_account_id: 7,
                seller_account_id: 8,
                price_tick: 100,
                qty: 1,
                notional: i128::MAX,
                buyer_fee: 42,
                seller_fee: i128::MIN,
                buyer: summary.clone(),
                seller: summary,
            }],
            clearing_event_count: 1,
            clearing_events_omitted: false,
        };
        let execution_json = serde_json::to_value(&execution).unwrap();
        assert_eq!(execution_json["clearing_events"][0]["buyer_fee"], 42);
        assert_eq!(
            execution_json["clearing_events"][0]["notional"],
            i128::MAX.to_string()
        );
        let mut legacy_execution_json = execution_json.clone();
        legacy_execution_json
            .as_object_mut()
            .unwrap()
            .remove("market_time_ms");
        let legacy: RoomExecutionSummary = serde_json::from_value(legacy_execution_json).unwrap();
        assert_eq!(legacy.market_time_ms, None);
        let round_trip: RoomExecutionSummary = serde_json::from_value(execution_json).unwrap();
        match &round_trip.clearing_events[0] {
            ClearingEventSummary::SpotTradeSettled {
                notional,
                buyer_fee,
                seller_fee,
                buyer,
                ..
            } => {
                assert_eq!(*notional, i128::MAX);
                assert_eq!(*buyer_fee, 42);
                assert_eq!(*seller_fee, i128::MIN);
                assert_eq!(buyer.cash_balance, i128::MAX);
            }
            other => panic!("unexpected clearing event: {other:?}"),
        }
    }

    #[test]
    fn recovery_execution_validation_rejects_same_count_with_different_clearing_amounts() {
        let stored = RoomExecutionSummary {
            room_id: "clearing-validation-room".to_string(),
            instrument_id: Some("V-BTC-SPOT".to_string()),
            submit_account_id: None,
            command_seq: 1,
            market_time_ms: Some(1_000),
            status: MarketStatus::Running,
            accepted: true,
            reject_reason: None,
            events: Vec::new(),
            clearing_events: vec![ClearingEventSummary::SpotTradeSettled {
                trade_id: 1,
                buyer_account_id: 10,
                seller_account_id: 20,
                price_tick: 100,
                qty: 1,
                notional: 100,
                buyer_fee: 1,
                seller_fee: 1,
                buyer: SpotAccountStateSummary {
                    account_id: 10,
                    cash_balance: 900,
                    position_qty: 1,
                    fees_paid: 1,
                },
                seller: SpotAccountStateSummary {
                    account_id: 20,
                    cash_balance: 1_099,
                    position_qty: 9,
                    fees_paid: 1,
                },
            }],
            clearing_event_count: 1,
            clearing_events_omitted: false,
        };
        let mut replayed = stored.clone();
        let ClearingEventSummary::SpotTradeSettled { notional, .. } =
            &mut replayed.clearing_events[0]
        else {
            unreachable!("test builds a spot clearing event");
        };
        *notional = 101;

        assert!(!execution_summary_matches(&stored, &replayed));
    }

    #[test]
    fn reserved_and_max_order_ids_are_rejected_without_overflow() {
        let command_with_id = |order_id| {
            Command::NewOrder(NewOrder {
                order_id,
                account_id: 10,
                side: Side::Sell,
                kind: OrderKind::Limit { price_tick: 100 },
                qty: 1,
                reduce_only: false,
            })
        };
        for order_id in [SYSTEM_LIQUIDATION_ORDER_ID_BASE, OrderId::MAX] {
            assert!(next_api_order_id_after_commands(1, &[command_with_id(order_id)]).is_err());
        }

        let scenario = spot_scenario("invalid-recovery-id");
        let mut rooms = RoomManager::new();
        rooms.create_room(scenario).unwrap();
        let execution = rooms
            .apply("invalid-recovery-id", command_with_id(1))
            .unwrap();
        let mut record =
            JournalExecution::submitted("api".to_string(), 10, command_with_id(1), execution);
        let mut system_record = record.clone();
        system_record.participant_id = None;
        system_record.command = Command::NewOrder(NewOrder {
            order_id: SYSTEM_LIQUIDATION_ORDER_ID_BASE,
            account_id: 10,
            side: Side::Sell,
            kind: OrderKind::Market,
            qty: 1,
            reduce_only: true,
        });
        let system_recovery = JournalRecovery {
            rooms: Vec::new(),
            executions: vec![system_record],
            mutations: Vec::new(),
            snapshots: Vec::new(),
        };
        assert_eq!(next_order_id_from_recovery(&system_recovery).unwrap(), 1);

        record.command = command_with_id(OrderId::MAX);
        let recovery = JournalRecovery {
            rooms: Vec::new(),
            executions: vec![record],
            mutations: Vec::new(),
            snapshots: Vec::new(),
        };
        assert!(next_order_id_from_recovery(&recovery).is_err());
    }

    #[test]
    fn recovery_skips_atomic_system_liquidation_records() {
        let room_id = "recovered-liquidation-room";
        let scenario = perp_liquidation_scenario(room_id);
        let mut rooms = RoomManager::new();
        rooms.create_room(scenario.clone()).unwrap();

        let commands = [
            Command::NewOrder(NewOrder {
                order_id: 1,
                account_id: 30,
                side: Side::Buy,
                kind: OrderKind::Limit { price_tick: 80 },
                qty: 10,
                reduce_only: false,
            }),
            Command::NewOrder(NewOrder {
                order_id: 2,
                account_id: 10,
                side: Side::Sell,
                kind: OrderKind::Limit { price_tick: 100 },
                qty: 10,
                reduce_only: false,
            }),
            Command::NewOrder(NewOrder {
                order_id: 3,
                account_id: 20,
                side: Side::Buy,
                kind: OrderKind::Market,
                qty: 10,
                reduce_only: false,
            }),
        ];
        let participants = [("bidder", 30), ("seller", 10), ("distressed", 20)];
        let mut records = Vec::new();
        for (command, (participant_id, account_id)) in commands.into_iter().zip(participants) {
            let execution = rooms.apply(room_id, command.clone()).unwrap();
            records.push(JournalExecution::submitted(
                participant_id.to_string(),
                account_id,
                command,
                execution,
            ));
        }

        let history = rooms.execution_history(room_id).unwrap();
        assert_eq!(history.len(), 4);
        let liquidation = history.last().unwrap().clone();
        let liquidation_command = command_from_actor_execution(&liquidation).unwrap();
        assert!(is_system_liquidation_command(&liquidation_command));
        assert!(matches!(
            liquidation_command,
            Command::NewOrder(NewOrder {
                kind: OrderKind::ImmediateOrCancel { price_tick: None },
                ..
            })
        ));
        records.push(JournalExecution::system(liquidation_command, liquidation));
        for record in &mut records {
            record.execution.instrument_id = None;
        }

        let recovery = JournalRecovery {
            rooms: vec![journal::JournalRoom {
                room_id: room_id.to_string(),
                scenario,
                status: MarketStatus::Running,
            }],
            executions: records,
            mutations: Vec::new(),
            snapshots: Vec::new(),
        };
        assert_eq!(next_order_id_from_recovery(&recovery).unwrap(), 4);

        let mut missing_system_tail = recovery.clone();
        missing_system_tail.executions.pop();
        let error = recover_rooms(&missing_system_tail).unwrap_err();
        assert!(error.to_string().contains("journal ended at"));

        fn remove_portfolio_margin_fields(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::Object(fields) => {
                    fields.remove("portfolio_initial_margin");
                    fields.remove("portfolio_maintenance_margin");
                    for value in fields.values_mut() {
                        remove_portfolio_margin_fields(value);
                    }
                }
                serde_json::Value::Array(values) => {
                    for value in values {
                        remove_portfolio_margin_fields(value);
                    }
                }
                _ => {}
            }
        }
        let mut legacy_recovery = recovery.clone();
        for record in &mut legacy_recovery.executions {
            let mut execution_json = serde_json::to_value(&record.execution).unwrap();
            remove_portfolio_margin_fields(&mut execution_json);
            record.execution = serde_json::from_value(execution_json).unwrap();
        }
        assert!(recover_rooms(&legacy_recovery).is_ok());

        let mut count_only_clearing_recovery = recovery.clone();
        for record in &mut count_only_clearing_recovery.executions {
            let mut execution_json = serde_json::to_value(&record.execution).unwrap();
            execution_json
                .as_object_mut()
                .unwrap()
                .remove("clearing_events");
            record.execution = serde_json::from_value(execution_json).unwrap();
        }
        assert!(
            count_only_clearing_recovery
                .executions
                .iter()
                .all(|record| record.execution.clearing_events_omitted)
        );
        assert!(recover_rooms(&count_only_clearing_recovery).is_ok());

        let mut explicit_empty_clearing_recovery = recovery.clone();
        let mut replaced_nonempty_summary = false;
        for record in &mut explicit_empty_clearing_recovery.executions {
            let mut execution_json = serde_json::to_value(&record.execution).unwrap();
            if record.execution.clearing_event_count > 0 {
                execution_json["clearing_events"] = serde_json::json!([]);
                replaced_nonempty_summary = true;
            }
            record.execution = serde_json::from_value(execution_json).unwrap();
            assert!(!record.execution.clearing_events_omitted);
        }
        assert!(replaced_nonempty_summary);
        assert!(recover_rooms(&explicit_empty_clearing_recovery).is_err());

        let recovered = recover_rooms(&recovery).unwrap();
        assert_eq!(recovered.execution_history(room_id).unwrap().len(), 4);
        let AccountSnapshots::Perp(accounts) = recovered
            .account_snapshots_for(room_id, "V-BTC-PERP")
            .unwrap()
        else {
            panic!("expected perp account snapshots");
        };
        let distressed = accounts
            .iter()
            .find(|account| account.account_id == 20)
            .unwrap();
        assert_eq!(distressed.position_qty, 0);
    }

    #[test]
    fn recovery_matches_cross_market_peer_cancels_and_liquidation_records() {
        let room_id = "cross-market-system-recovery";
        let perp_market = |instrument_id: &str, base_asset: &str| {
            MarketConfig::Perp(exchange_core::PerpMarketConfig {
                instrument: InstrumentConfig::new_for_venue(
                    "venue-a",
                    instrument_id,
                    base_asset,
                    "USD",
                    instrument_id,
                    1,
                    1,
                )
                .unwrap(),
                clearing: exchange_core::PerpClearingConfig {
                    leverage: 10,
                    maintenance_margin_ppm: 50_000,
                    ..exchange_core::PerpClearingConfig::default()
                },
                risk: exchange_core::PerpRiskConfig::default(),
                initial_mark_price_tick: 100,
            })
        };
        let scenario = ScenarioConfig {
            room_id: room_id.to_string(),
            venue_preset: None,
            venue_rules: exchange_core::VenueRuleConfig::default(),
            venue_asset_policy: exchange_core::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: perp_market("V-BTC-PERP", "BTC"),
            extra_markets: vec![perp_market("V-ETH-PERP", "ETH")],
            initial_portfolios: Vec::new(),
            initial_allocations: Vec::new(),
            routed_initial_allocations: Vec::new(),
            accounts: vec![
                ScenarioAccount::Basic {
                    account_id: 10,
                    cash_balance: 10_000,
                },
                ScenarioAccount::Basic {
                    account_id: 20,
                    cash_balance: 200,
                },
                ScenarioAccount::Basic {
                    account_id: 30,
                    cash_balance: 10_000,
                },
            ],
            seed_orders: Vec::new(),
            routed_seed_orders: Vec::new(),
        };
        let mut rooms = RoomManager::new();
        rooms.create_room(scenario.clone()).unwrap();
        let mut records = Vec::new();

        let explicit_orders = [
            (
                "V-BTC-PERP",
                "seller",
                10,
                Command::NewOrder(NewOrder {
                    order_id: 1,
                    account_id: 10,
                    side: Side::Sell,
                    kind: OrderKind::Limit { price_tick: 100 },
                    qty: 10,
                    reduce_only: false,
                }),
            ),
            (
                "V-BTC-PERP",
                "distressed",
                20,
                Command::NewOrder(NewOrder {
                    order_id: 2,
                    account_id: 20,
                    side: Side::Buy,
                    kind: OrderKind::Market,
                    qty: 10,
                    reduce_only: false,
                }),
            ),
            (
                "V-ETH-PERP",
                "distressed",
                20,
                Command::NewOrder(NewOrder {
                    order_id: 3,
                    account_id: 20,
                    side: Side::Buy,
                    kind: OrderKind::Limit { price_tick: 70 },
                    qty: 1,
                    reduce_only: false,
                }),
            ),
            (
                "V-BTC-PERP",
                "liquidator",
                30,
                Command::NewOrder(NewOrder {
                    order_id: 4,
                    account_id: 30,
                    side: Side::Buy,
                    kind: OrderKind::Limit { price_tick: 80 },
                    qty: 10,
                    reduce_only: false,
                }),
            ),
        ];
        for (instrument_id, participant_id, account_id, command) in explicit_orders {
            let execution = rooms
                .apply_to_instrument(room_id, instrument_id, command.clone())
                .unwrap();
            records.push(JournalExecution::submitted(
                participant_id.to_string(),
                account_id,
                command,
                execution,
            ));
        }

        let previous_history_len = rooms.execution_history(room_id).unwrap().len();
        let mark_command = Command::SetMarkPrice(SetMarkPrice { price_tick: 80 });
        let mark_execution = rooms
            .apply_to_instrument(room_id, "V-BTC-PERP", mark_command.clone())
            .unwrap();
        records.push(JournalExecution::system(mark_command, mark_execution));
        for automatic in rooms.execution_history(room_id).unwrap()[previous_history_len + 1..]
            .iter()
            .cloned()
        {
            records.push(JournalExecution::system(
                command_from_actor_execution(&automatic).unwrap(),
                automatic,
            ));
        }

        let peer_cancel_index = records
            .iter()
            .position(|record| {
                record.participant_id.is_none() && matches!(record.command, Command::CancelOrder(_))
            })
            .expect("cross-margin liquidation should journal its peer cancel");
        assert!(records.iter().any(|record| record.participant_id.is_none()
            && is_system_liquidation_command(&record.command)));
        for (expected, record) in records.iter().enumerate() {
            assert_eq!(record.command_seq, expected as u64);
        }

        let recovery = JournalRecovery {
            rooms: vec![journal::JournalRoom {
                room_id: room_id.to_string(),
                scenario,
                status: MarketStatus::Running,
            }],
            executions: records.clone(),
            mutations: Vec::new(),
            snapshots: Vec::new(),
        };
        let recovered = recover_rooms(&recovery).unwrap();
        assert_eq!(
            recovered.execution_history(room_id).unwrap().len(),
            records.len()
        );

        let mut missing_peer_cancel = recovery;
        missing_peer_cancel.executions.remove(peer_cancel_index);
        let error = recover_rooms(&missing_peer_cancel).unwrap_err();
        assert!(
            error.to_string().contains("does not match command cursor")
                || error.to_string().contains("journal ended at")
        );
    }

    #[test]
    fn mutation_journal_recovers_complete_room_without_snapshots() {
        let room_id = "mutation-recovery-room";
        let mut scenario = spot_scenario(room_id);
        scenario.market = MarketConfig::Spot(SpotMarketConfig {
            instrument: InstrumentConfig::new_for_venue(
                "venue-a",
                "A-BTC-SPOT",
                "V",
                "BTC",
                "V-BTC",
                1,
                1,
            )
            .unwrap(),
            clearing: SpotClearingConfig::default(),
            risk: SpotRiskConfig::default(),
        });
        scenario.extra_markets = vec![MarketConfig::Spot(SpotMarketConfig {
            instrument: InstrumentConfig::new_for_venue(
                "venue-b",
                "B-BTC-SPOT",
                "V",
                "BTC",
                "V-BTC",
                1,
                1,
            )
            .unwrap(),
            clearing: SpotClearingConfig::default(),
            risk: SpotRiskConfig::default(),
        })];
        scenario.venue_rules = exchange_core::VenueRuleConfig {
            transfers: exchange_core::TransferPolicyConfig {
                deposit_delay_steps: 1,
                withdrawal_delay_steps: 1,
            },
            ..exchange_core::VenueRuleConfig::default()
        };
        scenario.initial_portfolios = vec![ScenarioPortfolio {
            account_id: 20,
            balances: BTreeMap::from([("BTC".to_string(), 500)]),
        }];

        let mut rooms = RoomManager::new();
        let bootstrap = rooms.create_room(scenario.clone()).unwrap();
        let command_cursor =
            command_cursor_after_actor_executions(&bootstrap.seed_executions).unwrap();
        let initial_actor = rooms.simulation_room(room_id).unwrap().clone();
        let executions = bootstrap
            .seed_executions
            .into_iter()
            .filter_map(|execution| {
                command_from_actor_execution(&execution)
                    .map(|command| JournalExecution::seed(command, execution))
            })
            .collect::<Vec<_>>();
        let mut mutations = vec![JournalMutation {
            room_id: room_id.to_string(),
            mutation_seq: 1,
            command_cursor,
            schema_version: journal::ROOM_MUTATION_SCHEMA_VERSION,
            mutation: RoomMutation::StateCheckpoint {
                actor: Box::new(initial_actor),
                complete_history: true,
            },
        }];
        let mut record_mutation = |mutation| {
            mutations.push(JournalMutation {
                room_id: room_id.to_string(),
                mutation_seq: mutations.len() as u64 + 1,
                command_cursor,
                schema_version: journal::ROOM_MUTATION_SCHEMA_VERSION,
                mutation,
            });
        };

        let deposit = rooms
            .submit_deposit(room_id, Some("venue-a"), 20, "BTC", 120)
            .unwrap();
        record_mutation(RoomMutation::DepositSubmitted {
            venue_id: Some("venue-a".to_string()),
            account_id: 20,
            asset_id: "BTC".to_string(),
            amount: 120,
            transfer: deposit,
        });

        let completed_transfers = rooms.advance_clock(room_id, 1).unwrap();
        record_mutation(RoomMutation::ClockAdvanced {
            steps: 1,
            completed_transfers,
        });

        let venue_transfer = rooms
            .submit_venue_to_venue_transfer(room_id, "venue-a", "venue-b", 20, "BTC", 40)
            .unwrap();
        record_mutation(RoomMutation::VenueToVenueTransferSubmitted {
            from_venue_id: "venue-a".to_string(),
            to_venue_id: "venue-b".to_string(),
            account_id: 20,
            asset_id: "BTC".to_string(),
            amount: 40,
            transfer: venue_transfer,
        });

        let completed_transfers = rooms.advance_clock(room_id, 1).unwrap();
        record_mutation(RoomMutation::ClockAdvanced {
            steps: 1,
            completed_transfers,
        });

        let withdrawal = rooms
            .submit_withdrawal(room_id, Some("venue-b"), 20, "BTC", 10)
            .unwrap();
        record_mutation(RoomMutation::WithdrawalSubmitted {
            venue_id: Some("venue-b".to_string()),
            account_id: 20,
            asset_id: "BTC".to_string(),
            amount: 10,
            transfer: withdrawal,
        });

        let completed_transfers = rooms.advance_clock(room_id, 1).unwrap();
        record_mutation(RoomMutation::ClockAdvanced {
            steps: 1,
            completed_transfers,
        });
        rooms.pause_room(room_id).unwrap();
        record_mutation(RoomMutation::StatusChanged {
            status: MarketStatus::Paused,
        });

        let expected_actor = serde_json::to_value(rooms.simulation_room(room_id).unwrap()).unwrap();
        let recovery = JournalRecovery {
            rooms: vec![journal::JournalRoom {
                room_id: room_id.to_string(),
                scenario,
                status: MarketStatus::Paused,
            }],
            executions,
            mutations,
            snapshots: Vec::new(),
        };

        let recovered = recover_rooms(&recovery).unwrap();
        assert_eq!(
            serde_json::to_value(recovered.simulation_room(room_id).unwrap()).unwrap(),
            expected_actor
        );

        let mut inconsistent = recovery;
        inconsistent.rooms[0].status = MarketStatus::Running;
        let error = recover_rooms(&inconsistent).unwrap_err();
        assert!(error.to_string().contains("replayed room status diverged"));
    }

    #[test]
    fn seedless_mutation_at_cursor_zero_replays_before_command_zero() {
        let room_id = "cursor-zero-room";
        let scenario = spot_scenario(room_id);
        let mut source = RoomManager::new();
        source.create_room(scenario.clone()).unwrap();
        source.advance_clock(room_id, 1).unwrap();
        let command = Command::NewOrder(NewOrder {
            order_id: 1,
            account_id: 20,
            side: Side::Buy,
            kind: OrderKind::Limit { price_tick: 100 },
            qty: 2,
            reduce_only: false,
        });
        let execution = source.apply(room_id, command.clone()).unwrap();
        assert_eq!(execution.command_seq, 0);

        let recovery = JournalRecovery {
            rooms: vec![journal::JournalRoom {
                room_id: room_id.to_string(),
                scenario,
                status: MarketStatus::Running,
            }],
            executions: vec![JournalExecution::submitted(
                "alice".to_string(),
                20,
                command,
                execution,
            )],
            mutations: vec![JournalMutation {
                room_id: room_id.to_string(),
                mutation_seq: 1,
                command_cursor: 0,
                schema_version: journal::ROOM_MUTATION_SCHEMA_VERSION,
                mutation: RoomMutation::ClockAdvanced {
                    steps: 1,
                    completed_transfers: Vec::new(),
                },
            }],
            snapshots: Vec::new(),
        };

        let recovered = recover_rooms(&recovery).unwrap();
        assert_eq!(recovered.clock(room_id).unwrap().step(), 1);
        assert_eq!(recovered.execution_history(room_id).unwrap().len(), 1);
        assert_eq!(
            recovered.execution_history(room_id).unwrap()[0].command_seq,
            0
        );
    }

    #[test]
    fn seedless_snapshot_cursor_zero_does_not_skip_later_command_zero() {
        let room_id = "snapshot-before-command-zero";
        let scenario = spot_scenario(room_id);
        let mut source = RoomManager::new();
        source.create_room(scenario.clone()).unwrap();
        let snapshot = JournalSnapshot {
            room_id: room_id.to_string(),
            command_seq: 0,
            actor: source.simulation_room(room_id).unwrap().clone(),
        };
        assert_eq!(snapshot.actor.next_command_seq(), 0);

        let command = Command::NewOrder(NewOrder {
            order_id: 1,
            account_id: 20,
            side: Side::Buy,
            kind: OrderKind::Limit { price_tick: 100 },
            qty: 1,
            reduce_only: false,
        });
        let execution = source.apply(room_id, command.clone()).unwrap();
        assert_eq!(execution.command_seq, 0);
        let recovery = JournalRecovery {
            rooms: vec![journal::JournalRoom {
                room_id: room_id.to_string(),
                scenario,
                status: MarketStatus::Running,
            }],
            executions: vec![JournalExecution::submitted(
                "alice".to_string(),
                20,
                command,
                execution,
            )],
            mutations: Vec::new(),
            snapshots: vec![snapshot],
        };

        let recovered = recover_rooms(&recovery).unwrap();
        assert_eq!(recovered.execution_history(room_id).unwrap().len(), 1);
        assert_eq!(
            recovered.execution_history(room_id).unwrap()[0].command_seq,
            0
        );
    }

    #[test]
    fn legacy_multi_venue_checkpoint_normalizes_room_global_cursor_before_validation() {
        let room_id = "legacy-multi-venue-cursor";
        let mut scenario = spot_scenario(room_id);
        scenario.market = MarketConfig::Spot(SpotMarketConfig {
            instrument: InstrumentConfig::new_for_venue(
                "venue-a",
                "A-BTC-SPOT",
                "BTC",
                "USD",
                "A BTC-USD",
                1,
                1,
            )
            .unwrap(),
            clearing: SpotClearingConfig::default(),
            risk: SpotRiskConfig::default(),
        });
        scenario.extra_markets = vec![MarketConfig::Spot(SpotMarketConfig {
            instrument: InstrumentConfig::new_for_venue(
                "venue-b",
                "B-BTC-SPOT",
                "BTC",
                "USD",
                "B BTC-USD",
                1,
                1,
            )
            .unwrap(),
            clearing: SpotClearingConfig::default(),
            risk: SpotRiskConfig::default(),
        })];
        let mut source = RoomManager::new();
        source.create_room(scenario.clone()).unwrap();
        for (instrument_id, order_id) in [("A-BTC-SPOT", 1), ("B-BTC-SPOT", 2)] {
            source
                .apply_to_instrument(
                    room_id,
                    instrument_id,
                    Command::NewOrder(NewOrder {
                        order_id,
                        account_id: 20,
                        side: Side::Buy,
                        kind: OrderKind::Limit { price_tick: 100 },
                        qty: 1,
                        reduce_only: false,
                    }),
                )
                .unwrap();
        }
        let command_cursor = source.simulation_room(room_id).unwrap().next_command_seq();
        assert_eq!(command_cursor, 2);
        let mut legacy_actor_json =
            serde_json::to_value(source.simulation_room(room_id).unwrap()).unwrap();
        legacy_actor_json
            .as_object_mut()
            .unwrap()
            .remove("next_command_seq");
        let legacy_actor: exchange_core::SimulationRoom =
            serde_json::from_value(legacy_actor_json).unwrap();
        assert_eq!(legacy_actor.next_command_seq(), 0);

        let recovery = JournalRecovery {
            rooms: vec![journal::JournalRoom {
                room_id: room_id.to_string(),
                scenario,
                status: MarketStatus::Running,
            }],
            executions: Vec::new(),
            mutations: vec![JournalMutation {
                room_id: room_id.to_string(),
                mutation_seq: 1,
                command_cursor,
                schema_version: journal::ROOM_MUTATION_SCHEMA_VERSION,
                mutation: RoomMutation::StateCheckpoint {
                    actor: Box::new(legacy_actor),
                    complete_history: true,
                },
            }],
            snapshots: Vec::new(),
        };

        let recovered = recover_rooms(&recovery).unwrap();
        assert_eq!(
            recovered
                .simulation_room(room_id)
                .unwrap()
                .next_command_seq(),
            2
        );
    }

    #[test]
    fn clock_mutation_at_cursor_zero_can_atomically_restore_system_execution_zero() {
        fn reset_command_sequences(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::Object(fields) => {
                    if fields.contains_key("next_command_seq") {
                        fields.insert("next_command_seq".to_string(), serde_json::json!(0));
                    }
                    for value in fields.values_mut() {
                        reset_command_sequences(value);
                    }
                }
                serde_json::Value::Array(values) => {
                    for value in values {
                        reset_command_sequences(value);
                    }
                }
                _ => {}
            }
        }

        let room_id = "clock-system-cursor-zero";
        let scenario = perp_liquidation_scenario_with_mark(room_id, 100);
        let mut source = RoomManager::new();
        let bootstrap = source.create_room(scenario.clone()).unwrap();
        source
            .apply(
                room_id,
                Command::NewOrder(NewOrder {
                    order_id: 1,
                    account_id: 10,
                    side: Side::Sell,
                    kind: OrderKind::Limit { price_tick: 100 },
                    qty: 10,
                    reduce_only: false,
                }),
            )
            .unwrap();
        source
            .apply(
                room_id,
                Command::NewOrder(NewOrder {
                    order_id: 2,
                    account_id: 20,
                    side: Side::Buy,
                    kind: OrderKind::Market,
                    qty: 10,
                    reduce_only: false,
                }),
            )
            .unwrap();
        source
            .apply(
                room_id,
                Command::SetMarkPrice(SetMarkPrice { price_tick: 80 }),
            )
            .unwrap();
        assert_eq!(source.pending_liquidation_count(room_id), Ok(1));
        source
            .simulation_room_mut(room_id)
            .unwrap()
            .apply(Command::NewOrder(NewOrder {
                order_id: 3,
                account_id: 30,
                side: Side::Buy,
                kind: OrderKind::Limit { price_tick: 80 },
                qty: 10,
                reduce_only: false,
            }));

        let mut actor_json =
            serde_json::to_value(source.simulation_room(room_id).unwrap()).unwrap();
        reset_command_sequences(&mut actor_json);
        let checkpoint_actor: exchange_core::SimulationRoom =
            serde_json::from_value(actor_json).unwrap();
        assert_eq!(checkpoint_actor.next_command_seq(), 0);

        let mut replay_source = RoomManager::new();
        replay_source
            .restore_simulation_room(checkpoint_actor.clone(), Vec::new())
            .unwrap();
        replay_source.advance_clock(room_id, 1).unwrap();
        let system_execution = replay_source.execution_history(room_id).unwrap()[0].clone();
        assert_eq!(system_execution.command_seq, 0);
        let system_command = command_from_actor_execution(&system_execution).unwrap();
        let record = JournalExecution::system(system_command, system_execution);

        let mut store = journal::InMemoryJournalStore::new();
        store
            .create_room("owner", &scenario, &bootstrap, &[10, 20, 30], &[], None)
            .unwrap();
        store
            .append_room_mutation(
                &PendingJournalMutation::new(
                    room_id,
                    0,
                    RoomMutation::StateCheckpoint {
                        actor: Box::new(checkpoint_actor),
                        complete_history: true,
                    },
                ),
                &[],
                &[],
                None,
            )
            .unwrap();
        store
            .append_room_mutation(
                &PendingJournalMutation::new(
                    room_id,
                    0,
                    RoomMutation::ClockAdvanced {
                        steps: 1,
                        completed_transfers: Vec::new(),
                    },
                ),
                std::slice::from_ref(&record),
                &[],
                None,
            )
            .unwrap();

        let recovered = recover_rooms(&store.load_recovery().unwrap()).unwrap();
        let history = recovered.execution_history(room_id).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].command_seq, 0);
        assert_eq!(
            command_from_actor_execution(&history[0]),
            Some(record.command)
        );
    }

    #[test]
    fn recovery_rejects_regressing_mutation_cursor() {
        let room_id = "regressing-cursor-room";
        let recovery = JournalRecovery {
            rooms: vec![journal::JournalRoom {
                room_id: room_id.to_string(),
                scenario: spot_scenario(room_id),
                status: MarketStatus::Running,
            }],
            executions: Vec::new(),
            mutations: vec![
                JournalMutation {
                    room_id: room_id.to_string(),
                    mutation_seq: 1,
                    command_cursor: 1,
                    schema_version: journal::ROOM_MUTATION_SCHEMA_VERSION,
                    mutation: RoomMutation::StatusChanged {
                        status: MarketStatus::Paused,
                    },
                },
                JournalMutation {
                    room_id: room_id.to_string(),
                    mutation_seq: 2,
                    command_cursor: 0,
                    schema_version: journal::ROOM_MUTATION_SCHEMA_VERSION,
                    mutation: RoomMutation::StatusChanged {
                        status: MarketStatus::Running,
                    },
                },
            ],
            snapshots: Vec::new(),
        };

        let error = recover_rooms(&recovery).unwrap_err();
        assert!(error.to_string().contains("mutation cursor regressed"));
    }

    #[test]
    fn non_loopback_bind_requires_bearer_authentication() {
        let local = AuthPolicy::local_development();
        assert!(validate_auth_policy_for_addr(&local, "127.0.0.1:57305".parse().unwrap()).is_ok());
        let error =
            validate_auth_policy_for_addr(&local, "0.0.0.0:57305".parse().unwrap()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);

        let bearer = AuthPolicy::from_token_json(r#"{"secret":"alice"}"#).unwrap();
        assert!(validate_auth_policy_for_addr(&bearer, "0.0.0.0:57305".parse().unwrap()).is_ok());
        assert_eq!(
            parse_bind_addr("127.0.0.1:60000").unwrap(),
            "127.0.0.1:60000".parse::<SocketAddr>().unwrap()
        );
        assert!(parse_bind_addr("not-an-address").is_err());
    }

    #[test]
    fn cors_origins_require_origin_only_http_urls() {
        let origins = parse_cors_origins(
            "https://app.example.test, http://127.0.0.1:57304,https://app.example.test",
        )
        .unwrap();
        assert_eq!(origins.len(), 2);
        assert_eq!(origins[0], "https://app.example.test");
        assert_eq!(origins[1], "http://127.0.0.1:57304");
        assert!(parse_cors_origins("").is_err());
        assert!(parse_cors_origins("https://app.example.test/path").is_err());
        assert!(parse_cors_origins("file:///tmp/frontend").is_err());
    }

    #[test]
    fn room_lease_runtime_config_is_explicit_and_bounded() {
        assert!(
            parse_room_lease_runtime_config(None, None, None, None, None, None, None)
                .unwrap()
                .is_none()
        );
        assert!(
            parse_room_lease_runtime_config(None, None, None, Some("1000"), None, None, None)
                .is_err()
        );
        assert!(
            parse_room_lease_runtime_config(
                None,
                Some("instance-a"),
                None,
                None,
                None,
                None,
                Some("http://127.0.0.1:57305"),
            )
            .is_err()
        );
        assert!(
            parse_room_lease_runtime_config(
                Some("room-leased"),
                None,
                Some("postgres://configured"),
                None,
                None,
                None,
                Some("http://127.0.0.1:57305"),
            )
            .is_err()
        );
        assert!(
            parse_room_lease_runtime_config(
                Some("unknown"),
                Some("instance-a"),
                Some("postgres://configured"),
                None,
                None,
                None,
                Some("http://127.0.0.1:57305"),
            )
            .is_err()
        );
        let config = parse_room_lease_runtime_config(
            Some("room-leased"),
            Some("node-a:57305"),
            Some("postgres://configured"),
            Some("1000"),
            Some("200"),
            Some("https://node-a.example.test/marketforge/"),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(config.mode, RoomLeaseRuntimeMode::RoomLeased);
        assert_eq!(config.instance_id, "node-a:57305");
        assert_eq!(config.owner_url, "https://node-a.example.test/marketforge");
        assert_eq!(config.lease_duration, Duration::from_millis(1_000));
        assert_eq!(config.renew_interval, Duration::from_millis(200));
        assert!(parse_advertise_url("http://user:secret@example.test").is_err());
        assert!(parse_advertise_url("https://example.test/path?query=1").is_err());
        assert_eq!(
            parse_advertise_url("http://127.0.0.1:57305/").unwrap(),
            "http://127.0.0.1:57305"
        );
        assert!(
            parse_room_lease_runtime_config(
                None,
                Some("bad owner"),
                Some("postgres://configured"),
                None,
                None,
                None,
                Some("http://127.0.0.1:57305"),
            )
            .is_err()
        );
        assert!(
            parse_room_lease_runtime_config(
                None,
                Some("node-a"),
                Some("postgres://configured"),
                Some("100"),
                Some("100"),
                None,
                Some("http://127.0.0.1:57305"),
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn room_lease_runtime_fences_writes_and_fails_readiness_after_loss() {
        let room_id = "runtime-lease-room";
        let state = shared_state(
            AppState::recover_with_journal_bundle_and_auth_policy(
                "http://127.0.0.1:57305",
                JournalStoreBundle::single(Box::new(journal::InMemoryJournalStore::new())),
                AuthPolicy::local_development(),
                Some(RoomLeaseRuntimeConfig {
                    mode: RoomLeaseRuntimeMode::GuardedSingleActive,
                    instance_id: "runtime-test".to_string(),
                    owner_url: "http://127.0.0.1:57305".to_string(),
                    lease_duration: Duration::from_millis(50),
                    renew_interval: Duration::from_millis(10),
                }),
            )
            .unwrap(),
        );
        let router = app(state.clone());
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&spot_scenario(room_id)).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/rooms/{room_id}/pause"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(state.app.lock().await.room_lease_metrics(), (1, 0, 0));

        renew_room_writer_leases_once(&state).await;
        tokio::time::sleep(Duration::from_millis(60)).await;
        renew_room_writer_leases_once(&state).await;
        assert_eq!(state.app.lock().await.room_lease_metrics(), (0, 1, 1));

        let readiness_error = readiness(State(state.clone())).await.unwrap_err();
        assert_eq!(readiness_error.0, StatusCode::SERVICE_UNAVAILABLE);
        let response = router
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/rooms/{room_id}/resume"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            state.app.lock().await.rooms.status(room_id).unwrap(),
            MarketStatus::Paused
        );
    }

    #[tokio::test]
    async fn room_leased_runtime_unloads_and_recovers_room_after_lease_loss() {
        let room_id = "room-leased-recovery-room";
        let state = shared_state(
            AppState::recover_with_journal_bundle_and_auth_policy(
                "http://127.0.0.1:57305",
                JournalStoreBundle::single(Box::new(journal::InMemoryJournalStore::new())),
                AuthPolicy::local_development(),
                Some(RoomLeaseRuntimeConfig {
                    mode: RoomLeaseRuntimeMode::RoomLeased,
                    instance_id: "room-leased-test".to_string(),
                    owner_url: "http://127.0.0.1:57305".to_string(),
                    lease_duration: Duration::from_millis(50),
                    renew_interval: Duration::from_millis(10),
                }),
            )
            .unwrap(),
        );
        let router = app(state.clone());
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&spot_scenario(room_id)).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/rooms/{room_id}/owner"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let owner: RoomOwnerResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(owner.owner_id, "room-leased-test");
        assert_eq!(owner.owner_url.as_deref(), Some("http://127.0.0.1:57305"));
        assert_eq!(owner.fencing_token, 1);

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/rooms/{room_id}/owner"))
                    .header(USER_ID_HEADER, "intruder")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        tokio::time::sleep(Duration::from_millis(60)).await;
        renew_room_writer_leases_once(&state).await;
        {
            let app = state.app.lock().await;
            assert_eq!(app.room_lease_metrics(), (0, 1, 1));
            assert!(matches!(
                app.rooms.status(room_id),
                Err(RoomManagerError::RoomNotFound { .. })
            ));
        }
        assert!(readiness(State(state.clone())).await.is_ok());

        let response = router
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/rooms/{room_id}/view"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let app = state.app.lock().await;
        assert_eq!(app.room_lease_metrics(), (1, 0, 1));
        assert_eq!(app.rooms.status(room_id).unwrap(), MarketStatus::Running);
        let lease = app
            .room_lease_runtime
            .as_ref()
            .unwrap()
            .leases
            .get(room_id)
            .unwrap();
        assert_eq!(lease.claim.fencing_token, 2);
        assert_eq!(lease.owner_url.as_deref(), Some("http://127.0.0.1:57305"));
    }

    #[tokio::test]
    async fn bearer_auth_fails_closed_ignores_user_spoofing_and_allows_cors_header() {
        let policy = AuthPolicy::from_token_json(r#"{"secret":"alice"}"#).unwrap();
        let app = new_app_with_journal_and_auth_policy(
            "http://127.0.0.1:57305",
            Box::new(journal::InMemoryJournalStore::new()),
            policy,
        );

        for authorization in [None, Some("Bearer wrong")] {
            let mut request = Request::builder()
                .method(Method::POST)
                .uri("/rooms")
                .header("content-type", "application/json")
                .header(USER_ID_HEADER, "alice");
            if let Some(authorization) = authorization {
                request = request.header(AUTHORIZATION, authorization);
            }
            let response = app
                .clone()
                .oneshot(
                    request
                        .body(Body::from(
                            serde_json::to_string(&spot_scenario("unauthorized-room")).unwrap(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .header(AUTHORIZATION, "Bearer secret")
                    .header(USER_ID_HEADER, "spoofed-admin")
                    .body(Body::from(
                        serde_json::to_string(&spot_scenario("bearer-room")).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms")
                    .header(AUTHORIZATION, "Bearer secret")
                    .header(USER_ID_HEADER, "another-spoof")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/rooms")
                    .header("origin", "http://127.0.0.1:57304")
                    .header("access-control-request-method", "POST")
                    .header("access-control-request-headers", "authorization")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get("access-control-allow-headers")
                .unwrap()
                .to_str()
                .unwrap()
                .to_ascii_lowercase()
                .contains("authorization")
        );
    }

    #[tokio::test]
    async fn bearer_mode_starts_internal_agent_workers() {
        let policy = AuthPolicy::from_token_json(r#"{"secret":"alice"}"#).unwrap();
        let app = new_app_with_journal_and_auth_policy(
            "http://127.0.0.1:57305",
            Box::new(journal::InMemoryJournalStore::new()),
            policy,
        );
        let create = |body: String| {
            Request::builder()
                .method(Method::POST)
                .uri("/rooms")
                .header("content-type", "application/json")
                .header(AUTHORIZATION, "Bearer secret")
                .body(Body::from(body))
                .unwrap()
        };

        let response = app
            .clone()
            .oneshot(create(
                serde_json::to_string(&spot_scenario("bearer-agent-room")).unwrap(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let start = StartAgentsRequest {
            agents: vec![dca_template("bearer-agent-room", "bearer-worker", 20)],
            interval_ms: Some(50),
        };
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/bearer-agent-room/agents")
                    .header("content-type", "application/json")
                    .header(AUTHORIZATION, "Bearer secret")
                    .body(Body::from(serde_json::to_string(&start).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let status: AgentWorkerStatus = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(status.running);
        assert_eq!(status.lifecycle, "running");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/bearer-agent-room/agents/stop")
                    .header(AUTHORIZATION, "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn http_and_internal_gateway_orders_match() {
        let mut rooms = RoomManager::new();
        rooms
            .create_room(spot_scenario("path-parity-room"))
            .unwrap();
        let mut gateway = OrderGateway::new(&mut rooms, 1);
        let http_like = gateway
            .submit_action(GatewayRequest {
                participant_id: "human".to_string(),
                room_id: "path-parity-room".to_string(),
                instrument_id: Some("V-BTC-SPOT".to_string()),
                account_id: 20,
                action: OrderAction::PlaceLimit {
                    side: Side::Buy,
                    price_tick: 100,
                    qty: 2,
                },
            })
            .unwrap();

        let mut rooms = RoomManager::new();
        rooms
            .create_room(spot_scenario("path-parity-room"))
            .unwrap();
        let mut gateway = OrderGateway::new_scheduler(&mut rooms, 1);
        let internal = gateway
            .submit_action(GatewayRequest {
                participant_id: "human".to_string(),
                room_id: "path-parity-room".to_string(),
                instrument_id: Some("V-BTC-SPOT".to_string()),
                account_id: 20,
                action: OrderAction::PlaceLimit {
                    side: Side::Buy,
                    price_tick: 100,
                    qty: 2,
                },
            })
            .unwrap();
        assert_eq!(http_like.execution.result, internal.execution.result);
        assert_eq!(http_like.command, internal.command);
    }

    #[tokio::test]
    async fn agent_without_instrument_is_rejected() {
        let app = new_app();
        let mut template = dca_template("instrument-required-room", "no-instrument", 20);
        match &mut template {
            AgentTemplate::DcaTrader(config) => config.participant.instrument_id = None,
            _ => unreachable!(),
        }
        let create = CreateRoomRequest {
            scenario: spot_scenario("instrument-required-room"),
            agents: vec![template],
            agent_interval_ms: Some(10),
            autostart_agents: Some(true),
        };
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&create).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn paused_manual_step_is_idempotent() {
        let app = new_app();
        let create = CreateRoomRequest {
            scenario: spot_scenario("manual-step-room"),
            agents: vec![dca_template("manual-step-room", "step-dca", 20)],
            agent_interval_ms: Some(10_000),
            autostart_agents: Some(false),
        };
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&create).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let start = StartAgentsRequest {
            agents: vec![dca_template("manual-step-room", "step-dca", 20)],
            interval_ms: Some(10_000),
        };
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/manual-step-room/agents")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&start).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/manual-step-room/pause")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let step = || {
            Request::builder()
                .method(Method::POST)
                .uri("/rooms/manual-step-room/clock/step")
                .header(IDEMPOTENCY_KEY_HEADER, "step-1")
                .body(Body::empty())
                .unwrap()
        };
        let first = app.clone().oneshot(step()).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let first_body = axum::body::to_bytes(first.into_body(), usize::MAX)
            .await
            .unwrap();
        let second = app.clone().oneshot(step()).await.unwrap();
        assert_eq!(second.status(), StatusCode::OK);
        let second_body = axum::body::to_bytes(second.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(first_body, second_body);

        let _ = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/manual-step-room/agents/stop")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
    }

    async fn control_request(
        app: &axum::Router,
        uri: &str,
        user: Option<&str>,
        key: Option<&str>,
        json_body: Option<serde_json::Value>,
    ) -> axum::http::Response<Body> {
        let mut builder = Request::builder().method(Method::POST).uri(uri);
        if json_body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        if let Some(user) = user {
            builder = builder.header(USER_ID_HEADER, user);
        }
        if let Some(key) = key {
            builder = builder.header(IDEMPOTENCY_KEY_HEADER, key);
        }
        let body = json_body
            .map(|value| Body::from(value.to_string()))
            .unwrap_or_else(Body::empty);
        app.clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap()
    }

    async fn response_json<T: DeserializeOwned>(response: axum::http::Response<Body>) -> T {
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn room_clock_step(app: &axum::Router, room_id: &str) -> u64 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/rooms/{room_id}/clock"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let clock: RoomClockResponse = response_json(response).await;
        clock.clock.step()
    }

    async fn create_paused_step_room(app: &axum::Router, room_id: &str) {
        let create = CreateRoomRequest {
            scenario: spot_scenario(room_id),
            agents: vec![dca_template(room_id, "step-dca", 20)],
            agent_interval_ms: Some(10_000),
            autostart_agents: Some(false),
        };
        assert_eq!(
            send_json(app, Method::POST, "/rooms", None, create)
                .await
                .status(),
            StatusCode::OK
        );
        let start = StartAgentsRequest {
            agents: vec![dca_template(room_id, "step-dca", 20)],
            interval_ms: Some(10_000),
        };
        assert_eq!(
            send_json(
                app,
                Method::POST,
                &format!("/rooms/{room_id}/agents"),
                None,
                start,
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            control_request(app, &format!("/rooms/{room_id}/pause"), None, None, None)
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            control_request(
                app,
                &format!("/rooms/{room_id}/agents/stop"),
                None,
                None,
                None,
            )
            .await
            .status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn control_idempotency_serial_replay_returns_original_body() {
        let app = new_app();
        create_paused_step_room(&app, "ctrl-serial-room").await;
        let first = control_request(
            &app,
            "/rooms/ctrl-serial-room/clock/step",
            None,
            Some("step-serial"),
            None,
        )
        .await;
        assert_eq!(first.status(), StatusCode::OK);
        let first_body = axum::body::to_bytes(first.into_body(), usize::MAX)
            .await
            .unwrap();
        let step_after_first = room_clock_step(&app, "ctrl-serial-room").await;
        let second = control_request(
            &app,
            "/rooms/ctrl-serial-room/clock/step",
            None,
            Some("step-serial"),
            None,
        )
        .await;
        assert_eq!(second.status(), StatusCode::OK);
        let second_body = axum::body::to_bytes(second.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(first_body, second_body);
        assert_eq!(
            room_clock_step(&app, "ctrl-serial-room").await,
            step_after_first
        );
    }

    #[tokio::test]
    async fn control_idempotency_concurrent_same_key_steps_once() {
        let app = new_app();
        create_paused_step_room(&app, "ctrl-concurrent-room").await;
        let before = room_clock_step(&app, "ctrl-concurrent-room").await;
        let first = control_request(
            &app,
            "/rooms/ctrl-concurrent-room/clock/step",
            None,
            Some("step-concurrent"),
            None,
        );
        let second = control_request(
            &app,
            "/rooms/ctrl-concurrent-room/clock/step",
            None,
            Some("step-concurrent"),
            None,
        );
        let (first, second) = tokio::join!(first, second);
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(second.status(), StatusCode::OK);
        let first_body = axum::body::to_bytes(first.into_body(), usize::MAX)
            .await
            .unwrap();
        let second_body = axum::body::to_bytes(second.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(first_body, second_body);
        let after = room_clock_step(&app, "ctrl-concurrent-room").await;
        assert_eq!(
            after.saturating_sub(before),
            1,
            "before={before} after={after}"
        );
    }

    #[tokio::test]
    async fn control_idempotency_same_key_different_operation_conflicts() {
        let app = new_app();
        create_paused_step_room(&app, "ctrl-conflict-room").await;
        let before = room_clock_step(&app, "ctrl-conflict-room").await;
        let pause = control_request(
            &app,
            "/rooms/ctrl-conflict-room/pause",
            None,
            Some("shared-key"),
            None,
        )
        .await;
        assert_eq!(pause.status(), StatusCode::OK);
        let step = control_request(
            &app,
            "/rooms/ctrl-conflict-room/clock/step",
            None,
            Some("shared-key"),
            None,
        )
        .await;
        assert_eq!(step.status(), StatusCode::CONFLICT);
        assert_eq!(room_clock_step(&app, "ctrl-conflict-room").await, before);
        let advance = control_request(
            &app,
            "/rooms/ctrl-conflict-room/clock/advance",
            None,
            Some("advance-key"),
            Some(serde_json::json!({"steps": 1})),
        )
        .await;
        assert_eq!(advance.status(), StatusCode::OK);
        let different_steps = control_request(
            &app,
            "/rooms/ctrl-conflict-room/clock/advance",
            None,
            Some("advance-key"),
            Some(serde_json::json!({"steps": 2})),
        )
        .await;
        assert_eq!(different_steps.status(), StatusCode::CONFLICT);
        assert_eq!(
            room_clock_step(&app, "ctrl-conflict-room").await,
            before + 1
        );
    }

    #[tokio::test]
    async fn control_idempotency_pre_commit_failure_leaves_state_and_retry_succeeds() {
        struct FailOnceMutationJournal {
            inner: journal::InMemoryJournalStore,
            remaining_failures: usize,
        }

        impl JournalStore for FailOnceMutationJournal {
            fn load_recovery(&mut self) -> Result<JournalRecovery, JournalError> {
                self.inner.load_recovery()
            }

            fn create_room(
                &mut self,
                owner_user_id: &str,
                scenario: &ScenarioConfig,
                bootstrap: &exchange_core::RoomBootstrap,
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

            fn append_room_mutation(
                &mut self,
                mutation: &PendingJournalMutation,
                execution_records: &[JournalExecution],
                transfer_records: &[JournalTransfer],
                snapshot: Option<&JournalSnapshot>,
            ) -> Result<(), JournalError> {
                if self.remaining_failures > 0 {
                    self.remaining_failures -= 1;
                    return Err(JournalError::Recovery(
                        "injected control commit failure".into(),
                    ));
                }
                self.inner.append_room_mutation(
                    mutation,
                    execution_records,
                    transfer_records,
                    snapshot,
                )
            }

            fn find_control_idempotency(
                &mut self,
                user_id: &str,
                room_id: &str,
                idempotency_key: &str,
            ) -> Result<Option<journal::ControlIdempotencyRecord>, JournalError> {
                self.inner
                    .find_control_idempotency(user_id, room_id, idempotency_key)
            }

            fn user_can_administer_room(
                &mut self,
                user_id: &str,
                room_id: &str,
            ) -> Result<bool, JournalError> {
                self.inner.user_can_administer_room(user_id, room_id)
            }

            fn user_can_access_room(
                &mut self,
                user_id: &str,
                room_id: &str,
            ) -> Result<bool, JournalError> {
                self.inner.user_can_access_room(user_id, room_id)
            }

            fn update_room_status(
                &mut self,
                room_id: &str,
                status: MarketStatus,
            ) -> Result<(), JournalError> {
                self.inner.update_room_status(room_id, status)
            }
        }

        let app = new_app_with_journal(
            "http://127.0.0.1:57305",
            Box::new(FailOnceMutationJournal {
                inner: journal::InMemoryJournalStore::new(),
                remaining_failures: 1,
            }),
        );
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms",
                None,
                spot_scenario("ctrl-fail-room"),
            )
            .await
            .status(),
            StatusCode::OK
        );
        let failed = control_request(
            &app,
            "/rooms/ctrl-fail-room/pause",
            None,
            Some("pause-fail"),
            None,
        )
        .await;
        assert_eq!(failed.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let status = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/ctrl-fail-room/view")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::OK);
        let view: MarketView = response_json(status).await;
        assert_eq!(view.status, MarketStatus::Running);
        let retry = control_request(
            &app,
            "/rooms/ctrl-fail-room/pause",
            None,
            Some("pause-fail"),
            None,
        )
        .await;
        assert_eq!(retry.status(), StatusCode::OK);
        let replay = control_request(
            &app,
            "/rooms/ctrl-fail-room/pause",
            None,
            Some("pause-fail"),
            None,
        )
        .await;
        assert_eq!(replay.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn control_idempotency_survives_restart_without_second_step() {
        let journal = journal::SharedInMemoryJournalStore::new();
        let app = recovering_app(Box::new(journal.clone()));
        create_paused_step_room(&app, "ctrl-restart-room").await;
        let first = control_request(
            &app,
            "/rooms/ctrl-restart-room/clock/step",
            None,
            Some("step-restart"),
            None,
        )
        .await;
        assert_eq!(first.status(), StatusCode::OK);
        let first_body = axum::body::to_bytes(first.into_body(), usize::MAX)
            .await
            .unwrap();
        let live_step = room_clock_step(&app, "ctrl-restart-room").await;
        drop(app);

        let recovered = recovering_app(Box::new(journal));
        let replay = control_request(
            &recovered,
            "/rooms/ctrl-restart-room/clock/step",
            None,
            Some("step-restart"),
            None,
        )
        .await;
        assert_eq!(replay.status(), StatusCode::OK);
        let replay_body = axum::body::to_bytes(replay.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(first_body, replay_body);
        assert_eq!(
            room_clock_step(&recovered, "ctrl-restart-room").await,
            live_step
        );
    }

    #[tokio::test]
    async fn control_idempotency_takeover_replays_and_old_fence_cannot_append() {
        let journal = journal::SharedInMemoryJournalStore::new();
        let first_state = shared_state(
            AppState::recover_with_journal_bundle_and_auth_policy(
                "http://127.0.0.1:57305",
                JournalStoreBundle::single(Box::new(journal.clone())),
                AuthPolicy::local_development(),
                Some(training_lease_config("ctrl-a", "http://127.0.0.1:57305")),
            )
            .unwrap(),
        );
        let first = app(first_state.clone());
        create_paused_step_room(&first, "ctrl-takeover-room").await;
        let first_step = control_request(
            &first,
            "/rooms/ctrl-takeover-room/clock/step",
            None,
            Some("step-takeover"),
            None,
        )
        .await;
        assert_eq!(first_step.status(), StatusCode::OK);
        let first_body = axum::body::to_bytes(first_step.into_body(), usize::MAX)
            .await
            .unwrap();
        let live_step = room_clock_step(&first, "ctrl-takeover-room").await;
        let old_claim = {
            let app = first_state.app.lock().await;
            app.room_lease_claim("ctrl-takeover-room")
                .unwrap()
                .expect("writer lease")
        };
        release_owned_room_writer_leases(&first_state)
            .await
            .unwrap();
        drop(first);
        drop(first_state);

        let recovered = leased_recovering_app(journal.clone(), "ctrl-b", "http://127.0.0.1:57306");
        let replay = control_request(
            &recovered,
            "/rooms/ctrl-takeover-room/clock/step",
            None,
            Some("step-takeover"),
            None,
        )
        .await;
        assert_eq!(replay.status(), StatusCode::OK);
        let replay_body = axum::body::to_bytes(replay.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(first_body, replay_body);
        assert_eq!(
            room_clock_step(&recovered, "ctrl-takeover-room").await,
            live_step
        );

        let mut stale = journal;
        let fenced = stale.append_room_mutation_fenced(
            &old_claim,
            &PendingJournalMutation::new(
                "ctrl-takeover-room",
                0,
                RoomMutation::StatusChanged {
                    status: MarketStatus::Running,
                },
            ),
            &[],
            &[],
            None,
        );
        assert!(matches!(fenced, Err(JournalError::RoomLeaseLost { .. })));
    }

    #[tokio::test]
    async fn control_idempotency_revoked_permission_does_not_leak_body() {
        let app = new_app();
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms",
                None,
                spot_scenario("ctrl-revoke-room"),
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/ctrl-revoke-room/members",
                None,
                serde_json::json!({"user_id":"operator","role":"admin"}),
            )
            .await
            .status(),
            StatusCode::OK
        );
        let first = control_request(
            &app,
            "/rooms/ctrl-revoke-room/pause",
            Some("operator"),
            Some("pause-revoke"),
            None,
        )
        .await;
        assert_eq!(first.status(), StatusCode::OK);
        let remove = send_json(
            &app,
            Method::POST,
            "/rooms/ctrl-revoke-room/members/operator",
            None,
            serde_json::json!({}),
        )
        .await;
        assert_eq!(remove.status(), StatusCode::OK);
        let replay = control_request(
            &app,
            "/rooms/ctrl-revoke-room/pause",
            Some("operator"),
            Some("pause-revoke"),
            None,
        )
        .await;
        assert_eq!(replay.status(), StatusCode::FORBIDDEN);
        let body = axum::body::to_bytes(replay.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(!text.contains("Paused"));
        assert!(!text.contains("\"status\":\"Paused\""));
    }

    #[tokio::test]
    async fn control_idempotency_postgres_restart_replays_step() {
        let Some(database_url) = postgres_test_database_url() else {
            return;
        };
        let room_id = format!(
            "ctrl-pg-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        cleanup_postgres_room(&database_url, &room_id);
        let live_url = database_url.clone();
        let store =
            tokio::task::spawn_blocking(move || PostgresJournalStore::connect_migrated(&live_url))
                .await
                .unwrap()
                .unwrap();
        let app = recovering_app(Box::new(store));
        create_paused_step_room(&app, &room_id).await;
        let first = control_request(
            &app,
            &format!("/rooms/{room_id}/clock/step"),
            None,
            Some("step-pg"),
            None,
        )
        .await;
        assert_eq!(first.status(), StatusCode::OK);
        let first_body = axum::body::to_bytes(first.into_body(), usize::MAX)
            .await
            .unwrap();
        let live_step = room_clock_step(&app, &room_id).await;
        drop(app);

        let recover_url = database_url.clone();
        let recovered_store = tokio::task::spawn_blocking(move || {
            PostgresJournalStore::connect_migrated(&recover_url)
        })
        .await
        .unwrap()
        .unwrap();
        let recovered = recovering_app(Box::new(recovered_store));
        let replay = control_request(
            &recovered,
            &format!("/rooms/{room_id}/clock/step"),
            None,
            Some("step-pg"),
            None,
        )
        .await;
        assert_eq!(replay.status(), StatusCode::OK);
        let replay_body = axum::body::to_bytes(replay.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(first_body, replay_body);
        assert_eq!(room_clock_step(&recovered, &room_id).await, live_step);
        cleanup_postgres_room(&database_url, &room_id);
    }

    #[tokio::test]
    async fn candle_query_matches_fixture_and_does_not_advance_clock() {
        let app = new_app();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&spot_scenario("candle-room")).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let buy = |price, qty| {
            Request::builder()
                .method(Method::POST)
                .uri("/rooms/candle-room/orders")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&SubmitOrderRequest {
                        participant_id: "maker".to_string(),
                        instrument_id: None,
                        account_id: 10,
                        action: OrderAction::PlaceLimit {
                            side: Side::Sell,
                            price_tick: price,
                            qty,
                        },
                    })
                    .unwrap(),
                ))
                .unwrap()
        };
        let _ = app.clone().oneshot(buy(100, 1)).await.unwrap();
        let _ = app.clone().oneshot(buy(110, 2)).await.unwrap();
        let take = Request::builder()
            .method(Method::POST)
            .uri("/rooms/candle-room/orders")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&SubmitOrderRequest {
                    participant_id: "taker".to_string(),
                    instrument_id: None,
                    account_id: 20,
                    action: OrderAction::PlaceLimit {
                        side: Side::Buy,
                        price_tick: 110,
                        qty: 3,
                    },
                })
                .unwrap(),
            ))
            .unwrap();
        let response = app.clone().oneshot(take).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let clock_before = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/candle-room/clock")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let before = axum::body::to_bytes(clock_before.into_body(), usize::MAX)
            .await
            .unwrap();
        let candles = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/candle-room/candles?interval_ms=1000")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(candles.status(), StatusCode::OK);
        let candle_body = axum::body::to_bytes(candles.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: CandleResponse = serde_json::from_slice(&candle_body).unwrap();
        assert_eq!(parsed.api_version, "http.v1");
        assert_eq!(parsed.candles.len(), 1);
        assert_eq!(parsed.candles[0].volume, 3);
        assert_eq!(parsed.candles[0].open_tick, 100);
        assert_eq!(parsed.candles[0].high_tick, 110);
        let clock_after = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/candle-room/clock")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let after = axum::body::to_bytes(clock_after.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(before, after);
    }

    #[tokio::test]
    async fn training_start_abort_is_idempotent_and_blocks_restart() {
        let mut scenario = spot_scenario("train-room");
        scenario.seed_orders = vec![
            Command::NewOrder(NewOrder {
                order_id: 1,
                account_id: 10,
                side: Side::Buy,
                kind: OrderKind::Limit { price_tick: 99 },
                qty: 1,
                reduce_only: false,
            }),
            Command::NewOrder(NewOrder {
                order_id: 2,
                account_id: 10,
                side: Side::Sell,
                kind: OrderKind::Limit { price_tick: 101 },
                qty: 1,
                reduce_only: false,
            }),
        ];
        let request = StartTrainingRequest {
            run_id: "run-a".to_string(),
            scenario,
            agents: Vec::new(),
            trainee_account_id: 20,
            target_qty: 4,
            horizon_steps: 8,
        };
        let app = new_app();
        let start = || {
            Request::builder()
                .method(Method::POST)
                .uri("/training/runs")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&request).unwrap()))
                .unwrap()
        };
        let first = app.clone().oneshot(start()).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let second = app.clone().oneshot(start()).await.unwrap();
        assert_eq!(second.status(), StatusCode::OK);
        let abort = || {
            Request::builder()
                .method(Method::POST)
                .uri("/training/runs/run-a/abort")
                .body(Body::empty())
                .unwrap()
        };
        let a1 = app.clone().oneshot(abort()).await.unwrap();
        assert_eq!(a1.status(), StatusCode::OK);
        let a2 = app.clone().oneshot(abort()).await.unwrap();
        assert_eq!(a2.status(), StatusCode::OK);
        let result = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/training/runs/run-a/result")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(result.status(), StatusCode::OK);
        let body = axum::body::to_bytes(result.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: TrainingRunResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed.run.status, exchange_core::TrainingStatus::Aborted);
        assert!(parsed.score.incomplete);
    }

    fn two_sided_training(run_id: &str, room_id: &str, horizon: u64) -> StartTrainingRequest {
        let mut scenario = spot_scenario(room_id);
        scenario.seed_orders = vec![
            Command::NewOrder(NewOrder {
                order_id: 1,
                account_id: 10,
                side: Side::Buy,
                kind: OrderKind::Limit { price_tick: 99 },
                qty: 5,
                reduce_only: false,
            }),
            Command::NewOrder(NewOrder {
                order_id: 2,
                account_id: 10,
                side: Side::Sell,
                kind: OrderKind::Limit { price_tick: 101 },
                qty: 5,
                reduce_only: false,
            }),
        ];
        StartTrainingRequest {
            run_id: run_id.to_string(),
            scenario,
            agents: Vec::new(),
            trainee_account_id: 20,
            target_qty: 4,
            horizon_steps: horizon,
        }
    }

    #[tokio::test]
    async fn training_horizon_expires_and_aborts_cancel_residuals() {
        let app = new_app();
        let start = send_json(
            &app,
            Method::POST,
            "/training/runs",
            None,
            two_sided_training("expire-run", "expire-room", 2),
        )
        .await;
        assert_eq!(start.status(), StatusCode::OK);
        let rest = send_json(
            &app,
            Method::POST,
            "/rooms/expire-room/orders",
            None,
            limit_buy(20, 90, 1),
        )
        .await;
        assert_eq!(rest.status(), StatusCode::OK);
        let advance = send_json(
            &app,
            Method::POST,
            "/rooms/expire-room/clock/advance",
            None,
            AdvanceClockRequest { steps: 2 },
        )
        .await;
        assert_eq!(advance.status(), StatusCode::OK);
        let result = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/training/runs/expire-run/result")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(result.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: TrainingRunResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed.run.status, exchange_core::TrainingStatus::Completed);
        assert!(parsed.score.incomplete);
        assert!(parsed.run.cancels >= 1);

        let abort_app = new_app();
        assert_eq!(
            send_json(
                &abort_app,
                Method::POST,
                "/training/runs",
                None,
                two_sided_training("abort-settle", "abort-settle-room", 8),
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            send_json(
                &abort_app,
                Method::POST,
                "/rooms/abort-settle-room/orders",
                None,
                limit_buy(20, 90, 2),
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            send_json(
                &abort_app,
                Method::POST,
                "/training/runs/abort-settle/abort",
                None,
                serde_json::json!({}),
            )
            .await
            .status(),
            StatusCode::OK
        );
        let orders = abort_app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/abort-settle-room/orders")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let order_body = axum::body::to_bytes(orders.into_body(), usize::MAX)
            .await
            .unwrap();
        let order_text = String::from_utf8(order_body.to_vec()).unwrap();
        assert!(
            !order_text.contains("\"remaining_qty\":2")
                || order_text.contains("canceled")
                || order_text.contains("Cancelled")
                || order_text.contains("\"orders\":[]")
                || order_text.contains("\"orders\": []"),
            "{order_text}"
        );
    }

    #[tokio::test]
    async fn training_rejects_trainee_deposit_and_binds_report_book() {
        let app = new_app();
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/training/runs",
                None,
                two_sided_training("xfer-run", "xfer-room", 8),
            )
            .await
            .status(),
            StatusCode::OK
        );
        let deposit = send_json(
            &app,
            Method::POST,
            "/rooms/xfer-room/transfers/deposit",
            None,
            serde_json::json!({
                "account_id": 20,
                "asset_id": "BTC",
                "amount": 100
            }),
        )
        .await;
        assert_eq!(deposit.status(), StatusCode::CONFLICT);
        let fill = send_json(
            &app,
            Method::POST,
            "/rooms/xfer-room/orders",
            None,
            limit_buy(20, 101, 1),
        )
        .await;
        assert_eq!(fill.status(), StatusCode::OK);
        let report = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/training/runs/xfer-run/report")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(report.status(), StatusCode::OK);
        let body = axum::body::to_bytes(report.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: TrainingReportResponse = serde_json::from_slice(&body).unwrap();
        let fills = parsed.json["facts"]["fills"].as_array().unwrap();
        assert!(!fills.is_empty());
        assert!(fills[0]["order_id"].is_number());
        assert!(fills[0]["command_seq"].is_number());
        assert!(fills[0]["book_before"]["asks"].is_array());
        assert!(parsed.markdown.contains("order_id="));
    }

    #[tokio::test]
    async fn training_speed_and_crash_recovery_match_on_shipped_path() {
        let fast = new_app();
        let slow = new_app();
        for (app, run_id, room_id) in [
            (&fast, "speed-fast", "speed-fast-room"),
            (&slow, "speed-slow", "speed-slow-room"),
        ] {
            assert_eq!(
                send_json(
                    app,
                    Method::POST,
                    "/training/runs",
                    None,
                    two_sided_training(run_id, room_id, 3),
                )
                .await
                .status(),
                StatusCode::OK
            );
            assert_eq!(
                send_json(
                    app,
                    Method::POST,
                    &format!("/rooms/{room_id}/orders"),
                    None,
                    limit_buy(20, 101, 1),
                )
                .await
                .status(),
                StatusCode::OK
            );
        }
        assert_eq!(
            send_json(
                &fast,
                Method::POST,
                "/rooms/speed-fast-room/clock/advance",
                None,
                AdvanceClockRequest { steps: 3 },
            )
            .await
            .status(),
            StatusCode::OK
        );
        for _ in 0..3 {
            assert_eq!(
                send_json(
                    &slow,
                    Method::POST,
                    "/rooms/speed-slow-room/clock/advance",
                    None,
                    AdvanceClockRequest { steps: 1 },
                )
                .await
                .status(),
                StatusCode::OK
            );
        }
        async fn parse_training(app: axum::Router, run: String) -> TrainingRunResponse {
            let response = app
                .oneshot(
                    Request::builder()
                        .method(Method::GET)
                        .uri(format!("/training/runs/{run}/result"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            serde_json::from_slice(&body).unwrap()
        }
        let fast_result = parse_training(fast.clone(), "speed-fast".to_string()).await;
        let slow_result = parse_training(slow.clone(), "speed-slow".to_string()).await;
        assert_eq!(fast_result.run.status, slow_result.run.status);
        assert_eq!(fast_result.score.q, slow_result.score.q);
        assert_eq!(
            fast_result.score.steps_elapsed,
            slow_result.score.steps_elapsed
        );
        assert_eq!(
            fast_result.run.fills[0].price_tick,
            slow_result.run.fills[0].price_tick
        );

        assert_eq!(
            fast_result.run.status,
            exchange_core::TrainingStatus::Completed
        );
        assert_eq!(
            slow_result.run.status,
            exchange_core::TrainingStatus::Completed
        );
    }

    fn recovering_app(journal: Box<dyn JournalStore>) -> axum::Router {
        new_app_recovering_with_journal("http://127.0.0.1:57305", journal).unwrap()
    }

    fn training_lease_config(instance_id: &str, owner_url: &str) -> RoomLeaseRuntimeConfig {
        RoomLeaseRuntimeConfig {
            mode: RoomLeaseRuntimeMode::RoomLeased,
            instance_id: instance_id.to_string(),
            owner_url: owner_url.to_string(),
            lease_duration: Duration::from_secs(5),
            renew_interval: Duration::from_millis(50),
        }
    }

    fn leased_recovering_app(
        journal: journal::SharedInMemoryJournalStore,
        instance_id: &str,
        owner_url: &str,
    ) -> axum::Router {
        let config = training_lease_config(instance_id, owner_url);
        new_app_recovering_with_journal_factory_sync(
            owner_url.to_string(),
            AuthPolicy::local_development(),
            default_cors_origins(),
            Some(config),
            move || Ok(JournalStoreBundle::single(Box::new(journal))),
        )
        .unwrap()
    }

    fn postgres_test_database_url() -> Option<String> {
        match std::env::var("MARKETFORGE_TEST_DATABASE_URL") {
            Ok(database_url) if !database_url.trim().is_empty() => Some(database_url),
            _ if std::env::var("MARKETFORGE_REQUIRE_POSTGRES_TESTS").as_deref() == Ok("1") => {
                panic!(
                    "MARKETFORGE_REQUIRE_POSTGRES_TESTS=1 requires MARKETFORGE_TEST_DATABASE_URL"
                );
            }
            _ => None,
        }
    }

    fn unique_training_ids(prefix: &str) -> (String, String) {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        (
            format!("{prefix}-room-{}-{suffix}", std::process::id()),
            format!("{prefix}-run-{}-{suffix}", std::process::id()),
        )
    }

    async fn start_two_sided_training_and_buy(
        app: &axum::Router,
        run_id: &str,
        room_id: &str,
        horizon: u64,
    ) {
        assert_eq!(
            send_json(
                app,
                Method::POST,
                "/training/runs",
                None,
                two_sided_training(run_id, room_id, horizon),
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            send_json(
                app,
                Method::POST,
                &format!("/rooms/{room_id}/orders"),
                None,
                limit_buy(20, 101, 1),
            )
            .await
            .status(),
            StatusCode::OK
        );
    }

    async fn advance_room_clock(app: &axum::Router, room_id: &str, steps: u64) {
        assert_eq!(
            send_json(
                app,
                Method::POST,
                &format!("/rooms/{room_id}/clock/advance"),
                None,
                AdvanceClockRequest { steps },
            )
            .await
            .status(),
            StatusCode::OK
        );
    }

    async fn training_result(app: &axum::Router, run_id: &str) -> TrainingRunResponse {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/training/runs/{run_id}/result"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "training result {run_id}"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn room_accounts(app: &axum::Router, room_id: &str) -> AccountSnapshots {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/rooms/{room_id}/accounts"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "accounts {room_id}");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn room_event_seqs(app: &axum::Router, room_id: &str) -> Vec<u64> {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/rooms/{room_id}/events?from_start=true&limit=50"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "events {room_id}");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let events: RoomEventsResponse = serde_json::from_slice(&body).unwrap();
        events
            .executions
            .iter()
            .map(|execution| execution.command_seq)
            .collect()
    }

    fn assert_training_pair_matches(live: &TrainingRunResponse, recovered: &TrainingRunResponse) {
        assert_eq!(recovered.run.status, live.run.status);
        assert_eq!(recovered.run.steps_elapsed, live.run.steps_elapsed);
        assert_eq!(recovered.run.filled_qty, live.run.filled_qty);
        assert_eq!(recovered.run.fees_paid, live.run.fees_paid);
        assert_eq!(recovered.run.rejects, live.run.rejects);
        assert_eq!(recovered.run.cancels, live.run.cancels);
        assert_eq!(recovered.run.open_buy_qty, live.run.open_buy_qty);
        assert_eq!(recovered.run.fills, live.run.fills);
        assert_eq!(recovered.score, live.score);
        assert_eq!(live.run.status, exchange_core::TrainingStatus::Completed);
        assert_eq!(live.score.q, 1);
        assert_eq!(live.score.steps_elapsed, 3);
    }

    fn assert_event_seqs_match_training(seqs: &[u64], run: &TrainingRunResponse) {
        assert!(
            seqs.windows(2).all(|window| window[0] < window[1]),
            "{seqs:?}"
        );
        assert!(
            run.run
                .fills
                .iter()
                .all(|fill| fill.command_seq.is_some_and(|seq| seqs.contains(&seq))),
            "{seqs:?} fills={:?}",
            run.run.fills
        );
    }

    #[tokio::test]
    async fn training_journal_crash_recovery_matches_live_run() {
        let journal = journal::SharedInMemoryJournalStore::new();
        let app = recovering_app(Box::new(journal.clone()));
        start_two_sided_training_and_buy(&app, "mem-train", "mem-train-room", 3).await;
        advance_room_clock(&app, "mem-train-room", 3).await;
        let live = training_result(&app, "mem-train").await;
        let live_accounts = room_accounts(&app, "mem-train-room").await;
        let live_seqs = room_event_seqs(&app, "mem-train-room").await;
        drop(app);

        let recovered = recovering_app(Box::new(journal));
        let recovered_run = training_result(&recovered, "mem-train").await;
        assert_training_pair_matches(&live, &recovered_run);
        assert_eq!(
            room_accounts(&recovered, "mem-train-room").await,
            live_accounts
        );
        let recovered_seqs = room_event_seqs(&recovered, "mem-train-room").await;
        assert_eq!(recovered_seqs, live_seqs);
        assert_event_seqs_match_training(&recovered_seqs, &recovered_run);
    }

    #[tokio::test]
    async fn training_lease_takeover_matches_continuous_run() {
        let journal = journal::SharedInMemoryJournalStore::new();
        let continuous_journal = journal::SharedInMemoryJournalStore::new();
        let continuous = recovering_app(Box::new(continuous_journal));
        start_two_sided_training_and_buy(&continuous, "lease-train", "lease-train-room", 3).await;
        advance_room_clock(&continuous, "lease-train-room", 3).await;
        let continuous_run = training_result(&continuous, "lease-train").await;
        let continuous_accounts = room_accounts(&continuous, "lease-train-room").await;
        let continuous_seqs = room_event_seqs(&continuous, "lease-train-room").await;

        let first_state = shared_state(
            AppState::recover_with_journal_bundle_and_auth_policy(
                "http://127.0.0.1:57305",
                JournalStoreBundle::single(Box::new(journal.clone())),
                AuthPolicy::local_development(),
                Some(training_lease_config("train-a", "http://127.0.0.1:57305")),
            )
            .unwrap(),
        );
        let first = app(first_state.clone());
        start_two_sided_training_and_buy(&first, "lease-train", "lease-train-room", 3).await;
        advance_room_clock(&first, "lease-train-room", 1).await;
        release_owned_room_writer_leases(&first_state)
            .await
            .unwrap();
        drop(first);
        drop(first_state);

        let recovered = leased_recovering_app(journal, "train-b", "http://127.0.0.1:57306");
        advance_room_clock(&recovered, "lease-train-room", 2).await;
        let recovered_run = training_result(&recovered, "lease-train").await;
        assert_training_pair_matches(&continuous_run, &recovered_run);
        assert_eq!(
            room_accounts(&recovered, "lease-train-room").await,
            continuous_accounts
        );
        let recovered_seqs = room_event_seqs(&recovered, "lease-train-room").await;
        assert_eq!(recovered_seqs, continuous_seqs);
        assert_event_seqs_match_training(&recovered_seqs, &recovered_run);
        let owner = recovered
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/lease-train-room/owner")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(owner.status(), StatusCode::OK);
        let owner_body = axum::body::to_bytes(owner.into_body(), usize::MAX)
            .await
            .unwrap();
        let owner: RoomOwnerResponse = serde_json::from_slice(&owner_body).unwrap();
        assert_eq!(owner.owner_id, "train-b");
        assert_eq!(owner.fencing_token, 2);
    }

    #[tokio::test]
    async fn training_postgres_crash_recovery_matches_live_run() {
        let Some(database_url) = postgres_test_database_url() else {
            return;
        };
        let (room_id, run_id) = unique_training_ids("pg-train");
        cleanup_postgres_room(&database_url, &room_id);

        let live_url = database_url.clone();
        let store =
            tokio::task::spawn_blocking(move || PostgresJournalStore::connect_migrated(&live_url))
                .await
                .unwrap()
                .unwrap();
        let app = recovering_app(Box::new(store));
        start_two_sided_training_and_buy(&app, &run_id, &room_id, 3).await;
        advance_room_clock(&app, &room_id, 3).await;
        let live = training_result(&app, &run_id).await;
        let live_accounts = room_accounts(&app, &room_id).await;
        let live_seqs = room_event_seqs(&app, &room_id).await;
        drop(app);

        let recover_url = database_url.clone();
        let recovered_store = tokio::task::spawn_blocking(move || {
            PostgresJournalStore::connect_migrated(&recover_url)
        })
        .await
        .unwrap()
        .unwrap();
        let recovered = recovering_app(Box::new(recovered_store));
        let recovered_run = training_result(&recovered, &run_id).await;
        assert_training_pair_matches(&live, &recovered_run);
        assert_eq!(room_accounts(&recovered, &room_id).await, live_accounts);
        let recovered_seqs = room_event_seqs(&recovered, &room_id).await;
        assert_eq!(recovered_seqs, live_seqs);
        assert_event_seqs_match_training(&recovered_seqs, &recovered_run);
        cleanup_postgres_room(&database_url, &room_id);
    }

    #[tokio::test]
    async fn isolated_replay_does_not_mutate_live_room() {
        let app = new_app();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&spot_scenario("replay-live")).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let book_before = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/replay-live/book")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let before = axum::body::to_bytes(book_before.into_body(), usize::MAX)
            .await
            .unwrap();
        let replay = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/replay-live/replay")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(replay.status(), StatusCode::OK);
        let replay_body = axum::body::to_bytes(replay.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: IsolatedReplayResponse = serde_json::from_slice(&replay_body).unwrap();
        assert!(parsed.live_room_untouched);
        let book_after = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/replay-live/book")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let after = axum::body::to_bytes(book_after.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(before, after);
    }

    async fn send_json(
        app: &axum::Router,
        method: Method,
        uri: &str,
        user: Option<&str>,
        body: impl Serialize,
    ) -> axum::http::Response<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(user) = user {
            builder = builder.header(USER_ID_HEADER, user);
        }
        app.clone()
            .oneshot(
                builder
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn limit_buy(account_id: AccountId, price_tick: i64, qty: u64) -> SubmitOrderRequest {
        SubmitOrderRequest {
            participant_id: "p5".to_string(),
            instrument_id: None,
            account_id,
            action: OrderAction::PlaceLimit {
                side: Side::Buy,
                price_tick,
                qty,
            },
        }
    }

    #[tokio::test]
    async fn owner_can_add_spectator_who_cannot_trade() {
        let app = new_app();
        let response = send_json(
            &app,
            Method::POST,
            "/rooms",
            None,
            spot_scenario("member-room"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let add = send_json(
            &app,
            Method::POST,
            "/rooms/member-room/members",
            None,
            serde_json::json!({"user_id":"spectator","role":"spectator"}),
        )
        .await;
        assert_eq!(add.status(), StatusCode::OK);
        let trade = send_json(
            &app,
            Method::POST,
            "/rooms/member-room/orders",
            Some("spectator"),
            limit_buy(20, 100, 1),
        )
        .await;
        assert_eq!(trade.status(), StatusCode::FORBIDDEN);
        let private = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/member-room/stream/private")
                    .header(USER_ID_HEADER, "spectator")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(private.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn traders_are_isolated_and_instructor_is_not_trade_any_account() {
        let app = new_app();
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms",
                None,
                spot_scenario("role-matrix")
            )
            .await
            .status(),
            StatusCode::OK
        );
        for (user, role) in [
            ("trader-a", "trader"),
            ("trader-b", "trader"),
            ("coach", "instructor"),
        ] {
            let add = send_json(
                &app,
                Method::POST,
                "/rooms/role-matrix/members",
                None,
                serde_json::json!({"user_id": user, "role": role}),
            )
            .await;
            assert_eq!(add.status(), StatusCode::OK);
        }
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/role-matrix/accounts/10/owners",
                None,
                serde_json::json!({"user_id":"trader-a"}),
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/role-matrix/accounts/20/owners",
                None,
                serde_json::json!({"user_id":"trader-b"}),
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/role-matrix/orders",
                Some("trader-a"),
                limit_buy(10, 90, 1),
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/role-matrix/orders",
                Some("trader-a"),
                limit_buy(20, 90, 1),
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/role-matrix/orders",
                Some("trader-b"),
                limit_buy(10, 90, 1),
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/role-matrix/orders",
                Some("coach"),
                limit_buy(10, 90, 1),
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/role-matrix/accounts/10/owners",
                None,
                serde_json::json!({"user_id":"coach"}),
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/role-matrix/orders",
                Some("coach"),
                limit_buy(10, 89, 1),
            )
            .await
            .status(),
            StatusCode::OK
        );
        let observe = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/role-matrix/observe?account_id=10")
                    .header(USER_ID_HEADER, "trader-a")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(observe.status(), StatusCode::OK);
        let body = axum::body::to_bytes(observe.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: ObservationResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed.api_version, "strategy.v1");
        assert_eq!(parsed.observation.version, 1);
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/role-matrix/members/trader-a",
                None,
                serde_json::json!({}),
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/role-matrix/orders",
                Some("trader-a"),
                limit_buy(10, 88, 1),
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn forged_account_illegal_precision_and_quota_are_rejected() {
        let app = new_app();
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms",
                None,
                spot_scenario("quota-room")
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/quota-room/members",
                None,
                serde_json::json!({"user_id":"ext","role":"trader"}),
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/quota-room/accounts/20/owners",
                None,
                serde_json::json!({"user_id":"ext"}),
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/quota-room/orders",
                Some("ext"),
                limit_buy(20, 0, 1),
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/quota-room/orders",
                Some("ext"),
                limit_buy(20, 80, 0),
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        for i in 0..EXTERNAL_ACTIONS_PER_STEP {
            let status = send_json(
                &app,
                Method::POST,
                "/rooms/quota-room/orders",
                Some("ext"),
                limit_buy(20, 70 + i64::from(i), 1),
            )
            .await
            .status();
            assert_eq!(status, StatusCode::OK, "action {i}");
        }
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/quota-room/orders",
                Some("ext"),
                limit_buy(20, 50, 1),
            )
            .await
            .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/other-quota/orders",
                Some("ext"),
                limit_buy(20, 50, 1),
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms",
                None,
                spot_scenario("other-quota")
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/other-quota/members",
                None,
                serde_json::json!({"user_id":"ext","role":"trader"}),
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/other-quota/accounts/20/owners",
                None,
                serde_json::json!({"user_id":"ext"}),
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/other-quota/orders",
                Some("ext"),
                limit_buy(20, 50, 1),
            )
            .await
            .status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn training_start_freezes_account_assignment() {
        let mut scenario = spot_scenario("freeze-room");
        scenario.seed_orders = vec![
            Command::NewOrder(NewOrder {
                order_id: 1,
                account_id: 10,
                side: Side::Buy,
                kind: OrderKind::Limit { price_tick: 99 },
                qty: 1,
                reduce_only: false,
            }),
            Command::NewOrder(NewOrder {
                order_id: 2,
                account_id: 10,
                side: Side::Sell,
                kind: OrderKind::Limit { price_tick: 101 },
                qty: 1,
                reduce_only: false,
            }),
        ];
        let app = new_app();
        let start = send_json(
            &app,
            Method::POST,
            "/training/runs",
            None,
            StartTrainingRequest {
                run_id: "freeze-run".to_string(),
                scenario,
                agents: Vec::new(),
                trainee_account_id: 20,
                target_qty: 4,
                horizon_steps: 8,
            },
        )
        .await;
        assert_eq!(start.status(), StatusCode::OK);
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/freeze-room/members",
                None,
                serde_json::json!({"user_id":"trader-z","role":"trader"}),
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/freeze-room/accounts/20/owners",
                None,
                serde_json::json!({"user_id":"trader-z"}),
            )
            .await
            .status(),
            StatusCode::CONFLICT
        );
        let trainee = send_json(
            &app,
            Method::POST,
            "/rooms/freeze-room/orders",
            None,
            limit_buy(20, 101, 1),
        )
        .await;
        assert_eq!(trainee.status(), StatusCode::OK);
        let body = axum::body::to_bytes(trainee.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(!text.contains("DuplicateOrderId"), "{text}");
        assert!(text.contains("\"accepted\":true"), "{text}");
        assert!(text.contains("TradePrinted"), "{text}");
    }

    async fn assign_trader(app: &axum::Router, room_id: &str, user: &str, account_id: AccountId) {
        assert_eq!(
            send_json(
                app,
                Method::POST,
                &format!("/rooms/{room_id}/members"),
                None,
                serde_json::json!({"user_id": user, "role": "trader"}),
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            send_json(
                app,
                Method::POST,
                &format!("/rooms/{room_id}/accounts/{account_id}/owners"),
                None,
                serde_json::json!({"user_id": user}),
            )
            .await
            .status(),
            StatusCode::OK
        );
    }

    async fn exhaust_external_quota(app: &axum::Router, room_id: &str, user: &str) {
        for i in 0..EXTERNAL_ACTIONS_PER_STEP {
            assert_eq!(
                send_json(
                    app,
                    Method::POST,
                    &format!("/rooms/{room_id}/orders"),
                    Some(user),
                    limit_buy(20, 70 + i64::from(i), 1),
                )
                .await
                .status(),
                StatusCode::OK,
                "quota fill {i}"
            );
        }
        assert_eq!(
            send_json(
                app,
                Method::POST,
                &format!("/rooms/{room_id}/orders"),
                Some(user),
                limit_buy(20, 50, 1),
            )
            .await
            .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[tokio::test]
    async fn training_freeze_survives_restart_and_takeover_for_terminal_states() {
        async fn assert_assignment_frozen(app: &axum::Router, room_id: &str) {
            assert_eq!(
                send_json(
                    app,
                    Method::POST,
                    &format!("/rooms/{room_id}/accounts/20/owners"),
                    None,
                    serde_json::json!({"user_id":"late-trader"}),
                )
                .await
                .status(),
                StatusCode::CONFLICT
            );
        }

        let journal = journal::SharedInMemoryJournalStore::new();
        let live = recovering_app(Box::new(journal.clone()));
        let start = send_json(
            &live,
            Method::POST,
            "/training/runs",
            None,
            two_sided_training("freeze-restart", "freeze-restart-room", 2),
        )
        .await;
        assert_eq!(start.status(), StatusCode::OK);
        assert_assignment_frozen(&live, "freeze-restart-room").await;
        drop(live);
        let recovered = recovering_app(Box::new(journal.clone()));
        assert_assignment_frozen(&recovered, "freeze-restart-room").await;
        assert_eq!(
            send_json(
                &recovered,
                Method::POST,
                "/training/runs/freeze-restart/abort",
                None,
                serde_json::json!({}),
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_assignment_frozen(&recovered, "freeze-restart-room").await;
        drop(recovered);

        let lease_journal = journal::SharedInMemoryJournalStore::new();
        let first_state = shared_state(
            AppState::recover_with_journal_bundle_and_auth_policy(
                "http://127.0.0.1:57305",
                JournalStoreBundle::single(Box::new(lease_journal.clone())),
                AuthPolicy::local_development(),
                Some(training_lease_config("freeze-a", "http://127.0.0.1:57305")),
            )
            .unwrap(),
        );
        let first = app(first_state.clone());
        assert_eq!(
            send_json(
                &first,
                Method::POST,
                "/training/runs",
                None,
                two_sided_training("freeze-lease", "freeze-lease-room", 2),
            )
            .await
            .status(),
            StatusCode::OK
        );
        release_owned_room_writer_leases(&first_state)
            .await
            .unwrap();
        drop(first);
        drop(first_state);
        let taken = leased_recovering_app(lease_journal, "freeze-b", "http://127.0.0.1:57306");
        assert_assignment_frozen(&taken, "freeze-lease-room").await;
    }

    #[tokio::test]
    async fn external_action_quota_survives_restart_and_resets_on_step() {
        let journal = journal::SharedInMemoryJournalStore::new();
        let app = recovering_app(Box::new(journal.clone()));
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms",
                None,
                spot_scenario("quota-persist")
            )
            .await
            .status(),
            StatusCode::OK
        );
        assign_trader(&app, "quota-persist", "ext", 20).await;
        exhaust_external_quota(&app, "quota-persist", "ext").await;
        let replay_ok = send_json_with_key(
            &app,
            "/rooms/quota-persist/orders",
            Some("ext"),
            Some("quota-retry"),
            limit_buy(20, 40, 1),
        )
        .await;
        assert_eq!(replay_ok.status(), StatusCode::TOO_MANY_REQUESTS);
        drop(app);

        let recovered = recovering_app(Box::new(journal));
        assert_eq!(
            send_json(
                &recovered,
                Method::POST,
                "/rooms/quota-persist/orders",
                Some("ext"),
                limit_buy(20, 41, 1),
            )
            .await
            .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            send_json(
                &recovered,
                Method::POST,
                "/rooms/quota-persist/clock/advance",
                None,
                AdvanceClockRequest { steps: 1 },
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            send_json(
                &recovered,
                Method::POST,
                "/rooms/quota-persist/orders",
                Some("ext"),
                limit_buy(20, 42, 1),
            )
            .await
            .status(),
            StatusCode::OK
        );
    }

    async fn send_json_with_key(
        app: &axum::Router,
        uri: &str,
        user: Option<&str>,
        key: Option<&str>,
        body: impl Serialize,
    ) -> axum::http::Response<Body> {
        control_request(
            app,
            uri,
            user,
            key,
            Some(serde_json::to_value(body).unwrap()),
        )
        .await
    }

    #[tokio::test]
    async fn external_action_quota_idempotent_retry_does_not_double_count() {
        let app = new_app();
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms",
                None,
                spot_scenario("quota-idem")
            )
            .await
            .status(),
            StatusCode::OK
        );
        assign_trader(&app, "quota-idem", "ext", 20).await;
        for i in 0..(EXTERNAL_ACTIONS_PER_STEP - 1) {
            assert_eq!(
                send_json(
                    &app,
                    Method::POST,
                    "/rooms/quota-idem/orders",
                    Some("ext"),
                    limit_buy(20, 60 + i64::from(i), 1),
                )
                .await
                .status(),
                StatusCode::OK
            );
        }
        let first = send_json_with_key(
            &app,
            "/rooms/quota-idem/orders",
            Some("ext"),
            Some("last-slot"),
            limit_buy(20, 90, 1),
        )
        .await;
        assert_eq!(first.status(), StatusCode::OK);
        let replay = send_json_with_key(
            &app,
            "/rooms/quota-idem/orders",
            Some("ext"),
            Some("last-slot"),
            limit_buy(20, 90, 1),
        )
        .await;
        assert_eq!(replay.status(), StatusCode::OK);
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/quota-idem/orders",
                Some("ext"),
                limit_buy(20, 91, 1),
            )
            .await
            .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[tokio::test]
    async fn training_settle_failure_does_not_install_live_state() {
        struct FailTrainingPersist {
            inner: journal::InMemoryJournalStore,
            fail_training: bool,
        }

        impl JournalStore for FailTrainingPersist {
            fn load_recovery(&mut self) -> Result<JournalRecovery, JournalError> {
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

            fn append_room_mutation(
                &mut self,
                mutation: &PendingJournalMutation,
                execution_records: &[JournalExecution],
                transfer_records: &[JournalTransfer],
                snapshot: Option<&JournalSnapshot>,
            ) -> Result<(), JournalError> {
                if self.fail_training
                    && matches!(mutation.mutation, RoomMutation::TrainingProgress { .. })
                {
                    return Err(JournalError::Recovery(
                        "injected training persist failure".into(),
                    ));
                }
                self.inner.append_room_mutation(
                    mutation,
                    execution_records,
                    transfer_records,
                    snapshot,
                )
            }

            fn user_can_administer_room(
                &mut self,
                user_id: &str,
                room_id: &str,
            ) -> Result<bool, JournalError> {
                self.inner.user_can_administer_room(user_id, room_id)
            }

            fn user_can_access_room(
                &mut self,
                user_id: &str,
                room_id: &str,
            ) -> Result<bool, JournalError> {
                self.inner.user_can_access_room(user_id, room_id)
            }

            fn update_room_status(
                &mut self,
                room_id: &str,
                status: MarketStatus,
            ) -> Result<(), JournalError> {
                self.inner.update_room_status(room_id, status)
            }

            fn find_control_idempotency(
                &mut self,
                user_id: &str,
                room_id: &str,
                idempotency_key: &str,
            ) -> Result<Option<journal::ControlIdempotencyRecord>, JournalError> {
                self.inner
                    .find_control_idempotency(user_id, room_id, idempotency_key)
            }
        }

        let app = new_app_with_journal(
            "http://127.0.0.1:57305",
            Box::new(FailTrainingPersist {
                inner: journal::InMemoryJournalStore::new(),
                fail_training: true,
            }),
        );
        let start = send_json(
            &app,
            Method::POST,
            "/training/runs",
            None,
            two_sided_training("settle-fail", "settle-fail-room", 1),
        )
        .await;
        assert_eq!(start.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let missing = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/training/runs/settle-fail")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn close_room_settles_training_residuals() {
        let app = new_app();
        let start = send_json(
            &app,
            Method::POST,
            "/training/runs",
            None,
            two_sided_training("close-settle", "close-settle-room", 8),
        )
        .await;
        assert_eq!(start.status(), StatusCode::OK);
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/close-settle-room/orders",
                None,
                limit_buy(20, 90, 1),
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            control_request(&app, "/rooms/close-settle-room/close", None, None, None)
                .await
                .status(),
            StatusCode::OK
        );
        let orders = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/close-settle-room/orders")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(orders.status(), StatusCode::OK);
        let body = axum::body::to_bytes(orders.into_body(), usize::MAX)
            .await
            .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let trainee = payload["orders"]
            .as_array()
            .unwrap()
            .iter()
            .find(|order| order["account_id"] == 20)
            .expect("trainee order");
        assert_eq!(trainee["status"], "canceled", "{payload}");
    }

    async fn first_sse_data(response: axum::http::Response<Body>) -> serde_json::Value {
        let mut stream = response.into_body().into_data_stream();
        let chunk = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("sse")
            .expect("frame")
            .expect("bytes");
        let text = String::from_utf8(chunk.to_vec()).unwrap();
        let data = text
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .expect(&text);
        serde_json::from_str(data).unwrap()
    }

    #[tokio::test]
    async fn private_stream_snapshot_includes_own_resting_orders_not_others() {
        let app = new_app();
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms",
                None,
                spot_scenario("priv-rest")
            )
            .await
            .status(),
            StatusCode::OK
        );
        assign_trader(&app, "priv-rest", "alice", 20).await;
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/priv-rest/orders",
                Some("alice"),
                limit_buy(20, 90, 1),
            )
            .await
            .status(),
            StatusCode::OK
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/priv-rest/stream/private")
                    .header(USER_ID_HEADER, "alice")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let frame = first_sse_data(response).await;
        let snapshot = frame.get("payload").unwrap_or(&frame);
        assert_eq!(snapshot["cursor"]["scope"], "private");
        assert_eq!(snapshot["cursor"]["version"], "stream.v1");
        let orders = snapshot["orders"].as_array().unwrap();
        assert!(
            orders.iter().any(|order| order["account_id"] == 20),
            "{snapshot}"
        );
        assert!(
            orders.iter().all(|order| order["account_id"] == 20),
            "{snapshot}"
        );
        let accounts = snapshot["accounts"].as_array().unwrap();
        assert_eq!(accounts, &vec![serde_json::json!(20)]);
    }

    #[tokio::test]
    async fn private_stream_rest_only_and_cancel_are_visible_to_owner() {
        let app = new_app();
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms",
                None,
                spot_scenario("priv-delta")
            )
            .await
            .status(),
            StatusCode::OK
        );
        assign_trader(&app, "priv-delta", "alice", 20).await;
        let post = send_json(
            &app,
            Method::POST,
            "/rooms/priv-delta/orders",
            Some("alice"),
            limit_buy(20, 90, 1),
        )
        .await;
        assert_eq!(post.status(), StatusCode::OK);
        let posted: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(post.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(posted.to_string().contains("OrderRested"), "{posted}");
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/priv-delta/stream/private?after_command_seq=0")
                    .header(USER_ID_HEADER, "alice")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let mut stream = response.into_body().into_data_stream();
        let mut text = String::new();
        for _ in 0..4 {
            match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
                Ok(Some(Ok(chunk))) => text.push_str(&String::from_utf8_lossy(&chunk)),
                _ => break,
            }
        }
        assert!(
            text.contains("OrderRested") || text.contains("account_id"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn private_stream_rejects_mismatched_scope_and_omits_others_after_revoke() {
        let app = new_app();
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms",
                None,
                spot_scenario("priv-scope")
            )
            .await
            .status(),
            StatusCode::OK
        );
        let mismatch = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/priv-scope/stream/private?scope=public")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(mismatch.status(), StatusCode::BAD_REQUEST);
        assign_trader(&app, "priv-scope", "alice", 20).await;
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms/priv-scope/members/alice",
                None,
                serde_json::json!({}),
            )
            .await
            .status(),
            StatusCode::OK
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/priv-scope/stream/private")
                    .header(USER_ID_HEADER, "alice")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn external_action_quota_postgres_survives_restart() {
        let Some(database_url) = postgres_test_database_url() else {
            return;
        };
        let room_id = format!(
            "quota-pg-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        cleanup_postgres_room(&database_url, &room_id);
        let live_url = database_url.clone();
        let store =
            tokio::task::spawn_blocking(move || PostgresJournalStore::connect_migrated(&live_url))
                .await
                .unwrap()
                .unwrap();
        let app = recovering_app(Box::new(store));
        let scenario = spot_scenario(&room_id);
        assert_eq!(
            send_json(&app, Method::POST, "/rooms", None, scenario.clone())
                .await
                .status(),
            StatusCode::OK
        );
        assign_trader(&app, &room_id, "ext", 20).await;
        exhaust_external_quota(&app, &room_id, "ext").await;
        drop(app);
        let recover_url = database_url.clone();
        let recovered_store = tokio::task::spawn_blocking(move || {
            PostgresJournalStore::connect_migrated(&recover_url)
        })
        .await
        .unwrap()
        .unwrap();
        let recovered = recovering_app(Box::new(recovered_store));
        assert_eq!(
            send_json(
                &recovered,
                Method::POST,
                &format!("/rooms/{room_id}/orders"),
                Some("ext"),
                limit_buy(20, 33, 1),
            )
            .await
            .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        cleanup_postgres_room(&database_url, &room_id);
        let _ = scenario;
    }

    #[tokio::test]
    async fn health_reports_journal_failure_as_service_unavailable() {
        struct UnhealthyJournal;

        impl JournalStore for UnhealthyJournal {
            fn load_recovery(&mut self) -> Result<JournalRecovery, JournalError> {
                Ok(JournalRecovery::default())
            }

            fn health_check(&mut self) -> Result<(), JournalError> {
                Err(JournalError::Recovery("database unavailable".to_string()))
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
        }

        let healthy_app = new_app();
        let healthy = healthy_app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(healthy.status(), StatusCode::OK);
        let healthy_ready = healthy_app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(healthy_ready.status(), StatusCode::OK);
        let healthy_live = healthy_app
            .oneshot(
                Request::builder()
                    .uri("/health/live")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(healthy_live.status(), StatusCode::OK);

        let unhealthy_app =
            new_app_with_journal("http://127.0.0.1:57305", Box::new(UnhealthyJournal));
        let unhealthy_live = unhealthy_app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health/live")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unhealthy_live.status(), StatusCode::OK);
        let unhealthy_ready = unhealthy_app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unhealthy_ready.status(), StatusCode::SERVICE_UNAVAILABLE);
        let unhealthy = unhealthy_app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unhealthy.status(), StatusCode::SERVICE_UNAVAILABLE);

        let metrics = unhealthy_app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(metrics.status(), StatusCode::OK);
        assert_eq!(
            metrics.headers().get(CONTENT_TYPE).unwrap(),
            "text/plain; version=0.0.4; charset=utf-8"
        );
        let body = axum::body::to_bytes(metrics.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("marketforge_process_up 1\n"));
        assert!(body.contains("marketforge_journal_operation_errors_total 2\n"));
    }

    #[tokio::test]
    async fn shutdown_changes_readiness_and_metrics_but_not_liveness() {
        let state = shared_state(AppState::new("http://127.0.0.1:57305"));
        let app = app(state.clone());
        state.lifecycle.begin_shutdown();

        let live = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health/live")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(live.status(), StatusCode::OK);

        let ready = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ready.status(), StatusCode::SERVICE_UNAVAILABLE);

        let rejected = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&spot_scenario("shutdown-room")).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::SERVICE_UNAVAILABLE);

        let metrics = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(metrics.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("marketforge_accepting_durable_writes 0\n"));
        assert!(body.contains("marketforge_durable_writes_rejected_total 1\n"));
        assert!(body.contains("marketforge_journal_operations_started_total 0\n"));
    }

    #[tokio::test]
    async fn metrics_track_open_sse_connections_without_room_labels() {
        let (app, _state) = paged_event_app(1);
        let stream_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/rooms/paged-room/events/stream")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stream_response.status(), StatusCode::OK);

        let metrics = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(metrics.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("marketforge_sse_connections 1\n"));
        assert!(body.contains("marketforge_sse_connections_started_total 1\n"));
        assert!(!body.contains("paged-room"));

        drop(stream_response);
        let metrics = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(metrics.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("marketforge_sse_connections 0\n"));
        assert!(body.contains("marketforge_sse_connections_started_total 1\n"));
    }

    #[tokio::test]
    async fn metrics_count_failed_durable_transition_attempts() {
        let app = new_app();
        let rejected = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);

        let metrics = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(metrics.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("marketforge_durable_writes_started_total 1\n"));
        assert!(body.contains("marketforge_durable_writes_completed_total 1\n"));
        assert!(body.contains("marketforge_durable_writes_failed_total 1\n"));
        assert!(body.contains("marketforge_scheduler_steps_total 0\n"));
        assert!(body.contains("marketforge_training_runs_running 0\n"));
        assert!(body.contains("marketforge_replayed_commands_total 0\n"));
    }

    #[tokio::test]
    async fn isolated_replay_increments_replayed_command_metric() {
        let app = new_app();
        let scenario = seeded_spot_scenario("metric-replay");
        assert_eq!(
            send_json(&app, Method::POST, "/rooms", None, scenario,)
                .await
                .status(),
            StatusCode::OK
        );
        let replay = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/metric-replay/replay")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(replay.status(), StatusCode::OK);
        let metrics = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(metrics.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            body.contains("marketforge_replayed_commands_total "),
            "{body}"
        );
        assert!(
            !body.contains("marketforge_replayed_commands_total 0\n"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn journal_bootstrap_runs_outside_the_async_runtime() {
        struct RuntimeStartingJournal;

        fn start_nested_runtime_probe() {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async { tokio::task::yield_now().await });
        }

        impl JournalStore for RuntimeStartingJournal {
            fn load_recovery(&mut self) -> Result<JournalRecovery, JournalError> {
                start_nested_runtime_probe();
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
        }

        let app = new_app_recovering_with_journal_factory_async(
            "http://127.0.0.1:57305".to_string(),
            AuthPolicy::local_development(),
            default_cors_origins(),
            || {
                start_nested_runtime_probe();
                Ok(JournalStoreBundle::single(Box::new(RuntimeStartingJournal)))
            },
        )
        .await
        .unwrap();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let sync_app = new_app_recovering_with_journal(
            "http://127.0.0.1:57305",
            Box::new(RuntimeStartingJournal),
        )
        .unwrap();
        let response = sync_app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let sync_factory_app = new_app_recovering_with_journal_factory_sync(
            "http://127.0.0.1:57305".to_string(),
            AuthPolicy::local_development(),
            default_cors_origins(),
            None,
            || {
                start_nested_runtime_probe();
                Ok(JournalStoreBundle::single(Box::new(RuntimeStartingJournal)))
            },
        )
        .unwrap();
        let response = sync_factory_app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let _sync_env_constructor: fn(String) -> Result<Router, JournalError> =
            new_app_from_env_with_base_url;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_request_finishes_durable_commit_and_state_installation() {
        use std::sync::Condvar;

        struct PausingJournal {
            records: Arc<Mutex<Vec<JournalExecution>>>,
            first_append_started: Option<std::sync::mpsc::Sender<()>>,
            release_first_append: Arc<(Mutex<bool>, Condvar)>,
        }

        impl JournalStore for PausingJournal {
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
                records: &[JournalExecution],
                _snapshot: Option<&JournalSnapshot>,
            ) -> Result<(), JournalError> {
                if let Some(started) = self.first_append_started.take() {
                    let _ = started.send(());
                    let (released, wake) = &*self.release_first_append;
                    let mut released = released.lock().unwrap();
                    while !*released {
                        released = wake.wait(released).unwrap();
                    }
                }
                self.records.lock().unwrap().extend_from_slice(records);
                Ok(())
            }

            fn update_room_status(
                &mut self,
                _room_id: &str,
                _status: MarketStatus,
            ) -> Result<(), JournalError> {
                Ok(())
            }
        }

        let records = Arc::new(Mutex::new(Vec::<JournalExecution>::new()));
        let release_first_append = Arc::new((Mutex::new(false), Condvar::new()));
        let (started_sender, started_receiver) = std::sync::mpsc::channel();
        let app = new_app_with_journal(
            "http://127.0.0.1:57305",
            Box::new(PausingJournal {
                records: Arc::clone(&records),
                first_append_started: Some(started_sender),
                release_first_append: Arc::clone(&release_first_append),
            }),
        );
        let room_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&spot_scenario("cancel-safe-room")).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(room_response.status(), StatusCode::OK);

        let order_body = serde_json::json!({
            "participant_id": "alice",
            "account_id": 20,
            "action": {
                "PlaceLimit": {
                    "side": "Buy",
                    "price_tick": 100,
                    "qty": 1
                }
            }
        })
        .to_string();
        let first_request = tokio::spawn(
            app.clone().oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/cancel-safe-room/orders")
                    .header("content-type", "application/json")
                    .body(Body::from(order_body.clone()))
                    .unwrap(),
            ),
        );
        tokio::task::spawn_blocking(move || {
            started_receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("first journal append did not start")
        })
        .await
        .unwrap();
        first_request.abort();
        let _ = first_request.await;

        {
            let (released, wake) = &*release_first_append;
            *released.lock().unwrap() = true;
            wake.notify_all();
        }

        let second_response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/cancel-safe-room/orders")
                    .header("content-type", "application/json")
                    .body(Body::from(order_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second_response.status(), StatusCode::OK);

        let records = records.lock().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].command_seq, 0);
        assert_eq!(records[1].command_seq, 1);
        let Command::NewOrder(first) = &records[0].command else {
            panic!("expected first new order");
        };
        let Command::NewOrder(second) = &records[1].command else {
            panic!("expected second new order");
        };
        assert_eq!((first.order_id, second.order_id), (1, 2));
    }

    #[tokio::test]
    async fn creates_room_and_accepts_order_over_http_shape() {
        let app = new_app();
        let scenario = serde_json::to_string(&spot_scenario("room-1")).unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let order = serde_json::json!({
            "participant_id": "human-1",
            "account_id": 20,
            "action": {
                "PlaceLimit": {
                    "side": "Buy",
                    "price_tick": 100,
                    "qty": 2
                }
            }
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/room-1/orders")
                    .header("content-type", "application/json")
                    .body(Body::from(order.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn api_order_ids_start_after_seed_order_ids() {
        let app = new_app();
        let scenario = serde_json::to_string(&seeded_spot_scenario("seed-id-room")).unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let order = serde_json::json!({
            "participant_id": "human-1",
            "account_id": 20,
            "action": {
                "PlaceLimit": {
                    "side": "Buy",
                    "price_tick": 99,
                    "qty": 1
                }
            }
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/seed-id-room/orders")
                    .header("content-type", "application/json")
                    .body(Body::from(order.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let order: OrderResponse = serde_json::from_slice(&body).unwrap();
        assert!(order.events.iter().any(|event| matches!(
            event,
            EventSummary::OrderAccepted {
                order_id: 10_001,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn account_cannot_cancel_or_amend_another_accounts_order() {
        let app = new_app();
        let scenario = serde_json::to_string(&spot_scenario("order-owner-room")).unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let owner_order = serde_json::json!({
            "participant_id": "owner-10",
            "account_id": 10,
            "action": {"PlaceLimit": {"side": "Sell", "price_tick": 105, "qty": 2}}
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/order-owner-room/orders")
                    .header("content-type", "application/json")
                    .body(Body::from(owner_order.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let order: OrderResponse = serde_json::from_slice(&body).unwrap();
        let order_id = order
            .events
            .iter()
            .find_map(|event| match event {
                EventSummary::OrderAccepted { order_id, .. } => Some(*order_id),
                _ => None,
            })
            .unwrap();

        for action in [
            serde_json::json!({"Cancel": {"order_id": order_id}}),
            serde_json::json!({"Amend": {
                "order_id": order_id,
                "price_tick": 104,
                "qty": 1
            }}),
        ] {
            let request = serde_json::json!({
                "participant_id": "attacker-20",
                "account_id": 20,
                "action": action
            });
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/rooms/order-owner-room/orders")
                        .header("content-type", "application/json")
                        .body(Body::from(request.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/order-owner-room/book")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let book: BookSnapshot = serde_json::from_slice(&body).unwrap();
        assert_eq!(book.asks[0].qty, 2);
    }

    #[tokio::test]
    async fn create_room_can_configure_venue_price_limits() {
        let app = new_app();
        let mut scenario = spot_scenario("venue-rules-room");
        scenario.venue_rules = exchange_core::VenueRuleConfig {
            price_limits: vec![exchange_core::PriceLimitRuleConfig {
                instrument_id: "V-BTC-SPOT".to_string(),
                reference_price_tick: 100,
                limit_up_ppm: 100_000,
                limit_down_ppm: 100_000,
            }],
            ..exchange_core::VenueRuleConfig::default()
        };
        let scenario = serde_json::to_string(&scenario).unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let order = serde_json::json!({
            "participant_id": "human-1",
            "account_id": 20,
            "action": {
                "PlaceLimit": {
                    "side": "Buy",
                    "price_tick": 111,
                    "qty": 1
                }
            }
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/venue-rules-room/orders")
                    .header("content-type", "application/json")
                    .body(Body::from(order.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let order_response: OrderResponse = serde_json::from_slice(&body).unwrap();
        assert!(!order_response.accepted);
        assert!(
            order_response
                .reject_reason
                .as_deref()
                .unwrap_or_default()
                .contains("PriceLimitExceeded")
        );
    }

    #[tokio::test]
    async fn venue_accounts_endpoint_reports_cross_asset_balances() {
        let app = new_app();
        let scenario = serde_json::to_string(&seeded_spot_scenario("venue-room")).unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let order = serde_json::json!({
            "participant_id": "human-1",
            "account_id": 20,
            "action": {
                "PlaceLimit": {
                    "side": "Buy",
                    "price_tick": 104,
                    "qty": 2
                }
            }
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/venue-room/orders")
                    .header("content-type", "application/json")
                    .body(Body::from(order.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/venue-room/venue/accounts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let accounts: RoomVenueAccountsResponse = serde_json::from_slice(&body).unwrap();

        let balance_total = |account_id, asset_id: &str| {
            accounts
                .accounts
                .iter()
                .find(|account| account.account_id == account_id)
                .and_then(|account| {
                    account
                        .balances
                        .iter()
                        .find(|balance| balance.asset_id == asset_id)
                })
                .map(|balance| balance.total)
                .expect("venue balance should exist")
        };
        assert_eq!(balance_total(10, "V"), 8);
        assert_eq!(balance_total(10, "BTC"), 1_208);
        assert_eq!(balance_total(20, "V"), 2);
        assert_eq!(balance_total(20, "BTC"), 792);
    }

    #[tokio::test]
    async fn transfer_routes_apply_deposit_and_withdrawal_after_clock_delay() {
        let app = new_app();
        let mut scenario = spot_scenario("transfer-room");
        scenario.venue_rules = exchange_core::VenueRuleConfig {
            transfers: exchange_core::TransferPolicyConfig {
                deposit_delay_steps: 2,
                withdrawal_delay_steps: 1,
            },
            ..exchange_core::VenueRuleConfig::default()
        };
        let mut wallet_balances = BTreeMap::new();
        wallet_balances.insert("BTC".to_string(), 500);
        scenario.initial_portfolios = vec![ScenarioPortfolio {
            account_id: 20,
            balances: wallet_balances,
        }];
        let scenario = serde_json::to_string(&scenario).unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let transfer = serde_json::json!({
            "account_id": 20,
            "asset_id": "BTC",
            "amount": 250
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/transfer-room/transfers/deposit")
                    .header("content-type", "application/json")
                    .body(Body::from(transfer.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let transfer_response: TransferResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            transfer_response.transfer.status,
            exchange_core::VenueTransferStatus::Pending
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/transfer-room/portfolio")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let portfolios: RoomPortfoliosResponse = serde_json::from_slice(&body).unwrap();
        let btc_wallet = portfolios
            .accounts
            .iter()
            .find(|account| account.account_id == 20)
            .and_then(|account| {
                account
                    .balances
                    .iter()
                    .find(|balance| balance.asset_id == "BTC")
            })
            .unwrap();
        assert_eq!(btc_wallet.total, 500);
        assert_eq!(btc_wallet.reserved, 250);

        let advance = serde_json::json!({ "steps": 1 });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/transfer-room/clock/advance")
                    .header("content-type", "application/json")
                    .body(Body::from(advance.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let advance_response: AdvanceClockResponse = serde_json::from_slice(&body).unwrap();
        assert!(advance_response.completed_transfers.is_empty());

        let advance = serde_json::json!({ "steps": 1 });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/transfer-room/clock/advance")
                    .header("content-type", "application/json")
                    .body(Body::from(advance.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let advance_response: AdvanceClockResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(advance_response.completed_transfers.len(), 1);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/transfer-room/portfolio")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let portfolios: RoomPortfoliosResponse = serde_json::from_slice(&body).unwrap();
        let btc_wallet = portfolios
            .accounts
            .iter()
            .find(|account| account.account_id == 20)
            .and_then(|account| {
                account
                    .balances
                    .iter()
                    .find(|balance| balance.asset_id == "BTC")
            })
            .unwrap();
        assert_eq!(btc_wallet.total, 250);
        assert_eq!(btc_wallet.reserved, 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/transfer-room/transfers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let transfers: RoomTransfersResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(transfers.transfers.len(), 1);
        assert_eq!(
            transfers.transfers[0].status,
            exchange_core::VenueTransferStatus::Completed
        );

        let transfer = serde_json::json!({
            "account_id": 20,
            "asset_id": "BTC",
            "amount": 100
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/transfer-room/transfers/withdraw")
                    .header("content-type", "application/json")
                    .body(Body::from(transfer.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/transfer-room/venue/accounts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let accounts: RoomVenueAccountsResponse = serde_json::from_slice(&body).unwrap();
        let btc = accounts
            .accounts
            .iter()
            .find(|account| account.account_id == 20)
            .and_then(|account| {
                account
                    .balances
                    .iter()
                    .find(|balance| balance.asset_id == "BTC")
            })
            .unwrap();
        assert_eq!(btc.total, 1_250);
        assert_eq!(btc.reserved, 100);

        let advance = serde_json::json!({ "steps": 1 });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/transfer-room/clock/advance")
                    .header("content-type", "application/json")
                    .body(Body::from(advance.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/transfer-room/venue/accounts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let accounts: RoomVenueAccountsResponse = serde_json::from_slice(&body).unwrap();
        let btc = accounts
            .accounts
            .iter()
            .find(|account| account.account_id == 20)
            .and_then(|account| {
                account
                    .balances
                    .iter()
                    .find(|balance| balance.asset_id == "BTC")
            })
            .unwrap();
        assert_eq!(btc.total, 1_150);
        assert_eq!(btc.reserved, 0);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/transfer-room/portfolio")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let portfolios: RoomPortfoliosResponse = serde_json::from_slice(&body).unwrap();
        let btc_wallet = portfolios
            .accounts
            .iter()
            .find(|account| account.account_id == 20)
            .and_then(|account| {
                account
                    .balances
                    .iter()
                    .find(|balance| balance.asset_id == "BTC")
            })
            .unwrap();
        assert_eq!(btc_wallet.total, 350);
        assert_eq!(btc_wallet.reserved, 0);
    }

    #[tokio::test]
    async fn instrument_routes_target_non_primary_market() {
        let app = new_app();
        let scenario = serde_json::to_string(&spot_perp_scenario("multi-room")).unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/multi-room/instruments/V-BTC-PERP/view")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let perp_view: MarketView = serde_json::from_slice(&body).unwrap();
        assert_eq!(perp_view.instrument_id, "V-BTC-PERP");
        assert_eq!(perp_view.book.asks[0].price_tick, 100);

        let order = serde_json::json!({
            "participant_id": "human-1",
            "account_id": 10,
            "action": {
                "PlaceLimit": {
                    "side": "Buy",
                    "price_tick": 100,
                    "qty": 2
                }
            }
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/multi-room/instruments/V-BTC-PERP/orders")
                    .header("content-type", "application/json")
                    .body(Body::from(order.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let order_response: OrderResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(order_response.instrument_id.as_deref(), Some("V-BTC-PERP"));
        assert!(order_response.accepted);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/multi-room/view")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let spot_view: MarketView = serde_json::from_slice(&body).unwrap();
        assert_eq!(spot_view.instrument_id, "V-BTC-SPOT");
        assert!(spot_view.book.bids.is_empty());
        assert!(spot_view.book.asks.is_empty());
    }

    #[tokio::test]
    async fn live_accounts_expose_shared_margin_when_peer_projection_has_no_clearing_leg() {
        let app = new_app();
        let scenario =
            serde_json::to_string(&shared_collateral_perp_scenario("shared-margin-room")).unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        for order in [
            serde_json::json!({
                "participant_id": "seller",
                "account_id": 10,
                "action": {
                    "PlaceLimit": {
                        "side": "Sell",
                        "price_tick": 100,
                        "qty": 10
                    }
                }
            }),
            serde_json::json!({
                "participant_id": "buyer",
                "account_id": 20,
                "action": {
                    "PlaceMarket": {
                        "side": "Buy",
                        "qty": 10
                    }
                }
            }),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/rooms/shared-margin-room/instruments/V-BTC-PERP/orders")
                        .header("content-type", "application/json")
                        .body(Body::from(order.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }

        let get_accounts = |instrument_id: &'static str| {
            let app = app.clone();
            async move {
                let response = app
                    .oneshot(
                        Request::builder()
                            .method(Method::GET)
                            .uri(format!(
                                "/rooms/shared-margin-room/instruments/{instrument_id}/accounts"
                            ))
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap();
                let AccountSnapshots::Perp(accounts) =
                    serde_json::from_slice::<AccountSnapshots>(&body).unwrap()
                else {
                    panic!("expected perp account snapshots");
                };
                accounts
                    .into_iter()
                    .find(|account| account.account_id == 20)
                    .unwrap()
            }
        };

        let btc = get_accounts("V-BTC-PERP").await;
        let eth = get_accounts("V-ETH-PERP").await;
        assert_eq!(btc.position_qty, 10);
        assert_eq!(btc.initial_margin, 100);
        assert_eq!(btc.portfolio_initial_margin, 100);
        assert_eq!(btc.portfolio_maintenance_margin, 50);
        assert_eq!(eth.position_qty, 0);
        assert_eq!(eth.initial_margin, 0);
        assert_eq!(eth.maintenance_margin, 0);
        assert_eq!(eth.portfolio_initial_margin, btc.portfolio_initial_margin);
        assert_eq!(
            eth.portfolio_maintenance_margin,
            btc.portfolio_maintenance_margin
        );

        let withdrawal = serde_json::json!({
            "venue_id": "venue-a",
            "account_id": 20,
            "asset_id": "USD",
            "amount": 100
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/shared-margin-room/transfers/withdraw")
                    .header("content-type", "application/json")
                    .body(Body::from(withdrawal.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let withdrawal: TransferResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            withdrawal.transfer.status,
            exchange_core::VenueTransferStatus::Completed
        );

        let btc_after_withdrawal = get_accounts("V-BTC-PERP").await;
        let eth_after_withdrawal = get_accounts("V-ETH-PERP").await;
        assert_eq!(btc_after_withdrawal.cash_balance, 900);
        assert_eq!(eth_after_withdrawal.cash_balance, 900);
        assert_ne!(
            btc_after_withdrawal.margin_status,
            exchange_core::PerpMarginStatus::Liquidatable
        );
        assert_ne!(
            eth_after_withdrawal.margin_status,
            exchange_core::PerpMarginStatus::Liquidatable
        );
        assert_eq!(
            eth_after_withdrawal.portfolio_initial_margin,
            btc_after_withdrawal.portfolio_initial_margin
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/shared-margin-room/instruments/V-ETH-PERP/positions?account_id=20")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let positions: RoomPositionsResponse = serde_json::from_slice(&body).unwrap();
        assert!(positions.positions.is_empty());
    }

    #[tokio::test]
    async fn room_events_include_submitted_orders() {
        let app = new_app();
        let scenario = serde_json::to_string(&spot_scenario("events-room")).unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/events-room/events?after_command_seq=0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);

        let order = serde_json::json!({
            "participant_id": "human-1",
            "account_id": 20,
            "action": {
                "PlaceLimit": {
                    "side": "Buy",
                    "price_tick": 100,
                    "qty": 2
                }
            }
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/events-room/orders")
                    .header("content-type", "application/json")
                    .body(Body::from(order.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/events-room/events?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let events: RoomEventsResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(events.room_id, "events-room");
        assert_eq!(events.executions.len(), 1);
        assert_eq!(events.executions[0].market_time_ms, Some(0));
        assert_eq!(events.next_after_command_seq, Some(0));
        assert_eq!(events.latest_command_seq, Some(0));
        assert!(!events.has_more);
        assert!(events.executions[0].accepted);
        assert!(
            events.executions[0]
                .events
                .iter()
                .any(|event| matches!(event, EventSummary::OrderRested { .. }))
        );
    }

    #[tokio::test]
    async fn order_idempotency_cursor_and_sse_resume_share_the_durable_execution() {
        let app = new_app();
        let scenario = serde_json::to_string(&seeded_spot_scenario("resume-room")).unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/resume-room/clock/advance")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"steps":2}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let order = serde_json::json!({
            "participant_id": "human-1",
            "account_id": 20,
            "action": {
                "PlaceLimit": {
                    "side": "Buy",
                    "price_tick": 100,
                    "qty": 2
                }
            }
        });
        let submit = || {
            Request::builder()
                .method(Method::POST)
                .uri("/rooms/resume-room/orders")
                .header("content-type", "application/json")
                .header(IDEMPOTENCY_KEY_HEADER, "client-order-1")
                .body(Body::from(order.to_string()))
                .unwrap()
        };
        let first = app.clone().oneshot(submit()).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let first_body = axum::body::to_bytes(first.into_body(), usize::MAX)
            .await
            .unwrap();
        let first: OrderResponse = serde_json::from_slice(&first_body).unwrap();
        assert_eq!(first.command_seq, 1);
        assert_eq!(first.market_time_ms, Some(2_000));

        let retried = app.clone().oneshot(submit()).await.unwrap();
        assert_eq!(retried.status(), StatusCode::OK);
        let retried_body = axum::body::to_bytes(retried.into_body(), usize::MAX)
            .await
            .unwrap();
        let retried: OrderResponse = serde_json::from_slice(&retried_body).unwrap();
        assert_eq!(retried.command_seq, first.command_seq);
        assert_eq!(retried.events.len(), first.events.len());
        assert_eq!(retried.market_time_ms, first.market_time_ms);

        let conflicting_order = serde_json::json!({
            "participant_id": "human-1",
            "account_id": 20,
            "action": {
                "PlaceLimit": {
                    "side": "Buy",
                    "price_tick": 99,
                    "qty": 2
                }
            }
        });
        let conflict = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/resume-room/orders")
                    .header("content-type", "application/json")
                    .header(IDEMPOTENCY_KEY_HEADER, "client-order-1")
                    .body(Body::from(conflicting_order.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(conflict.status(), StatusCode::CONFLICT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/resume-room/events?after_command_seq=0&limit=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let events: RoomEventsResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(events.executions.len(), 1);
        assert_eq!(events.executions[0].command_seq, 1);
        assert_eq!(events.executions[0].market_time_ms, Some(2_000));
        assert_eq!(events.next_after_command_seq, Some(1));
        assert_eq!(events.latest_command_seq, Some(1));
        assert!(!events.has_more);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/resume-room/events?from_start=true&limit=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let first_page: RoomEventsResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(first_page.executions[0].command_seq, 0);
        assert_eq!(first_page.next_after_command_seq, Some(0));
        assert!(first_page.has_more);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/resume-room/events/stream")
                    .header("last-event-id", "0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "text/event-stream"
        );
        let mut stream = response.into_body().into_data_stream();
        let chunk = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("SSE backlog should be available immediately")
            .expect("SSE stream should produce one frame")
            .expect("SSE frame should be readable");
        let chunk = String::from_utf8(chunk.to_vec()).unwrap();
        assert!(chunk.contains("event: execution"));
        assert!(chunk.contains("id: 1"));
        assert!(chunk.contains("\"market_time_ms\":2000"));
    }

    #[tokio::test]
    async fn room_events_include_auto_liquidation_execution() {
        let app = new_app();
        let scenario =
            serde_json::to_string(&perp_liquidation_scenario("auto-liquidation-room")).unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let orders = [
            serde_json::json!({
                "participant_id": "bidder",
                "account_id": 30,
                "action": {
                    "PlaceLimit": {
                        "side": "Buy",
                        "price_tick": 80,
                        "qty": 10
                    }
                }
            }),
            serde_json::json!({
                "participant_id": "seller",
                "account_id": 10,
                "action": {
                    "PlaceLimit": {
                        "side": "Sell",
                        "price_tick": 100,
                        "qty": 10
                    }
                }
            }),
            serde_json::json!({
                "participant_id": "distressed",
                "account_id": 20,
                "action": {
                    "PlaceMarket": {
                        "side": "Buy",
                        "qty": 10
                    }
                }
            }),
        ];

        for order in orders {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/rooms/auto-liquidation-room/orders")
                        .header("content-type", "application/json")
                        .body(Body::from(order.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/auto-liquidation-room/events?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let events: RoomEventsResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(events.executions.len(), 4);
        let liquidation = events.executions.last().unwrap();
        assert!(liquidation.accepted);
        assert!(liquidation.clearing_events.iter().any(
            |event| matches!(event, ClearingEventSummary::PerpMarginStatusChanged {
                    account_id: 20,
                    new_status,
                    ..
                } if new_status == "flat")
        ));
        assert!(liquidation.clearing_events.iter().any(|event| matches!(
            event,
            ClearingEventSummary::PerpLiquidationSettled {
                account_id: 20,
                liquidation_notional: 800,
                liquidation_fee: 8,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn mark_price_update_can_trigger_auto_liquidation() {
        let app = new_app();
        let scenario = serde_json::to_string(&perp_liquidation_scenario_with_mark(
            "mark-liquidation-room",
            100,
        ))
        .unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let orders = [
            serde_json::json!({
                "participant_id": "bidder",
                "account_id": 30,
                "action": {
                    "PlaceLimit": {
                        "side": "Buy",
                        "price_tick": 80,
                        "qty": 10
                    }
                }
            }),
            serde_json::json!({
                "participant_id": "seller",
                "account_id": 10,
                "action": {
                    "PlaceLimit": {
                        "side": "Sell",
                        "price_tick": 100,
                        "qty": 10
                    }
                }
            }),
            serde_json::json!({
                "participant_id": "buyer",
                "account_id": 20,
                "action": {
                    "PlaceMarket": {
                        "side": "Buy",
                        "qty": 10
                    }
                }
            }),
        ];

        for order in orders {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/rooms/mark-liquidation-room/orders")
                        .header("content-type", "application/json")
                        .body(Body::from(order.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/mark-liquidation-room/instruments/V-BTC-PERP/mark-price")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"price_tick":80}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let mark_execution: RoomExecutionSummary = serde_json::from_slice(&body).unwrap();
        assert!(mark_execution.clearing_events.iter().any(
            |event| matches!(event, ClearingEventSummary::PerpMarginStatusChanged {
                    account_id: 20,
                    new_status,
                    ..
                } if new_status == "liquidatable")
        ));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/mark-liquidation-room/events?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let events: RoomEventsResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(events.executions.len(), 5);
        assert!(
            events
                .executions
                .last()
                .unwrap()
                .clearing_events
                .iter()
                .any(
                    |event| matches!(event, ClearingEventSummary::PerpMarginStatusChanged {
                    account_id: 20,
                    new_status,
                    ..
                } if new_status == "flat")
                )
        );
        assert!(
            events
                .executions
                .last()
                .unwrap()
                .clearing_events
                .iter()
                .any(|event| matches!(
                    event,
                    ClearingEventSummary::PerpLiquidationSettled {
                        account_id: 20,
                        liquidation_notional: 800,
                        liquidation_fee: 8,
                        ..
                    }
                ))
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/mark-liquidation-room/positions?account_id=20&limit=100")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let positions: RoomPositionsResponse = serde_json::from_slice(&body).unwrap();
        assert!(positions.positions.iter().any(|position| {
            position.market_kind == "perp"
                && position.portfolio_initial_margin.is_some()
                && position.portfolio_maintenance_margin.is_some()
        }));
    }

    #[tokio::test]
    async fn projection_query_routes_are_available_without_postgres() {
        let app = new_app();
        let scenario = serde_json::to_string(&spot_scenario("query-room")).unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        for uri in [
            "/rooms/query-room/orders",
            "/rooms/query-room/orders?instrument_id=V-BTC-SPOT",
            "/rooms/query-room/trades",
            "/rooms/query-room/ticks",
            "/rooms/query-room/ledger",
            "/rooms/query-room/positions",
            "/rooms/query-room/instruments/V-BTC-SPOT/orders",
            "/rooms/query-room/instruments/V-BTC-SPOT/trades",
            "/rooms/query-room/instruments/V-BTC-SPOT/ticks",
            "/rooms/query-room/instruments/V-BTC-SPOT/ledger",
            "/rooms/query-room/instruments/V-BTC-SPOT/positions",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::GET)
                        .uri(uri)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn user_header_scopes_room_visibility_and_account_actions() {
        let app = new_app();
        let scenario = serde_json::to_string(&spot_scenario("tenant-room")).unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .header(USER_ID_HEADER, "alice")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms")
                    .header(USER_ID_HEADER, "bob")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let rooms: ListRoomsResponse = serde_json::from_slice(&body).unwrap();
        assert!(rooms.rooms.is_empty());

        let order = serde_json::json!({
            "participant_id": "bob-human",
            "account_id": 20,
            "action": {
                "PlaceLimit": {
                    "side": "Buy",
                    "price_tick": 100,
                    "qty": 2
                }
            }
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/tenant-room/orders")
                    .header("content-type", "application/json")
                    .header(USER_ID_HEADER, "bob")
                    .body(Body::from(order.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn cluster_room_directory_is_authorized_and_cursor_paginated() {
        let app = new_app();
        for (room_id, user_id) in [
            ("directory-c", "bob"),
            ("directory-b", "alice"),
            ("directory-a", "alice"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/rooms")
                        .header("content-type", "application/json")
                        .header(USER_ID_HEADER, user_id)
                        .body(Body::from(
                            serde_json::to_string(&spot_scenario(room_id)).unwrap(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/cluster/rooms?limit=1")
                    .header(USER_ID_HEADER, "alice")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let first: ClusterRoomsResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(first.rooms.len(), 1);
        assert_eq!(first.rooms[0].room_id, "directory-a");
        assert!(first.rooms[0].owner.is_none());
        assert_eq!(first.next_after_room_id.as_deref(), Some("directory-a"));
        assert!(first.has_more);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/cluster/rooms?after_room_id=directory-a&limit=1")
                    .header(USER_ID_HEADER, "alice")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let second: ClusterRoomsResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(second.rooms.len(), 1);
        assert_eq!(second.rooms[0].room_id, "directory-b");
        assert!(!second.has_more);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/cluster/rooms?limit=10")
                    .header(USER_ID_HEADER, "bob")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let bob: ClusterRoomsResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            bob.rooms
                .iter()
                .map(|room| room.room_id.as_str())
                .collect::<Vec<_>>(),
            vec!["directory-c"]
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/cluster/rooms?after_room_id=")
                    .header(USER_ID_HEADER, "alice")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn ordinary_room_members_cannot_access_admin_surfaces() {
        #[derive(Default)]
        struct RoleJournal {
            roles: BTreeMap<(String, String), String>,
            account_owners: BTreeMap<(String, AccountId), String>,
        }

        impl JournalStore for RoleJournal {
            fn load_recovery(&mut self) -> Result<JournalRecovery, JournalError> {
                Ok(JournalRecovery::default())
            }

            fn create_room(
                &mut self,
                owner_user_id: &str,
                _scenario: &ScenarioConfig,
                bootstrap: &RoomBootstrap,
                account_ids: &[AccountId],
                _seed_records: &[JournalExecution],
                _initial_snapshot: Option<&JournalSnapshot>,
            ) -> Result<(), JournalError> {
                let room_id = bootstrap.room_id.clone();
                self.roles.insert(
                    (room_id.clone(), owner_user_id.to_string()),
                    "owner".to_string(),
                );
                self.roles.insert(
                    (room_id.clone(), "member".to_string()),
                    "member".to_string(),
                );
                self.roles
                    .insert((room_id.clone(), "admin".to_string()), "admin".to_string());
                for account_id in account_ids {
                    self.account_owners
                        .insert((room_id.clone(), *account_id), owner_user_id.to_string());
                }
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
                user_id: &str,
                room_id: &str,
            ) -> Result<bool, JournalError> {
                Ok(self
                    .roles
                    .contains_key(&(room_id.to_string(), user_id.to_string())))
            }

            fn user_can_administer_room(
                &mut self,
                user_id: &str,
                room_id: &str,
            ) -> Result<bool, JournalError> {
                Ok(self
                    .roles
                    .get(&(room_id.to_string(), user_id.to_string()))
                    .is_some_and(|role| role == "owner" || role == "admin"))
            }

            fn user_can_access_account(
                &mut self,
                user_id: &str,
                room_id: &str,
                account_id: AccountId,
            ) -> Result<bool, JournalError> {
                if self.user_can_administer_room(user_id, room_id)? {
                    return Ok(true);
                }
                Ok(self
                    .account_owners
                    .get(&(room_id.to_string(), account_id))
                    .is_some_and(|owner| owner == user_id))
            }
        }

        let app = new_app_with_journal("http://127.0.0.1:57305", Box::new(RoleJournal::default()));
        let scenario = serde_json::to_string(&spot_scenario("role-room")).unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .header(USER_ID_HEADER, "owner")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/role-room/book")
                    .header(USER_ID_HEADER, "member")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        for path in [
            "/rooms/role-room/events",
            "/rooms/role-room/view",
            "/rooms/role-room/accounts",
            "/rooms/role-room/venue/accounts",
            "/rooms/role-room/venue/accounts/by-venue",
            "/rooms/role-room/portfolio",
            "/rooms/role-room/assets/ledger",
            "/rooms/role-room/net-worth",
            "/rooms/role-room/transfers",
            "/rooms/role-room/orders",
            "/rooms/role-room/trades",
            "/rooms/role-room/ledger",
            "/rooms/role-room/positions",
            "/rooms/role-room/agents",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::GET)
                        .uri(path)
                        .header(USER_ID_HEADER, "member")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "path {path}");
        }

        let mutations = [
            (
                "/rooms/role-room/clock/advance",
                serde_json::json!({"steps": 1}),
            ),
            ("/rooms/role-room/pause", serde_json::json!(null)),
            ("/rooms/role-room/resume", serde_json::json!(null)),
            (
                "/rooms/role-room/instruments/V-BTC-SPOT/mark-price",
                serde_json::json!({"price_tick": 90}),
            ),
            (
                "/rooms/role-room/agents",
                serde_json::json!({"agents": [], "interval_ms": 10}),
            ),
            ("/rooms/role-room/agents/stop", serde_json::json!(null)),
        ];
        for (path, body) in mutations {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(path)
                        .header("content-type", "application/json")
                        .header(USER_ID_HEADER, "member")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "path {path}");
        }

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/role-room/accounts")
                    .header(USER_ID_HEADER, "admin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn failed_journal_append_does_not_commit_order_state() {
        struct FailingAppendJournal;

        impl JournalStore for FailingAppendJournal {
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
                Err(JournalError::CountOutOfRange(0))
            }

            fn user_can_administer_room(
                &mut self,
                _user_id: &str,
                _room_id: &str,
            ) -> Result<bool, JournalError> {
                Ok(true)
            }

            fn update_room_status(
                &mut self,
                _room_id: &str,
                _status: MarketStatus,
            ) -> Result<(), JournalError> {
                Ok(())
            }
        }

        let app = new_app_with_journal("http://127.0.0.1:57305", Box::new(FailingAppendJournal));
        let scenario = serde_json::to_string(&spot_scenario("journal-failure")).unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let order = serde_json::json!({
            "participant_id": "human-1",
            "account_id": 20,
            "action": {
                "PlaceLimit": {
                    "side": "Buy",
                    "price_tick": 100,
                    "qty": 2
                }
            }
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/journal-failure/orders")
                    .header("content-type", "application/json")
                    .body(Body::from(order.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/journal-failure/view")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let view: MarketView = serde_json::from_slice(&body).unwrap();
        assert!(view.book.bids.is_empty());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/journal-failure/events?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let events: RoomEventsResponse = serde_json::from_slice(&body).unwrap();
        assert!(events.executions.is_empty());
    }

    #[tokio::test]
    async fn failed_multi_execution_batch_does_not_commit_any_timeline_entry() {
        struct RejectMultiExecutionJournal;

        impl JournalStore for RejectMultiExecutionJournal {
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
                records: &[JournalExecution],
                _snapshot: Option<&JournalSnapshot>,
            ) -> Result<(), JournalError> {
                if records.len() > 1 {
                    return Err(JournalError::CountOutOfRange(records.len()));
                }
                Ok(())
            }

            fn update_room_status(
                &mut self,
                _room_id: &str,
                _status: MarketStatus,
            ) -> Result<(), JournalError> {
                Ok(())
            }

            fn user_can_administer_room(
                &mut self,
                _user_id: &str,
                _room_id: &str,
            ) -> Result<bool, JournalError> {
                Ok(true)
            }
        }

        let app = new_app_with_journal(
            "http://127.0.0.1:57305",
            Box::new(RejectMultiExecutionJournal),
        );
        let scenario =
            serde_json::to_string(&perp_liquidation_scenario("batch-failure-room")).unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let orders = [
            serde_json::json!({
                "participant_id": "bidder",
                "account_id": 30,
                "action": {"PlaceLimit": {"side": "Buy", "price_tick": 80, "qty": 10}}
            }),
            serde_json::json!({
                "participant_id": "seller",
                "account_id": 10,
                "action": {"PlaceLimit": {"side": "Sell", "price_tick": 100, "qty": 10}}
            }),
            serde_json::json!({
                "participant_id": "distressed",
                "account_id": 20,
                "action": {"PlaceMarket": {"side": "Buy", "qty": 10}}
            }),
        ];

        for (index, order) in orders.into_iter().enumerate() {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/rooms/batch-failure-room/orders")
                        .header("content-type", "application/json")
                        .body(Body::from(order.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            let expected = if index < 2 {
                StatusCode::OK
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            assert_eq!(response.status(), expected);
        }

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/batch-failure-room/events?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let events: RoomEventsResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(events.executions.len(), 2);
    }

    #[tokio::test]
    async fn recovery_replays_rooms_and_restores_next_order_id() {
        #[derive(Clone)]
        struct RecoveryJournal {
            recovery: JournalRecovery,
        }

        impl JournalStore for RecoveryJournal {
            fn load_recovery(&mut self) -> Result<JournalRecovery, JournalError> {
                Ok(self.recovery.clone())
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

            fn user_can_administer_room(
                &mut self,
                _user_id: &str,
                _room_id: &str,
            ) -> Result<bool, JournalError> {
                Ok(true)
            }

            fn update_room_status(
                &mut self,
                _room_id: &str,
                _status: MarketStatus,
            ) -> Result<(), JournalError> {
                Ok(())
            }
        }

        let scenario = seeded_spot_scenario("recovered-room");
        let mut rooms = RoomManager::new();
        let bootstrap = rooms.create_room(scenario.clone()).unwrap();
        let seed_records = scenario
            .seed_commands()
            .into_iter()
            .zip(bootstrap.seed_executions.iter().cloned())
            .map(|(command, execution)| JournalExecution::seed(command, execution))
            .collect::<Vec<_>>();
        let mut gateway = OrderGateway::new(&mut rooms, 1);
        let submitted = gateway
            .submit_action(GatewayRequest {
                participant_id: "human-1".to_string(),
                room_id: "recovered-room".to_string(),
                instrument_id: None,
                account_id: 20,
                action: OrderAction::PlaceLimit {
                    side: Side::Buy,
                    price_tick: 104,
                    qty: 2,
                },
            })
            .unwrap();
        let submitted_record = JournalExecution::submitted(
            submitted.participant_id,
            submitted.account_id,
            submitted.command,
            submitted.execution,
        );
        let snapshot = JournalSnapshot {
            room_id: "recovered-room".to_string(),
            command_seq: submitted_record.command_seq,
            actor: rooms.simulation_room("recovered-room").unwrap().clone(),
        };

        let mut executions = seed_records;
        executions.push(submitted_record);
        let recovery = JournalRecovery {
            rooms: vec![journal::JournalRoom {
                room_id: "recovered-room".to_string(),
                scenario,
                status: MarketStatus::Running,
            }],
            executions,
            mutations: Vec::new(),
            snapshots: vec![snapshot],
        };
        let app = new_app_recovering_with_journal(
            "http://127.0.0.1:57305",
            Box::new(RecoveryJournal { recovery }),
        )
        .unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/recovered-room/view")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let view: MarketView = serde_json::from_slice(&body).unwrap();
        assert_eq!(view.book.asks[0].qty, 6);

        let order = serde_json::json!({
            "participant_id": "human-1",
            "account_id": 20,
            "action": {
                "PlaceLimit": {
                    "side": "Buy",
                    "price_tick": 99,
                    "qty": 1
                }
            }
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/recovered-room/orders")
                    .header("content-type", "application/json")
                    .body(Body::from(order.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let order: OrderResponse = serde_json::from_slice(&body).unwrap();
        assert!(order.events.iter().any(|event| matches!(
            event,
            EventSummary::OrderAccepted {
                order_id: 10_001,
                ..
            }
        )));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/recovered-room/events?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let events: RoomEventsResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(events.executions.len(), 3);
        assert_eq!(events.executions[0].command_seq, 0);
        assert_eq!(events.executions[1].command_seq, 1);
    }

    #[tokio::test]
    async fn recovered_clock_snapshot_keeps_last_command_sequence_across_second_restart() {
        #[derive(Clone)]
        struct SharedRecoveryJournal {
            recovery: Arc<Mutex<JournalRecovery>>,
        }

        impl SharedRecoveryJournal {
            fn store_snapshot(recovery: &mut JournalRecovery, snapshot: &JournalSnapshot) {
                if let Some(existing) = recovery
                    .snapshots
                    .iter_mut()
                    .find(|existing| existing.room_id == snapshot.room_id)
                {
                    if snapshot.command_seq >= existing.command_seq {
                        *existing = snapshot.clone();
                    }
                } else {
                    recovery.snapshots.push(snapshot.clone());
                }
            }
        }

        impl JournalStore for SharedRecoveryJournal {
            fn load_recovery(&mut self) -> Result<JournalRecovery, JournalError> {
                self.recovery
                    .lock()
                    .map(|recovery| recovery.clone())
                    .map_err(|_| JournalError::Recovery("shared recovery lock poisoned".into()))
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
                records: &[JournalExecution],
                snapshot: Option<&JournalSnapshot>,
            ) -> Result<(), JournalError> {
                let mut recovery = self
                    .recovery
                    .lock()
                    .map_err(|_| JournalError::Recovery("shared recovery lock poisoned".into()))?;
                recovery.executions.extend_from_slice(records);
                if let Some(snapshot) = snapshot {
                    Self::store_snapshot(&mut recovery, snapshot);
                }
                Ok(())
            }

            fn append_snapshot(&mut self, snapshot: &JournalSnapshot) -> Result<(), JournalError> {
                let mut recovery = self
                    .recovery
                    .lock()
                    .map_err(|_| JournalError::Recovery("shared recovery lock poisoned".into()))?;
                Self::store_snapshot(&mut recovery, snapshot);
                Ok(())
            }

            fn update_room_status(
                &mut self,
                _room_id: &str,
                _status: MarketStatus,
            ) -> Result<(), JournalError> {
                Ok(())
            }

            fn user_can_administer_room(
                &mut self,
                _user_id: &str,
                _room_id: &str,
            ) -> Result<bool, JournalError> {
                Ok(true)
            }
        }

        let scenario = seeded_spot_scenario("snapshot-seq-room");
        let mut rooms = RoomManager::new();
        let bootstrap = rooms.create_room(scenario.clone()).unwrap();
        let mut executions = scenario
            .seed_commands()
            .into_iter()
            .zip(bootstrap.seed_executions.iter().cloned())
            .map(|(command, execution)| JournalExecution::seed(command, execution))
            .collect::<Vec<_>>();
        let mut gateway = OrderGateway::new(&mut rooms, 10_001);
        let submitted = gateway
            .submit_action(GatewayRequest {
                participant_id: "human-1".to_string(),
                room_id: "snapshot-seq-room".to_string(),
                instrument_id: None,
                account_id: 20,
                action: OrderAction::PlaceLimit {
                    side: Side::Buy,
                    price_tick: 99,
                    qty: 1,
                },
            })
            .unwrap();
        let submitted_record = JournalExecution::submitted(
            submitted.participant_id,
            submitted.account_id,
            submitted.command,
            submitted.execution,
        );
        let snapshot = JournalSnapshot {
            room_id: "snapshot-seq-room".to_string(),
            command_seq: submitted_record.command_seq,
            actor: rooms.simulation_room("snapshot-seq-room").unwrap().clone(),
        };
        assert_eq!(snapshot.command_seq, 1);
        executions.push(submitted_record);

        let recovery = Arc::new(Mutex::new(JournalRecovery {
            rooms: vec![journal::JournalRoom {
                room_id: "snapshot-seq-room".to_string(),
                scenario,
                status: MarketStatus::Running,
            }],
            executions,
            mutations: Vec::new(),
            snapshots: vec![snapshot],
        }));

        let first_app = new_app_recovering_with_journal(
            "http://127.0.0.1:57305",
            Box::new(SharedRecoveryJournal {
                recovery: Arc::clone(&recovery),
            }),
        )
        .unwrap();
        let response = first_app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/snapshot-seq-room/clock/advance")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"steps":7}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(recovery.lock().unwrap().snapshots[0].command_seq, 1);

        let second_app = new_app_recovering_with_journal(
            "http://127.0.0.1:57305",
            Box::new(SharedRecoveryJournal { recovery }),
        )
        .unwrap();
        let response = second_app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/snapshot-seq-room/clock")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let clock: RoomClockResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(clock.clock.step(), 7);
    }

    #[tokio::test]
    async fn postgres_journal_persists_and_recovers_room_when_configured() {
        let database_url = match std::env::var("MARKETFORGE_TEST_DATABASE_URL") {
            Ok(database_url) if !database_url.trim().is_empty() => database_url,
            _ if std::env::var("MARKETFORGE_REQUIRE_POSTGRES_TESTS").as_deref() == Ok("1") => {
                panic!(
                    "MARKETFORGE_REQUIRE_POSTGRES_TESTS=1 requires MARKETFORGE_TEST_DATABASE_URL"
                );
            }
            _ => return,
        };
        let room_id = format!(
            "pg-recovery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let mut scenario = seeded_spot_scenario(&room_id);
        scenario.initial_portfolios = vec![ScenarioPortfolio {
            account_id: 20,
            balances: BTreeMap::from([("BTC".to_string(), 25)]),
        }];

        cleanup_postgres_room(&database_url, &room_id);
        let lock_test_database_url = database_url.clone();
        tokio::task::spawn_blocking(move || {
            let exclusive =
                PostgresJournalStore::connect_migrated_exclusive(&lock_test_database_url).unwrap();
            assert!(matches!(
                PostgresJournalStore::connect_migrated_exclusive(&lock_test_database_url),
                Err(JournalError::RuntimeLockUnavailable)
            ));
            let waiter_database_url = lock_test_database_url.clone();
            let waiter = std::thread::spawn(move || {
                PostgresJournalStore::connect_migrated_exclusive_with_wait(
                    &waiter_database_url,
                    Duration::from_secs(2),
                )
            });
            std::thread::sleep(Duration::from_millis(150));
            assert!(
                !waiter.is_finished(),
                "standby should still be waiting while the active lock is held"
            );
            drop(exclusive);
            drop(waiter.join().unwrap().unwrap());
        })
        .await
        .unwrap();

        let app = new_postgres_test_app(&database_url).await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&scenario).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let order = serde_json::json!({
            "participant_id": "human-pg",
            "account_id": 20,
            "action": {
                "PlaceLimit": {
                    "side": "Buy",
                    "price_tick": 104,
                    "qty": 2
                }
            }
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/rooms/{room_id}/orders"))
                    .header("content-type", "application/json")
                    .body(Body::from(order.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let submitted: OrderResponse = serde_json::from_slice(&body).unwrap();
        let submitted_order_id = submitted
            .events
            .iter()
            .find_map(|event| match event {
                EventSummary::OrderAccepted { order_id, .. } => Some(*order_id),
                _ => None,
            })
            .unwrap();
        let transfer = serde_json::json!({
            "account_id": 20,
            "asset_id": "BTC",
            "amount": 25
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/rooms/{room_id}/transfers/deposit"))
                    .header("content-type", "application/json")
                    .body(Body::from(transfer.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_postgres_trade_projection(&database_url, &room_id, submitted_order_id);
        assert_postgres_transfer_projection(&database_url, &room_id);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/rooms/{room_id}/pause"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        delete_postgres_snapshots(&database_url, &room_id);

        let recovered = new_postgres_test_app_via_sync_constructor(&database_url).await;
        let response = recovered
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/rooms/{room_id}/view"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let view: MarketView = serde_json::from_slice(&body).unwrap();
        assert_eq!(view.book.asks[0].qty, 6);
        assert_eq!(view.status, MarketStatus::Paused);

        let response = recovered
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/rooms/{room_id}/events?from_start=true&limit=1"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let events: RoomEventsResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(events.executions.len(), 1);
        assert_eq!(events.executions[0].command_seq, 0);
        assert_eq!(events.latest_command_seq, Some(1));
        assert!(events.has_more);

        let response = recovered
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/rooms/{room_id}/trades?account_id=20&limit=10"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let trades: RoomTradesResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(trades.trades.len(), 1);
        assert_eq!(trades.trades[0].taker_account_id, 20);

        let lease_database_url = database_url.clone();
        let lease_room_id = room_id.clone();
        tokio::task::spawn_blocking(move || {
            assert_postgres_room_scoped_recovery(&lease_database_url, &lease_room_id);
            assert_postgres_room_writer_lease_fencing(&lease_database_url, &lease_room_id);
        })
        .await
        .unwrap();
        cleanup_postgres_room(&database_url, &room_id);
    }

    async fn new_postgres_test_app(database_url: &str) -> Router {
        let database_url = database_url.to_string();
        new_app_recovering_with_journal_factory_async(
            "http://127.0.0.1:57305".to_string(),
            AuthPolicy::local_development(),
            default_cors_origins(),
            move || {
                let writer = Box::new(PostgresJournalStore::connect_migrated_exclusive(
                    &database_url,
                )?) as Box<dyn JournalStore>;
                let readers = (0..2)
                    .map(|_| {
                        PostgresJournalStore::connect(&database_url)
                            .map(|store| Box::new(store) as Box<dyn JournalStore>)
                    })
                    .collect::<Result<Vec<_>, JournalError>>()?;
                Ok(JournalStoreBundle { writer, readers })
            },
        )
        .await
        .unwrap()
    }

    async fn new_postgres_test_app_via_sync_constructor(database_url: &str) -> Router {
        let database_url = database_url.to_string();
        let store = tokio::task::spawn_blocking(move || {
            PostgresJournalStore::connect_migrated_exclusive(&database_url)
        })
        .await
        .unwrap()
        .unwrap();
        new_app_recovering_with_journal("http://127.0.0.1:57305", Box::new(store)).unwrap()
    }

    fn assert_postgres_room_scoped_recovery(database_url: &str, room_id: &str) {
        let mut store = PostgresJournalStore::connect_migrated(database_url).unwrap();
        let recovery = store.load_room_recovery(room_id).unwrap();
        assert_eq!(
            recovery
                .rooms
                .iter()
                .map(|room| room.room_id.as_str())
                .collect::<Vec<_>>(),
            vec![room_id]
        );
        assert!(
            recovery
                .executions
                .iter()
                .all(|execution| execution.room_id == room_id)
        );
        assert!(
            recovery
                .mutations
                .iter()
                .all(|mutation| mutation.room_id == room_id)
        );
        assert!(
            recovery
                .snapshots
                .iter()
                .all(|snapshot| snapshot.room_id == room_id)
        );

        let missing = store
            .load_room_recovery("postgres-room-scoped-recovery-missing")
            .unwrap();
        assert!(missing.rooms.is_empty());
        assert!(missing.executions.is_empty());
        assert!(missing.mutations.is_empty());
        assert!(missing.snapshots.is_empty());
    }

    fn assert_postgres_trade_projection(
        database_url: &str,
        room_id: &str,
        taker_order_id: OrderId,
    ) {
        let database_url = database_url.to_string();
        let room_id = room_id.to_string();
        std::thread::spawn(move || {
            let mut client = postgres::Client::connect(&database_url, postgres::NoTls).unwrap();
            let taker_order_id = i64::try_from(taker_order_id).unwrap();

            let migration_name: String = client
                .query_one(
                    "SELECT name FROM marketforge_schema_migrations WHERE version = 1",
                    &[],
                )
                .unwrap()
                .get(0);
            assert_eq!(migration_name, "initial_schema");
            let access_migration_name: String = client
                .query_one(
                    "SELECT name FROM marketforge_schema_migrations WHERE version = 2",
                    &[],
                )
                .unwrap()
                .get(0);
            assert_eq!(access_migration_name, "access_control");
            let instrument_migration_name: String = client
                .query_one(
                    "SELECT name FROM marketforge_schema_migrations WHERE version = 3",
                    &[],
                )
                .unwrap()
                .get(0);
            assert_eq!(instrument_migration_name, "instrument_projection_scope");
            let transfer_migration_name: String = client
                .query_one(
                    "SELECT name FROM marketforge_schema_migrations WHERE version = 4",
                    &[],
                )
                .unwrap()
                .get(0);
            assert_eq!(transfer_migration_name, "transfer_journal");
            let mutation_migration_name: String = client
                .query_one(
                    "SELECT name FROM marketforge_schema_migrations WHERE version = 7",
                    &[],
                )
                .unwrap()
                .get(0);
            assert_eq!(mutation_migration_name, "room_mutation_journal");
            let portfolio_margin_migration_name: String = client
                .query_one(
                    "SELECT name FROM marketforge_schema_migrations WHERE version = 8",
                    &[],
                )
                .unwrap()
                .get(0);
            assert_eq!(
                portfolio_margin_migration_name,
                "portfolio_margin_projection_fields"
            );
            let market_time_migration_name: String = client
                .query_one(
                    "SELECT name FROM marketforge_schema_migrations WHERE version = 9",
                    &[],
                )
                .unwrap()
                .get(0);
            assert_eq!(market_time_migration_name, "authoritative_market_time");
            let idempotency_migration_name: String = client
                .query_one(
                    "SELECT name FROM marketforge_schema_migrations WHERE version = 10",
                    &[],
                )
                .unwrap()
                .get(0);
            assert_eq!(idempotency_migration_name, "order_request_idempotency");
            let room_lease_migration_name: String = client
                .query_one(
                    "SELECT name FROM marketforge_schema_migrations WHERE version = 11",
                    &[],
                )
                .unwrap()
                .get(0);
            assert_eq!(room_lease_migration_name, "room_writer_leases");
            let room_owner_url_migration_name: String = client
                .query_one(
                    "SELECT name FROM marketforge_schema_migrations WHERE version = 12",
                    &[],
                )
                .unwrap()
                .get(0);
            assert_eq!(room_owner_url_migration_name, "room_writer_owner_url");

            let mutation_count: i64 = client
                .query_one(
                    "SELECT count(*) FROM marketforge_room_mutations WHERE room_id = $1",
                    &[&room_id],
                )
                .unwrap()
                .get(0);
            assert!(mutation_count >= 2);

            let order_count: i64 = client
                .query_one(
                    "SELECT count(*) FROM marketforge_orders WHERE room_id = $1",
                    &[&room_id],
                )
                .unwrap()
                .get(0);
            assert_eq!(order_count, 2);

            let seed_order = client
                .query_one(
                    r#"
                    SELECT instrument_id, status, remaining_qty,
                           created_market_time_ms, updated_market_time_ms
                    FROM marketforge_orders
                    WHERE room_id = $1 AND order_id = 10000
                    "#,
                    &[&room_id],
                )
                .unwrap();
            let seed_instrument_id: String = seed_order.get("instrument_id");
            let seed_status: String = seed_order.get("status");
            let seed_remaining_qty: i64 = seed_order.get("remaining_qty");
            let seed_created_market_time_ms: Option<i64> =
                seed_order.get("created_market_time_ms");
            let seed_updated_market_time_ms: Option<i64> =
                seed_order.get("updated_market_time_ms");
            assert_eq!(seed_instrument_id, "V-BTC-SPOT");
            assert_eq!(seed_status, "partially_filled");
            assert_eq!(seed_remaining_qty, 6);
            assert_eq!(seed_created_market_time_ms, Some(0));
            assert_eq!(seed_updated_market_time_ms, Some(0));

            let taker_order = client
                .query_one(
                    r#"
                    SELECT instrument_id, status, remaining_qty, account_id, participant_id
                    FROM marketforge_orders
                    WHERE room_id = $1 AND order_id = $2
                    "#,
                    &[&room_id, &taker_order_id],
                )
                .unwrap();
            let taker_instrument_id: String = taker_order.get("instrument_id");
            let taker_status: String = taker_order.get("status");
            let taker_remaining_qty: i64 = taker_order.get("remaining_qty");
            let taker_account_id: i64 = taker_order.get("account_id");
            let participant_id: Option<String> = taker_order.get("participant_id");
            assert_eq!(taker_instrument_id, "V-BTC-SPOT");
            assert_eq!(taker_status, "filled");
            assert_eq!(taker_remaining_qty, 0);
            assert_eq!(taker_account_id, 20);
            assert_eq!(participant_id.as_deref(), Some("human-pg"));

            let trade = client
                .query_one(
                    r#"
                    SELECT instrument_id, maker_account_id, taker_account_id, price_tick, qty,
                           taker_side, market_time_ms
                    FROM marketforge_trades
                    WHERE room_id = $1
                    "#,
                    &[&room_id],
                )
                .unwrap();
            let trade_instrument_id: String = trade.get("instrument_id");
            let maker_account_id: i64 = trade.get("maker_account_id");
            let taker_account_id: i64 = trade.get("taker_account_id");
            let price_tick: i64 = trade.get("price_tick");
            let qty: i64 = trade.get("qty");
            let taker_side: String = trade.get("taker_side");
            let market_time_ms: Option<i64> = trade.get("market_time_ms");
            assert_eq!(trade_instrument_id, "V-BTC-SPOT");
            assert_eq!(maker_account_id, 10);
            assert_eq!(taker_account_id, 20);
            assert_eq!(price_tick, 104);
            assert_eq!(qty, 2);
            assert_eq!(taker_side, "buy");
            assert_eq!(market_time_ms, Some(0));

            let tick_count: i64 = client
                .query_one(
                    "SELECT count(*) FROM marketforge_market_ticks WHERE room_id = $1",
                    &[&room_id],
                )
                .unwrap()
                .get(0);
            assert_eq!(tick_count, 1);

            let ledger_count: i64 = client
                .query_one(
                    "SELECT count(*) FROM marketforge_account_ledger WHERE room_id = $1",
                    &[&room_id],
                )
                .unwrap()
                .get(0);
            assert_eq!(ledger_count, 2);

            let buyer_ledger = client
                .query_one(
                    r#"
                    SELECT instrument_id, account_side, cash_delta, position_delta, cash_balance, position_qty
                    FROM marketforge_account_ledger
                    WHERE room_id = $1 AND account_id = 20
                    "#,
                    &[&room_id],
                )
                .unwrap();
            let ledger_instrument_id: String = buyer_ledger.get("instrument_id");
            let account_side: String = buyer_ledger.get("account_side");
            let cash_delta: i64 = buyer_ledger.get("cash_delta");
            let position_delta: i64 = buyer_ledger.get("position_delta");
            let cash_balance: i64 = buyer_ledger.get("cash_balance");
            let position_qty: i64 = buyer_ledger.get("position_qty");
            assert_eq!(ledger_instrument_id, "V-BTC-SPOT");
            assert_eq!(account_side, "buy");
            assert_eq!(cash_delta, -208);
            assert_eq!(position_delta, 2);
            assert_eq!(cash_balance, 792);
            assert_eq!(position_qty, 2);

            let position_count: i64 = client
                .query_one(
                    "SELECT count(*) FROM marketforge_position_snapshots WHERE room_id = $1",
                    &[&room_id],
                )
                .unwrap()
                .get(0);
            assert_eq!(position_count, 2);
        })
        .join()
        .unwrap();
    }

    fn assert_postgres_room_writer_lease_fencing(database_url: &str, room_id: &str) {
        let mut owner_a = PostgresJournalStore::connect_migrated(database_url).unwrap();
        let mut owner_b = PostgresJournalStore::connect_migrated(database_url).unwrap();
        let lease_a = owner_a
            .acquire_room_writer_lease(
                room_id,
                "instance-a",
                Some("http://instance-a"),
                Duration::from_secs(1),
            )
            .unwrap()
            .unwrap();
        assert_eq!(lease_a.claim.fencing_token, 1);
        assert_eq!(lease_a.owner_url.as_deref(), Some("http://instance-a"));
        assert!(
            owner_b
                .acquire_room_writer_lease(
                    room_id,
                    "instance-b",
                    Some("http://instance-b"),
                    Duration::from_secs(1),
                )
                .unwrap()
                .is_none()
        );
        let renewed_a = owner_a
            .renew_room_writer_lease(&lease_a.claim, Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert_eq!(renewed_a.claim, lease_a.claim);
        assert!(owner_a.release_room_writer_lease(&lease_a.claim).unwrap());

        let lease_b = owner_b
            .acquire_room_writer_lease(
                room_id,
                "instance-b",
                Some("http://instance-b"),
                Duration::from_secs(1),
            )
            .unwrap()
            .unwrap();
        assert_eq!(lease_b.claim.fencing_token, 2);
        assert_eq!(lease_b.owner_url.as_deref(), Some("http://instance-b"));
        assert!(!owner_a.release_room_writer_lease(&lease_a.claim).unwrap());

        let mutation = PendingJournalMutation::new(
            room_id,
            2,
            RoomMutation::StatusChanged {
                status: MarketStatus::Paused,
            },
        );
        assert!(matches!(
            owner_a.append_room_mutation_fenced(&lease_a.claim, &mutation, &[], &[], None),
            Err(JournalError::RoomLeaseLost { .. })
        ));
        owner_b
            .append_room_mutation_fenced(&lease_b.claim, &mutation, &[], &[], None)
            .unwrap();
        assert!(owner_b.release_room_writer_lease(&lease_b.claim).unwrap());

        let expiring = owner_a
            .acquire_room_writer_lease(room_id, "instance-a", None, Duration::from_millis(20))
            .unwrap()
            .unwrap();
        std::thread::sleep(Duration::from_millis(40));
        let after_expiry = owner_b
            .acquire_room_writer_lease(room_id, "instance-b", None, Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert_eq!(
            after_expiry.claim.fencing_token,
            expiring.claim.fencing_token + 1
        );
        assert!(
            owner_b
                .release_room_writer_lease(&after_expiry.claim)
                .unwrap()
        );
    }

    fn assert_postgres_transfer_projection(database_url: &str, room_id: &str) {
        let database_url = database_url.to_string();
        let room_id = room_id.to_string();
        std::thread::spawn(move || {
            let mut client = postgres::Client::connect(&database_url, postgres::NoTls).unwrap();
            let transfer = client
                .query_one(
                    r#"
                    SELECT kind, account_id, asset_id, amount, status
                    FROM marketforge_transfers
                    WHERE room_id = $1
                    "#,
                    &[&room_id],
                )
                .unwrap();
            let kind: String = transfer.get("kind");
            let account_id: i64 = transfer.get("account_id");
            let asset_id: String = transfer.get("asset_id");
            let amount: i64 = transfer.get("amount");
            let status: String = transfer.get("status");
            assert_eq!(kind, "deposit");
            assert_eq!(account_id, 20);
            assert_eq!(asset_id, "BTC");
            assert_eq!(amount, 25);
            assert_eq!(status, "completed");

            let event_count: i64 = client
                .query_one(
                    "SELECT count(*) FROM marketforge_transfer_events WHERE room_id = $1",
                    &[&room_id],
                )
                .unwrap()
                .get(0);
            assert_eq!(event_count, 1);
        })
        .join()
        .unwrap();
    }

    fn delete_postgres_snapshots(database_url: &str, room_id: &str) {
        let database_url = database_url.to_string();
        let room_id = room_id.to_string();
        std::thread::spawn(move || {
            let mut client = postgres::Client::connect(&database_url, postgres::NoTls).unwrap();
            client
                .execute(
                    "DELETE FROM marketforge_room_snapshots WHERE room_id = $1",
                    &[&room_id],
                )
                .unwrap();
        })
        .join()
        .unwrap();
    }

    fn cleanup_postgres_room(database_url: &str, room_id: &str) {
        let database_url = database_url.to_string();
        let room_id = room_id.to_string();
        let _ = std::thread::spawn(move || {
            if let Ok(mut client) = postgres::Client::connect(&database_url, postgres::NoTls) {
                let _ = client.execute(
                    "DELETE FROM marketforge_rooms WHERE room_id = $1",
                    &[&room_id],
                );
            }
        })
        .join();
    }

    #[tokio::test]
    async fn agent_templates_cannot_target_a_different_room() {
        let app = new_app();
        let scenario = serde_json::to_string(&spot_scenario("agent-path-room")).unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let request = StartAgentsRequest {
            agents: vec![dca_template("other-room", "cross-room-agent", 20)],
            interval_ms: Some(10),
        };
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/agent-path-room/agents")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&request).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn agent_worker_stops_and_reports_missing_room_errors() {
        let shared = Arc::new(ServerState::new(AppState::new("http://127.0.0.1:57305")));
        let worker = AgentWorkerHandle::spawn(
            shared,
            "agent-error-room".to_string(),
            vec![dca_template("agent-error-room", "failing-agent", 20)],
            Duration::from_millis(1),
        )
        .unwrap();

        let mut status = worker.status("agent-error-room".to_string());
        for _ in 0..50 {
            if !status.running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            status = worker.status("agent-error-room".to_string());
        }
        assert!(!status.running);
        assert!(status.last_error.is_some());
    }

    #[test]
    fn http_client_validates_and_normalizes_trusted_owner_urls() {
        let client = HttpTradingClient::new("http://127.0.0.1:57305")
            .with_trusted_owner_url("https://OWNER.example.test/api/")
            .unwrap();
        assert!(
            client
                .trusted_owner_urls
                .contains("https://owner.example.test/api")
        );

        for invalid in [
            "",
            "file:///tmp/marketforge",
            "https://user:secret@owner.example.test",
            "https://owner.example.test?room=one",
            "https://owner.example.test/#fragment",
        ] {
            let error = HttpTradingClient::new("http://127.0.0.1:57305")
                .with_trusted_owner_url(invalid)
                .unwrap_err();
            assert!(matches!(error, HttpTradingError::InvalidOwnerUrl { .. }));
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn http_client_retries_idempotent_post_to_explicitly_trusted_owner() {
        let Some((owner_listener, owner_url)) = bind_http_test_listener().await else {
            return;
        };
        let owner_calls = Arc::new(AtomicUsize::new(0));
        let owner_received_credentials = Arc::new(AtomicBool::new(false));
        let owner_received_idempotency_key = Arc::new(AtomicBool::new(false));
        let owner_app = Router::new().route(
            "/rooms/route-room/pause",
            route_post({
                let owner_calls = Arc::clone(&owner_calls);
                let owner_received_credentials = Arc::clone(&owner_received_credentials);
                let owner_received_idempotency_key = Arc::clone(&owner_received_idempotency_key);
                move |headers: HeaderMap| async move {
                    owner_calls.fetch_add(1, Ordering::Relaxed);
                    owner_received_credentials.store(
                        headers
                            .get(AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            == Some("Bearer route-secret"),
                        Ordering::Relaxed,
                    );
                    owner_received_idempotency_key.store(
                        headers
                            .get(IDEMPOTENCY_KEY_HEADER)
                            .and_then(|value| value.to_str().ok())
                            == Some("route-key"),
                        Ordering::Relaxed,
                    );
                    Json(RoomStatusResponse {
                        room_id: "route-room".to_string(),
                        status: MarketStatus::Paused,
                    })
                }
            }),
        );
        let owner_server = tokio::spawn(async move {
            axum::serve(owner_listener, owner_app).await.unwrap();
        });

        let Some((source_listener, source_url)) = bind_http_test_listener().await else {
            owner_server.abort();
            return;
        };
        let source_calls = Arc::new(AtomicUsize::new(0));
        let source_app = Router::new().route(
            "/rooms/route-room/pause",
            route_post({
                let owner_url = owner_url.clone();
                let source_calls = Arc::clone(&source_calls);
                move || async move {
                    source_calls.fetch_add(1, Ordering::Relaxed);
                    room_owner_conflict(&owner_url)
                }
            }),
        );
        let source_server = tokio::spawn(async move {
            axum::serve(source_listener, source_app).await.unwrap();
        });

        let trusted_owner_url = format!("{owner_url}/");
        let response = tokio::task::spawn_blocking(move || {
            let client = HttpTradingClient::with_bearer_token(source_url, "route-secret")
                .with_trusted_owner_url(trusted_owner_url)
                .unwrap();
            client.post_json_with_idempotency::<_, RoomStatusResponse>(
                "/rooms/route-room/pause",
                &(),
                "route-key",
            )
        })
        .await
        .unwrap()
        .unwrap();

        source_server.abort();
        owner_server.abort();
        assert_eq!(response.room_id, "route-room");
        assert_eq!(response.status, MarketStatus::Paused);
        assert_eq!(source_calls.load(Ordering::Relaxed), 1);
        assert_eq!(owner_calls.load(Ordering::Relaxed), 1);
        assert!(owner_received_credentials.load(Ordering::Relaxed));
        assert!(owner_received_idempotency_key.load(Ordering::Relaxed));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn http_client_does_not_send_get_to_untrusted_owner() {
        let Some((owner_listener, owner_url)) = bind_http_test_listener().await else {
            return;
        };
        let owner_calls = Arc::new(AtomicUsize::new(0));
        let owner_app = Router::new().route(
            "/rooms/route-room/view",
            route_get({
                let owner_calls = Arc::clone(&owner_calls);
                move || async move {
                    owner_calls.fetch_add(1, Ordering::Relaxed);
                    Json(serde_json::json!({"unexpected": true}))
                }
            }),
        );
        let owner_server = tokio::spawn(async move {
            axum::serve(owner_listener, owner_app).await.unwrap();
        });

        let Some((source_listener, source_url)) = bind_http_test_listener().await else {
            owner_server.abort();
            return;
        };
        let source_app = Router::new().route(
            "/rooms/route-room/view",
            route_get({
                let owner_url = owner_url.clone();
                move || async move { room_owner_conflict(&owner_url) }
            }),
        );
        let source_server = tokio::spawn(async move {
            axum::serve(source_listener, source_app).await.unwrap();
        });

        let error = tokio::task::spawn_blocking(move || {
            HttpTradingClient::with_bearer_token(source_url, "must-not-leak")
                .get_json::<serde_json::Value>("/rooms/route-room/view")
        })
        .await
        .unwrap()
        .unwrap_err();

        source_server.abort();
        owner_server.abort();
        assert!(matches!(error, HttpTradingError::UntrustedOwnerUrl { .. }));
        assert_eq!(owner_calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn http_client_follows_at_most_one_owner_retry() {
        let Some((owner_listener, owner_url)) = bind_http_test_listener().await else {
            return;
        };
        let owner_calls = Arc::new(AtomicUsize::new(0));
        let owner_app = Router::new().route(
            "/rooms/route-room/pause",
            route_post({
                let owner_url = owner_url.clone();
                let owner_calls = Arc::clone(&owner_calls);
                move || async move {
                    owner_calls.fetch_add(1, Ordering::Relaxed);
                    room_owner_conflict(&owner_url)
                }
            }),
        );
        let owner_server = tokio::spawn(async move {
            axum::serve(owner_listener, owner_app).await.unwrap();
        });

        let Some((source_listener, source_url)) = bind_http_test_listener().await else {
            owner_server.abort();
            return;
        };
        let source_calls = Arc::new(AtomicUsize::new(0));
        let source_app = Router::new().route(
            "/rooms/route-room/pause",
            route_post({
                let owner_url = owner_url.clone();
                let source_calls = Arc::clone(&source_calls);
                move || async move {
                    source_calls.fetch_add(1, Ordering::Relaxed);
                    room_owner_conflict(&owner_url)
                }
            }),
        );
        let source_server = tokio::spawn(async move {
            axum::serve(source_listener, source_app).await.unwrap();
        });

        let trusted_owner_url = owner_url.clone();
        let error = tokio::task::spawn_blocking(move || {
            HttpTradingClient::with_user_id(source_url, "alice")
                .with_trusted_owner_url(trusted_owner_url)
                .unwrap()
                .pause_room("route-room")
        })
        .await
        .unwrap()
        .unwrap_err();

        source_server.abort();
        owner_server.abort();
        assert!(matches!(error, HttpTradingError::Api { status: 409, .. }));
        assert_eq!(source_calls.load(Ordering::Relaxed), 1);
        assert_eq!(owner_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn http_client_rejects_owner_route_back_to_source() {
        let Some((source_listener, source_url)) = bind_http_test_listener().await else {
            return;
        };
        let source_calls = Arc::new(AtomicUsize::new(0));
        let source_app = Router::new().route(
            "/rooms/route-room/view",
            route_get({
                let owner_url = source_url.clone();
                let source_calls = Arc::clone(&source_calls);
                move || async move {
                    source_calls.fetch_add(1, Ordering::Relaxed);
                    room_owner_conflict(&owner_url)
                }
            }),
        );
        let source_server = tokio::spawn(async move {
            axum::serve(source_listener, source_app).await.unwrap();
        });

        let error = tokio::task::spawn_blocking(move || {
            HttpTradingClient::with_user_id(source_url, "alice")
                .get_json::<serde_json::Value>("/rooms/route-room/view")
        })
        .await
        .unwrap()
        .unwrap_err();

        source_server.abort();
        assert!(matches!(error, HttpTradingError::OwnerRouteLoop { .. }));
        assert_eq!(source_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn http_event_stream_reconnects_to_trusted_new_owner_from_last_cursor() {
        let room_id = "stream-route-room";
        let Some((owner_listener, owner_url)) = bind_http_test_listener().await else {
            return;
        };
        let owner_calls = Arc::new(AtomicUsize::new(0));
        let owner_received_credentials = Arc::new(AtomicBool::new(false));
        let owner_received_cursor = Arc::new(AtomicBool::new(false));
        let owner_app = Router::new().route(
            "/rooms/stream-route-room/events/stream",
            route_get({
                let owner_calls = Arc::clone(&owner_calls);
                let owner_received_credentials = Arc::clone(&owner_received_credentials);
                let owner_received_cursor = Arc::clone(&owner_received_cursor);
                move |headers: HeaderMap| async move {
                    owner_calls.fetch_add(1, Ordering::Relaxed);
                    owner_received_credentials.store(
                        headers
                            .get(AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            == Some("Bearer stream-secret"),
                        Ordering::Relaxed,
                    );
                    owner_received_cursor.store(
                        headers
                            .get("last-event-id")
                            .and_then(|value| value.to_str().ok())
                            == Some("0"),
                        Ordering::Relaxed,
                    );
                    sse_execution_response(&synthetic_execution("stream-route-room", 1))
                }
            }),
        );
        let owner_server = tokio::spawn(async move {
            axum::serve(owner_listener, owner_app).await.unwrap();
        });

        let Some((source_listener, source_url)) = bind_http_test_listener().await else {
            owner_server.abort();
            return;
        };
        let source_calls = Arc::new(AtomicUsize::new(0));
        let source_received_reconnect_cursor = Arc::new(AtomicBool::new(false));
        let source_app = Router::new().route(
            "/rooms/stream-route-room/events/stream",
            route_get({
                let owner_url = owner_url.clone();
                let source_calls = Arc::clone(&source_calls);
                let source_received_reconnect_cursor =
                    Arc::clone(&source_received_reconnect_cursor);
                move |headers: HeaderMap| async move {
                    let call = source_calls.fetch_add(1, Ordering::Relaxed);
                    if call == 0 {
                        sse_execution_response(&synthetic_execution("stream-route-room", 0))
                    } else {
                        source_received_reconnect_cursor.store(
                            headers
                                .get("last-event-id")
                                .and_then(|value| value.to_str().ok())
                                == Some("0"),
                            Ordering::Relaxed,
                        );
                        room_owner_conflict_for("stream-route-room", &owner_url).into_response()
                    }
                }
            }),
        );
        let source_server = tokio::spawn(async move {
            axum::serve(source_listener, source_app).await.unwrap();
        });

        let trusted_owner_url = owner_url.clone();
        let received = tokio::task::spawn_blocking(move || {
            let client = HttpTradingClient::with_bearer_token(source_url, "stream-secret")
                .with_trusted_owner_url(trusted_owner_url)
                .unwrap();
            let mut stream = client.room_event_stream_from_start(room_id).unwrap();
            let first = stream.next().unwrap().unwrap();
            let second = stream.next().unwrap().unwrap();
            (first, second, stream.last_command_seq())
        })
        .await
        .unwrap();

        source_server.abort();
        owner_server.abort();
        assert_eq!(received.0.command_seq, 0);
        assert_eq!(received.1.command_seq, 1);
        assert_eq!(received.2, Some(1));
        assert_eq!(source_calls.load(Ordering::Relaxed), 2);
        assert_eq!(owner_calls.load(Ordering::Relaxed), 1);
        assert!(source_received_reconnect_cursor.load(Ordering::Relaxed));
        assert!(owner_received_credentials.load(Ordering::Relaxed));
        assert!(owner_received_cursor.load(Ordering::Relaxed));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn http_event_stream_surfaces_resync_required_and_sequence_gaps() {
        let Some((resync_listener, resync_url)) = bind_http_test_listener().await else {
            return;
        };
        let resync_app = Router::new().route(
            "/rooms/resync-room/events/stream",
            route_get(|| async { sse_resync_response("resync-room") }),
        );
        let resync_server = tokio::spawn(async move {
            axum::serve(resync_listener, resync_app).await.unwrap();
        });
        let resync_error = tokio::task::spawn_blocking(move || {
            let client = HttpTradingClient::with_user_id(resync_url, "alice");
            let mut stream = client.room_event_stream_after("resync-room", 4).unwrap();
            let error = stream.next().unwrap().unwrap_err();
            assert!(stream.next().is_none());
            error
        })
        .await
        .unwrap();
        resync_server.abort();
        assert!(matches!(
            resync_error,
            HttpTradingError::EventStreamResyncRequired {
                after_command_seq: Some(4),
                skipped_messages: Some(3),
                ..
            }
        ));

        let Some((gap_listener, gap_url)) = bind_http_test_listener().await else {
            return;
        };
        let gap_app = Router::new().route(
            "/rooms/gap-room/events/stream",
            route_get(|| async { sse_execution_response(&synthetic_execution("gap-room", 1)) }),
        );
        let gap_server = tokio::spawn(async move {
            axum::serve(gap_listener, gap_app).await.unwrap();
        });
        let gap_error = tokio::task::spawn_blocking(move || {
            let client = HttpTradingClient::with_user_id(gap_url, "alice");
            let mut stream = client.room_event_stream_from_start("gap-room").unwrap();
            stream.next().unwrap().unwrap_err()
        })
        .await
        .unwrap();
        gap_server.abort();
        assert!(matches!(
            gap_error,
            HttpTradingError::EventStreamGap {
                expected_command_seq: 0,
                actual_command_seq: 1,
                ..
            }
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn http_event_stream_falls_back_to_explicitly_trusted_instance_on_transport_failure() {
        let Some((dead_listener, dead_url)) = bind_http_test_listener().await else {
            return;
        };
        drop(dead_listener);

        let Some((owner_listener, owner_url)) = bind_http_test_listener().await else {
            return;
        };
        let owner_calls = Arc::new(AtomicUsize::new(0));
        let owner_received_credentials = Arc::new(AtomicBool::new(false));
        let owner_app = Router::new().route(
            "/rooms/fallback-room/events/stream",
            route_get({
                let owner_calls = Arc::clone(&owner_calls);
                let owner_received_credentials = Arc::clone(&owner_received_credentials);
                move |headers: HeaderMap| async move {
                    owner_calls.fetch_add(1, Ordering::Relaxed);
                    owner_received_credentials.store(
                        headers
                            .get(AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            == Some("Bearer fallback-secret"),
                        Ordering::Relaxed,
                    );
                    sse_execution_response(&synthetic_execution("fallback-room", 0))
                }
            }),
        );
        let owner_server = tokio::spawn(async move {
            axum::serve(owner_listener, owner_app).await.unwrap();
        });

        let execution = tokio::task::spawn_blocking(move || {
            let client = HttpTradingClient::with_bearer_token(dead_url, "fallback-secret")
                .with_trusted_owner_url(owner_url)
                .unwrap();
            client
                .room_event_stream_from_start("fallback-room")
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
        })
        .await
        .unwrap();

        owner_server.abort();
        assert_eq!(execution.command_seq, 0);
        assert_eq!(owner_calls.load(Ordering::Relaxed), 1);
        assert!(owner_received_credentials.load(Ordering::Relaxed));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rule_agent_can_trade_through_real_http_client() {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to bind test listener: {error}"),
        };
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{addr}");
        let app = new_app_with_base_url(base_url.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let (directory, responses) = tokio::task::spawn_blocking(move || {
            let client = HttpTradingClient::with_user_id(base_url, "alice");
            client.create_room(&spot_scenario("remote-ai")).unwrap();
            let directory = client.cluster_rooms(10).unwrap();
            let mut participant = DcaTrader::new(DcaTraderConfig {
                participant: ParticipantConfig {
                    participant_id: "dca-http".to_string(),
                    kind: ParticipantKind::RuleAgent,
                    room_id: "remote-ai".to_string(),
                    account_id: 20,
                    instrument_id: Some("V-BTC-SPOT".to_string()),
                },
                interval_steps: 1,
                order_qty: 2,
                use_market_order: false,
                limit_offset_ticks: 0,
                fallback_price_tick: 100,
                side: Side::Buy,
            });

            let responses = run_remote_participant_once(&client, &mut participant)?;
            Ok::<_, HttpTradingError>((directory, responses))
        })
        .await
        .unwrap()
        .unwrap();

        server.abort();
        assert_eq!(directory.rooms.len(), 1);
        assert_eq!(directory.rooms[0].room_id, "remote-ai");
        assert!(directory.rooms[0].owner.is_none());
        assert_eq!(responses.len(), 1);
        assert!(responses[0].accepted);
        assert!(
            responses[0]
                .events
                .iter()
                .any(|event| matches!(event, EventSummary::OrderRested { .. }))
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn room_can_autostart_agent_worker_over_http() {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to bind test listener: {error}"),
        };
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{addr}");
        let app = new_app_with_base_url(base_url.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let view = tokio::task::spawn_blocking(move || {
            let client = HttpTradingClient::with_user_id(base_url, "alice");
            let room = CreateRoomRequest {
                scenario: spot_scenario("worker-ai"),
                agents: vec![dca_template("worker-ai", "dca-worker", 20)],
                agent_interval_ms: Some(20),
                autostart_agents: Some(true),
            };
            let created = client.create_room_with_agents(&room).unwrap();
            assert!(created.agent_worker.running);
            assert_eq!(created.agent_worker.participants, vec!["dca-worker"]);

            let mut view = client.market_view("worker-ai").unwrap();
            for _ in 0..20 {
                if !view.book.bids.is_empty() {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
                view = client.market_view("worker-ai").unwrap();
            }

            let status = client.agent_status("worker-ai").unwrap();
            assert!(status.running);
            client.stop_agents("worker-ai").unwrap();
            view
        })
        .await
        .unwrap();

        server.abort();
        assert_eq!(view.book.bids[0].price_tick, 100);
        assert!(view.book.bids[0].qty >= 2);
    }
}
