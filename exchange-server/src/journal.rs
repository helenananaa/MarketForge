use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
    env,
    error::Error,
    fmt, thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use exchange_core::{
    ActorExecution, Command, MarketStatus, RoomBootstrap, ScenarioConfig, SimulationRoom,
    VenueToVenueTransfer,
    model::{AccountId, OrderKind, Side},
    transfer::{VenueTransfer, VenueTransferKind, VenueTransferRejectReason, VenueTransferStatus},
};
use postgres::{Client, NoTls};
use serde::{Deserialize, Serialize, de::Error as _};
use serde_json::Value;

use crate::{ClearingEventSummary, EventSummary, RoomExecutionSummary};

const DATABASE_URL_ENV: &str = "MARKETFORGE_DATABASE_URL";
const RUNTIME_MODE_ENV: &str = "MARKETFORGE_RUNTIME_MODE";
pub const JOURNAL_READ_WORKERS_ENV: &str = "MARKETFORGE_JOURNAL_READ_WORKERS";
pub const DEFAULT_POSTGRES_READ_WORKERS: usize = 4;
const MAX_POSTGRES_READ_WORKERS: usize = 32;
pub const RUNTIME_LOCK_WAIT_MS_ENV: &str = "MARKETFORGE_RUNTIME_LOCK_WAIT_MS";
const MAX_RUNTIME_LOCK_WAIT_MS: u64 = 3_600_000;
const RUNTIME_LOCK_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Upper bound for one room-writer lease grant or renewal.
pub const MAX_ROOM_LEASE_DURATION_MS: u64 = 300_000;
const POSTGRES_RUNTIME_LOCK_NAMESPACE: i32 = i32::from_be_bytes(*b"MKTF");
const POSTGRES_RUNTIME_LOCK_ID: i32 = i32::from_be_bytes(*b"RUN1");
const POSTGRES_MIGRATION_LOCK_ID: i32 = i32::from_be_bytes(*b"MIGR");
const MIGRATIONS: &[SchemaMigration] = &[
    SchemaMigration {
        version: 1,
        name: "initial_schema",
        sql: include_str!("../migrations/0001_initial_schema.sql"),
    },
    SchemaMigration {
        version: 2,
        name: "access_control",
        sql: include_str!("../migrations/0002_access_control.sql"),
    },
    SchemaMigration {
        version: 3,
        name: "instrument_projection_scope",
        sql: include_str!("../migrations/0003_instrument_projection_scope.sql"),
    },
    SchemaMigration {
        version: 4,
        name: "transfer_journal",
        sql: include_str!("../migrations/0004_transfer_journal.sql"),
    },
    SchemaMigration {
        version: 5,
        name: "margin_projection_fields",
        sql: include_str!("../migrations/0005_margin_projection_fields.sql"),
    },
    SchemaMigration {
        version: 6,
        name: "claim_unowned_legacy_rooms",
        sql: include_str!("../migrations/0006_claim_unowned_legacy_rooms.sql"),
    },
    SchemaMigration {
        version: 7,
        name: "room_mutation_journal",
        sql: include_str!("../migrations/0007_room_mutation_journal.sql"),
    },
    SchemaMigration {
        version: 8,
        name: "portfolio_margin_projection_fields",
        sql: include_str!("../migrations/0008_portfolio_margin_projection_fields.sql"),
    },
    SchemaMigration {
        version: 9,
        name: "authoritative_market_time",
        sql: include_str!("../migrations/0009_authoritative_market_time.sql"),
    },
    SchemaMigration {
        version: 10,
        name: "order_request_idempotency",
        sql: include_str!("../migrations/0010_order_request_idempotency.sql"),
    },
    SchemaMigration {
        version: 11,
        name: "room_writer_leases",
        sql: include_str!("../migrations/0011_room_writer_leases.sql"),
    },
    SchemaMigration {
        version: 12,
        name: "room_writer_owner_url",
        sql: include_str!("../migrations/0012_room_writer_owner_url.sql"),
    },
    SchemaMigration {
        version: 13,
        name: "scheduler_and_control_idempotency",
        sql: include_str!("../migrations/0013_scheduler_and_control_idempotency.sql"),
    },
];

pub const ROOM_MUTATION_SCHEMA_VERSION: u16 = 1;

fn is_admin_role(role: &str) -> bool {
    matches!(role, "owner" | "admin")
}

fn is_account_holder_role(role: &str) -> bool {
    matches!(role, "instructor" | "trader")
}

fn is_known_member_role(role: &str) -> bool {
    matches!(
        role,
        "owner" | "admin" | "instructor" | "trader" | "spectator"
    )
}

/// Identity and monotonic token that a room writer must present on every write.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RoomLeaseClaim {
    pub room_id: String,
    pub owner_id: String,
    pub fencing_token: u64,
}

/// A granted room-writer lease and its database-clock expiration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RoomWriterLease {
    pub claim: RoomLeaseClaim,
    pub owner_url: Option<String>,
    pub expires_at_unix_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoomRoutingRecord {
    pub room_id: String,
    pub owner: Option<RoomWriterLease>,
}

pub trait JournalStore: Send {
    fn load_recovery(&mut self) -> Result<JournalRecovery, JournalError>;

    fn load_room_recovery(&mut self, room_id: &str) -> Result<JournalRecovery, JournalError> {
        let mut recovery = self.load_recovery()?;
        recovery.rooms.retain(|room| room.room_id == room_id);
        recovery
            .executions
            .retain(|execution| execution.room_id == room_id);
        recovery
            .mutations
            .retain(|mutation| mutation.room_id == room_id);
        recovery
            .snapshots
            .retain(|snapshot| snapshot.room_id == room_id);
        Ok(recovery)
    }

    fn health_check(&mut self) -> Result<(), JournalError> {
        Ok(())
    }

    fn acquire_room_writer_lease(
        &mut self,
        _room_id: &str,
        _owner_id: &str,
        _owner_url: Option<&str>,
        _duration: Duration,
    ) -> Result<Option<RoomWriterLease>, JournalError> {
        Err(JournalError::UnsupportedOperation(
            "acquire_room_writer_lease",
        ))
    }

    fn current_room_writer_lease(
        &mut self,
        _room_id: &str,
    ) -> Result<Option<RoomWriterLease>, JournalError> {
        Err(JournalError::UnsupportedOperation(
            "current_room_writer_lease",
        ))
    }

    fn query_room_routes(
        &mut self,
        _user_id: &str,
        _after_room_id: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<RoomRoutingRecord>, JournalError> {
        Err(JournalError::UnsupportedOperation("query_room_routes"))
    }

    fn renew_room_writer_lease(
        &mut self,
        _claim: &RoomLeaseClaim,
        _duration: Duration,
    ) -> Result<Option<RoomWriterLease>, JournalError> {
        Err(JournalError::UnsupportedOperation(
            "renew_room_writer_lease",
        ))
    }

    fn release_room_writer_lease(&mut self, _claim: &RoomLeaseClaim) -> Result<bool, JournalError> {
        Err(JournalError::UnsupportedOperation(
            "release_room_writer_lease",
        ))
    }

    fn find_idempotent_execution(
        &mut self,
        _user_id: &str,
        _room_id: &str,
        _idempotency_key: &str,
    ) -> Result<Option<JournalExecution>, JournalError> {
        Ok(None)
    }

    fn query_executions(
        &mut self,
        _room_id: &str,
        _after_command_seq: Option<u64>,
        _from_start: bool,
        _limit: usize,
    ) -> Result<ExecutionPage, JournalError> {
        Err(JournalError::UnsupportedOperation("query_executions"))
    }

    fn create_room(
        &mut self,
        owner_user_id: &str,
        scenario: &ScenarioConfig,
        bootstrap: &RoomBootstrap,
        account_ids: &[AccountId],
        seed_records: &[JournalExecution],
        initial_snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError>;

    #[allow(clippy::too_many_arguments)]
    fn create_room_with_writer_lease(
        &mut self,
        _owner_user_id: &str,
        _scenario: &ScenarioConfig,
        _bootstrap: &RoomBootstrap,
        _account_ids: &[AccountId],
        _seed_records: &[JournalExecution],
        _initial_snapshot: Option<&JournalSnapshot>,
        _writer_owner_id: &str,
        _writer_owner_url: Option<&str>,
        _lease_duration: Duration,
    ) -> Result<RoomWriterLease, JournalError> {
        Err(JournalError::UnsupportedOperation(
            "create_room_with_writer_lease",
        ))
    }

    fn append_executions(
        &mut self,
        records: &[JournalExecution],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError>;

    fn append_execution(
        &mut self,
        record: &JournalExecution,
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        self.append_executions(std::slice::from_ref(record), snapshot)
    }

    fn append_executions_fenced(
        &mut self,
        _claim: &RoomLeaseClaim,
        _records: &[JournalExecution],
        _snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        Err(JournalError::UnsupportedOperation(
            "append_executions_fenced",
        ))
    }

    fn append_transfers(
        &mut self,
        _records: &[JournalTransfer],
        _snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        Ok(())
    }

    fn append_snapshot(&mut self, _snapshot: &JournalSnapshot) -> Result<(), JournalError> {
        Ok(())
    }

    fn append_room_mutation(
        &mut self,
        mutation: &PendingJournalMutation,
        execution_records: &[JournalExecution],
        transfer_records: &[JournalTransfer],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        validate_pending_mutation(mutation)?;
        validate_mutation_execution_records(mutation, execution_records)?;
        if !execution_records.is_empty() {
            self.append_executions(execution_records, None)?;
        }
        if transfer_records.is_empty() {
            if let Some(snapshot) = snapshot {
                self.append_snapshot(snapshot)?;
            }
        } else {
            self.append_transfers(transfer_records, snapshot)?;
        }
        if let RoomMutation::StatusChanged { status } = &mutation.mutation {
            self.update_room_status(&mutation.room_id, *status)?;
        }
        Ok(())
    }

    fn append_room_mutation_fenced(
        &mut self,
        _claim: &RoomLeaseClaim,
        _mutation: &PendingJournalMutation,
        _execution_records: &[JournalExecution],
        _transfer_records: &[JournalTransfer],
        _snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        Err(JournalError::UnsupportedOperation(
            "append_room_mutation_fenced",
        ))
    }

    fn update_room_status(
        &mut self,
        room_id: &str,
        status: MarketStatus,
    ) -> Result<(), JournalError>;

    fn upsert_room_member(
        &mut self,
        _room_id: &str,
        _user_id: &str,
        _role: &str,
    ) -> Result<(), JournalError> {
        Err(JournalError::UnsupportedOperation("upsert_room_member"))
    }

    fn remove_room_member(&mut self, _room_id: &str, _user_id: &str) -> Result<(), JournalError> {
        Err(JournalError::UnsupportedOperation("remove_room_member"))
    }

    fn assign_account_owner(
        &mut self,
        _room_id: &str,
        _account_id: AccountId,
        _user_id: &str,
    ) -> Result<(), JournalError> {
        Err(JournalError::UnsupportedOperation("assign_account_owner"))
    }

    fn user_room_role(
        &mut self,
        _user_id: &str,
        _room_id: &str,
    ) -> Result<Option<String>, JournalError> {
        Ok(None)
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
        Ok(false)
    }

    fn user_can_access_account(
        &mut self,
        _user_id: &str,
        _room_id: &str,
        _account_id: AccountId,
    ) -> Result<bool, JournalError> {
        Ok(true)
    }

    fn query_orders(
        &mut self,
        _user_id: &str,
        _room_id: &str,
        _instrument_id: Option<&str>,
        _account_id: Option<AccountId>,
        _limit: usize,
    ) -> Result<Vec<OrderProjection>, JournalError> {
        Ok(Vec::new())
    }

    fn query_trades(
        &mut self,
        _user_id: &str,
        _room_id: &str,
        _instrument_id: Option<&str>,
        _account_id: Option<AccountId>,
        _limit: usize,
    ) -> Result<Vec<TradeProjection>, JournalError> {
        Ok(Vec::new())
    }

    fn query_market_ticks(
        &mut self,
        _user_id: &str,
        _room_id: &str,
        _instrument_id: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<MarketTickProjection>, JournalError> {
        Ok(Vec::new())
    }

    fn query_account_ledger(
        &mut self,
        _user_id: &str,
        _room_id: &str,
        _instrument_id: Option<&str>,
        _account_id: Option<AccountId>,
        _limit: usize,
    ) -> Result<Vec<AccountLedgerProjection>, JournalError> {
        Ok(Vec::new())
    }

    fn query_position_snapshots(
        &mut self,
        _user_id: &str,
        _room_id: &str,
        _instrument_id: Option<&str>,
        _account_id: Option<AccountId>,
        _limit: usize,
    ) -> Result<Vec<PositionSnapshotProjection>, JournalError> {
        Ok(Vec::new())
    }

    fn query_transfers(
        &mut self,
        _user_id: &str,
        _room_id: &str,
        _account_id: Option<AccountId>,
        _limit: usize,
    ) -> Result<Vec<VenueTransfer>, JournalError> {
        Ok(Vec::new())
    }
}

pub fn journal_store_from_env() -> Result<Box<dyn JournalStore>, JournalError> {
    let runtime_lock_wait = runtime_lock_wait_from_env()?;
    let room_leased = room_leased_runtime_from_env()?;
    if room_leased && !runtime_lock_wait.is_zero() {
        return Err(JournalError::Recovery(format!(
            "{RUNTIME_LOCK_WAIT_MS_ENV} must be zero when {RUNTIME_MODE_ENV}=room-leased"
        )));
    }
    match env::var(DATABASE_URL_ENV) {
        Ok(url) if !url.trim().is_empty() => {
            let store = if room_leased {
                PostgresJournalStore::connect_migrated_shared(&url)?
            } else {
                PostgresJournalStore::connect_migrated_exclusive_with_wait(&url, runtime_lock_wait)?
            };
            Ok(Box::new(store))
        }
        Ok(_) | Err(env::VarError::NotPresent) if runtime_lock_wait.is_zero() => {
            Ok(Box::new(InMemoryJournalStore::new()))
        }
        Ok(_) | Err(env::VarError::NotPresent) => Err(JournalError::Recovery(format!(
            "{RUNTIME_LOCK_WAIT_MS_ENV} requires a non-empty {DATABASE_URL_ENV}"
        ))),
        Err(error) => Err(JournalError::Recovery(format!(
            "invalid {DATABASE_URL_ENV}: {error}"
        ))),
    }
}

pub(crate) struct JournalStoreBundle {
    pub(crate) writer: Box<dyn JournalStore>,
    pub(crate) readers: Vec<Box<dyn JournalStore>>,
}

impl JournalStoreBundle {
    pub(crate) fn single(writer: Box<dyn JournalStore>) -> Self {
        Self {
            writer,
            readers: Vec::new(),
        }
    }
}

pub(crate) fn journal_stores_from_env() -> Result<JournalStoreBundle, JournalError> {
    let runtime_lock_wait = runtime_lock_wait_from_env()?;
    let room_leased = room_leased_runtime_from_env()?;
    if room_leased && !runtime_lock_wait.is_zero() {
        return Err(JournalError::Recovery(format!(
            "{RUNTIME_LOCK_WAIT_MS_ENV} must be zero when {RUNTIME_MODE_ENV}=room-leased"
        )));
    }
    let database_url = match env::var(DATABASE_URL_ENV) {
        Ok(url) if !url.trim().is_empty() => Some(url),
        Ok(_) | Err(env::VarError::NotPresent) => None,
        Err(error) => {
            return Err(JournalError::Recovery(format!(
                "invalid {DATABASE_URL_ENV}: {error}"
            )));
        }
    };
    let configured_read_workers = match env::var(JOURNAL_READ_WORKERS_ENV) {
        Ok(value) => Some(parse_journal_read_workers(&value)?),
        Err(env::VarError::NotPresent) => None,
        Err(error) => {
            return Err(JournalError::Recovery(format!(
                "invalid {JOURNAL_READ_WORKERS_ENV}: {error}"
            )));
        }
    };

    match database_url {
        Some(url) => {
            let read_workers = configured_read_workers.unwrap_or(DEFAULT_POSTGRES_READ_WORKERS);
            let writer = if room_leased {
                Box::new(PostgresJournalStore::connect_migrated_shared(&url)?)
                    as Box<dyn JournalStore>
            } else {
                Box::new(PostgresJournalStore::connect_migrated_exclusive_with_wait(
                    &url,
                    runtime_lock_wait,
                )?) as Box<dyn JournalStore>
            };
            let readers = (0..read_workers)
                .map(|_| {
                    PostgresJournalStore::connect(&url)
                        .map(|store| Box::new(store) as Box<dyn JournalStore>)
                })
                .collect::<Result<Vec<_>, JournalError>>()?;
            Ok(JournalStoreBundle { writer, readers })
        }
        None => {
            if !runtime_lock_wait.is_zero() {
                return Err(JournalError::Recovery(format!(
                    "{RUNTIME_LOCK_WAIT_MS_ENV} requires a non-empty {DATABASE_URL_ENV}"
                )));
            }
            if configured_read_workers.is_some_and(|workers| workers != 0) {
                return Err(JournalError::Recovery(format!(
                    "{JOURNAL_READ_WORKERS_ENV} requires a non-empty {DATABASE_URL_ENV}"
                )));
            }
            Ok(JournalStoreBundle::single(Box::new(
                InMemoryJournalStore::new(),
            )))
        }
    }
}

fn parse_journal_read_workers(value: &str) -> Result<usize, JournalError> {
    let workers = value.parse::<usize>().map_err(|_| {
        JournalError::Recovery(format!(
            "invalid {JOURNAL_READ_WORKERS_ENV} value {value:?}; expected an integer from 0 to {MAX_POSTGRES_READ_WORKERS}"
        ))
    })?;
    if workers > MAX_POSTGRES_READ_WORKERS {
        return Err(JournalError::Recovery(format!(
            "invalid {JOURNAL_READ_WORKERS_ENV} value {workers}; maximum is {MAX_POSTGRES_READ_WORKERS}"
        )));
    }
    Ok(workers)
}

fn runtime_lock_wait_from_env() -> Result<Duration, JournalError> {
    match env::var(RUNTIME_LOCK_WAIT_MS_ENV) {
        Ok(value) => parse_runtime_lock_wait_ms(&value).map(Duration::from_millis),
        Err(env::VarError::NotPresent) => Ok(Duration::ZERO),
        Err(error) => Err(JournalError::Recovery(format!(
            "invalid {RUNTIME_LOCK_WAIT_MS_ENV}: {error}"
        ))),
    }
}

fn room_leased_runtime_from_env() -> Result<bool, JournalError> {
    match env::var(RUNTIME_MODE_ENV) {
        Ok(value) if value == "single-active" => Ok(false),
        Ok(value) if value == "room-leased" => Ok(true),
        Ok(value) => Err(JournalError::Recovery(format!(
            "invalid {RUNTIME_MODE_ENV} value {value:?}; expected single-active or room-leased"
        ))),
        Err(env::VarError::NotPresent) => Ok(false),
        Err(error) => Err(JournalError::Recovery(format!(
            "invalid {RUNTIME_MODE_ENV}: {error}"
        ))),
    }
}

fn parse_runtime_lock_wait_ms(value: &str) -> Result<u64, JournalError> {
    let wait_ms = value.parse::<u64>().map_err(|_| {
        JournalError::Recovery(format!(
            "invalid {RUNTIME_LOCK_WAIT_MS_ENV} value {value:?}; expected an integer from 0 to {MAX_RUNTIME_LOCK_WAIT_MS}"
        ))
    })?;
    if wait_ms > MAX_RUNTIME_LOCK_WAIT_MS {
        return Err(JournalError::Recovery(format!(
            "invalid {RUNTIME_LOCK_WAIT_MS_ENV} value {wait_ms}; maximum is {MAX_RUNTIME_LOCK_WAIT_MS}"
        )));
    }
    Ok(wait_ms)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct JournalExecution {
    pub room_id: String,
    pub command_seq: u64,
    pub participant_id: Option<String>,
    pub account_id: Option<AccountId>,
    #[serde(default)]
    pub request_user_id: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub request_fingerprint: Option<String>,
    pub command: Command,
    pub execution: RoomExecutionSummary,
}

impl JournalExecution {
    pub fn seed(command: Command, execution: ActorExecution) -> Self {
        let execution = RoomExecutionSummary::from_execution(execution);
        Self {
            room_id: execution.room_id.clone(),
            command_seq: execution.command_seq,
            participant_id: None,
            account_id: command_account_id(&command),
            request_user_id: None,
            idempotency_key: None,
            request_fingerprint: None,
            command,
            execution,
        }
    }

    pub fn submitted(
        participant_id: String,
        account_id: AccountId,
        command: Command,
        execution: ActorExecution,
    ) -> Self {
        let execution = RoomExecutionSummary::from_execution(execution);
        Self {
            room_id: execution.room_id.clone(),
            command_seq: execution.command_seq,
            participant_id: Some(participant_id),
            account_id: Some(account_id),
            request_user_id: None,
            idempotency_key: None,
            request_fingerprint: None,
            command,
            execution,
        }
    }

    pub fn system(command: Command, execution: ActorExecution) -> Self {
        let execution = RoomExecutionSummary::from_execution(execution);
        Self {
            room_id: execution.room_id.clone(),
            command_seq: execution.command_seq,
            participant_id: None,
            account_id: command_account_id(&command),
            request_user_id: None,
            idempotency_key: None,
            request_fingerprint: None,
            command,
            execution,
        }
    }

    pub fn with_idempotency(
        mut self,
        request_user_id: impl Into<String>,
        idempotency_key: impl Into<String>,
        request_fingerprint: impl Into<String>,
    ) -> Self {
        self.request_user_id = Some(request_user_id.into());
        self.idempotency_key = Some(idempotency_key.into());
        self.request_fingerprint = Some(request_fingerprint.into());
        self
    }

    fn command_json(&self) -> Result<Value, JournalError> {
        serde_json::to_value(&self.command).map_err(JournalError::Serialize)
    }

    fn execution_json(&self) -> Result<Value, JournalError> {
        serde_json::to_value(&self.execution).map_err(JournalError::Serialize)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct JournalTransfer {
    pub room_id: String,
    pub transfer: VenueTransfer,
}

impl JournalTransfer {
    pub fn recorded(room_id: impl Into<String>, transfer: VenueTransfer) -> Self {
        Self {
            room_id: room_id.into(),
            transfer,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PendingJournalMutation {
    pub room_id: String,
    /// Sequence number of the next command after this mutation's replay point.
    pub command_cursor: u64,
    pub mutation: RoomMutation,
}

impl PendingJournalMutation {
    pub fn new(room_id: impl Into<String>, command_cursor: u64, mutation: RoomMutation) -> Self {
        Self {
            room_id: room_id.into(),
            command_cursor,
            mutation,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct JournalMutation {
    pub room_id: String,
    pub mutation_seq: u64,
    /// Sequence number of the next command after this mutation's replay point.
    pub command_cursor: u64,
    pub schema_version: u16,
    pub mutation: RoomMutation,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoomMutation {
    StateCheckpoint {
        actor: Box<SimulationRoom>,
        #[serde(default)]
        complete_history: bool,
    },
    ClockAdvanced {
        steps: u64,
        completed_transfers: Vec<VenueTransfer>,
    },
    DepositSubmitted {
        venue_id: Option<String>,
        account_id: AccountId,
        asset_id: String,
        amount: i128,
        transfer: VenueTransfer,
    },
    WithdrawalSubmitted {
        venue_id: Option<String>,
        account_id: AccountId,
        asset_id: String,
        amount: i128,
        transfer: VenueTransfer,
    },
    VenueToVenueTransferSubmitted {
        from_venue_id: String,
        to_venue_id: String,
        account_id: AccountId,
        asset_id: String,
        amount: i128,
        transfer: VenueToVenueTransfer,
    },
    StatusChanged {
        status: MarketStatus,
    },
    SchedulerProgress {
        clock_steps: u64,
        state: exchange_core::SchedulerState,
    },
    TrainingProgress {
        run: Box<exchange_core::TrainingRun>,
    },
}

#[derive(Deserialize)]
struct StateCheckpointMutationPayload {
    actor: Box<SimulationRoom>,
    #[serde(default)]
    complete_history: bool,
}

#[derive(Deserialize)]
struct ClockAdvancedMutationPayload {
    steps: u64,
    completed_transfers: Vec<VenueTransfer>,
}

#[derive(Deserialize)]
struct TransferSubmittedMutationPayload {
    venue_id: Option<String>,
    account_id: AccountId,
    asset_id: String,
    amount: i128,
    transfer: VenueTransfer,
}

#[derive(Deserialize)]
struct VenueToVenueTransferSubmittedMutationPayload {
    from_venue_id: String,
    to_venue_id: String,
    account_id: AccountId,
    asset_id: String,
    amount: i128,
    transfer: VenueToVenueTransfer,
}

#[derive(Deserialize)]
struct StatusChangedMutationPayload {
    status: MarketStatus,
}

#[derive(Deserialize)]
struct SchedulerProgressMutationPayload {
    clock_steps: u64,
    state: exchange_core::SchedulerState,
}

#[derive(Deserialize)]
struct TrainingProgressMutationPayload {
    run: exchange_core::TrainingRun,
}

impl<'de> Deserialize<'de> for RoomMutation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let payload = Value::deserialize(deserializer)?;
        let kind = payload.get("kind").and_then(Value::as_str).ok_or_else(|| {
            D::Error::custom("room mutation payload is missing string field `kind`")
        })?;

        match kind {
            "state_checkpoint" => {
                let payload: StateCheckpointMutationPayload =
                    serde_json::from_value(payload).map_err(D::Error::custom)?;
                Ok(Self::StateCheckpoint {
                    actor: payload.actor,
                    complete_history: payload.complete_history,
                })
            }
            "clock_advanced" => {
                let payload: ClockAdvancedMutationPayload =
                    serde_json::from_value(payload).map_err(D::Error::custom)?;
                Ok(Self::ClockAdvanced {
                    steps: payload.steps,
                    completed_transfers: payload.completed_transfers,
                })
            }
            "deposit_submitted" => {
                let payload: TransferSubmittedMutationPayload =
                    serde_json::from_value(payload).map_err(D::Error::custom)?;
                Ok(Self::DepositSubmitted {
                    venue_id: payload.venue_id,
                    account_id: payload.account_id,
                    asset_id: payload.asset_id,
                    amount: payload.amount,
                    transfer: payload.transfer,
                })
            }
            "withdrawal_submitted" => {
                let payload: TransferSubmittedMutationPayload =
                    serde_json::from_value(payload).map_err(D::Error::custom)?;
                Ok(Self::WithdrawalSubmitted {
                    venue_id: payload.venue_id,
                    account_id: payload.account_id,
                    asset_id: payload.asset_id,
                    amount: payload.amount,
                    transfer: payload.transfer,
                })
            }
            "venue_to_venue_transfer_submitted" => {
                let payload: VenueToVenueTransferSubmittedMutationPayload =
                    serde_json::from_value(payload).map_err(D::Error::custom)?;
                Ok(Self::VenueToVenueTransferSubmitted {
                    from_venue_id: payload.from_venue_id,
                    to_venue_id: payload.to_venue_id,
                    account_id: payload.account_id,
                    asset_id: payload.asset_id,
                    amount: payload.amount,
                    transfer: payload.transfer,
                })
            }
            "status_changed" => {
                let payload: StatusChangedMutationPayload =
                    serde_json::from_value(payload).map_err(D::Error::custom)?;
                Ok(Self::StatusChanged {
                    status: payload.status,
                })
            }
            "scheduler_progress" => {
                let payload: SchedulerProgressMutationPayload =
                    serde_json::from_value(payload).map_err(D::Error::custom)?;
                Ok(Self::SchedulerProgress {
                    clock_steps: payload.clock_steps,
                    state: payload.state,
                })
            }
            "training_progress" => {
                let payload: TrainingProgressMutationPayload =
                    serde_json::from_value(payload).map_err(D::Error::custom)?;
                Ok(Self::TrainingProgress {
                    run: Box::new(payload.run),
                })
            }
            other => Err(D::Error::custom(format!(
                "unknown room mutation kind `{other}`"
            ))),
        }
    }
}

impl RoomMutation {
    fn kind_name(&self) -> &'static str {
        match self {
            Self::StateCheckpoint { .. } => "state_checkpoint",
            Self::ClockAdvanced { .. } => "clock_advanced",
            Self::DepositSubmitted { .. } => "deposit_submitted",
            Self::WithdrawalSubmitted { .. } => "withdrawal_submitted",
            Self::VenueToVenueTransferSubmitted { .. } => "venue_to_venue_transfer_submitted",
            Self::StatusChanged { .. } => "status_changed",
            Self::SchedulerProgress { .. } => "scheduler_progress",
            Self::TrainingProgress { .. } => "training_progress",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct JournalRecovery {
    pub rooms: Vec<JournalRoom>,
    pub executions: Vec<JournalExecution>,
    #[serde(default)]
    pub mutations: Vec<JournalMutation>,
    pub snapshots: Vec<JournalSnapshot>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ExecutionPage {
    pub executions: Vec<RoomExecutionSummary>,
    pub latest_command_seq: Option<u64>,
    pub has_more: bool,
}

fn execution_page_from_sorted(
    executions: &[RoomExecutionSummary],
    after_command_seq: Option<u64>,
    from_start: bool,
    limit: usize,
) -> ExecutionPage {
    let limit = limit.clamp(1, 500);
    let latest_command_seq = executions.last().map(|execution| execution.command_seq);
    let (executions, has_more) = if let Some(after_command_seq) = after_command_seq {
        let start =
            executions.partition_point(|execution| execution.command_seq <= after_command_seq);
        let end = start.saturating_add(limit).min(executions.len());
        (executions[start..end].to_vec(), end < executions.len())
    } else if from_start {
        let end = limit.min(executions.len());
        (executions[..end].to_vec(), end < executions.len())
    } else {
        let start = executions.len().saturating_sub(limit);
        (executions[start..].to_vec(), false)
    };
    ExecutionPage {
        executions,
        latest_command_seq,
        has_more,
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct JournalRoom {
    pub room_id: String,
    pub scenario: ScenarioConfig,
    pub status: MarketStatus,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct JournalSnapshot {
    pub room_id: String,
    pub command_seq: u64,
    pub actor: SimulationRoom,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OrderProjection {
    pub room_id: String,
    pub instrument_id: String,
    pub order_id: i64,
    pub account_id: i64,
    pub participant_id: Option<String>,
    pub side: String,
    pub order_type: String,
    pub limit_price_tick: Option<i64>,
    pub original_qty: i64,
    pub status: String,
    pub remaining_qty: i64,
    pub created_command_seq: i64,
    pub updated_command_seq: i64,
    #[serde(default)]
    pub created_market_time_ms: Option<i64>,
    #[serde(default)]
    pub updated_market_time_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TradeProjection {
    pub room_id: String,
    pub instrument_id: String,
    pub trade_id: i64,
    pub command_seq: i64,
    pub event_seq: i64,
    #[serde(default)]
    pub market_time_ms: Option<i64>,
    pub maker_order_id: i64,
    pub maker_account_id: i64,
    pub taker_order_id: i64,
    pub taker_account_id: i64,
    pub price_tick: i64,
    pub qty: i64,
    pub taker_side: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MarketTickProjection {
    pub room_id: String,
    pub instrument_id: String,
    pub command_seq: i64,
    pub event_seq: i64,
    #[serde(default)]
    pub market_time_ms: Option<i64>,
    pub trade_id: i64,
    pub price_tick: i64,
    pub qty: i64,
    pub taker_side: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AccountLedgerProjection {
    pub room_id: String,
    pub instrument_id: String,
    pub command_seq: i64,
    pub ledger_seq: i64,
    pub market_kind: String,
    pub account_id: i64,
    pub trade_id: i64,
    pub account_side: String,
    pub cash_delta: i64,
    pub position_delta: i64,
    pub fee: i64,
    pub realized_pnl: i64,
    pub price_tick: i64,
    pub qty: i64,
    pub notional: i64,
    pub cash_balance: i64,
    pub position_qty: i64,
    pub avg_entry_price_tick: Option<i64>,
    pub realized_pnl_total: Option<i64>,
    pub unrealized_pnl: Option<i64>,
    pub equity: Option<i64>,
    pub initial_margin: Option<i64>,
    pub maintenance_margin: Option<i64>,
    pub portfolio_initial_margin: Option<i64>,
    pub portfolio_maintenance_margin: Option<i64>,
    pub margin_status: Option<String>,
    pub fees_paid: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PositionSnapshotProjection {
    pub room_id: String,
    pub instrument_id: String,
    pub command_seq: i64,
    pub ledger_seq: i64,
    pub market_kind: String,
    pub account_id: i64,
    pub trade_id: i64,
    pub cash_balance: i64,
    pub position_qty: i64,
    pub avg_entry_price_tick: Option<i64>,
    pub realized_pnl: Option<i64>,
    pub unrealized_pnl: Option<i64>,
    pub equity: Option<i64>,
    pub initial_margin: Option<i64>,
    pub maintenance_margin: Option<i64>,
    pub portfolio_initial_margin: Option<i64>,
    pub portfolio_maintenance_margin: Option<i64>,
    pub margin_status: Option<String>,
    pub fees_paid: i64,
}

#[derive(Clone, Debug)]
struct InMemoryRoomWriterLease {
    owner_id: String,
    owner_url: Option<String>,
    fencing_token: u64,
    expires_at: SystemTime,
}

#[derive(Clone, Debug, Default)]
pub struct InMemoryJournalStore {
    rooms: Vec<StoredRoom>,
    executions: Vec<JournalExecution>,
    mutations: Vec<JournalMutation>,
    next_mutation_seq: u64,
    transfers: BTreeMap<(String, u64), VenueTransfer>,
    snapshots: Vec<JournalSnapshot>,
    room_members: BTreeMap<(String, String), String>,
    account_owners: BTreeSet<(String, AccountId, String)>,
    room_writer_leases: BTreeMap<String, InMemoryRoomWriterLease>,
}

impl InMemoryJournalStore {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub(crate) fn set_room_member_for_test(&mut self, room_id: &str, user_id: &str, role: &str) {
        self.room_members
            .insert((room_id.to_string(), user_id.to_string()), role.to_string());
    }

    #[cfg(test)]
    pub(crate) fn set_account_owner_for_test(
        &mut self,
        room_id: &str,
        account_id: AccountId,
        user_id: &str,
    ) {
        self.account_owners
            .insert((room_id.to_string(), account_id, user_id.to_string()));
    }

    fn user_is_room_admin(&self, user_id: &str, room_id: &str) -> bool {
        self.room_members
            .get(&(room_id.to_string(), user_id.to_string()))
            .is_some_and(|role| is_admin_role(role))
    }

    fn user_owns_account(&self, user_id: &str, room_id: &str, account_id: AccountId) -> bool {
        self.account_owners
            .contains(&(room_id.to_string(), account_id, user_id.to_string()))
    }

    fn user_may_access_account(&self, user_id: &str, room_id: &str, account_id: AccountId) -> bool {
        let Some(role) = self
            .room_members
            .get(&(room_id.to_string(), user_id.to_string()))
        else {
            return false;
        };
        if is_admin_role(role) {
            return true;
        }
        is_account_holder_role(role) && self.user_owns_account(user_id, room_id, account_id)
    }

    fn store_mutation(&mut self, mutation: &PendingJournalMutation) -> Result<(), JournalError> {
        validate_pending_mutation(mutation)?;
        let mutation_seq = self
            .next_mutation_seq
            .checked_add(1)
            .ok_or(JournalError::SequenceOutOfRange(u64::MAX))?;
        self.next_mutation_seq = mutation_seq;
        self.mutations.push(JournalMutation {
            room_id: mutation.room_id.clone(),
            mutation_seq,
            command_cursor: mutation.command_cursor,
            schema_version: ROOM_MUTATION_SCHEMA_VERSION,
            mutation: mutation.mutation.clone(),
        });
        Ok(())
    }

    fn ensure_room_write_fence(&self, claim: &RoomLeaseClaim) -> Result<(), JournalError> {
        validate_room_lease_claim(claim)?;
        let current = self.room_writer_leases.get(&claim.room_id);
        if current.is_some_and(|lease| {
            lease.owner_id == claim.owner_id
                && lease.fencing_token == claim.fencing_token
                && lease.expires_at > SystemTime::now()
        }) {
            return Ok(());
        }
        Err(room_lease_lost(claim))
    }
}

impl JournalStore for InMemoryJournalStore {
    fn load_recovery(&mut self) -> Result<JournalRecovery, JournalError> {
        let mut executions = self.executions.clone();
        executions.sort_by(|left, right| {
            (&left.room_id, left.command_seq).cmp(&(&right.room_id, right.command_seq))
        });
        Ok(JournalRecovery {
            rooms: self
                .rooms
                .iter()
                .map(|room| JournalRoom {
                    room_id: room.room_id.clone(),
                    scenario: room.scenario.clone(),
                    status: room.status,
                })
                .collect(),
            executions,
            mutations: self.mutations.clone(),
            snapshots: self.latest_snapshots(),
        })
    }

    fn acquire_room_writer_lease(
        &mut self,
        room_id: &str,
        owner_id: &str,
        owner_url: Option<&str>,
        duration: Duration,
    ) -> Result<Option<RoomWriterLease>, JournalError> {
        let duration_ms = validate_room_lease_request(room_id, owner_id, duration)?;
        validate_room_lease_owner_url(owner_url)?;
        if !self.rooms.iter().any(|room| room.room_id == room_id) {
            return Ok(None);
        }

        let now = SystemTime::now();
        if self
            .room_writer_leases
            .get(room_id)
            .is_some_and(|lease| lease.expires_at > now)
        {
            return Ok(None);
        }
        let fencing_token = self
            .room_writer_leases
            .get(room_id)
            .map_or(Ok(1), |lease| {
                lease
                    .fencing_token
                    .checked_add(1)
                    .ok_or(JournalError::SequenceOutOfRange(u64::MAX))
            })?;
        let expires_at = now.checked_add(Duration::from_millis(duration_ms)).ok_or(
            JournalError::ArithmeticOverflow {
                field: "lease_expires_at",
            },
        )?;
        self.room_writer_leases.insert(
            room_id.to_string(),
            InMemoryRoomWriterLease {
                owner_id: owner_id.to_string(),
                owner_url: owner_url.map(str::to_string),
                fencing_token,
                expires_at,
            },
        );
        Ok(Some(room_writer_lease(
            room_id,
            owner_id,
            owner_url,
            fencing_token,
            expires_at,
        )?))
    }

    fn current_room_writer_lease(
        &mut self,
        room_id: &str,
    ) -> Result<Option<RoomWriterLease>, JournalError> {
        let Some(lease) = self.room_writer_leases.get(room_id) else {
            return Ok(None);
        };
        if lease.expires_at <= SystemTime::now() {
            return Ok(None);
        }
        room_writer_lease(
            room_id,
            &lease.owner_id,
            lease.owner_url.as_deref(),
            lease.fencing_token,
            lease.expires_at,
        )
        .map(Some)
    }

    fn query_room_routes(
        &mut self,
        user_id: &str,
        after_room_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RoomRoutingRecord>, JournalError> {
        let now = SystemTime::now();
        let mut room_ids = self
            .rooms
            .iter()
            .filter(|room| {
                self.room_members
                    .contains_key(&(room.room_id.clone(), user_id.to_string()))
            })
            .map(|room| room.room_id.clone())
            .filter(|room_id| after_room_id.is_none_or(|after| room_id.as_str() > after))
            .collect::<Vec<_>>();
        room_ids.sort();
        room_ids.truncate(limit.clamp(1, 501));

        room_ids
            .into_iter()
            .map(|room_id| {
                let owner = self
                    .room_writer_leases
                    .get(&room_id)
                    .filter(|lease| lease.expires_at > now)
                    .map(|lease| {
                        room_writer_lease(
                            &room_id,
                            &lease.owner_id,
                            lease.owner_url.as_deref(),
                            lease.fencing_token,
                            lease.expires_at,
                        )
                    })
                    .transpose()?;
                Ok(RoomRoutingRecord { room_id, owner })
            })
            .collect()
    }

    fn renew_room_writer_lease(
        &mut self,
        claim: &RoomLeaseClaim,
        duration: Duration,
    ) -> Result<Option<RoomWriterLease>, JournalError> {
        let duration_ms = validate_room_lease_request(&claim.room_id, &claim.owner_id, duration)?;
        validate_room_lease_claim(claim)?;
        let now = SystemTime::now();
        let Some(current) = self.room_writer_leases.get_mut(&claim.room_id) else {
            return Ok(None);
        };
        if current.owner_id != claim.owner_id
            || current.fencing_token != claim.fencing_token
            || current.expires_at <= now
        {
            return Ok(None);
        }
        current.expires_at = now.checked_add(Duration::from_millis(duration_ms)).ok_or(
            JournalError::ArithmeticOverflow {
                field: "lease_expires_at",
            },
        )?;
        Ok(Some(room_writer_lease(
            &claim.room_id,
            &claim.owner_id,
            current.owner_url.as_deref(),
            claim.fencing_token,
            current.expires_at,
        )?))
    }

    fn release_room_writer_lease(&mut self, claim: &RoomLeaseClaim) -> Result<bool, JournalError> {
        validate_room_lease_claim(claim)?;
        let Some(current) = self.room_writer_leases.get_mut(&claim.room_id) else {
            return Ok(false);
        };
        if current.owner_id != claim.owner_id || current.fencing_token != claim.fencing_token {
            return Ok(false);
        }
        current.expires_at = UNIX_EPOCH;
        Ok(true)
    }

    fn find_idempotent_execution(
        &mut self,
        user_id: &str,
        room_id: &str,
        idempotency_key: &str,
    ) -> Result<Option<JournalExecution>, JournalError> {
        Ok(self
            .executions
            .iter()
            .rev()
            .find(|record| {
                record.room_id == room_id
                    && record.request_user_id.as_deref() == Some(user_id)
                    && record.idempotency_key.as_deref() == Some(idempotency_key)
            })
            .cloned())
    }

    fn query_executions(
        &mut self,
        room_id: &str,
        after_command_seq: Option<u64>,
        from_start: bool,
        limit: usize,
    ) -> Result<ExecutionPage, JournalError> {
        let mut executions = self
            .executions
            .iter()
            .filter(|record| record.room_id == room_id)
            .map(|record| record.execution.clone())
            .collect::<Vec<_>>();
        executions.sort_by_key(|execution| execution.command_seq);
        Ok(execution_page_from_sorted(
            &executions,
            after_command_seq,
            from_start,
            limit,
        ))
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
        let checkpoint_cursor = command_cursor_after_records(seed_records)?;
        let mut projected_records = self.executions.clone();
        projected_records.extend(seed_records.iter().cloned());
        MemoryProjections::from_executions(&projected_records)?;
        if let Some(snapshot) = initial_snapshot {
            validate_snapshot(snapshot)?;
        }

        self.rooms.push(StoredRoom {
            room_id: bootstrap.room_id.clone(),
            scenario: scenario.clone(),
            status: MarketStatus::Running,
        });
        self.executions.extend(seed_records.iter().cloned());
        if let Some(snapshot) = initial_snapshot {
            self.snapshots.push(snapshot.clone());
            self.store_mutation(&PendingJournalMutation::new(
                snapshot.room_id.clone(),
                checkpoint_cursor,
                RoomMutation::StateCheckpoint {
                    actor: Box::new(snapshot.actor.clone()),
                    complete_history: true,
                },
            ))?;
        }
        self.room_members.insert(
            (bootstrap.room_id.clone(), owner_user_id.to_string()),
            "owner".to_string(),
        );
        for account_id in account_ids {
            self.account_owners.insert((
                bootstrap.room_id.clone(),
                *account_id,
                owner_user_id.to_string(),
            ));
        }
        Ok(())
    }

    fn create_room_with_writer_lease(
        &mut self,
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
        validate_room_lease_request(&bootstrap.room_id, writer_owner_id, lease_duration)?;
        let original = self.clone();
        if let Err(error) = self.create_room(
            owner_user_id,
            scenario,
            bootstrap,
            account_ids,
            seed_records,
            initial_snapshot,
        ) {
            *self = original;
            return Err(error);
        }
        match self.acquire_room_writer_lease(
            &bootstrap.room_id,
            writer_owner_id,
            writer_owner_url,
            lease_duration,
        ) {
            Ok(Some(lease)) => Ok(lease),
            Ok(None) => {
                *self = original;
                Err(JournalError::Recovery(format!(
                    "new room {} could not acquire its initial writer lease",
                    bootstrap.room_id
                )))
            }
            Err(error) => {
                *self = original;
                Err(error)
            }
        }
    }

    fn append_executions(
        &mut self,
        records: &[JournalExecution],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        let mut projected_records = self.executions.clone();
        projected_records.extend(records.iter().cloned());
        MemoryProjections::from_executions(&projected_records)?;
        if let Some(snapshot) = snapshot {
            validate_snapshot(snapshot)?;
        }

        self.executions.extend(records.iter().cloned());
        if let Some(snapshot) = snapshot {
            self.snapshots.push(snapshot.clone());
        }
        Ok(())
    }

    fn append_executions_fenced(
        &mut self,
        claim: &RoomLeaseClaim,
        records: &[JournalExecution],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        validate_fenced_execution_batch(claim, records, snapshot)?;
        self.ensure_room_write_fence(claim)?;
        self.append_executions(records, snapshot)
    }

    fn append_transfers(
        &mut self,
        records: &[JournalTransfer],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        for record in records {
            validate_transfer(record)?;
        }
        if let Some(snapshot) = snapshot {
            validate_snapshot(snapshot)?;
        }
        for record in records {
            self.transfers.insert(
                (record.room_id.clone(), record.transfer.transfer_id),
                record.transfer.clone(),
            );
        }
        if let Some(snapshot) = snapshot {
            self.snapshots.push(snapshot.clone());
        }
        Ok(())
    }

    fn append_snapshot(&mut self, snapshot: &JournalSnapshot) -> Result<(), JournalError> {
        validate_snapshot(snapshot)?;
        self.snapshots.push(snapshot.clone());
        Ok(())
    }

    fn append_room_mutation(
        &mut self,
        mutation: &PendingJournalMutation,
        execution_records: &[JournalExecution],
        transfer_records: &[JournalTransfer],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        validate_pending_mutation(mutation)?;
        validate_mutation_execution_records(mutation, execution_records)?;
        let mut projected_records = self.executions.clone();
        projected_records.extend(execution_records.iter().cloned());
        MemoryProjections::from_executions(&projected_records)?;
        for record in transfer_records {
            validate_transfer(record)?;
            if record.room_id != mutation.room_id {
                return Err(JournalError::Recovery(format!(
                    "transfer room {} does not match mutation room {}",
                    record.room_id, mutation.room_id
                )));
            }
        }
        if let Some(snapshot) = snapshot {
            validate_snapshot(snapshot)?;
            if snapshot.room_id != mutation.room_id {
                return Err(JournalError::Recovery(format!(
                    "snapshot room {} does not match mutation room {}",
                    snapshot.room_id, mutation.room_id
                )));
            }
            if let Some(last_execution) = execution_records.last()
                && snapshot.command_seq < last_execution.command_seq
            {
                return Err(JournalError::Recovery(format!(
                    "snapshot command sequence {} precedes mutation execution sequence {}",
                    snapshot.command_seq, last_execution.command_seq
                )));
            }
        }

        self.store_mutation(mutation)?;
        self.executions.extend(execution_records.iter().cloned());
        for record in transfer_records {
            self.transfers.insert(
                (record.room_id.clone(), record.transfer.transfer_id),
                record.transfer.clone(),
            );
        }
        if let Some(snapshot) = snapshot {
            self.snapshots.push(snapshot.clone());
        }
        if let RoomMutation::StatusChanged { status } = &mutation.mutation
            && let Some(room) = self
                .rooms
                .iter_mut()
                .find(|room| room.room_id == mutation.room_id)
        {
            room.status = *status;
        }
        Ok(())
    }

    fn append_room_mutation_fenced(
        &mut self,
        claim: &RoomLeaseClaim,
        mutation: &PendingJournalMutation,
        execution_records: &[JournalExecution],
        transfer_records: &[JournalTransfer],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        validate_fenced_room_mutation(claim, mutation)?;
        self.ensure_room_write_fence(claim)?;
        self.append_room_mutation(mutation, execution_records, transfer_records, snapshot)
    }

    fn upsert_room_member(
        &mut self,
        room_id: &str,
        user_id: &str,
        role: &str,
    ) -> Result<(), JournalError> {
        if !is_known_member_role(role) {
            return Err(JournalError::Recovery(format!("invalid role {role}")));
        }
        self.room_members
            .insert((room_id.to_string(), user_id.to_string()), role.to_string());
        Ok(())
    }

    fn remove_room_member(&mut self, room_id: &str, user_id: &str) -> Result<(), JournalError> {
        self.account_owners.retain(|(member_room, _, member_user)| {
            !(member_room == room_id && member_user == user_id)
        });
        self.room_members
            .remove(&(room_id.to_string(), user_id.to_string()));
        Ok(())
    }

    fn assign_account_owner(
        &mut self,
        room_id: &str,
        account_id: AccountId,
        user_id: &str,
    ) -> Result<(), JournalError> {
        let role = self
            .room_members
            .get(&(room_id.to_string(), user_id.to_string()))
            .cloned();
        match role.as_deref() {
            Some(role) if is_account_holder_role(role) => {
                self.account_owners
                    .insert((room_id.to_string(), account_id, user_id.to_string()));
                Ok(())
            }
            Some(role) => Err(JournalError::Recovery(format!(
                "role {role} cannot be assigned an account"
            ))),
            None => Err(JournalError::Recovery(format!(
                "user {user_id} is not a member of {room_id}"
            ))),
        }
    }

    fn user_room_role(
        &mut self,
        user_id: &str,
        room_id: &str,
    ) -> Result<Option<String>, JournalError> {
        Ok(self
            .room_members
            .get(&(room_id.to_string(), user_id.to_string()))
            .cloned())
    }

    fn user_can_access_room(&mut self, user_id: &str, room_id: &str) -> Result<bool, JournalError> {
        Ok(self
            .room_members
            .contains_key(&(room_id.to_string(), user_id.to_string())))
    }

    fn user_can_administer_room(
        &mut self,
        user_id: &str,
        room_id: &str,
    ) -> Result<bool, JournalError> {
        Ok(self.user_is_room_admin(user_id, room_id))
    }

    fn user_can_access_account(
        &mut self,
        user_id: &str,
        room_id: &str,
        account_id: AccountId,
    ) -> Result<bool, JournalError> {
        Ok(self.user_may_access_account(user_id, room_id, account_id))
    }

    fn update_room_status(
        &mut self,
        room_id: &str,
        status: MarketStatus,
    ) -> Result<(), JournalError> {
        if let Some(room) = self.rooms.iter_mut().find(|room| room.room_id == room_id) {
            room.status = status;
        }
        Ok(())
    }

    fn query_orders(
        &mut self,
        user_id: &str,
        room_id: &str,
        instrument_id: Option<&str>,
        account_id: Option<AccountId>,
        limit: usize,
    ) -> Result<Vec<OrderProjection>, JournalError> {
        let projections = MemoryProjections::from_executions(&self.executions)?;
        let mut orders = projections
            .orders
            .into_values()
            .filter(|order| {
                order.room_id == room_id
                    && instrument_id.is_none_or(|id| order.instrument_id == id)
                    && account_id.is_none_or(|id| u64::try_from(order.account_id) == Ok(id))
                    && u64::try_from(order.account_id)
                        .is_ok_and(|id| self.user_may_access_account(user_id, room_id, id))
            })
            .collect::<Vec<_>>();
        orders.sort_by_key(|order| Reverse((order.updated_command_seq, order.order_id)));
        orders.truncate(limit.clamp(1, 500));
        Ok(orders)
    }

    fn query_trades(
        &mut self,
        user_id: &str,
        room_id: &str,
        instrument_id: Option<&str>,
        account_id: Option<AccountId>,
        limit: usize,
    ) -> Result<Vec<TradeProjection>, JournalError> {
        let projections = MemoryProjections::from_executions(&self.executions)?;
        let mut trades = projections
            .trades
            .into_values()
            .filter(|trade| {
                let maker_account_id = u64::try_from(trade.maker_account_id).ok();
                let taker_account_id = u64::try_from(trade.taker_account_id).ok();
                trade.room_id == room_id
                    && instrument_id.is_none_or(|id| trade.instrument_id == id)
                    && account_id.is_none_or(|id| {
                        maker_account_id == Some(id) || taker_account_id == Some(id)
                    })
                    && (maker_account_id
                        .is_some_and(|id| self.user_may_access_account(user_id, room_id, id))
                        || taker_account_id
                            .is_some_and(|id| self.user_may_access_account(user_id, room_id, id)))
            })
            .collect::<Vec<_>>();
        trades.sort_by_key(|trade| Reverse((trade.command_seq, trade.event_seq)));
        trades.truncate(limit.clamp(1, 500));
        Ok(trades)
    }

    fn query_market_ticks(
        &mut self,
        user_id: &str,
        room_id: &str,
        instrument_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MarketTickProjection>, JournalError> {
        if !self
            .room_members
            .contains_key(&(room_id.to_string(), user_id.to_string()))
        {
            return Ok(Vec::new());
        }
        let projections = MemoryProjections::from_executions(&self.executions)?;
        let mut ticks = projections
            .market_ticks
            .into_values()
            .filter(|tick| {
                tick.room_id == room_id && instrument_id.is_none_or(|id| tick.instrument_id == id)
            })
            .collect::<Vec<_>>();
        ticks.sort_by_key(|tick| Reverse((tick.command_seq, tick.event_seq)));
        ticks.truncate(limit.clamp(1, 500));
        Ok(ticks)
    }

    fn query_account_ledger(
        &mut self,
        user_id: &str,
        room_id: &str,
        instrument_id: Option<&str>,
        account_id: Option<AccountId>,
        limit: usize,
    ) -> Result<Vec<AccountLedgerProjection>, JournalError> {
        let projections = MemoryProjections::from_executions(&self.executions)?;
        let mut ledger = projections
            .account_ledger
            .into_iter()
            .filter(|row| {
                let row_account_id = u64::try_from(row.account_id).ok();
                row.room_id == room_id
                    && instrument_id.is_none_or(|id| row.instrument_id == id)
                    && account_id.is_none_or(|id| row_account_id == Some(id))
                    && row_account_id
                        .is_some_and(|id| self.user_may_access_account(user_id, room_id, id))
            })
            .collect::<Vec<_>>();
        ledger.sort_by_key(|row| Reverse((row.command_seq, row.ledger_seq)));
        ledger.truncate(limit.clamp(1, 500));
        Ok(ledger)
    }

    fn query_position_snapshots(
        &mut self,
        user_id: &str,
        room_id: &str,
        instrument_id: Option<&str>,
        account_id: Option<AccountId>,
        limit: usize,
    ) -> Result<Vec<PositionSnapshotProjection>, JournalError> {
        let projections = MemoryProjections::from_executions(&self.executions)?;
        let mut positions = projections
            .position_snapshots
            .into_iter()
            .filter(|row| {
                let row_account_id = u64::try_from(row.account_id).ok();
                row.room_id == room_id
                    && instrument_id.is_none_or(|id| row.instrument_id == id)
                    && account_id.is_none_or(|id| row_account_id == Some(id))
                    && row_account_id
                        .is_some_and(|id| self.user_may_access_account(user_id, room_id, id))
            })
            .collect::<Vec<_>>();
        positions.sort_by_key(|row| Reverse((row.command_seq, row.ledger_seq)));
        positions.truncate(limit.clamp(1, 500));
        Ok(positions)
    }

    fn query_transfers(
        &mut self,
        user_id: &str,
        room_id: &str,
        account_id: Option<AccountId>,
        limit: usize,
    ) -> Result<Vec<VenueTransfer>, JournalError> {
        let mut transfers = self
            .transfers
            .iter()
            .filter(|((transfer_room_id, _), transfer)| {
                transfer_room_id == room_id
                    && account_id.is_none_or(|account_id| transfer.account_id == account_id)
                    && self.user_may_access_account(user_id, room_id, transfer.account_id)
            })
            .map(|(_, transfer)| transfer.clone())
            .collect::<Vec<_>>();
        transfers.sort_by_key(|transfer| Reverse(transfer.transfer_id));
        transfers.truncate(limit.clamp(1, 500));
        Ok(transfers)
    }
}

impl InMemoryJournalStore {
    fn latest_snapshots(&self) -> Vec<JournalSnapshot> {
        let mut snapshots = std::collections::BTreeMap::<&str, &JournalSnapshot>::new();
        for snapshot in &self.snapshots {
            let current = snapshots.get(snapshot.room_id.as_str());
            if current.is_none_or(|current| snapshot.command_seq >= current.command_seq) {
                snapshots.insert(snapshot.room_id.as_str(), snapshot);
            }
        }

        snapshots
            .values()
            .map(|snapshot| (*snapshot).clone())
            .collect()
    }
}

pub struct PostgresJournalStore {
    database_url: String,
    client: Client,
    runtime_lock: PostgresRuntimeLock,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PostgresRuntimeLock {
    None,
    Shared,
    Exclusive,
}

impl PostgresJournalStore {
    pub fn connect(database_url: &str) -> Result<Self, JournalError> {
        Ok(Self {
            database_url: database_url.to_string(),
            client: Client::connect(database_url, NoTls).map_err(JournalError::Postgres)?,
            runtime_lock: PostgresRuntimeLock::None,
        })
    }

    pub fn connect_migrated(database_url: &str) -> Result<Self, JournalError> {
        let mut store = Self::connect(database_url)?;
        store.ensure_schema_serialized()?;
        Ok(store)
    }

    pub(crate) fn connect_migrated_shared(database_url: &str) -> Result<Self, JournalError> {
        let mut store = Self::connect(database_url)?;
        store.acquire_shared_runtime_lock()?;
        store.ensure_schema_serialized()?;
        Ok(store)
    }

    #[cfg(test)]
    pub(crate) fn connect_migrated_exclusive(database_url: &str) -> Result<Self, JournalError> {
        Self::connect_migrated_exclusive_with_wait(database_url, Duration::ZERO)
    }

    pub(crate) fn connect_migrated_exclusive_with_wait(
        database_url: &str,
        wait: Duration,
    ) -> Result<Self, JournalError> {
        let mut store = Self::connect(database_url)?;
        store.acquire_exclusive_runtime_lock(wait)?;
        store.ensure_schema_serialized()?;
        Ok(store)
    }

    fn acquire_exclusive_runtime_lock(&mut self, wait: Duration) -> Result<(), JournalError> {
        let started = Instant::now();
        loop {
            let acquired: bool = self
                .client
                .query_one(
                    "SELECT pg_try_advisory_lock($1, $2)",
                    &[&POSTGRES_RUNTIME_LOCK_NAMESPACE, &POSTGRES_RUNTIME_LOCK_ID],
                )
                .map_err(JournalError::Postgres)?
                .get(0);
            if acquired {
                self.runtime_lock = PostgresRuntimeLock::Exclusive;
                return Ok(());
            }

            let Some(remaining) = wait.checked_sub(started.elapsed()) else {
                return Err(JournalError::RuntimeLockUnavailable);
            };
            if remaining.is_zero() {
                return Err(JournalError::RuntimeLockUnavailable);
            }
            thread::sleep(remaining.min(RUNTIME_LOCK_POLL_INTERVAL));
        }
    }

    fn acquire_shared_runtime_lock(&mut self) -> Result<(), JournalError> {
        let acquired: bool = self
            .client
            .query_one(
                "SELECT pg_try_advisory_lock_shared($1, $2)",
                &[&POSTGRES_RUNTIME_LOCK_NAMESPACE, &POSTGRES_RUNTIME_LOCK_ID],
            )
            .map_err(JournalError::Postgres)?
            .get(0);
        if !acquired {
            return Err(JournalError::RuntimeLockUnavailable);
        }
        self.runtime_lock = PostgresRuntimeLock::Shared;
        Ok(())
    }

    fn ensure_schema(&mut self) -> Result<(), JournalError> {
        run_schema_migrations(&mut self.client)
    }

    fn ensure_schema_serialized(&mut self) -> Result<(), JournalError> {
        self.client
            .query_one(
                "SELECT pg_advisory_lock($1, $2)",
                &[
                    &POSTGRES_RUNTIME_LOCK_NAMESPACE,
                    &POSTGRES_MIGRATION_LOCK_ID,
                ],
            )
            .map_err(JournalError::Postgres)?;
        let migration_result = self.ensure_schema();
        let unlock_result = self
            .client
            .query_one(
                "SELECT pg_advisory_unlock($1, $2)",
                &[
                    &POSTGRES_RUNTIME_LOCK_NAMESPACE,
                    &POSTGRES_MIGRATION_LOCK_ID,
                ],
            )
            .map_err(JournalError::Postgres);
        migration_result?;
        let unlocked: bool = unlock_result?.get(0);
        if !unlocked {
            return Err(JournalError::Recovery(
                "PostgreSQL migration advisory lock was not held during unlock".to_string(),
            ));
        }
        Ok(())
    }

    fn assert_room_write_fence(
        tx: &mut postgres::Transaction<'_>,
        claim: &RoomLeaseClaim,
    ) -> Result<(), JournalError> {
        validate_room_lease_claim(claim)?;
        let fencing_token = i64_from_u64(claim.fencing_token, "fencing_token")?;
        let held = tx
            .query_opt(
                r#"
                SELECT 1
                FROM marketforge_room_writer_leases
                WHERE room_id = $1
                  AND owner_id = $2
                  AND fencing_token = $3
                  AND lease_expires_at > clock_timestamp()
                FOR UPDATE
                "#,
                &[&claim.room_id, &claim.owner_id, &fencing_token],
            )
            .map_err(JournalError::Postgres)?
            .is_some();
        if held {
            Ok(())
        } else {
            Err(room_lease_lost(claim))
        }
    }

    fn insert_execution(
        tx: &mut postgres::Transaction<'_>,
        record: &JournalExecution,
    ) -> Result<(), JournalError> {
        validate_execution_idempotency(record)?;
        let command_seq = i64::try_from(record.command_seq)
            .map_err(|_| JournalError::SequenceOutOfRange(record.command_seq))?;
        let clearing_event_count = i32::try_from(record.execution.clearing_event_count)
            .map_err(|_| JournalError::CountOutOfRange(record.execution.clearing_event_count))?;
        let account_id = record.account_id.map(|id| id.to_string());
        let market_time_ms = record
            .execution
            .market_time_ms
            .map(|value| i64_from_u64(value, "market_time_ms"))
            .transpose()?;
        let command_json = record.command_json()?;
        let execution_json = record.execution_json()?;

        tx.execute(
            r#"
            INSERT INTO marketforge_executions (
                room_id,
                command_seq,
                participant_id,
                account_id,
                command_json,
                execution_json,
                accepted,
                reject_reason,
                clearing_event_count,
                market_time_ms,
                request_user_id,
                idempotency_key,
                request_fingerprint
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            "#,
            &[
                &record.room_id,
                &command_seq,
                &record.participant_id,
                &account_id,
                &command_json,
                &execution_json,
                &record.execution.accepted,
                &record.execution.reject_reason,
                &clearing_event_count,
                &market_time_ms,
                &record.request_user_id,
                &record.idempotency_key,
                &record.request_fingerprint,
            ],
        )
        .map_err(JournalError::Postgres)?;

        Self::insert_projected_execution(tx, record)?;

        Ok(())
    }

    fn insert_projected_execution(
        tx: &mut postgres::Transaction<'_>,
        record: &JournalExecution,
    ) -> Result<(), JournalError> {
        let command_seq = i64_from_u64(record.command_seq, "command_seq")?;
        let market_time_ms = record
            .execution
            .market_time_ms
            .map(|value| i64_from_u64(value, "market_time_ms"))
            .transpose()?;
        let instrument_id = record
            .execution
            .instrument_id
            .as_deref()
            .unwrap_or("legacy-primary");
        let mut ignored_rejected_duplicate_order_id = None;

        if let Command::NewOrder(order) = &record.command {
            let order_id = i64_from_u64(order.order_id, "order_id")?;
            let account_id = i64_from_u64(order.account_id, "account_id")?;
            let original_qty = i64_from_u64(order.qty, "qty")?;
            let (base_order_type, limit_price_tick) = match order.kind {
                OrderKind::Limit { price_tick } => ("limit", Some(price_tick)),
                OrderKind::Market => ("market", None),
                OrderKind::PostOnly { price_tick } => ("post_only", Some(price_tick)),
                OrderKind::ImmediateOrCancel { price_tick } => ("immediate_or_cancel", price_tick),
                OrderKind::FillOrKill { price_tick } => ("fill_or_kill", price_tick),
            };
            let order_type = if order.reduce_only {
                format!("reduce_only_{base_order_type}")
            } else {
                base_order_type.to_string()
            };
            let initial_status = if !record.execution.accepted && record.execution.events.is_empty()
            {
                "rejected"
            } else {
                "submitted"
            };

            let inserted = tx
                .execute(
                    r#"
                INSERT INTO marketforge_orders (
                    room_id,
                    instrument_id,
                    order_id,
                    account_id,
                    participant_id,
                    side,
                    order_type,
                    limit_price_tick,
                    original_qty,
                    status,
                    remaining_qty,
                    created_command_seq,
                    updated_command_seq,
                    created_market_time_ms,
                    updated_market_time_ms
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $9, $11, $11, $12, $12)
                ON CONFLICT (room_id, instrument_id, order_id) DO NOTHING
                "#,
                    &[
                        &record.room_id,
                        &instrument_id,
                        &order_id,
                        &account_id,
                        &record.participant_id,
                        &side_name(order.side),
                        &order_type,
                        &limit_price_tick,
                        &original_qty,
                        &initial_status,
                        &command_seq,
                        &market_time_ms,
                    ],
                )
                .map_err(JournalError::Postgres)?;
            if inserted == 0 {
                if new_order_did_not_create_order(record, order.order_id) {
                    ignored_rejected_duplicate_order_id = Some(order.order_id);
                } else {
                    return Err(JournalError::ProjectionConflict {
                        entity: "order",
                        room_id: record.room_id.clone(),
                        instrument_id: instrument_id.to_string(),
                        id: order_id,
                    });
                }
            }
        }

        for event in &record.execution.events {
            Self::insert_order_event(tx, record, event)?;
            if ignored_rejected_duplicate_order_id != event_order_id(event) {
                Self::apply_order_event_projection(tx, record, event)?;
            }
            if let EventSummary::TradePrinted {
                seq,
                trade_id,
                maker_order_id,
                maker_account_id,
                taker_order_id,
                taker_account_id,
                price_tick,
                qty,
                taker_side,
            } = event
            {
                let event_seq = i64_from_u64(*seq, "event_seq")?;
                let trade_id = i64_from_u64(*trade_id, "trade_id")?;
                let maker_order_id = i64_from_u64(*maker_order_id, "maker_order_id")?;
                let maker_account_id = i64_from_u64(*maker_account_id, "maker_account_id")?;
                let taker_order_id = i64_from_u64(*taker_order_id, "taker_order_id")?;
                let taker_account_id = i64_from_u64(*taker_account_id, "taker_account_id")?;
                let qty = i64_from_u64(*qty, "qty")?;

                tx.execute(
                    r#"
                    INSERT INTO marketforge_trades (
                        room_id,
                        instrument_id,
                        trade_id,
                        command_seq,
                        event_seq,
                        maker_order_id,
                        maker_account_id,
                        taker_order_id,
                        taker_account_id,
                        price_tick,
                        qty,
                        taker_side,
                        market_time_ms
                    )
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
                    "#,
                    &[
                        &record.room_id,
                        &instrument_id,
                        &trade_id,
                        &command_seq,
                        &event_seq,
                        &maker_order_id,
                        &maker_account_id,
                        &taker_order_id,
                        &taker_account_id,
                        price_tick,
                        &qty,
                        &side_name(*taker_side),
                        &market_time_ms,
                    ],
                )
                .map_err(JournalError::Postgres)?;

                tx.execute(
                    r#"
                    INSERT INTO marketforge_market_ticks (
                        room_id,
                        instrument_id,
                        command_seq,
                        event_seq,
                        trade_id,
                        price_tick,
                        qty,
                        taker_side,
                        market_time_ms
                    )
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                    ON CONFLICT (room_id, command_seq, event_seq)
                    DO UPDATE SET
                        trade_id = EXCLUDED.trade_id,
                        price_tick = EXCLUDED.price_tick,
                        qty = EXCLUDED.qty,
                        taker_side = EXCLUDED.taker_side,
                        market_time_ms = EXCLUDED.market_time_ms
                    "#,
                    &[
                        &record.room_id,
                        &instrument_id,
                        &command_seq,
                        &event_seq,
                        &trade_id,
                        price_tick,
                        &qty,
                        &side_name(*taker_side),
                        &market_time_ms,
                    ],
                )
                .map_err(JournalError::Postgres)?;
            }
        }
        Self::insert_clearing_projection(tx, record)?;

        Ok(())
    }

    fn insert_order_event(
        tx: &mut postgres::Transaction<'_>,
        record: &JournalExecution,
        event: &EventSummary,
    ) -> Result<(), JournalError> {
        let command_seq = i64_from_u64(record.command_seq, "command_seq")?;
        let instrument_id = record
            .execution
            .instrument_id
            .as_deref()
            .unwrap_or("legacy-primary");
        let event_seq = i64_from_u64(event_seq(event), "event_seq")?;
        let order_id = event_order_id(event)
            .map(|order_id| i64_from_u64(order_id, "order_id"))
            .transpose()?;
        let payload_json = serde_json::to_value(event).map_err(JournalError::Serialize)?;
        let reason = event_reason(event);
        let price_tick = event_price_tick(event);
        let qty = event_qty(event)
            .map(|qty| i64_from_u64(qty, "qty"))
            .transpose()?;
        let remaining_qty = event_remaining_qty(event)
            .map(|qty| i64_from_u64(qty, "remaining_qty"))
            .transpose()?;

        tx.execute(
            r#"
            INSERT INTO marketforge_order_events (
                room_id,
                instrument_id,
                command_seq,
                event_seq,
                event_type,
                order_id,
                reason,
                price_tick,
                qty,
                remaining_qty,
                payload_json
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            ON CONFLICT (room_id, command_seq, event_seq)
            DO UPDATE SET
                event_type = EXCLUDED.event_type,
                order_id = EXCLUDED.order_id,
                reason = EXCLUDED.reason,
                price_tick = EXCLUDED.price_tick,
                qty = EXCLUDED.qty,
                remaining_qty = EXCLUDED.remaining_qty,
                payload_json = EXCLUDED.payload_json
            "#,
            &[
                &record.room_id,
                &instrument_id,
                &command_seq,
                &event_seq,
                &event_type(event),
                &order_id,
                &reason,
                &price_tick,
                &qty,
                &remaining_qty,
                &payload_json,
            ],
        )
        .map_err(JournalError::Postgres)?;

        Ok(())
    }

    fn apply_order_event_projection(
        tx: &mut postgres::Transaction<'_>,
        record: &JournalExecution,
        event: &EventSummary,
    ) -> Result<(), JournalError> {
        let Some((order_id, status, remaining_qty, limit_price_tick)) = order_status_update(event)
        else {
            return Ok(());
        };

        let order_id = i64_from_u64(order_id, "order_id")?;
        let command_seq = i64_from_u64(record.command_seq, "command_seq")?;
        let market_time_ms = record
            .execution
            .market_time_ms
            .map(|value| i64_from_u64(value, "market_time_ms"))
            .transpose()?;
        let instrument_id = record
            .execution
            .instrument_id
            .as_deref()
            .unwrap_or("legacy-primary");
        let remaining_qty = remaining_qty
            .map(|qty| i64_from_u64(qty, "remaining_qty"))
            .transpose()?;

        tx.execute(
            r#"
            UPDATE marketforge_orders
            SET
                status = $4,
                remaining_qty = COALESCE($5, remaining_qty),
                limit_price_tick = COALESCE($6, limit_price_tick),
                updated_command_seq = $7,
                updated_market_time_ms = $8,
                updated_at = now()
            WHERE room_id = $1 AND instrument_id = $2 AND order_id = $3
            "#,
            &[
                &record.room_id,
                &instrument_id,
                &order_id,
                &status,
                &remaining_qty,
                &limit_price_tick,
                &command_seq,
                &market_time_ms,
            ],
        )
        .map_err(JournalError::Postgres)?;

        Ok(())
    }

    fn insert_clearing_projection(
        tx: &mut postgres::Transaction<'_>,
        record: &JournalExecution,
    ) -> Result<(), JournalError> {
        let command_seq = i64_from_u64(record.command_seq, "command_seq")?;
        let instrument_id = record
            .execution
            .instrument_id
            .as_deref()
            .unwrap_or("legacy-primary");

        for (index, event) in record.execution.clearing_events.iter().enumerate() {
            let rows = clearing_ledger_rows(index, event)?;
            for row in rows {
                tx.execute(
                    r#"
                    INSERT INTO marketforge_account_ledger (
                        room_id,
                        instrument_id,
                        command_seq,
                        ledger_seq,
                        market_kind,
                        account_id,
                        trade_id,
                        account_side,
                        cash_delta,
                        position_delta,
                        fee,
                        realized_pnl,
                        price_tick,
                        qty,
                        notional,
                        cash_balance,
                        position_qty,
                        avg_entry_price_tick,
                        realized_pnl_total,
                        unrealized_pnl,
                        equity,
                        initial_margin,
                        maintenance_margin,
                        portfolio_initial_margin,
                        portfolio_maintenance_margin,
                        margin_status,
                        fees_paid,
                        payload_json
                    )
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                            $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24,
                            $25, $26, $27, $28)
                    ON CONFLICT (room_id, command_seq, ledger_seq)
                    DO UPDATE SET
                        market_kind = EXCLUDED.market_kind,
                        account_id = EXCLUDED.account_id,
                        trade_id = EXCLUDED.trade_id,
                        account_side = EXCLUDED.account_side,
                        cash_delta = EXCLUDED.cash_delta,
                        position_delta = EXCLUDED.position_delta,
                        fee = EXCLUDED.fee,
                        realized_pnl = EXCLUDED.realized_pnl,
                        price_tick = EXCLUDED.price_tick,
                        qty = EXCLUDED.qty,
                        notional = EXCLUDED.notional,
                        cash_balance = EXCLUDED.cash_balance,
                        position_qty = EXCLUDED.position_qty,
                        avg_entry_price_tick = EXCLUDED.avg_entry_price_tick,
                        realized_pnl_total = EXCLUDED.realized_pnl_total,
                        unrealized_pnl = EXCLUDED.unrealized_pnl,
                        equity = EXCLUDED.equity,
                        initial_margin = EXCLUDED.initial_margin,
                        maintenance_margin = EXCLUDED.maintenance_margin,
                        portfolio_initial_margin = EXCLUDED.portfolio_initial_margin,
                        portfolio_maintenance_margin = EXCLUDED.portfolio_maintenance_margin,
                        margin_status = EXCLUDED.margin_status,
                        fees_paid = EXCLUDED.fees_paid,
                        payload_json = EXCLUDED.payload_json
                    "#,
                    &[
                        &record.room_id,
                        &instrument_id,
                        &command_seq,
                        &row.ledger_seq,
                        &row.market_kind,
                        &row.account_id,
                        &row.trade_id,
                        &row.account_side,
                        &row.cash_delta,
                        &row.position_delta,
                        &row.fee,
                        &row.realized_pnl,
                        &row.price_tick,
                        &row.qty,
                        &row.notional,
                        &row.cash_balance,
                        &row.position_qty,
                        &row.avg_entry_price_tick,
                        &row.realized_pnl_total,
                        &row.unrealized_pnl,
                        &row.equity,
                        &row.initial_margin,
                        &row.maintenance_margin,
                        &row.portfolio_initial_margin,
                        &row.portfolio_maintenance_margin,
                        &row.margin_status,
                        &row.fees_paid,
                        &row.payload_json,
                    ],
                )
                .map_err(JournalError::Postgres)?;

                tx.execute(
                    r#"
                    INSERT INTO marketforge_position_snapshots (
                        room_id,
                        instrument_id,
                        command_seq,
                        ledger_seq,
                        market_kind,
                        account_id,
                        trade_id,
                        cash_balance,
                        position_qty,
                        avg_entry_price_tick,
                        realized_pnl,
                        unrealized_pnl,
                        equity,
                        initial_margin,
                        maintenance_margin,
                        portfolio_initial_margin,
                        portfolio_maintenance_margin,
                        margin_status,
                        fees_paid,
                        payload_json
                    )
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                            $11, $12, $13, $14, $15, $16, $17, $18, $19, $20)
                    ON CONFLICT (room_id, command_seq, ledger_seq)
                    DO UPDATE SET
                        market_kind = EXCLUDED.market_kind,
                        account_id = EXCLUDED.account_id,
                        trade_id = EXCLUDED.trade_id,
                        cash_balance = EXCLUDED.cash_balance,
                        position_qty = EXCLUDED.position_qty,
                        avg_entry_price_tick = EXCLUDED.avg_entry_price_tick,
                        realized_pnl = EXCLUDED.realized_pnl,
                        unrealized_pnl = EXCLUDED.unrealized_pnl,
                        equity = EXCLUDED.equity,
                        initial_margin = EXCLUDED.initial_margin,
                        maintenance_margin = EXCLUDED.maintenance_margin,
                        portfolio_initial_margin = EXCLUDED.portfolio_initial_margin,
                        portfolio_maintenance_margin = EXCLUDED.portfolio_maintenance_margin,
                        margin_status = EXCLUDED.margin_status,
                        fees_paid = EXCLUDED.fees_paid,
                        payload_json = EXCLUDED.payload_json
                    "#,
                    &[
                        &record.room_id,
                        &instrument_id,
                        &command_seq,
                        &row.ledger_seq,
                        &row.market_kind,
                        &row.account_id,
                        &row.trade_id,
                        &row.cash_balance,
                        &row.position_qty,
                        &row.avg_entry_price_tick,
                        &row.realized_pnl_total,
                        &row.unrealized_pnl,
                        &row.equity,
                        &row.initial_margin,
                        &row.maintenance_margin,
                        &row.portfolio_initial_margin,
                        &row.portfolio_maintenance_margin,
                        &row.margin_status,
                        &row.fees_paid,
                        &row.payload_json,
                    ],
                )
                .map_err(JournalError::Postgres)?;
            }
        }

        Ok(())
    }

    fn insert_snapshot(
        tx: &mut postgres::Transaction<'_>,
        snapshot: &JournalSnapshot,
    ) -> Result<(), JournalError> {
        let command_seq = i64::try_from(snapshot.command_seq)
            .map_err(|_| JournalError::SequenceOutOfRange(snapshot.command_seq))?;
        let actor_json = serde_json::to_value(&snapshot.actor).map_err(JournalError::Serialize)?;

        tx.execute(
            r#"
            INSERT INTO marketforge_room_snapshots (room_id, command_seq, actor_json)
            VALUES ($1, $2, $3)
            ON CONFLICT (room_id, command_seq)
            DO UPDATE SET actor_json = EXCLUDED.actor_json, created_at = now()
            "#,
            &[&snapshot.room_id, &command_seq, &actor_json],
        )
        .map_err(JournalError::Postgres)?;

        Ok(())
    }

    fn insert_room_mutation(
        tx: &mut postgres::Transaction<'_>,
        mutation: &PendingJournalMutation,
    ) -> Result<u64, JournalError> {
        validate_pending_mutation(mutation)?;
        let command_cursor = i64_from_u64(mutation.command_cursor, "command_cursor")?;
        let schema_version = i32::from(ROOM_MUTATION_SCHEMA_VERSION);
        let mutation_kind = mutation.mutation.kind_name();
        let payload_json =
            serde_json::to_value(&mutation.mutation).map_err(JournalError::Serialize)?;
        let row = tx
            .query_one(
                r#"
                INSERT INTO marketforge_room_mutations (
                    room_id,
                    command_cursor,
                    schema_version,
                    mutation_kind,
                    payload_json
                )
                VALUES ($1, $2, $3, $4, $5)
                RETURNING mutation_seq
                "#,
                &[
                    &mutation.room_id,
                    &command_cursor,
                    &schema_version,
                    &mutation_kind,
                    &payload_json,
                ],
            )
            .map_err(JournalError::Postgres)?;
        let mutation_seq: i64 = row.get("mutation_seq");
        u64::try_from(mutation_seq).map_err(|_| JournalError::InvalidSequence(mutation_seq))
    }

    fn insert_transfer(
        tx: &mut postgres::Transaction<'_>,
        record: &JournalTransfer,
    ) -> Result<(), JournalError> {
        let transfer = &record.transfer;
        let transfer_id = i64_from_u64(transfer.transfer_id, "transfer_id")?;
        let event_seq = transfer_event_seq(transfer)?;
        let account_id = i64_from_u64(transfer.account_id, "account_id")?;
        let amount = i64_from_i128(transfer.amount, "amount")?;
        let requested_at_step = i64_from_u64(transfer.requested_at_step, "requested_at_step")?;
        let available_after_step =
            i64_from_u64(transfer.available_after_step, "available_after_step")?;
        let completed_at_step = transfer
            .completed_at_step
            .map(|step| i64_from_u64(step, "completed_at_step"))
            .transpose()?;
        let payload_json = serde_json::to_value(transfer).map_err(JournalError::Serialize)?;
        let kind = transfer_kind_name(transfer.kind);
        let status = transfer_status_name(transfer.status);
        let reject_reason = transfer
            .reject_reason
            .as_ref()
            .map(transfer_reject_reason_name);

        tx.execute(
            r#"
            INSERT INTO marketforge_transfer_events (
                room_id,
                transfer_id,
                event_seq,
                kind,
                account_id,
                asset_id,
                amount,
                requested_at_step,
                available_after_step,
                completed_at_step,
                status,
                reject_reason,
                payload_json
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            ON CONFLICT (room_id, transfer_id, event_seq)
            DO UPDATE SET
                kind = EXCLUDED.kind,
                account_id = EXCLUDED.account_id,
                asset_id = EXCLUDED.asset_id,
                amount = EXCLUDED.amount,
                requested_at_step = EXCLUDED.requested_at_step,
                available_after_step = EXCLUDED.available_after_step,
                completed_at_step = EXCLUDED.completed_at_step,
                status = EXCLUDED.status,
                reject_reason = EXCLUDED.reject_reason,
                payload_json = EXCLUDED.payload_json
            "#,
            &[
                &record.room_id,
                &transfer_id,
                &event_seq,
                &kind,
                &account_id,
                &transfer.asset_id,
                &amount,
                &requested_at_step,
                &available_after_step,
                &completed_at_step,
                &status,
                &reject_reason,
                &payload_json,
            ],
        )
        .map_err(JournalError::Postgres)?;

        tx.execute(
            r#"
            INSERT INTO marketforge_transfers (
                room_id,
                transfer_id,
                kind,
                account_id,
                asset_id,
                amount,
                requested_at_step,
                available_after_step,
                completed_at_step,
                status,
                reject_reason,
                payload_json
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            ON CONFLICT (room_id, transfer_id)
            DO UPDATE SET
                kind = EXCLUDED.kind,
                account_id = EXCLUDED.account_id,
                asset_id = EXCLUDED.asset_id,
                amount = EXCLUDED.amount,
                requested_at_step = EXCLUDED.requested_at_step,
                available_after_step = EXCLUDED.available_after_step,
                completed_at_step = EXCLUDED.completed_at_step,
                status = EXCLUDED.status,
                reject_reason = EXCLUDED.reject_reason,
                payload_json = EXCLUDED.payload_json,
                updated_at = now()
            "#,
            &[
                &record.room_id,
                &transfer_id,
                &kind,
                &account_id,
                &transfer.asset_id,
                &amount,
                &requested_at_step,
                &available_after_step,
                &completed_at_step,
                &status,
                &reject_reason,
                &payload_json,
            ],
        )
        .map_err(JournalError::Postgres)?;

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn create_room_transaction(
        &mut self,
        owner_user_id: &str,
        scenario: &ScenarioConfig,
        bootstrap: &RoomBootstrap,
        account_ids: &[AccountId],
        seed_records: &[JournalExecution],
        initial_snapshot: Option<&JournalSnapshot>,
        initial_writer_lease: Option<(&str, Option<&str>, u64)>,
    ) -> Result<Option<RoomWriterLease>, JournalError> {
        let checkpoint_cursor = command_cursor_after_records(seed_records)?;
        let owner_user_id = owner_user_id.to_string();
        let scenario = scenario.clone();
        let bootstrap = bootstrap.clone();
        let account_ids = account_ids.to_vec();
        let seed_records = seed_records.to_vec();
        let initial_snapshot = initial_snapshot.cloned();
        let initial_writer_lease =
            initial_writer_lease.map(|(owner_id, owner_url, duration_ms)| {
                (
                    owner_id.to_string(),
                    owner_url.map(str::to_string),
                    duration_ms,
                )
            });

        run_postgres(&mut self.client, move |client| {
            let scenario_json = serde_json::to_value(scenario).map_err(JournalError::Serialize)?;
            let mut tx = client.transaction().map_err(JournalError::Postgres)?;

            tx.execute(
                r#"
                INSERT INTO marketforge_rooms (room_id, scenario_json, status)
                VALUES ($1, $2, $3)
                "#,
                &[
                    &bootstrap.room_id,
                    &scenario_json,
                    &status_name(MarketStatus::Running),
                ],
            )
            .map_err(JournalError::Postgres)?;

            tx.execute(
                r#"
                INSERT INTO marketforge_users (user_id)
                VALUES ($1)
                ON CONFLICT (user_id) DO NOTHING
                "#,
                &[&owner_user_id],
            )
            .map_err(JournalError::Postgres)?;

            tx.execute(
                r#"
                INSERT INTO marketforge_room_members (room_id, user_id, role)
                VALUES ($1, $2, 'owner')
                ON CONFLICT (room_id, user_id)
                DO UPDATE SET role = EXCLUDED.role
                "#,
                &[&bootstrap.room_id, &owner_user_id],
            )
            .map_err(JournalError::Postgres)?;

            for account_id in &account_ids {
                let account_id = i64_from_u64(*account_id, "account_id")?;
                tx.execute(
                    r#"
                    INSERT INTO marketforge_account_owners (room_id, account_id, user_id)
                    VALUES ($1, $2, $3)
                    ON CONFLICT (room_id, account_id, user_id) DO NOTHING
                    "#,
                    &[&bootstrap.room_id, &account_id, &owner_user_id],
                )
                .map_err(JournalError::Postgres)?;
            }

            for record in &seed_records {
                Self::insert_execution(&mut tx, record)?;
            }
            if let Some(snapshot) = &initial_snapshot {
                Self::insert_snapshot(&mut tx, snapshot)?;
                Self::insert_room_mutation(
                    &mut tx,
                    &PendingJournalMutation::new(
                        snapshot.room_id.clone(),
                        checkpoint_cursor,
                        RoomMutation::StateCheckpoint {
                            actor: Box::new(snapshot.actor.clone()),
                            complete_history: true,
                        },
                    ),
                )?;
            }

            let lease = initial_writer_lease
                .map(|(writer_owner_id, writer_owner_url, duration_ms)| {
                    let duration_ms = i64_from_u64(duration_ms, "lease_duration_ms")?;
                    let row = tx
                        .query_one(
                            r#"
                            INSERT INTO marketforge_room_writer_leases (
                                room_id, owner_id, owner_url, fencing_token, lease_expires_at
                            )
                            VALUES (
                                $1, $2, $3, 1,
                                clock_timestamp()
                                    + ($4::bigint * interval '1 millisecond')
                            )
                            RETURNING owner_id, owner_url, fencing_token,
                                      (extract(epoch FROM lease_expires_at) * 1000)::bigint
                                          AS expires_at_unix_ms
                            "#,
                            &[
                                &bootstrap.room_id,
                                &writer_owner_id,
                                &writer_owner_url,
                                &duration_ms,
                            ],
                        )
                        .map_err(JournalError::Postgres)?;
                    postgres_room_writer_lease(&bootstrap.room_id, row)
                })
                .transpose()?;

            tx.commit().map_err(JournalError::Postgres)?;
            Ok(lease)
        })
    }
}

fn load_postgres_recovery(
    client: &mut Client,
    room_id: Option<&str>,
) -> Result<JournalRecovery, JournalError> {
    let rooms = query_recovery_rows(
        client,
        room_id,
        r#"
        SELECT room_id, scenario_json, status
        FROM marketforge_rooms
        ORDER BY created_at, room_id
        "#,
        r#"
        SELECT room_id, scenario_json, status
        FROM marketforge_rooms
        WHERE room_id = $1
        ORDER BY created_at, room_id
        "#,
    )?
    .into_iter()
    .map(|row| {
        let room_id: String = row.get("room_id");
        let scenario_json: Value = row.get("scenario_json");
        let status: String = row.get("status");
        Ok(JournalRoom {
            room_id,
            scenario: serde_json::from_value(scenario_json).map_err(JournalError::Serialize)?,
            status: status_from_name(&status)?,
        })
    })
    .collect::<Result<Vec<_>, JournalError>>()?;

    let executions = query_recovery_rows(
        client,
        room_id,
        r#"
        SELECT room_id, command_seq, participant_id, account_id,
               request_user_id, idempotency_key, request_fingerprint,
               command_json, execution_json
        FROM marketforge_executions
        ORDER BY room_id, command_seq
        "#,
        r#"
        SELECT room_id, command_seq, participant_id, account_id,
               request_user_id, idempotency_key, request_fingerprint,
               command_json, execution_json
        FROM marketforge_executions
        WHERE room_id = $1
        ORDER BY room_id, command_seq
        "#,
    )?
    .into_iter()
    .map(|row| {
        let command_seq: i64 = row.get("command_seq");
        let account_id: Option<String> = row.get("account_id");
        let command_json: Value = row.get("command_json");
        let execution_json: Value = row.get("execution_json");

        Ok(JournalExecution {
            room_id: row.get("room_id"),
            command_seq: u64::try_from(command_seq)
                .map_err(|_| JournalError::InvalidSequence(command_seq))?,
            participant_id: row.get("participant_id"),
            account_id: account_id
                .map(|id| id.parse().map_err(|_| JournalError::InvalidAccountId(id)))
                .transpose()?,
            request_user_id: row.get("request_user_id"),
            idempotency_key: row.get("idempotency_key"),
            request_fingerprint: row.get("request_fingerprint"),
            command: serde_json::from_value(command_json).map_err(JournalError::Serialize)?,
            execution: serde_json::from_value(execution_json).map_err(JournalError::Serialize)?,
        })
    })
    .collect::<Result<Vec<_>, JournalError>>()?;

    let mutations = query_recovery_rows(
        client,
        room_id,
        r#"
        SELECT room_id, mutation_seq, command_cursor,
               schema_version, mutation_kind, payload_json
        FROM marketforge_room_mutations
        ORDER BY room_id, mutation_seq
        "#,
        r#"
        SELECT room_id, mutation_seq, command_cursor,
               schema_version, mutation_kind, payload_json
        FROM marketforge_room_mutations
        WHERE room_id = $1
        ORDER BY room_id, mutation_seq
        "#,
    )?
    .into_iter()
    .map(|row| {
        let mutation_seq: i64 = row.get("mutation_seq");
        let command_cursor: i64 = row.get("command_cursor");
        let schema_version: i32 = row.get("schema_version");
        let mutation_kind: String = row.get("mutation_kind");
        let payload_json: Value = row.get("payload_json");
        let schema_version = u16::try_from(schema_version).map_err(|_| {
            JournalError::Recovery(format!(
                "invalid room mutation schema version {schema_version}"
            ))
        })?;
        if schema_version != ROOM_MUTATION_SCHEMA_VERSION {
            return Err(JournalError::Recovery(format!(
                "unsupported room mutation schema version {schema_version}"
            )));
        }
        let mutation: RoomMutation =
            serde_json::from_value(payload_json).map_err(JournalError::Serialize)?;
        if mutation.kind_name() != mutation_kind {
            return Err(JournalError::Recovery(format!(
                "room mutation kind {mutation_kind} does not match payload kind {}",
                mutation.kind_name()
            )));
        }

        Ok(JournalMutation {
            room_id: row.get("room_id"),
            mutation_seq: u64::try_from(mutation_seq)
                .map_err(|_| JournalError::InvalidSequence(mutation_seq))?,
            command_cursor: u64::try_from(command_cursor)
                .map_err(|_| JournalError::InvalidSequence(command_cursor))?,
            schema_version,
            mutation,
        })
    })
    .collect::<Result<Vec<_>, JournalError>>()?;

    let snapshots = query_recovery_rows(
        client,
        room_id,
        r#"
        SELECT DISTINCT ON (room_id) room_id, command_seq, actor_json
        FROM marketforge_room_snapshots
        ORDER BY room_id, command_seq DESC
        "#,
        r#"
        SELECT DISTINCT ON (room_id) room_id, command_seq, actor_json
        FROM marketforge_room_snapshots
        WHERE room_id = $1
        ORDER BY room_id, command_seq DESC
        "#,
    )?
    .into_iter()
    .map(|row| {
        let command_seq: i64 = row.get("command_seq");
        let actor_json: Value = row.get("actor_json");
        let Some(actor) = deserialize_snapshot_actor(actor_json)? else {
            return Ok(None);
        };
        Ok(Some(JournalSnapshot {
            room_id: row.get("room_id"),
            command_seq: u64::try_from(command_seq)
                .map_err(|_| JournalError::InvalidSequence(command_seq))?,
            actor,
        }))
    })
    .collect::<Result<Vec<_>, JournalError>>()?
    .into_iter()
    .flatten()
    .collect();

    Ok(JournalRecovery {
        rooms,
        executions,
        mutations,
        snapshots,
    })
}

fn query_recovery_rows(
    client: &mut Client,
    room_id: Option<&str>,
    all_rooms_sql: &str,
    one_room_sql: &str,
) -> Result<Vec<postgres::Row>, JournalError> {
    match room_id {
        Some(room_id) => client
            .query(one_room_sql, &[&room_id])
            .map_err(JournalError::Postgres),
        None => client
            .query(all_rooms_sql, &[])
            .map_err(JournalError::Postgres),
    }
}

impl JournalStore for PostgresJournalStore {
    fn acquire_room_writer_lease(
        &mut self,
        room_id: &str,
        owner_id: &str,
        owner_url: Option<&str>,
        duration: Duration,
    ) -> Result<Option<RoomWriterLease>, JournalError> {
        let duration_ms = i64_from_u64(
            validate_room_lease_request(room_id, owner_id, duration)?,
            "lease_duration_ms",
        )?;
        validate_room_lease_owner_url(owner_url)?;
        let room_id = room_id.to_string();
        let owner_id = owner_id.to_string();
        let owner_url = owner_url.map(str::to_string);
        run_postgres(&mut self.client, move |client| {
            let row = client
                .query_opt(
                    r#"
                    INSERT INTO marketforge_room_writer_leases (
                        room_id, owner_id, owner_url, fencing_token, lease_expires_at
                    )
                    SELECT room_id, $2, $3, 1,
                           clock_timestamp() + ($4::bigint * interval '1 millisecond')
                    FROM marketforge_rooms
                    WHERE room_id = $1
                    ON CONFLICT (room_id)
                    DO UPDATE SET
                        owner_id = EXCLUDED.owner_id,
                        owner_url = EXCLUDED.owner_url,
                        fencing_token = marketforge_room_writer_leases.fencing_token + 1,
                        lease_expires_at = EXCLUDED.lease_expires_at,
                        updated_at = clock_timestamp()
                    WHERE marketforge_room_writer_leases.lease_expires_at <= clock_timestamp()
                    RETURNING owner_id, owner_url, fencing_token,
                              (extract(epoch FROM lease_expires_at) * 1000)::bigint
                                  AS expires_at_unix_ms
                    "#,
                    &[&room_id, &owner_id, &owner_url, &duration_ms],
                )
                .map_err(JournalError::Postgres)?;
            row.map(|row| postgres_room_writer_lease(&room_id, row))
                .transpose()
        })
    }

    fn current_room_writer_lease(
        &mut self,
        room_id: &str,
    ) -> Result<Option<RoomWriterLease>, JournalError> {
        let room_id = room_id.to_string();
        run_postgres(&mut self.client, move |client| {
            let row = client
                .query_opt(
                    r#"
                    SELECT owner_id, owner_url, fencing_token,
                           (extract(epoch FROM lease_expires_at) * 1000)::bigint
                               AS expires_at_unix_ms
                    FROM marketforge_room_writer_leases
                    WHERE room_id = $1
                      AND lease_expires_at > clock_timestamp()
                    "#,
                    &[&room_id],
                )
                .map_err(JournalError::Postgres)?;
            row.map(|row| postgres_room_writer_lease(&room_id, row))
                .transpose()
        })
    }

    fn query_room_routes(
        &mut self,
        user_id: &str,
        after_room_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RoomRoutingRecord>, JournalError> {
        let user_id = user_id.to_string();
        let after_room_id = after_room_id.map(str::to_string);
        let limit =
            i64::try_from(limit.clamp(1, 501)).map_err(|_| JournalError::CountOutOfRange(limit))?;
        run_postgres(&mut self.client, move |client| {
            client
                .query(
                    r#"
                    SELECT room.room_id,
                           lease.owner_id,
                           lease.owner_url,
                           lease.fencing_token,
                           (extract(epoch FROM lease.lease_expires_at) * 1000)::bigint
                               AS expires_at_unix_ms
                    FROM marketforge_rooms room
                    INNER JOIN marketforge_room_members member
                        ON member.room_id = room.room_id
                       AND member.user_id = $1
                    LEFT JOIN marketforge_room_writer_leases lease
                        ON lease.room_id = room.room_id
                       AND lease.lease_expires_at > clock_timestamp()
                    WHERE ($2::text IS NULL OR room.room_id > $2)
                    ORDER BY room.room_id ASC
                    LIMIT $3
                    "#,
                    &[&user_id, &after_room_id, &limit],
                )
                .map_err(JournalError::Postgres)?
                .into_iter()
                .map(postgres_room_routing_record)
                .collect()
        })
    }

    fn renew_room_writer_lease(
        &mut self,
        claim: &RoomLeaseClaim,
        duration: Duration,
    ) -> Result<Option<RoomWriterLease>, JournalError> {
        let duration_ms = i64_from_u64(
            validate_room_lease_request(&claim.room_id, &claim.owner_id, duration)?,
            "lease_duration_ms",
        )?;
        validate_room_lease_claim(claim)?;
        let fencing_token = i64_from_u64(claim.fencing_token, "fencing_token")?;
        let claim = claim.clone();
        run_postgres(&mut self.client, move |client| {
            let row = client
                .query_opt(
                    r#"
                    UPDATE marketforge_room_writer_leases
                    SET lease_expires_at =
                            clock_timestamp() + ($4::bigint * interval '1 millisecond'),
                        updated_at = clock_timestamp()
                    WHERE room_id = $1
                      AND owner_id = $2
                      AND fencing_token = $3
                      AND lease_expires_at > clock_timestamp()
                    RETURNING owner_id, owner_url, fencing_token,
                              (extract(epoch FROM lease_expires_at) * 1000)::bigint
                                  AS expires_at_unix_ms
                    "#,
                    &[
                        &claim.room_id,
                        &claim.owner_id,
                        &fencing_token,
                        &duration_ms,
                    ],
                )
                .map_err(JournalError::Postgres)?;
            row.map(|row| postgres_room_writer_lease(&claim.room_id, row))
                .transpose()
        })
    }

    fn release_room_writer_lease(&mut self, claim: &RoomLeaseClaim) -> Result<bool, JournalError> {
        validate_room_lease_claim(claim)?;
        let fencing_token = i64_from_u64(claim.fencing_token, "fencing_token")?;
        let claim = claim.clone();
        run_postgres(&mut self.client, move |client| {
            let updated = client
                .execute(
                    r#"
                    UPDATE marketforge_room_writer_leases
                    SET lease_expires_at = clock_timestamp(),
                        updated_at = clock_timestamp()
                    WHERE room_id = $1
                      AND owner_id = $2
                      AND fencing_token = $3
                    "#,
                    &[&claim.room_id, &claim.owner_id, &fencing_token],
                )
                .map_err(JournalError::Postgres)?;
            Ok(updated == 1)
        })
    }

    fn load_recovery(&mut self) -> Result<JournalRecovery, JournalError> {
        run_postgres(&mut self.client, |client| {
            load_postgres_recovery(client, None)
        })
    }

    fn load_room_recovery(&mut self, room_id: &str) -> Result<JournalRecovery, JournalError> {
        let room_id = room_id.to_string();
        run_postgres(&mut self.client, move |client| {
            load_postgres_recovery(client, Some(&room_id))
        })
    }

    fn health_check(&mut self) -> Result<(), JournalError> {
        if self.client.is_closed() {
            if self.runtime_lock != PostgresRuntimeLock::None {
                return Err(JournalError::RuntimeLockLost);
            }
            self.client =
                Client::connect(&self.database_url, NoTls).map_err(JournalError::Postgres)?;
        }
        self.client.simple_query("SELECT 1").map_err(|error| {
            if self.runtime_lock != PostgresRuntimeLock::None {
                JournalError::RuntimeLockLost
            } else {
                JournalError::Postgres(error)
            }
        })?;
        Ok(())
    }

    fn query_executions(
        &mut self,
        room_id: &str,
        after_command_seq: Option<u64>,
        from_start: bool,
        limit: usize,
    ) -> Result<ExecutionPage, JournalError> {
        let room_id = room_id.to_string();
        let after_command_seq = after_command_seq
            .map(|value| i64_from_u64(value, "after_command_seq"))
            .transpose()?;
        let limit = limit.clamp(1, 500);
        let limit_i64 = i64::try_from(limit).map_err(|_| JournalError::CountOutOfRange(limit))?;
        let paged_limit = limit
            .checked_add(1)
            .ok_or(JournalError::CountOutOfRange(limit))?;
        let paged_limit_i64 =
            i64::try_from(paged_limit).map_err(|_| JournalError::CountOutOfRange(paged_limit))?;

        run_postgres(&mut self.client, move |client| {
            let latest: Option<i64> = client
                .query_one(
                    "SELECT MAX(command_seq) FROM marketforge_executions WHERE room_id = $1",
                    &[&room_id],
                )
                .map_err(JournalError::Postgres)?
                .get(0);
            let latest_command_seq = latest
                .map(|value| u64::try_from(value).map_err(|_| JournalError::InvalidSequence(value)))
                .transpose()?;

            let (rows, ascending_page) = if let Some(after_command_seq) = after_command_seq {
                (
                    client
                        .query(
                            r#"
                            SELECT execution_json
                            FROM marketforge_executions
                            WHERE room_id = $1 AND command_seq > $2
                            ORDER BY command_seq ASC
                            LIMIT $3
                            "#,
                            &[&room_id, &after_command_seq, &paged_limit_i64],
                        )
                        .map_err(JournalError::Postgres)?,
                    true,
                )
            } else if from_start {
                (
                    client
                        .query(
                            r#"
                            SELECT execution_json
                            FROM marketforge_executions
                            WHERE room_id = $1
                            ORDER BY command_seq ASC
                            LIMIT $2
                            "#,
                            &[&room_id, &paged_limit_i64],
                        )
                        .map_err(JournalError::Postgres)?,
                    true,
                )
            } else {
                (
                    client
                        .query(
                            r#"
                            SELECT execution_json
                            FROM marketforge_executions
                            WHERE room_id = $1
                            ORDER BY command_seq DESC
                            LIMIT $2
                            "#,
                            &[&room_id, &limit_i64],
                        )
                        .map_err(JournalError::Postgres)?,
                    false,
                )
            };

            let mut executions = rows
                .into_iter()
                .map(|row| {
                    let execution_json: Value = row.get("execution_json");
                    serde_json::from_value(execution_json).map_err(JournalError::Serialize)
                })
                .collect::<Result<Vec<RoomExecutionSummary>, JournalError>>()?;
            let has_more = ascending_page && executions.len() > limit;
            if has_more {
                executions.truncate(limit);
            }
            if !ascending_page {
                executions.reverse();
            }
            Ok(ExecutionPage {
                executions,
                latest_command_seq,
                has_more,
            })
        })
    }

    fn find_idempotent_execution(
        &mut self,
        user_id: &str,
        room_id: &str,
        idempotency_key: &str,
    ) -> Result<Option<JournalExecution>, JournalError> {
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        let idempotency_key = idempotency_key.to_string();
        run_postgres(&mut self.client, move |client| {
            let Some(row) = client
                .query_opt(
                    r#"
                    SELECT room_id, command_seq, participant_id, account_id,
                           request_user_id, idempotency_key, request_fingerprint,
                           command_json, execution_json
                    FROM marketforge_executions
                    WHERE room_id = $1
                      AND request_user_id = $2
                      AND idempotency_key = $3
                    "#,
                    &[&room_id, &user_id, &idempotency_key],
                )
                .map_err(JournalError::Postgres)?
            else {
                return Ok(None);
            };

            let command_seq: i64 = row.get("command_seq");
            let account_id: Option<String> = row.get("account_id");
            let command_json: Value = row.get("command_json");
            let execution_json: Value = row.get("execution_json");
            Ok(Some(JournalExecution {
                room_id: row.get("room_id"),
                command_seq: u64::try_from(command_seq)
                    .map_err(|_| JournalError::InvalidSequence(command_seq))?,
                participant_id: row.get("participant_id"),
                account_id: account_id
                    .map(|id| id.parse().map_err(|_| JournalError::InvalidAccountId(id)))
                    .transpose()?,
                request_user_id: row.get("request_user_id"),
                idempotency_key: row.get("idempotency_key"),
                request_fingerprint: row.get("request_fingerprint"),
                command: serde_json::from_value(command_json).map_err(JournalError::Serialize)?,
                execution: serde_json::from_value(execution_json)
                    .map_err(JournalError::Serialize)?,
            }))
        })
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
        self.create_room_transaction(
            owner_user_id,
            scenario,
            bootstrap,
            account_ids,
            seed_records,
            initial_snapshot,
            None,
        )
        .map(|_| ())
    }

    fn create_room_with_writer_lease(
        &mut self,
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
        let duration_ms =
            validate_room_lease_request(&bootstrap.room_id, writer_owner_id, lease_duration)?;
        validate_room_lease_owner_url(writer_owner_url)?;
        self.create_room_transaction(
            owner_user_id,
            scenario,
            bootstrap,
            account_ids,
            seed_records,
            initial_snapshot,
            Some((writer_owner_id, writer_owner_url, duration_ms)),
        )?
        .ok_or_else(|| {
            JournalError::Recovery(format!(
                "new room {} did not create its initial writer lease",
                bootstrap.room_id
            ))
        })
    }

    fn append_executions_fenced(
        &mut self,
        claim: &RoomLeaseClaim,
        records: &[JournalExecution],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        validate_fenced_execution_batch(claim, records, snapshot)?;
        let claim = claim.clone();
        let records = records.to_vec();
        let snapshot = snapshot.cloned();
        run_postgres(&mut self.client, move |client| {
            let mut tx = client.transaction().map_err(JournalError::Postgres)?;
            Self::assert_room_write_fence(&mut tx, &claim)?;
            for record in &records {
                Self::insert_execution(&mut tx, record)?;
            }
            if let Some(snapshot) = &snapshot {
                Self::insert_snapshot(&mut tx, snapshot)?;
            }
            tx.commit().map_err(JournalError::Postgres)
        })
    }

    fn append_executions(
        &mut self,
        records: &[JournalExecution],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        let records = records.to_vec();
        let snapshot = snapshot.cloned();
        run_postgres(&mut self.client, move |client| {
            let mut tx = client.transaction().map_err(JournalError::Postgres)?;
            for record in &records {
                Self::insert_execution(&mut tx, record)?;
            }
            if let Some(snapshot) = &snapshot {
                Self::insert_snapshot(&mut tx, snapshot)?;
            }
            tx.commit().map_err(JournalError::Postgres)
        })
    }

    fn append_transfers(
        &mut self,
        records: &[JournalTransfer],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        let records = records.to_vec();
        let snapshot = snapshot.cloned();
        run_postgres(&mut self.client, move |client| {
            let mut tx = client.transaction().map_err(JournalError::Postgres)?;
            for record in &records {
                Self::insert_transfer(&mut tx, record)?;
            }
            if let Some(snapshot) = &snapshot {
                Self::insert_snapshot(&mut tx, snapshot)?;
            }
            tx.commit().map_err(JournalError::Postgres)
        })
    }

    fn append_snapshot(&mut self, snapshot: &JournalSnapshot) -> Result<(), JournalError> {
        let snapshot = snapshot.clone();
        run_postgres(&mut self.client, move |client| {
            let mut tx = client.transaction().map_err(JournalError::Postgres)?;
            Self::insert_snapshot(&mut tx, &snapshot)?;
            tx.commit().map_err(JournalError::Postgres)
        })
    }

    fn append_room_mutation(
        &mut self,
        mutation: &PendingJournalMutation,
        execution_records: &[JournalExecution],
        transfer_records: &[JournalTransfer],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        validate_pending_mutation(mutation)?;
        validate_mutation_execution_records(mutation, execution_records)?;
        for record in transfer_records {
            validate_transfer(record)?;
            if record.room_id != mutation.room_id {
                return Err(JournalError::Recovery(format!(
                    "transfer room {} does not match mutation room {}",
                    record.room_id, mutation.room_id
                )));
            }
        }
        if let Some(snapshot) = snapshot {
            validate_snapshot(snapshot)?;
            if snapshot.room_id != mutation.room_id {
                return Err(JournalError::Recovery(format!(
                    "snapshot room {} does not match mutation room {}",
                    snapshot.room_id, mutation.room_id
                )));
            }
            if let Some(last_execution) = execution_records.last()
                && snapshot.command_seq < last_execution.command_seq
            {
                return Err(JournalError::Recovery(format!(
                    "snapshot command sequence {} precedes mutation execution sequence {}",
                    snapshot.command_seq, last_execution.command_seq
                )));
            }
        }

        let mutation = mutation.clone();
        let execution_records = execution_records.to_vec();
        let transfer_records = transfer_records.to_vec();
        let snapshot = snapshot.cloned();
        run_postgres(&mut self.client, move |client| {
            let mut tx = client.transaction().map_err(JournalError::Postgres)?;
            Self::insert_room_mutation(&mut tx, &mutation)?;
            for record in &execution_records {
                Self::insert_execution(&mut tx, record)?;
            }
            for record in &transfer_records {
                Self::insert_transfer(&mut tx, record)?;
            }
            if let Some(snapshot) = &snapshot {
                Self::insert_snapshot(&mut tx, snapshot)?;
            }
            if let RoomMutation::StatusChanged { status } = &mutation.mutation {
                tx.execute(
                    r#"
                    UPDATE marketforge_rooms
                    SET status = $2, updated_at = now()
                    WHERE room_id = $1
                    "#,
                    &[&mutation.room_id, &status_name(*status)],
                )
                .map_err(JournalError::Postgres)?;
            }
            tx.commit().map_err(JournalError::Postgres)
        })
    }

    fn append_room_mutation_fenced(
        &mut self,
        claim: &RoomLeaseClaim,
        mutation: &PendingJournalMutation,
        execution_records: &[JournalExecution],
        transfer_records: &[JournalTransfer],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        validate_fenced_room_mutation(claim, mutation)?;
        validate_pending_mutation(mutation)?;
        validate_mutation_execution_records(mutation, execution_records)?;
        for record in transfer_records {
            validate_transfer(record)?;
            if record.room_id != mutation.room_id {
                return Err(JournalError::Recovery(format!(
                    "transfer room {} does not match mutation room {}",
                    record.room_id, mutation.room_id
                )));
            }
        }
        if let Some(snapshot) = snapshot {
            validate_snapshot(snapshot)?;
            if snapshot.room_id != mutation.room_id {
                return Err(JournalError::Recovery(format!(
                    "snapshot room {} does not match mutation room {}",
                    snapshot.room_id, mutation.room_id
                )));
            }
            if let Some(last_execution) = execution_records.last()
                && snapshot.command_seq < last_execution.command_seq
            {
                return Err(JournalError::Recovery(format!(
                    "snapshot command sequence {} precedes mutation execution sequence {}",
                    snapshot.command_seq, last_execution.command_seq
                )));
            }
        }

        let claim = claim.clone();
        let mutation = mutation.clone();
        let execution_records = execution_records.to_vec();
        let transfer_records = transfer_records.to_vec();
        let snapshot = snapshot.cloned();
        run_postgres(&mut self.client, move |client| {
            let mut tx = client.transaction().map_err(JournalError::Postgres)?;
            Self::assert_room_write_fence(&mut tx, &claim)?;
            Self::insert_room_mutation(&mut tx, &mutation)?;
            for record in &execution_records {
                Self::insert_execution(&mut tx, record)?;
            }
            for record in &transfer_records {
                Self::insert_transfer(&mut tx, record)?;
            }
            if let Some(snapshot) = &snapshot {
                Self::insert_snapshot(&mut tx, snapshot)?;
            }
            if let RoomMutation::StatusChanged { status } = &mutation.mutation {
                tx.execute(
                    r#"
                    UPDATE marketforge_rooms
                    SET status = $2, updated_at = now()
                    WHERE room_id = $1
                    "#,
                    &[&mutation.room_id, &status_name(*status)],
                )
                .map_err(JournalError::Postgres)?;
            }
            tx.commit().map_err(JournalError::Postgres)
        })
    }

    fn update_room_status(
        &mut self,
        room_id: &str,
        status: MarketStatus,
    ) -> Result<(), JournalError> {
        let room_id = room_id.to_string();
        run_postgres(&mut self.client, move |client| {
            client
                .execute(
                    r#"
                UPDATE marketforge_rooms
                SET status = $2, updated_at = now()
                WHERE room_id = $1
                "#,
                    &[&room_id, &status_name(status)],
                )
                .map_err(JournalError::Postgres)?;
            Ok(())
        })
    }

    fn upsert_room_member(
        &mut self,
        room_id: &str,
        user_id: &str,
        role: &str,
    ) -> Result<(), JournalError> {
        if !is_known_member_role(role) {
            return Err(JournalError::Recovery(format!("invalid role {role}")));
        }
        let room_id = room_id.to_string();
        let user_id = user_id.to_string();
        let role = role.to_string();
        run_postgres(&mut self.client, move |client| {
            client
                .execute(
                    r#"
                    INSERT INTO marketforge_users (user_id)
                    VALUES ($1)
                    ON CONFLICT (user_id) DO NOTHING
                    "#,
                    &[&user_id],
                )
                .map_err(JournalError::Postgres)?;
            client
                .execute(
                    r#"
                    INSERT INTO marketforge_room_members (room_id, user_id, role)
                    VALUES ($1, $2, $3)
                    ON CONFLICT (room_id, user_id)
                    DO UPDATE SET role = EXCLUDED.role
                    "#,
                    &[&room_id, &user_id, &role],
                )
                .map_err(JournalError::Postgres)?;
            Ok(())
        })
    }

    fn remove_room_member(&mut self, room_id: &str, user_id: &str) -> Result<(), JournalError> {
        let room_id = room_id.to_string();
        let user_id = user_id.to_string();
        run_postgres(&mut self.client, move |client| {
            client
                .execute(
                    r#"
                    DELETE FROM marketforge_account_owners
                    WHERE room_id = $1 AND user_id = $2
                    "#,
                    &[&room_id, &user_id],
                )
                .map_err(JournalError::Postgres)?;
            client
                .execute(
                    r#"
                    DELETE FROM marketforge_room_members
                    WHERE room_id = $1 AND user_id = $2
                    "#,
                    &[&room_id, &user_id],
                )
                .map_err(JournalError::Postgres)?;
            Ok(())
        })
    }

    fn assign_account_owner(
        &mut self,
        room_id: &str,
        account_id: AccountId,
        user_id: &str,
    ) -> Result<(), JournalError> {
        let room_id = room_id.to_string();
        let user_id = user_id.to_string();
        let account_id = i64_from_u64(account_id, "account_id")?;
        run_postgres(&mut self.client, move |client| {
            let role: Option<String> = client
                .query_opt(
                    r#"
                    SELECT role
                    FROM marketforge_room_members
                    WHERE room_id = $1 AND user_id = $2
                    "#,
                    &[&room_id, &user_id],
                )
                .map_err(JournalError::Postgres)?
                .map(|row| row.get(0));
            match role.as_deref() {
                Some(role) if is_account_holder_role(role) => {}
                Some(role) => {
                    return Err(JournalError::Recovery(format!(
                        "role {role} cannot be assigned an account"
                    )));
                }
                None => {
                    return Err(JournalError::Recovery(format!(
                        "user {user_id} is not a member of {room_id}"
                    )));
                }
            }
            client
                .execute(
                    r#"
                    INSERT INTO marketforge_account_owners (room_id, account_id, user_id)
                    VALUES ($1, $2, $3)
                    ON CONFLICT (room_id, account_id, user_id) DO NOTHING
                    "#,
                    &[&room_id, &account_id, &user_id],
                )
                .map_err(JournalError::Postgres)?;
            Ok(())
        })
    }

    fn user_room_role(
        &mut self,
        user_id: &str,
        room_id: &str,
    ) -> Result<Option<String>, JournalError> {
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        run_postgres(&mut self.client, move |client| {
            Ok(client
                .query_opt(
                    r#"
                    SELECT role
                    FROM marketforge_room_members
                    WHERE room_id = $1 AND user_id = $2
                    "#,
                    &[&room_id, &user_id],
                )
                .map_err(JournalError::Postgres)?
                .map(|row| row.get(0)))
        })
    }

    fn user_can_access_room(&mut self, user_id: &str, room_id: &str) -> Result<bool, JournalError> {
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        run_postgres(&mut self.client, move |client| {
            let count: i64 = client
                .query_one(
                    r#"
                    SELECT count(*)
                    FROM marketforge_room_members
                    WHERE room_id = $1 AND user_id = $2
                    "#,
                    &[&room_id, &user_id],
                )
                .map_err(JournalError::Postgres)?
                .get(0);
            Ok(count > 0)
        })
    }

    fn user_can_administer_room(
        &mut self,
        user_id: &str,
        room_id: &str,
    ) -> Result<bool, JournalError> {
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        run_postgres(&mut self.client, move |client| {
            let count: i64 = client
                .query_one(
                    r#"
                    SELECT count(*)
                    FROM marketforge_room_members
                    WHERE room_id = $1
                      AND user_id = $2
                      AND role IN ('owner', 'admin')
                    "#,
                    &[&room_id, &user_id],
                )
                .map_err(JournalError::Postgres)?
                .get(0);
            Ok(count > 0)
        })
    }

    fn user_can_access_account(
        &mut self,
        user_id: &str,
        room_id: &str,
        account_id: AccountId,
    ) -> Result<bool, JournalError> {
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        let account_id = i64_from_u64(account_id, "account_id")?;
        run_postgres(&mut self.client, move |client| {
            let count: i64 = client
                .query_one(
                    r#"
                    SELECT count(*)
                    FROM marketforge_room_members member
                    WHERE member.room_id = $1
                      AND member.user_id = $2
                      AND (
                          member.role IN ('owner', 'admin')
                          OR (
                              member.role IN ('instructor', 'trader')
                              AND EXISTS (
                                  SELECT 1
                                  FROM marketforge_account_owners owner
                                  WHERE owner.room_id = $1
                                    AND owner.account_id = $3
                                    AND owner.user_id = $2
                              )
                          )
                      )
                    "#,
                    &[&room_id, &user_id, &account_id],
                )
                .map_err(JournalError::Postgres)?
                .get(0);
            Ok(count > 0)
        })
    }

    fn query_orders(
        &mut self,
        user_id: &str,
        room_id: &str,
        instrument_id: Option<&str>,
        account_id: Option<AccountId>,
        limit: usize,
    ) -> Result<Vec<OrderProjection>, JournalError> {
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        let instrument_id = instrument_id.map(str::to_string);
        let account_id = optional_i64_account_id(account_id)?;
        let limit = bounded_query_limit(limit)?;
        run_postgres(&mut self.client, move |client| {
            client
                .query(
                    r#"
                    SELECT room_id, instrument_id, order_id, account_id, participant_id, side, order_type,
                           limit_price_tick, original_qty, status, remaining_qty,
                           created_command_seq, updated_command_seq,
                           created_market_time_ms, updated_market_time_ms
                    FROM marketforge_orders
                    WHERE room_id = $1
                      AND ($2::TEXT IS NULL OR instrument_id = $2)
                      AND ($3::BIGINT IS NULL OR account_id = $3)
                      AND (
                          EXISTS (
                              SELECT 1
                              FROM marketforge_room_members member
                              WHERE member.room_id = marketforge_orders.room_id
                                AND member.user_id = $5
                                AND member.role IN ('owner', 'admin')
                          )
                          OR (
                              EXISTS (
                                  SELECT 1
                                  FROM marketforge_account_owners owner
                                  WHERE owner.room_id = marketforge_orders.room_id
                                    AND owner.account_id = marketforge_orders.account_id
                                    AND owner.user_id = $5
                              )
                              AND EXISTS (
                                  SELECT 1
                                  FROM marketforge_room_members member
                                  WHERE member.room_id = marketforge_orders.room_id
                                    AND member.user_id = $5
                                    AND member.role IN ('instructor', 'trader')
                              )
                          )
                      )
                    ORDER BY updated_command_seq DESC, order_id DESC
                    LIMIT $4
                    "#,
                    &[&room_id, &instrument_id, &account_id, &limit, &user_id],
                )
                .map_err(JournalError::Postgres)?
                .into_iter()
                .map(|row| {
                    Ok(OrderProjection {
                        room_id: row.get("room_id"),
                        instrument_id: row.get("instrument_id"),
                        order_id: row.get("order_id"),
                        account_id: row.get("account_id"),
                        participant_id: row.get("participant_id"),
                        side: row.get("side"),
                        order_type: row.get("order_type"),
                        limit_price_tick: row.get("limit_price_tick"),
                        original_qty: row.get("original_qty"),
                        status: row.get("status"),
                        remaining_qty: row.get("remaining_qty"),
                        created_command_seq: row.get("created_command_seq"),
                        updated_command_seq: row.get("updated_command_seq"),
                        created_market_time_ms: row.get("created_market_time_ms"),
                        updated_market_time_ms: row.get("updated_market_time_ms"),
                    })
                })
                .collect()
        })
    }

    fn query_trades(
        &mut self,
        user_id: &str,
        room_id: &str,
        instrument_id: Option<&str>,
        account_id: Option<AccountId>,
        limit: usize,
    ) -> Result<Vec<TradeProjection>, JournalError> {
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        let instrument_id = instrument_id.map(str::to_string);
        let account_id = optional_i64_account_id(account_id)?;
        let limit = bounded_query_limit(limit)?;
        run_postgres(&mut self.client, move |client| {
            client
                .query(
                    r#"
                    SELECT room_id, instrument_id, trade_id, command_seq, event_seq, maker_order_id,
                           maker_account_id, taker_order_id, taker_account_id,
                           price_tick, qty, taker_side, market_time_ms
                    FROM marketforge_trades
                    WHERE room_id = $1
                      AND ($2::TEXT IS NULL OR instrument_id = $2)
                      AND ($3::BIGINT IS NULL OR maker_account_id = $3 OR taker_account_id = $3)
                      AND (
                          EXISTS (
                              SELECT 1
                              FROM marketforge_room_members member
                              WHERE member.room_id = marketforge_trades.room_id
                                AND member.user_id = $5
                                AND member.role IN ('owner', 'admin')
                          )
                          OR (
                              EXISTS (
                                  SELECT 1
                                  FROM marketforge_account_owners owner
                                  WHERE owner.room_id = marketforge_trades.room_id
                                    AND owner.user_id = $5
                                    AND owner.account_id IN (maker_account_id, taker_account_id)
                              )
                              AND EXISTS (
                                  SELECT 1
                                  FROM marketforge_room_members member
                                  WHERE member.room_id = marketforge_trades.room_id
                                    AND member.user_id = $5
                                    AND member.role IN ('instructor', 'trader')
                              )
                          )
                      )
                    ORDER BY command_seq DESC, event_seq DESC
                    LIMIT $4
                    "#,
                    &[&room_id, &instrument_id, &account_id, &limit, &user_id],
                )
                .map_err(JournalError::Postgres)?
                .into_iter()
                .map(|row| {
                    Ok(TradeProjection {
                        room_id: row.get("room_id"),
                        instrument_id: row.get("instrument_id"),
                        trade_id: row.get("trade_id"),
                        command_seq: row.get("command_seq"),
                        event_seq: row.get("event_seq"),
                        market_time_ms: row.get("market_time_ms"),
                        maker_order_id: row.get("maker_order_id"),
                        maker_account_id: row.get("maker_account_id"),
                        taker_order_id: row.get("taker_order_id"),
                        taker_account_id: row.get("taker_account_id"),
                        price_tick: row.get("price_tick"),
                        qty: row.get("qty"),
                        taker_side: row.get("taker_side"),
                    })
                })
                .collect()
        })
    }

    fn query_market_ticks(
        &mut self,
        user_id: &str,
        room_id: &str,
        instrument_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MarketTickProjection>, JournalError> {
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        let instrument_id = instrument_id.map(str::to_string);
        let limit = bounded_query_limit(limit)?;
        run_postgres(&mut self.client, move |client| {
            client
                .query(
                    r#"
                    SELECT room_id, instrument_id, command_seq, event_seq, trade_id, price_tick, qty,
                           taker_side, market_time_ms
                    FROM marketforge_market_ticks
                    WHERE room_id = $1
                      AND ($2::TEXT IS NULL OR instrument_id = $2)
                      AND EXISTS (
                          SELECT 1
                          FROM marketforge_room_members member
                          WHERE member.room_id = marketforge_market_ticks.room_id
                            AND member.user_id = $4
                      )
                    ORDER BY command_seq DESC, event_seq DESC
                    LIMIT $3
                    "#,
                    &[&room_id, &instrument_id, &limit, &user_id],
                )
                .map_err(JournalError::Postgres)?
                .into_iter()
                .map(|row| {
                    Ok(MarketTickProjection {
                        room_id: row.get("room_id"),
                        instrument_id: row.get("instrument_id"),
                        command_seq: row.get("command_seq"),
                        event_seq: row.get("event_seq"),
                        market_time_ms: row.get("market_time_ms"),
                        trade_id: row.get("trade_id"),
                        price_tick: row.get("price_tick"),
                        qty: row.get("qty"),
                        taker_side: row.get("taker_side"),
                    })
                })
                .collect()
        })
    }

    fn query_account_ledger(
        &mut self,
        user_id: &str,
        room_id: &str,
        instrument_id: Option<&str>,
        account_id: Option<AccountId>,
        limit: usize,
    ) -> Result<Vec<AccountLedgerProjection>, JournalError> {
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        let instrument_id = instrument_id.map(str::to_string);
        let account_id = optional_i64_account_id(account_id)?;
        let limit = bounded_query_limit(limit)?;
        run_postgres(&mut self.client, move |client| {
            client
                .query(
                    r#"
                    SELECT room_id, instrument_id, command_seq, ledger_seq, market_kind, account_id, trade_id,
                           account_side, cash_delta, position_delta, fee, realized_pnl,
                           price_tick, qty, notional, cash_balance, position_qty,
                           avg_entry_price_tick, realized_pnl_total, unrealized_pnl,
                           equity, initial_margin, maintenance_margin,
                           portfolio_initial_margin, portfolio_maintenance_margin,
                           margin_status, fees_paid
                    FROM marketforge_account_ledger
                    WHERE room_id = $1
                      AND ($2::TEXT IS NULL OR instrument_id = $2)
                      AND ($3::BIGINT IS NULL OR account_id = $3)
                      AND (
                          EXISTS (
                              SELECT 1
                              FROM marketforge_room_members member
                              WHERE member.room_id = marketforge_account_ledger.room_id
                                AND member.user_id = $5
                                AND member.role IN ('owner', 'admin')
                          )
                          OR (
                              EXISTS (
                                  SELECT 1
                                  FROM marketforge_account_owners owner
                                  WHERE owner.room_id = marketforge_account_ledger.room_id
                                    AND owner.account_id = marketforge_account_ledger.account_id
                                    AND owner.user_id = $5
                              )
                              AND EXISTS (
                                  SELECT 1
                                  FROM marketforge_room_members member
                                  WHERE member.room_id = marketforge_account_ledger.room_id
                                    AND member.user_id = $5
                                    AND member.role IN ('instructor', 'trader')
                              )
                          )
                      )
                    ORDER BY command_seq DESC, ledger_seq DESC
                    LIMIT $4
                    "#,
                    &[&room_id, &instrument_id, &account_id, &limit, &user_id],
                )
                .map_err(JournalError::Postgres)?
                .into_iter()
                .map(|row| {
                    Ok(AccountLedgerProjection {
                        room_id: row.get("room_id"),
                        instrument_id: row.get("instrument_id"),
                        command_seq: row.get("command_seq"),
                        ledger_seq: row.get("ledger_seq"),
                        market_kind: row.get("market_kind"),
                        account_id: row.get("account_id"),
                        trade_id: row.get("trade_id"),
                        account_side: row.get("account_side"),
                        cash_delta: row.get("cash_delta"),
                        position_delta: row.get("position_delta"),
                        fee: row.get("fee"),
                        realized_pnl: row.get("realized_pnl"),
                        price_tick: row.get("price_tick"),
                        qty: row.get("qty"),
                        notional: row.get("notional"),
                        cash_balance: row.get("cash_balance"),
                        position_qty: row.get("position_qty"),
                        avg_entry_price_tick: row.get("avg_entry_price_tick"),
                        realized_pnl_total: row.get("realized_pnl_total"),
                        unrealized_pnl: row.get("unrealized_pnl"),
                        equity: row.get("equity"),
                        initial_margin: row.get("initial_margin"),
                        maintenance_margin: row.get("maintenance_margin"),
                        portfolio_initial_margin: row.get("portfolio_initial_margin"),
                        portfolio_maintenance_margin: row.get("portfolio_maintenance_margin"),
                        margin_status: row.get("margin_status"),
                        fees_paid: row.get("fees_paid"),
                    })
                })
                .collect()
        })
    }

    fn query_position_snapshots(
        &mut self,
        user_id: &str,
        room_id: &str,
        instrument_id: Option<&str>,
        account_id: Option<AccountId>,
        limit: usize,
    ) -> Result<Vec<PositionSnapshotProjection>, JournalError> {
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        let instrument_id = instrument_id.map(str::to_string);
        let account_id = optional_i64_account_id(account_id)?;
        let limit = bounded_query_limit(limit)?;
        run_postgres(&mut self.client, move |client| {
            client
                .query(
                    r#"
                    SELECT room_id, instrument_id, command_seq, ledger_seq, market_kind, account_id, trade_id,
                           cash_balance, position_qty, avg_entry_price_tick, realized_pnl,
                           unrealized_pnl, equity, initial_margin, maintenance_margin,
                           portfolio_initial_margin, portfolio_maintenance_margin,
                           margin_status, fees_paid
                    FROM marketforge_position_snapshots
                    WHERE room_id = $1
                      AND ($2::TEXT IS NULL OR instrument_id = $2)
                      AND ($3::BIGINT IS NULL OR account_id = $3)
                      AND (
                          EXISTS (
                              SELECT 1
                              FROM marketforge_room_members member
                              WHERE member.room_id = marketforge_position_snapshots.room_id
                                AND member.user_id = $5
                                AND member.role IN ('owner', 'admin')
                          )
                          OR (
                              EXISTS (
                                  SELECT 1
                                  FROM marketforge_account_owners owner
                                  WHERE owner.room_id = marketforge_position_snapshots.room_id
                                    AND owner.account_id = marketforge_position_snapshots.account_id
                                    AND owner.user_id = $5
                              )
                              AND EXISTS (
                                  SELECT 1
                                  FROM marketforge_room_members member
                                  WHERE member.room_id = marketforge_position_snapshots.room_id
                                    AND member.user_id = $5
                                    AND member.role IN ('instructor', 'trader')
                              )
                          )
                      )
                    ORDER BY command_seq DESC, ledger_seq DESC
                    LIMIT $4
                    "#,
                    &[&room_id, &instrument_id, &account_id, &limit, &user_id],
                )
                .map_err(JournalError::Postgres)?
                .into_iter()
                .map(|row| {
                    Ok(PositionSnapshotProjection {
                        room_id: row.get("room_id"),
                        instrument_id: row.get("instrument_id"),
                        command_seq: row.get("command_seq"),
                        ledger_seq: row.get("ledger_seq"),
                        market_kind: row.get("market_kind"),
                        account_id: row.get("account_id"),
                        trade_id: row.get("trade_id"),
                        cash_balance: row.get("cash_balance"),
                        position_qty: row.get("position_qty"),
                        avg_entry_price_tick: row.get("avg_entry_price_tick"),
                        realized_pnl: row.get("realized_pnl"),
                        unrealized_pnl: row.get("unrealized_pnl"),
                        equity: row.get("equity"),
                        initial_margin: row.get("initial_margin"),
                        maintenance_margin: row.get("maintenance_margin"),
                        portfolio_initial_margin: row.get("portfolio_initial_margin"),
                        portfolio_maintenance_margin: row.get("portfolio_maintenance_margin"),
                        margin_status: row.get("margin_status"),
                        fees_paid: row.get("fees_paid"),
                    })
                })
                .collect()
        })
    }

    fn query_transfers(
        &mut self,
        user_id: &str,
        room_id: &str,
        account_id: Option<AccountId>,
        limit: usize,
    ) -> Result<Vec<VenueTransfer>, JournalError> {
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        let account_id = optional_i64_account_id(account_id)?;
        let limit = bounded_query_limit(limit)?;
        run_postgres(&mut self.client, move |client| {
            client
                .query(
                    r#"
                    SELECT payload_json
                    FROM marketforge_transfers
                    WHERE room_id = $1
                      AND ($2::BIGINT IS NULL OR account_id = $2)
                      AND (
                          EXISTS (
                              SELECT 1
                              FROM marketforge_room_members member
                              WHERE member.room_id = marketforge_transfers.room_id
                                AND member.user_id = $4
                                AND member.role IN ('owner', 'admin')
                          )
                          OR (
                              EXISTS (
                                  SELECT 1
                                  FROM marketforge_account_owners owner
                                  WHERE owner.room_id = marketforge_transfers.room_id
                                    AND owner.account_id = marketforge_transfers.account_id
                                    AND owner.user_id = $4
                              )
                              AND EXISTS (
                                  SELECT 1
                                  FROM marketforge_room_members member
                                  WHERE member.room_id = marketforge_transfers.room_id
                                    AND member.user_id = $4
                                    AND member.role IN ('instructor', 'trader')
                              )
                          )
                      )
                    ORDER BY transfer_id DESC
                    LIMIT $3
                    "#,
                    &[&room_id, &account_id, &limit, &user_id],
                )
                .map_err(JournalError::Postgres)?
                .into_iter()
                .map(|row| {
                    let payload_json: Value = row.get("payload_json");
                    serde_json::from_value(payload_json).map_err(JournalError::Serialize)
                })
                .collect()
        })
    }
}

#[derive(Clone, Debug)]
struct StoredRoom {
    room_id: String,
    scenario: ScenarioConfig,
    status: MarketStatus,
}

#[derive(Debug, Default)]
struct MemoryProjections {
    orders: BTreeMap<(String, String, i64), OrderProjection>,
    trades: BTreeMap<(String, String, i64), TradeProjection>,
    market_ticks: BTreeMap<(String, i64, i64), MarketTickProjection>,
    account_ledger: Vec<AccountLedgerProjection>,
    position_snapshots: Vec<PositionSnapshotProjection>,
}

impl MemoryProjections {
    fn from_executions(records: &[JournalExecution]) -> Result<Self, JournalError> {
        let mut projected = Self::default();
        let mut execution_keys = BTreeSet::new();
        let mut idempotency_keys = BTreeSet::new();
        let mut ledger_keys = BTreeSet::new();

        for record in records {
            validate_execution_idempotency(record)?;
            if !execution_keys.insert((record.room_id.clone(), record.command_seq)) {
                return Err(JournalError::DuplicateExecution {
                    room_id: record.room_id.clone(),
                    command_seq: record.command_seq,
                });
            }
            if let (Some(user_id), Some(idempotency_key)) = (
                record.request_user_id.as_deref(),
                record.idempotency_key.as_deref(),
            ) && !idempotency_keys.insert((
                record.room_id.clone(),
                user_id.to_string(),
                idempotency_key.to_string(),
            )) {
                return Err(JournalError::Recovery(format!(
                    "duplicate idempotency key {idempotency_key:?} for user {user_id:?} in room {}",
                    record.room_id
                )));
            }
            i32::try_from(record.execution.clearing_event_count).map_err(|_| {
                JournalError::CountOutOfRange(record.execution.clearing_event_count)
            })?;
            record.command_json()?;
            record.execution_json()?;
            projected.apply_execution(record, &mut ledger_keys)?;
        }

        Ok(projected)
    }

    fn apply_execution(
        &mut self,
        record: &JournalExecution,
        ledger_keys: &mut BTreeSet<(String, i64, i64)>,
    ) -> Result<(), JournalError> {
        let command_seq = i64_from_u64(record.command_seq, "command_seq")?;
        let market_time_ms = record
            .execution
            .market_time_ms
            .map(|value| i64_from_u64(value, "market_time_ms"))
            .transpose()?;
        let instrument_id = record
            .execution
            .instrument_id
            .clone()
            .unwrap_or_else(|| "legacy-primary".to_string());
        let mut ignored_rejected_duplicate_order_id = None;

        if let Command::NewOrder(order) = &record.command {
            let order_id = i64_from_u64(order.order_id, "order_id")?;
            let account_id = i64_from_u64(order.account_id, "account_id")?;
            let original_qty = i64_from_u64(order.qty, "qty")?;
            let (base_order_type, limit_price_tick) = match order.kind {
                OrderKind::Limit { price_tick } => ("limit", Some(price_tick)),
                OrderKind::Market => ("market", None),
                OrderKind::PostOnly { price_tick } => ("post_only", Some(price_tick)),
                OrderKind::ImmediateOrCancel { price_tick } => ("immediate_or_cancel", price_tick),
                OrderKind::FillOrKill { price_tick } => ("fill_or_kill", price_tick),
            };
            let order_type = if order.reduce_only {
                format!("reduce_only_{base_order_type}")
            } else {
                base_order_type.to_string()
            };
            let key = (record.room_id.clone(), instrument_id.clone(), order_id);
            if let std::collections::btree_map::Entry::Vacant(e) = self.orders.entry(key) {
                e.insert(OrderProjection {
                    room_id: record.room_id.clone(),
                    instrument_id: instrument_id.clone(),
                    order_id,
                    account_id,
                    participant_id: record.participant_id.clone(),
                    side: side_name(order.side).to_string(),
                    order_type,
                    limit_price_tick,
                    original_qty,
                    status: if !record.execution.accepted && record.execution.events.is_empty() {
                        "rejected".to_string()
                    } else {
                        "submitted".to_string()
                    },
                    remaining_qty: original_qty,
                    created_command_seq: command_seq,
                    updated_command_seq: command_seq,
                    created_market_time_ms: market_time_ms,
                    updated_market_time_ms: market_time_ms,
                });
            } else {
                if new_order_did_not_create_order(record, order.order_id) {
                    ignored_rejected_duplicate_order_id = Some(order.order_id);
                } else {
                    return Err(JournalError::ProjectionConflict {
                        entity: "order",
                        room_id: record.room_id.clone(),
                        instrument_id,
                        id: order_id,
                    });
                }
            }
        }

        for event in &record.execution.events {
            let event_seq = i64_from_u64(event_seq(event), "event_seq")?;
            event_order_id(event)
                .map(|order_id| i64_from_u64(order_id, "order_id"))
                .transpose()?;
            event_qty(event)
                .map(|qty| i64_from_u64(qty, "qty"))
                .transpose()?;
            event_remaining_qty(event)
                .map(|qty| i64_from_u64(qty, "remaining_qty"))
                .transpose()?;
            serde_json::to_value(event).map_err(JournalError::Serialize)?;

            if let Some((order_id, status, remaining_qty, limit_price_tick)) =
                order_status_update(event)
            {
                if ignored_rejected_duplicate_order_id == Some(order_id) {
                    continue;
                }
                let order_id = i64_from_u64(order_id, "order_id")?;
                if let Some(order) =
                    self.orders
                        .get_mut(&(record.room_id.clone(), instrument_id.clone(), order_id))
                {
                    order.status = status.to_string();
                    if let Some(remaining_qty) = remaining_qty {
                        order.remaining_qty = i64_from_u64(remaining_qty, "remaining_qty")?;
                    }
                    if let Some(limit_price_tick) = limit_price_tick {
                        order.limit_price_tick = Some(limit_price_tick);
                    }
                    order.updated_command_seq = command_seq;
                    order.updated_market_time_ms = market_time_ms;
                }
            }

            if let EventSummary::TradePrinted {
                trade_id,
                maker_order_id,
                maker_account_id,
                taker_order_id,
                taker_account_id,
                price_tick,
                qty,
                taker_side,
                ..
            } = event
            {
                let trade_id = i64_from_u64(*trade_id, "trade_id")?;
                let trade = TradeProjection {
                    room_id: record.room_id.clone(),
                    instrument_id: instrument_id.clone(),
                    trade_id,
                    command_seq,
                    event_seq,
                    market_time_ms,
                    maker_order_id: i64_from_u64(*maker_order_id, "maker_order_id")?,
                    maker_account_id: i64_from_u64(*maker_account_id, "maker_account_id")?,
                    taker_order_id: i64_from_u64(*taker_order_id, "taker_order_id")?,
                    taker_account_id: i64_from_u64(*taker_account_id, "taker_account_id")?,
                    price_tick: *price_tick,
                    qty: i64_from_u64(*qty, "qty")?,
                    taker_side: side_name(*taker_side).to_string(),
                };
                let trade_key = (record.room_id.clone(), instrument_id.clone(), trade_id);
                if self.trades.insert(trade_key, trade.clone()).is_some() {
                    return Err(JournalError::ProjectionConflict {
                        entity: "trade",
                        room_id: record.room_id.clone(),
                        instrument_id: instrument_id.clone(),
                        id: trade_id,
                    });
                }
                let tick_key = (record.room_id.clone(), command_seq, event_seq);
                if self
                    .market_ticks
                    .insert(
                        tick_key,
                        MarketTickProjection {
                            room_id: record.room_id.clone(),
                            instrument_id: instrument_id.clone(),
                            command_seq,
                            event_seq,
                            market_time_ms,
                            trade_id,
                            price_tick: *price_tick,
                            qty: i64_from_u64(*qty, "qty")?,
                            taker_side: side_name(*taker_side).to_string(),
                        },
                    )
                    .is_some()
                {
                    return Err(JournalError::ProjectionConflict {
                        entity: "market_tick",
                        room_id: record.room_id.clone(),
                        instrument_id: instrument_id.clone(),
                        id: event_seq,
                    });
                }
            }
        }

        for (clearing_index, event) in record.execution.clearing_events.iter().enumerate() {
            for row in clearing_ledger_rows(clearing_index, event)? {
                if !ledger_keys.insert((record.room_id.clone(), command_seq, row.ledger_seq)) {
                    return Err(JournalError::ProjectionConflict {
                        entity: "ledger",
                        room_id: record.room_id.clone(),
                        instrument_id: instrument_id.clone(),
                        id: row.ledger_seq,
                    });
                }
                self.account_ledger.push(AccountLedgerProjection {
                    room_id: record.room_id.clone(),
                    instrument_id: instrument_id.clone(),
                    command_seq,
                    ledger_seq: row.ledger_seq,
                    market_kind: row.market_kind.to_string(),
                    account_id: row.account_id,
                    trade_id: row.trade_id,
                    account_side: row.account_side.to_string(),
                    cash_delta: row.cash_delta,
                    position_delta: row.position_delta,
                    fee: row.fee,
                    realized_pnl: row.realized_pnl,
                    price_tick: row.price_tick,
                    qty: row.qty,
                    notional: row.notional,
                    cash_balance: row.cash_balance,
                    position_qty: row.position_qty,
                    avg_entry_price_tick: row.avg_entry_price_tick,
                    realized_pnl_total: row.realized_pnl_total,
                    unrealized_pnl: row.unrealized_pnl,
                    equity: row.equity,
                    initial_margin: row.initial_margin,
                    maintenance_margin: row.maintenance_margin,
                    portfolio_initial_margin: row.portfolio_initial_margin,
                    portfolio_maintenance_margin: row.portfolio_maintenance_margin,
                    margin_status: row.margin_status.clone(),
                    fees_paid: row.fees_paid,
                });
                self.position_snapshots.push(PositionSnapshotProjection {
                    room_id: record.room_id.clone(),
                    instrument_id: instrument_id.clone(),
                    command_seq,
                    ledger_seq: row.ledger_seq,
                    market_kind: row.market_kind.to_string(),
                    account_id: row.account_id,
                    trade_id: row.trade_id,
                    cash_balance: row.cash_balance,
                    position_qty: row.position_qty,
                    avg_entry_price_tick: row.avg_entry_price_tick,
                    realized_pnl: row.realized_pnl_total,
                    unrealized_pnl: row.unrealized_pnl,
                    equity: row.equity,
                    initial_margin: row.initial_margin,
                    maintenance_margin: row.maintenance_margin,
                    portfolio_initial_margin: row.portfolio_initial_margin,
                    portfolio_maintenance_margin: row.portfolio_maintenance_margin,
                    margin_status: row.margin_status,
                    fees_paid: row.fees_paid,
                });
            }
        }

        Ok(())
    }
}

fn validate_execution_idempotency(record: &JournalExecution) -> Result<(), JournalError> {
    match (
        record.request_user_id.as_deref(),
        record.idempotency_key.as_deref(),
        record.request_fingerprint.as_deref(),
    ) {
        (None, None, None) => Ok(()),
        (Some(user_id), Some(idempotency_key), Some(request_fingerprint))
            if !user_id.is_empty()
                && !idempotency_key.is_empty()
                && !request_fingerprint.is_empty() =>
        {
            Ok(())
        }
        _ => Err(JournalError::Recovery(format!(
            "execution {} in room {} has incomplete idempotency metadata",
            record.command_seq, record.room_id
        ))),
    }
}

fn validate_room_lease_request(
    room_id: &str,
    owner_id: &str,
    duration: Duration,
) -> Result<u64, JournalError> {
    if room_id.trim().is_empty() {
        return Err(JournalError::Recovery(
            "room writer lease requires a non-empty room id".to_string(),
        ));
    }
    if owner_id.trim().is_empty() {
        return Err(JournalError::Recovery(
            "room writer lease requires a non-empty owner id".to_string(),
        ));
    }
    let duration_ms = u64::try_from(duration.as_millis()).map_err(|_| {
        JournalError::Recovery(format!(
            "room writer lease duration exceeds {MAX_ROOM_LEASE_DURATION_MS} milliseconds"
        ))
    })?;
    if duration_ms == 0 || duration_ms > MAX_ROOM_LEASE_DURATION_MS {
        return Err(JournalError::Recovery(format!(
            "room writer lease duration must be from 1 to {MAX_ROOM_LEASE_DURATION_MS} milliseconds"
        )));
    }
    Ok(duration_ms)
}

fn validate_room_lease_owner_url(owner_url: Option<&str>) -> Result<(), JournalError> {
    if owner_url.is_some_and(|url| url.is_empty() || url.len() > 2_048) {
        return Err(JournalError::Recovery(
            "room writer lease owner URL must contain 1 to 2048 bytes".to_string(),
        ));
    }
    Ok(())
}

fn validate_room_lease_claim(claim: &RoomLeaseClaim) -> Result<(), JournalError> {
    if claim.room_id.trim().is_empty() || claim.owner_id.trim().is_empty() {
        return Err(JournalError::Recovery(
            "room writer lease claim requires non-empty room and owner ids".to_string(),
        ));
    }
    if claim.fencing_token == 0 {
        return Err(JournalError::Recovery(
            "room writer lease fencing token must be positive".to_string(),
        ));
    }
    Ok(())
}

fn validate_fenced_execution_batch(
    claim: &RoomLeaseClaim,
    records: &[JournalExecution],
    snapshot: Option<&JournalSnapshot>,
) -> Result<(), JournalError> {
    validate_room_lease_claim(claim)?;
    if let Some(record) = records
        .iter()
        .find(|record| record.room_id != claim.room_id)
    {
        return Err(JournalError::Recovery(format!(
            "fenced execution room {} does not match lease room {}",
            record.room_id, claim.room_id
        )));
    }
    if let Some(snapshot) = snapshot
        && snapshot.room_id != claim.room_id
    {
        return Err(JournalError::Recovery(format!(
            "fenced snapshot room {} does not match lease room {}",
            snapshot.room_id, claim.room_id
        )));
    }
    Ok(())
}

fn validate_fenced_room_mutation(
    claim: &RoomLeaseClaim,
    mutation: &PendingJournalMutation,
) -> Result<(), JournalError> {
    validate_room_lease_claim(claim)?;
    if mutation.room_id != claim.room_id {
        return Err(JournalError::Recovery(format!(
            "fenced mutation room {} does not match lease room {}",
            mutation.room_id, claim.room_id
        )));
    }
    Ok(())
}

fn room_lease_lost(claim: &RoomLeaseClaim) -> JournalError {
    JournalError::RoomLeaseLost {
        room_id: claim.room_id.clone(),
        owner_id: claim.owner_id.clone(),
        fencing_token: claim.fencing_token,
    }
}

fn room_writer_lease(
    room_id: &str,
    owner_id: &str,
    owner_url: Option<&str>,
    fencing_token: u64,
    expires_at: SystemTime,
) -> Result<RoomWriterLease, JournalError> {
    let expires_at_unix_ms = u64::try_from(
        expires_at
            .duration_since(UNIX_EPOCH)
            .map_err(|_| {
                JournalError::Recovery(
                    "room writer lease expiration precedes the Unix epoch".to_string(),
                )
            })?
            .as_millis(),
    )
    .map_err(|_| JournalError::ArithmeticOverflow {
        field: "lease_expires_at_unix_ms",
    })?;
    Ok(RoomWriterLease {
        claim: RoomLeaseClaim {
            room_id: room_id.to_string(),
            owner_id: owner_id.to_string(),
            fencing_token,
        },
        owner_url: owner_url.map(str::to_string),
        expires_at_unix_ms,
    })
}

fn postgres_room_writer_lease(
    room_id: &str,
    row: postgres::Row,
) -> Result<RoomWriterLease, JournalError> {
    let fencing_token: i64 = row.get("fencing_token");
    let expires_at_unix_ms: i64 = row.get("expires_at_unix_ms");
    Ok(RoomWriterLease {
        claim: RoomLeaseClaim {
            room_id: room_id.to_string(),
            owner_id: row.get("owner_id"),
            fencing_token: u64::try_from(fencing_token)
                .map_err(|_| JournalError::InvalidSequence(fencing_token))?,
        },
        owner_url: row.get("owner_url"),
        expires_at_unix_ms: u64::try_from(expires_at_unix_ms)
            .map_err(|_| JournalError::InvalidSequence(expires_at_unix_ms))?,
    })
}

fn postgres_room_routing_record(row: postgres::Row) -> Result<RoomRoutingRecord, JournalError> {
    let room_id: String = row.get("room_id");
    let owner_id: Option<String> = row.get("owner_id");
    let owner = match owner_id {
        Some(owner_id) => {
            let fencing_token: Option<i64> = row.get("fencing_token");
            let expires_at_unix_ms: Option<i64> = row.get("expires_at_unix_ms");
            let fencing_token = fencing_token.ok_or_else(|| {
                JournalError::Recovery(format!(
                    "active room writer lease for {room_id} has no fencing token"
                ))
            })?;
            let expires_at_unix_ms = expires_at_unix_ms.ok_or_else(|| {
                JournalError::Recovery(format!(
                    "active room writer lease for {room_id} has no expiration"
                ))
            })?;
            Some(RoomWriterLease {
                claim: RoomLeaseClaim {
                    room_id: room_id.clone(),
                    owner_id,
                    fencing_token: u64::try_from(fencing_token)
                        .map_err(|_| JournalError::InvalidSequence(fencing_token))?,
                },
                owner_url: row.get("owner_url"),
                expires_at_unix_ms: u64::try_from(expires_at_unix_ms)
                    .map_err(|_| JournalError::InvalidSequence(expires_at_unix_ms))?,
            })
        }
        None => None,
    };
    Ok(RoomRoutingRecord { room_id, owner })
}

#[derive(Clone, Copy, Debug)]
struct SchemaMigration {
    version: i32,
    name: &'static str,
    sql: &'static str,
}

#[derive(Debug)]
pub enum JournalError {
    Postgres(postgres::Error),
    Serialize(serde_json::Error),
    InvalidAccountId(String),
    InvalidSequence(i64),
    InvalidStatus(String),
    Recovery(String),
    SequenceOutOfRange(u64),
    CountOutOfRange(usize),
    ValueOutOfRange {
        field: &'static str,
        value: u64,
    },
    SignedValueOutOfRange {
        field: &'static str,
        value: i128,
    },
    ArithmeticOverflow {
        field: &'static str,
    },
    RuntimeLockUnavailable,
    RuntimeLockLost,
    RoomLeaseLost {
        room_id: String,
        owner_id: String,
        fencing_token: u64,
    },
    RoomLeaseNotOwned {
        room_id: String,
        owner_id: String,
    },
    RoomLeaseOwnedBy {
        room_id: String,
        owner_id: String,
        owner_url: Option<String>,
        fencing_token: u64,
        expires_at_unix_ms: u64,
    },
    UnsupportedOperation(&'static str),
    DuplicateExecution {
        room_id: String,
        command_seq: u64,
    },
    ProjectionConflict {
        entity: &'static str,
        room_id: String,
        instrument_id: String,
        id: i64,
    },
    MigrationVersionMismatch {
        version: i32,
        expected: String,
        found: String,
    },
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Postgres(error) => write!(f, "postgres journal error: {error}"),
            Self::Serialize(error) => write!(f, "journal serialization error: {error}"),
            Self::InvalidAccountId(id) => write!(f, "invalid journal account id: {id}"),
            Self::InvalidSequence(seq) => write!(f, "invalid journal sequence: {seq}"),
            Self::InvalidStatus(status) => write!(f, "invalid journal room status: {status}"),
            Self::Recovery(error) => write!(f, "journal recovery error: {error}"),
            Self::SequenceOutOfRange(seq) => write!(f, "journal sequence out of range: {seq}"),
            Self::CountOutOfRange(count) => write!(f, "journal count out of range: {count}"),
            Self::ValueOutOfRange { field, value } => {
                write!(f, "journal value out of range for {field}: {value}")
            }
            Self::SignedValueOutOfRange { field, value } => {
                write!(f, "journal signed value out of range for {field}: {value}")
            }
            Self::ArithmeticOverflow { field } => {
                write!(f, "journal arithmetic overflow for {field}")
            }
            Self::RuntimeLockUnavailable => {
                f.write_str("another MarketForge server already owns the PostgreSQL runtime lock")
            }
            Self::RuntimeLockLost => {
                f.write_str("the PostgreSQL runtime-lock connection was lost; restart is required")
            }
            Self::RoomLeaseLost {
                room_id,
                owner_id,
                fencing_token,
            } => write!(
                f,
                "room writer lease lost for room {room_id}, owner {owner_id}, fencing token {fencing_token}"
            ),
            Self::RoomLeaseNotOwned { room_id, owner_id } => write!(
                f,
                "instance {owner_id} does not own a writer lease for room {room_id}"
            ),
            Self::RoomLeaseOwnedBy {
                room_id,
                owner_id,
                owner_url,
                fencing_token,
                expires_at_unix_ms,
            } => {
                write!(
                    f,
                    "room {room_id} is owned by instance {owner_id} with fencing token {fencing_token} until {expires_at_unix_ms} ms since Unix epoch"
                )?;
                if let Some(owner_url) = owner_url {
                    write!(f, " at {owner_url}")?;
                }
                Ok(())
            }
            Self::UnsupportedOperation(operation) => {
                write!(f, "journal operation is not supported: {operation}")
            }
            Self::DuplicateExecution {
                room_id,
                command_seq,
            } => write!(
                f,
                "duplicate journal execution for room {room_id} at command sequence {command_seq}"
            ),
            Self::ProjectionConflict {
                entity,
                room_id,
                instrument_id,
                id,
            } => write!(
                f,
                "duplicate {entity} projection for room {room_id}, instrument {instrument_id}, id {id}"
            ),
            Self::MigrationVersionMismatch {
                version,
                expected,
                found,
            } => write!(
                f,
                "schema migration version {version} name mismatch: expected {expected}, found {found}"
            ),
        }
    }
}

impl Error for JournalError {}

#[derive(Clone, Debug)]
struct LedgerProjection {
    ledger_seq: i64,
    market_kind: &'static str,
    account_id: i64,
    trade_id: i64,
    account_side: &'static str,
    cash_delta: i64,
    position_delta: i64,
    fee: i64,
    realized_pnl: i64,
    price_tick: i64,
    qty: i64,
    notional: i64,
    cash_balance: i64,
    position_qty: i64,
    avg_entry_price_tick: Option<i64>,
    realized_pnl_total: Option<i64>,
    unrealized_pnl: Option<i64>,
    equity: Option<i64>,
    initial_margin: Option<i64>,
    maintenance_margin: Option<i64>,
    portfolio_initial_margin: Option<i64>,
    portfolio_maintenance_margin: Option<i64>,
    margin_status: Option<String>,
    fees_paid: i64,
    payload_json: Value,
}

fn i64_from_u64(value: u64, field: &'static str) -> Result<i64, JournalError> {
    i64::try_from(value).map_err(|_| JournalError::ValueOutOfRange { field, value })
}

fn optional_i64_account_id(account_id: Option<AccountId>) -> Result<Option<i64>, JournalError> {
    account_id
        .map(|id| i64_from_u64(id, "account_id"))
        .transpose()
}

fn optional_i64_from_i128(
    value: Option<i128>,
    field: &'static str,
) -> Result<Option<i64>, JournalError> {
    value.map(|value| i64_from_i128(value, field)).transpose()
}

fn bounded_query_limit(limit: usize) -> Result<i64, JournalError> {
    let limit = limit.clamp(1, 500);
    i64::try_from(limit).map_err(|_| JournalError::CountOutOfRange(limit))
}

fn i64_from_i128(value: i128, field: &'static str) -> Result<i64, JournalError> {
    i64::try_from(value).map_err(|_| JournalError::SignedValueOutOfRange { field, value })
}

fn checked_add_i128(left: i128, right: i128, field: &'static str) -> Result<i128, JournalError> {
    left.checked_add(right)
        .ok_or(JournalError::ArithmeticOverflow { field })
}

fn checked_sub_i128(left: i128, right: i128, field: &'static str) -> Result<i128, JournalError> {
    left.checked_sub(right)
        .ok_or(JournalError::ArithmeticOverflow { field })
}

fn checked_neg_i128(value: i128, field: &'static str) -> Result<i128, JournalError> {
    value
        .checked_neg()
        .ok_or(JournalError::ArithmeticOverflow { field })
}

fn validate_snapshot(snapshot: &JournalSnapshot) -> Result<(), JournalError> {
    i64_from_u64(snapshot.command_seq, "command_seq")?;
    serde_json::to_value(&snapshot.actor).map_err(JournalError::Serialize)?;
    Ok(())
}

fn command_cursor_after_records(records: &[JournalExecution]) -> Result<u64, JournalError> {
    records.last().map_or(Ok(0), |record| {
        record
            .command_seq
            .checked_add(1)
            .ok_or(JournalError::SequenceOutOfRange(record.command_seq))
    })
}

fn validate_pending_mutation(mutation: &PendingJournalMutation) -> Result<(), JournalError> {
    i64_from_u64(mutation.command_cursor, "command_cursor")?;
    serde_json::to_value(&mutation.mutation).map_err(JournalError::Serialize)?;
    match &mutation.mutation {
        RoomMutation::StateCheckpoint { actor, .. } => {
            if actor.room_id() != mutation.room_id {
                return Err(JournalError::Recovery(format!(
                    "checkpoint room {} does not match mutation room {}",
                    actor.room_id(),
                    mutation.room_id
                )));
            }
            if actor.next_command_seq() != mutation.command_cursor {
                return Err(JournalError::Recovery(format!(
                    "checkpoint actor cursor {} does not match mutation cursor {}",
                    actor.next_command_seq(),
                    mutation.command_cursor
                )));
            }
        }
        RoomMutation::ClockAdvanced {
            completed_transfers,
            ..
        } => {
            for transfer in completed_transfers {
                validate_transfer(&JournalTransfer::recorded(
                    mutation.room_id.clone(),
                    transfer.clone(),
                ))?;
            }
        }
        RoomMutation::DepositSubmitted { transfer, .. }
        | RoomMutation::WithdrawalSubmitted { transfer, .. } => {
            validate_transfer(&JournalTransfer::recorded(
                mutation.room_id.clone(),
                transfer.clone(),
            ))?;
        }
        RoomMutation::VenueToVenueTransferSubmitted { transfer, .. } => {
            validate_transfer(&JournalTransfer::recorded(
                mutation.room_id.clone(),
                transfer.withdrawal.clone(),
            ))?;
            if let Some(deposit) = &transfer.deposit {
                validate_transfer(&JournalTransfer::recorded(
                    mutation.room_id.clone(),
                    deposit.clone(),
                ))?;
            }
        }
        RoomMutation::StatusChanged { .. } => {}
        RoomMutation::SchedulerProgress { state, .. } => {
            if state.room_id != mutation.room_id {
                return Err(JournalError::Recovery(format!(
                    "scheduler progress room {} does not match mutation room {}",
                    state.room_id, mutation.room_id
                )));
            }
        }
        RoomMutation::TrainingProgress { run } => {
            if run.spec.room_id != mutation.room_id {
                return Err(JournalError::Recovery(format!(
                    "training progress room {} does not match mutation room {}",
                    run.spec.room_id, mutation.room_id
                )));
            }
        }
    }
    Ok(())
}

fn validate_mutation_execution_records(
    mutation: &PendingJournalMutation,
    records: &[JournalExecution],
) -> Result<(), JournalError> {
    let mut expected_command_seq = mutation.command_cursor;
    for record in records {
        if record.room_id != mutation.room_id {
            return Err(JournalError::Recovery(format!(
                "execution room {} does not match mutation room {}",
                record.room_id, mutation.room_id
            )));
        }
        if record.command_seq != expected_command_seq {
            return Err(JournalError::Recovery(format!(
                "mutation execution sequence {} does not match command cursor {}",
                record.command_seq, expected_command_seq
            )));
        }
        expected_command_seq = expected_command_seq
            .checked_add(1)
            .ok_or(JournalError::SequenceOutOfRange(record.command_seq))?;
    }
    Ok(())
}

fn deserialize_snapshot_actor(actor_json: Value) -> Result<Option<SimulationRoom>, JournalError> {
    let is_legacy_single_exchange_actor = actor_json.get("config").is_some()
        && actor_json.get("engine").is_some()
        && actor_json.get("exchanges").is_none()
        && actor_json.get("primary_venue_id").is_none();

    match serde_json::from_value(actor_json) {
        Ok(actor) => Ok(Some(actor)),
        Err(_) if is_legacy_single_exchange_actor => Ok(None),
        Err(error) => Err(JournalError::Serialize(error)),
    }
}

fn validate_transfer(record: &JournalTransfer) -> Result<(), JournalError> {
    let transfer = &record.transfer;
    i64_from_u64(transfer.transfer_id, "transfer_id")?;
    transfer_event_seq(transfer)?;
    i64_from_u64(transfer.account_id, "account_id")?;
    i64_from_i128(transfer.amount, "amount")?;
    i64_from_u64(transfer.requested_at_step, "requested_at_step")?;
    i64_from_u64(transfer.available_after_step, "available_after_step")?;
    transfer
        .completed_at_step
        .map(|step| i64_from_u64(step, "completed_at_step"))
        .transpose()?;
    serde_json::to_value(transfer).map_err(JournalError::Serialize)?;
    Ok(())
}

fn ledger_seq(clearing_index: usize, leg: u64) -> Result<i64, JournalError> {
    const LEDGER_SEQ_STRIDE: u64 = 1_000;

    let index =
        u64::try_from(clearing_index).map_err(|_| JournalError::CountOutOfRange(clearing_index))?;
    let seq = index
        .checked_mul(LEDGER_SEQ_STRIDE)
        .and_then(|seq| seq.checked_add(leg))
        .ok_or(JournalError::ValueOutOfRange {
            field: "ledger_seq",
            value: u64::MAX,
        })?;
    i64_from_u64(seq, "ledger_seq")
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Buy => "buy",
        Side::Sell => "sell",
    }
}

fn transfer_kind_name(kind: VenueTransferKind) -> &'static str {
    match kind {
        VenueTransferKind::Deposit => "deposit",
        VenueTransferKind::Withdrawal => "withdrawal",
    }
}

fn transfer_status_name(status: VenueTransferStatus) -> &'static str {
    match status {
        VenueTransferStatus::Pending => "pending",
        VenueTransferStatus::Completed => "completed",
        VenueTransferStatus::Rejected => "rejected",
    }
}

fn transfer_reject_reason_name(reason: &VenueTransferRejectReason) -> String {
    match reason {
        VenueTransferRejectReason::NonPositiveAmount => "non_positive_amount".to_string(),
        VenueTransferRejectReason::InsufficientAvailableBalance => {
            "insufficient_available_balance".to_string()
        }
        VenueTransferRejectReason::InsufficientPortfolioBalance => {
            "insufficient_portfolio_balance".to_string()
        }
        VenueTransferRejectReason::AssetNotAcceptedByVenue => {
            "asset_not_accepted_by_venue".to_string()
        }
        VenueTransferRejectReason::AssetNotWithdrawableFromVenue => {
            "asset_not_withdrawable_from_venue".to_string()
        }
        VenueTransferRejectReason::BalanceOverflow => "balance_overflow".to_string(),
    }
}

fn transfer_event_seq(transfer: &VenueTransfer) -> Result<i64, JournalError> {
    let event_seq = match transfer.status {
        VenueTransferStatus::Pending => 0,
        VenueTransferStatus::Rejected => 0,
        VenueTransferStatus::Completed
            if transfer.completed_at_step == Some(transfer.requested_at_step) =>
        {
            0
        }
        VenueTransferStatus::Completed => 1,
    };
    Ok(event_seq)
}

fn clearing_ledger_rows(
    clearing_index: usize,
    event: &ClearingEventSummary,
) -> Result<Vec<LedgerProjection>, JournalError> {
    let payload_json = serde_json::to_value(event).map_err(JournalError::Serialize)?;
    match event {
        ClearingEventSummary::SpotTradeSettled {
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
        } => {
            let trade_id = i64_from_u64(*trade_id, "trade_id")?;
            let qty_i64 = i64_from_u64(*qty, "qty")?;
            let qty_i128 = i128::from(*qty);
            Ok(vec![
                LedgerProjection {
                    ledger_seq: ledger_seq(clearing_index, 0)?,
                    market_kind: "spot",
                    account_id: i64_from_u64(*buyer_account_id, "buyer_account_id")?,
                    trade_id,
                    account_side: "buy",
                    cash_delta: i64_from_i128(
                        checked_neg_i128(
                            checked_add_i128(*notional, *buyer_fee, "cash_delta")?,
                            "cash_delta",
                        )?,
                        "cash_delta",
                    )?,
                    position_delta: i64_from_i128(qty_i128, "position_delta")?,
                    fee: i64_from_i128(*buyer_fee, "fee")?,
                    realized_pnl: 0,
                    price_tick: *price_tick,
                    qty: qty_i64,
                    notional: i64_from_i128(*notional, "notional")?,
                    cash_balance: i64_from_i128(buyer.cash_balance, "cash_balance")?,
                    position_qty: i64_from_i128(buyer.position_qty, "position_qty")?,
                    avg_entry_price_tick: None,
                    realized_pnl_total: None,
                    unrealized_pnl: None,
                    equity: spot_equity_at_price(
                        buyer.cash_balance,
                        buyer.position_qty,
                        *price_tick,
                    )?,
                    initial_margin: None,
                    maintenance_margin: None,
                    portfolio_initial_margin: None,
                    portfolio_maintenance_margin: None,
                    margin_status: None,
                    fees_paid: i64_from_i128(buyer.fees_paid, "fees_paid")?,
                    payload_json: payload_json.clone(),
                },
                LedgerProjection {
                    ledger_seq: ledger_seq(clearing_index, 1)?,
                    market_kind: "spot",
                    account_id: i64_from_u64(*seller_account_id, "seller_account_id")?,
                    trade_id,
                    account_side: "sell",
                    cash_delta: i64_from_i128(
                        checked_sub_i128(*notional, *seller_fee, "cash_delta")?,
                        "cash_delta",
                    )?,
                    position_delta: i64_from_i128(-qty_i128, "position_delta")?,
                    fee: i64_from_i128(*seller_fee, "fee")?,
                    realized_pnl: 0,
                    price_tick: *price_tick,
                    qty: qty_i64,
                    notional: i64_from_i128(*notional, "notional")?,
                    cash_balance: i64_from_i128(seller.cash_balance, "cash_balance")?,
                    position_qty: i64_from_i128(seller.position_qty, "position_qty")?,
                    avg_entry_price_tick: None,
                    realized_pnl_total: None,
                    unrealized_pnl: None,
                    equity: spot_equity_at_price(
                        seller.cash_balance,
                        seller.position_qty,
                        *price_tick,
                    )?,
                    initial_margin: None,
                    maintenance_margin: None,
                    portfolio_initial_margin: None,
                    portfolio_maintenance_margin: None,
                    margin_status: None,
                    fees_paid: i64_from_i128(seller.fees_paid, "fees_paid")?,
                    payload_json,
                },
            ])
        }
        ClearingEventSummary::PerpTradeSettled {
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
        } => {
            let trade_id = i64_from_u64(*trade_id, "trade_id")?;
            let qty_i64 = i64_from_u64(*qty, "qty")?;
            let qty_i128 = i128::from(*qty);
            Ok(vec![
                LedgerProjection {
                    ledger_seq: ledger_seq(clearing_index, 0)?,
                    market_kind: "perp",
                    account_id: i64_from_u64(*buyer_account_id, "buyer_account_id")?,
                    trade_id,
                    account_side: "buy",
                    cash_delta: i64_from_i128(
                        checked_sub_i128(*buyer_realized_pnl, *buyer_fee, "cash_delta")?,
                        "cash_delta",
                    )?,
                    position_delta: i64_from_i128(qty_i128, "position_delta")?,
                    fee: i64_from_i128(*buyer_fee, "fee")?,
                    realized_pnl: i64_from_i128(*buyer_realized_pnl, "realized_pnl")?,
                    price_tick: *price_tick,
                    qty: qty_i64,
                    notional: i64_from_i128(*notional, "notional")?,
                    cash_balance: i64_from_i128(buyer.cash_balance, "cash_balance")?,
                    position_qty: i64_from_i128(buyer.position_qty, "position_qty")?,
                    avg_entry_price_tick: Some(buyer.avg_entry_price_tick),
                    realized_pnl_total: Some(i64_from_i128(
                        buyer.realized_pnl,
                        "realized_pnl_total",
                    )?),
                    unrealized_pnl: Some(i64_from_i128(buyer.unrealized_pnl, "unrealized_pnl")?),
                    equity: Some(i64_from_i128(buyer.equity, "equity")?),
                    initial_margin: Some(i64_from_i128(buyer.initial_margin, "initial_margin")?),
                    maintenance_margin: Some(i64_from_i128(
                        buyer.maintenance_margin,
                        "maintenance_margin",
                    )?),
                    portfolio_initial_margin: optional_i64_from_i128(
                        buyer.portfolio_initial_margin,
                        "portfolio_initial_margin",
                    )?,
                    portfolio_maintenance_margin: optional_i64_from_i128(
                        buyer.portfolio_maintenance_margin,
                        "portfolio_maintenance_margin",
                    )?,
                    margin_status: Some(buyer.margin_status.clone()),
                    fees_paid: i64_from_i128(buyer.fees_paid, "fees_paid")?,
                    payload_json: payload_json.clone(),
                },
                LedgerProjection {
                    ledger_seq: ledger_seq(clearing_index, 1)?,
                    market_kind: "perp",
                    account_id: i64_from_u64(*seller_account_id, "seller_account_id")?,
                    trade_id,
                    account_side: "sell",
                    cash_delta: i64_from_i128(
                        checked_sub_i128(*seller_realized_pnl, *seller_fee, "cash_delta")?,
                        "cash_delta",
                    )?,
                    position_delta: i64_from_i128(-qty_i128, "position_delta")?,
                    fee: i64_from_i128(*seller_fee, "fee")?,
                    realized_pnl: i64_from_i128(*seller_realized_pnl, "realized_pnl")?,
                    price_tick: *price_tick,
                    qty: qty_i64,
                    notional: i64_from_i128(*notional, "notional")?,
                    cash_balance: i64_from_i128(seller.cash_balance, "cash_balance")?,
                    position_qty: i64_from_i128(seller.position_qty, "position_qty")?,
                    avg_entry_price_tick: Some(seller.avg_entry_price_tick),
                    realized_pnl_total: Some(i64_from_i128(
                        seller.realized_pnl,
                        "realized_pnl_total",
                    )?),
                    unrealized_pnl: Some(i64_from_i128(seller.unrealized_pnl, "unrealized_pnl")?),
                    equity: Some(i64_from_i128(seller.equity, "equity")?),
                    initial_margin: Some(i64_from_i128(seller.initial_margin, "initial_margin")?),
                    maintenance_margin: Some(i64_from_i128(
                        seller.maintenance_margin,
                        "maintenance_margin",
                    )?),
                    portfolio_initial_margin: optional_i64_from_i128(
                        seller.portfolio_initial_margin,
                        "portfolio_initial_margin",
                    )?,
                    portfolio_maintenance_margin: optional_i64_from_i128(
                        seller.portfolio_maintenance_margin,
                        "portfolio_maintenance_margin",
                    )?,
                    margin_status: Some(seller.margin_status.clone()),
                    fees_paid: i64_from_i128(seller.fees_paid, "fees_paid")?,
                    payload_json,
                },
            ])
        }
        ClearingEventSummary::PerpLiquidationSettled {
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
            account,
            ..
        } => {
            let liquidation_cash_delta = checked_add_i128(
                checked_add_i128(
                    checked_add_i128(
                        checked_add_i128(
                            checked_neg_i128(*liquidation_fee, "cash_delta")?,
                            *insurance_fund_payment,
                            "cash_delta",
                        )?,
                        *auto_deleveraging_loss,
                        "cash_delta",
                    )?,
                    *socialized_loss,
                    "cash_delta",
                )?,
                *bad_debt,
                "cash_delta",
            )?;
            let mut projections = vec![LedgerProjection {
                ledger_seq: ledger_seq(clearing_index, 0)?,
                market_kind: "perp",
                account_id: i64_from_u64(*account_id, "account_id")?,
                trade_id: i64_from_u64(*order_id, "order_id")?,
                account_side: "liquidation",
                cash_delta: i64_from_i128(liquidation_cash_delta, "cash_delta")?,
                position_delta: 0,
                fee: i64_from_i128(*liquidation_fee, "fee")?,
                realized_pnl: 0,
                price_tick: 0,
                qty: 0,
                notional: i64_from_i128(*liquidation_notional, "notional")?,
                cash_balance: i64_from_i128(account.cash_balance, "cash_balance")?,
                position_qty: i64_from_i128(account.position_qty, "position_qty")?,
                avg_entry_price_tick: Some(account.avg_entry_price_tick),
                realized_pnl_total: Some(i64_from_i128(
                    account.realized_pnl,
                    "realized_pnl_total",
                )?),
                unrealized_pnl: Some(i64_from_i128(account.unrealized_pnl, "unrealized_pnl")?),
                equity: Some(i64_from_i128(account.equity, "equity")?),
                initial_margin: Some(i64_from_i128(account.initial_margin, "initial_margin")?),
                maintenance_margin: Some(i64_from_i128(
                    account.maintenance_margin,
                    "maintenance_margin",
                )?),
                portfolio_initial_margin: optional_i64_from_i128(
                    account.portfolio_initial_margin,
                    "portfolio_initial_margin",
                )?,
                portfolio_maintenance_margin: optional_i64_from_i128(
                    account.portfolio_maintenance_margin,
                    "portfolio_maintenance_margin",
                )?,
                margin_status: Some(account.margin_status.clone()),
                fees_paid: i64_from_i128(account.fees_paid, "fees_paid")?,
                payload_json: payload_json.clone(),
            }];

            for (allocation_index, allocation) in auto_deleveraging_allocations.iter().enumerate() {
                let allocation_leg = allocation_index
                    .checked_add(1)
                    .and_then(|index| u64::try_from(index).ok())
                    .ok_or(JournalError::ValueOutOfRange {
                        field: "ledger_seq",
                        value: u64::MAX,
                    })?;
                projections.push(LedgerProjection {
                    ledger_seq: ledger_seq(clearing_index, allocation_leg)?,
                    market_kind: "perp",
                    account_id: i64_from_u64(allocation.account_id, "account_id")?,
                    trade_id: i64_from_u64(*order_id, "order_id")?,
                    account_side: "auto_deleveraging",
                    cash_delta: i64_from_i128(
                        checked_sub_i128(allocation.realized_pnl, allocation.loss, "cash_delta")?,
                        "cash_delta",
                    )?,
                    position_delta: i64_from_i128(allocation.position_delta, "position_delta")?,
                    fee: 0,
                    realized_pnl: i64_from_i128(allocation.realized_pnl, "realized_pnl")?,
                    price_tick: allocation.price_tick,
                    qty: i64_from_u64(allocation.qty, "qty")?,
                    notional: i64_from_i128(*liquidation_notional, "notional")?,
                    cash_balance: i64_from_i128(allocation.account.cash_balance, "cash_balance")?,
                    position_qty: i64_from_i128(allocation.account.position_qty, "position_qty")?,
                    avg_entry_price_tick: Some(allocation.account.avg_entry_price_tick),
                    realized_pnl_total: Some(i64_from_i128(
                        allocation.account.realized_pnl,
                        "realized_pnl_total",
                    )?),
                    unrealized_pnl: Some(i64_from_i128(
                        allocation.account.unrealized_pnl,
                        "unrealized_pnl",
                    )?),
                    equity: Some(i64_from_i128(allocation.account.equity, "equity")?),
                    initial_margin: Some(i64_from_i128(
                        allocation.account.initial_margin,
                        "initial_margin",
                    )?),
                    maintenance_margin: Some(i64_from_i128(
                        allocation.account.maintenance_margin,
                        "maintenance_margin",
                    )?),
                    portfolio_initial_margin: optional_i64_from_i128(
                        allocation.account.portfolio_initial_margin,
                        "portfolio_initial_margin",
                    )?,
                    portfolio_maintenance_margin: optional_i64_from_i128(
                        allocation.account.portfolio_maintenance_margin,
                        "portfolio_maintenance_margin",
                    )?,
                    margin_status: Some(allocation.account.margin_status.clone()),
                    fees_paid: i64_from_i128(allocation.account.fees_paid, "fees_paid")?,
                    payload_json: payload_json.clone(),
                });
            }

            for (allocation_index, allocation) in socialized_loss_allocations.iter().enumerate() {
                let allocation_leg = auto_deleveraging_allocations
                    .len()
                    .checked_add(allocation_index)
                    .and_then(|index| index.checked_add(1))
                    .and_then(|index| u64::try_from(index).ok())
                    .ok_or(JournalError::ValueOutOfRange {
                        field: "ledger_seq",
                        value: u64::MAX,
                    })?;
                projections.push(LedgerProjection {
                    ledger_seq: ledger_seq(clearing_index, allocation_leg)?,
                    market_kind: "perp",
                    account_id: i64_from_u64(allocation.account_id, "account_id")?,
                    trade_id: i64_from_u64(*order_id, "order_id")?,
                    account_side: "socialized_loss",
                    cash_delta: i64_from_i128(
                        checked_neg_i128(allocation.loss, "cash_delta")?,
                        "cash_delta",
                    )?,
                    position_delta: 0,
                    fee: 0,
                    realized_pnl: 0,
                    price_tick: 0,
                    qty: 0,
                    notional: i64_from_i128(*liquidation_notional, "notional")?,
                    cash_balance: i64_from_i128(allocation.account.cash_balance, "cash_balance")?,
                    position_qty: i64_from_i128(allocation.account.position_qty, "position_qty")?,
                    avg_entry_price_tick: Some(allocation.account.avg_entry_price_tick),
                    realized_pnl_total: Some(i64_from_i128(
                        allocation.account.realized_pnl,
                        "realized_pnl_total",
                    )?),
                    unrealized_pnl: Some(i64_from_i128(
                        allocation.account.unrealized_pnl,
                        "unrealized_pnl",
                    )?),
                    equity: Some(i64_from_i128(allocation.account.equity, "equity")?),
                    initial_margin: Some(i64_from_i128(
                        allocation.account.initial_margin,
                        "initial_margin",
                    )?),
                    maintenance_margin: Some(i64_from_i128(
                        allocation.account.maintenance_margin,
                        "maintenance_margin",
                    )?),
                    portfolio_initial_margin: optional_i64_from_i128(
                        allocation.account.portfolio_initial_margin,
                        "portfolio_initial_margin",
                    )?,
                    portfolio_maintenance_margin: optional_i64_from_i128(
                        allocation.account.portfolio_maintenance_margin,
                        "portfolio_maintenance_margin",
                    )?,
                    margin_status: Some(allocation.account.margin_status.clone()),
                    fees_paid: i64_from_i128(allocation.account.fees_paid, "fees_paid")?,
                    payload_json: payload_json.clone(),
                });
            }

            Ok(projections)
        }
        ClearingEventSummary::PerpMarginStatusChanged { .. } => Ok(Vec::new()),
    }
}

fn spot_equity_at_price(
    cash_balance: i128,
    position_qty: i128,
    price_tick: i64,
) -> Result<Option<i64>, JournalError> {
    if price_tick < 0 {
        return Ok(None);
    }
    let position_value = position_qty
        .checked_mul(i128::from(price_tick))
        .ok_or(JournalError::ArithmeticOverflow { field: "equity" })?;
    let equity = checked_add_i128(cash_balance, position_value, "equity")?;
    i64_from_i128(equity, "equity").map(Some)
}

fn run_schema_migrations(client: &mut Client) -> Result<(), JournalError> {
    client
        .batch_execute(
            r#"
            CREATE TABLE IF NOT EXISTS marketforge_schema_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                applied_at TIMESTAMPTZ NOT NULL DEFAULT now()
            );
            "#,
        )
        .map_err(JournalError::Postgres)?;

    for migration in MIGRATIONS {
        let applied = client
            .query_opt(
                "SELECT name FROM marketforge_schema_migrations WHERE version = $1",
                &[&migration.version],
            )
            .map_err(JournalError::Postgres)?;

        if let Some(row) = applied {
            let found: String = row.get("name");
            if found != migration.name {
                return Err(JournalError::MigrationVersionMismatch {
                    version: migration.version,
                    expected: migration.name.to_string(),
                    found,
                });
            }
            continue;
        }

        let mut tx = client.transaction().map_err(JournalError::Postgres)?;
        tx.batch_execute(migration.sql)
            .map_err(JournalError::Postgres)?;
        tx.execute(
            "INSERT INTO marketforge_schema_migrations (version, name) VALUES ($1, $2)",
            &[&migration.version, &migration.name],
        )
        .map_err(JournalError::Postgres)?;
        tx.commit().map_err(JournalError::Postgres)?;
    }

    Ok(())
}

fn event_seq(event: &EventSummary) -> u64 {
    match event {
        EventSummary::OrderAccepted { seq, .. }
        | EventSummary::OrderRejected { seq, .. }
        | EventSummary::RiskRejected { seq, .. }
        | EventSummary::TradePrinted { seq, .. }
        | EventSummary::OrderPartiallyFilled { seq, .. }
        | EventSummary::OrderFilled { seq, .. }
        | EventSummary::OrderRested { seq, .. }
        | EventSummary::OrderExpired { seq, .. }
        | EventSummary::OrderCanceled { seq, .. }
        | EventSummary::CancelRejected { seq, .. }
        | EventSummary::OrderAmended { seq, .. }
        | EventSummary::AmendRejected { seq, .. } => *seq,
    }
}

fn event_type(event: &EventSummary) -> &'static str {
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

fn event_order_id(event: &EventSummary) -> Option<u64> {
    match event {
        EventSummary::OrderAccepted { order_id, .. }
        | EventSummary::OrderRejected { order_id, .. }
        | EventSummary::RiskRejected { order_id, .. }
        | EventSummary::OrderPartiallyFilled { order_id, .. }
        | EventSummary::OrderFilled { order_id, .. }
        | EventSummary::OrderRested { order_id, .. }
        | EventSummary::OrderExpired { order_id, .. }
        | EventSummary::OrderCanceled { order_id, .. }
        | EventSummary::CancelRejected { order_id, .. }
        | EventSummary::OrderAmended { order_id, .. }
        | EventSummary::AmendRejected { order_id, .. } => Some(*order_id),
        EventSummary::TradePrinted { .. } => None,
    }
}

fn event_reason(event: &EventSummary) -> Option<String> {
    match event {
        EventSummary::OrderRejected { reason, .. }
        | EventSummary::RiskRejected { reason, .. }
        | EventSummary::CancelRejected { reason, .. }
        | EventSummary::AmendRejected { reason, .. } => Some(reason.clone()),
        _ => None,
    }
}

fn event_price_tick(event: &EventSummary) -> Option<i64> {
    match event {
        EventSummary::TradePrinted { price_tick, .. }
        | EventSummary::OrderRested { price_tick, .. } => Some(*price_tick),
        EventSummary::OrderAmended { new_price_tick, .. } => Some(*new_price_tick),
        _ => None,
    }
}

fn event_qty(event: &EventSummary) -> Option<u64> {
    match event {
        EventSummary::TradePrinted { qty, .. } => Some(*qty),
        EventSummary::OrderExpired { unfilled_qty, .. } => Some(*unfilled_qty),
        EventSummary::OrderAmended { new_qty, .. } => Some(*new_qty),
        _ => None,
    }
}

fn event_remaining_qty(event: &EventSummary) -> Option<u64> {
    match event {
        EventSummary::OrderPartiallyFilled { remaining_qty, .. }
        | EventSummary::OrderRested { remaining_qty, .. }
        | EventSummary::OrderCanceled { remaining_qty, .. } => Some(*remaining_qty),
        EventSummary::OrderAmended { new_qty, .. } => Some(*new_qty),
        EventSummary::OrderFilled { .. } => Some(0),
        _ => None,
    }
}

fn order_status_update(
    event: &EventSummary,
) -> Option<(u64, &'static str, Option<u64>, Option<i64>)> {
    match event {
        EventSummary::OrderAccepted { order_id, .. } => Some((*order_id, "accepted", None, None)),
        EventSummary::OrderRejected { order_id, .. }
        | EventSummary::RiskRejected { order_id, .. } => Some((*order_id, "rejected", None, None)),
        EventSummary::OrderPartiallyFilled {
            order_id,
            remaining_qty,
            ..
        } => Some((*order_id, "partially_filled", Some(*remaining_qty), None)),
        EventSummary::OrderFilled { order_id, .. } => Some((*order_id, "filled", Some(0), None)),
        EventSummary::OrderRested {
            order_id,
            price_tick,
            remaining_qty,
            ..
        } => Some((*order_id, "open", Some(*remaining_qty), Some(*price_tick))),
        EventSummary::OrderExpired {
            order_id,
            unfilled_qty,
            ..
        } => Some((*order_id, "expired", Some(*unfilled_qty), None)),
        EventSummary::OrderCanceled {
            order_id,
            remaining_qty,
            ..
        } => Some((*order_id, "canceled", Some(*remaining_qty), None)),
        EventSummary::OrderAmended {
            order_id,
            new_price_tick,
            new_qty,
            ..
        } => Some((*order_id, "open", Some(*new_qty), Some(*new_price_tick))),
        EventSummary::TradePrinted { .. }
        | EventSummary::CancelRejected { .. }
        | EventSummary::AmendRejected { .. } => None,
    }
}

fn new_order_did_not_create_order(record: &JournalExecution, order_id: u64) -> bool {
    let rejected = !record.execution.accepted
        || record.execution.events.iter().any(|event| {
            matches!(
                event,
                EventSummary::OrderRejected {
                    order_id: rejected_order_id,
                    ..
                } | EventSummary::RiskRejected {
                    order_id: rejected_order_id,
                    ..
                } if *rejected_order_id == order_id
            )
        });
    let created = record.execution.events.iter().any(|event| match event {
        EventSummary::OrderAccepted {
            order_id: event_order_id,
            ..
        }
        | EventSummary::OrderPartiallyFilled {
            order_id: event_order_id,
            ..
        }
        | EventSummary::OrderFilled {
            order_id: event_order_id,
            ..
        }
        | EventSummary::OrderRested {
            order_id: event_order_id,
            ..
        }
        | EventSummary::OrderExpired {
            order_id: event_order_id,
            ..
        } => *event_order_id == order_id,
        EventSummary::TradePrinted {
            maker_order_id,
            taker_order_id,
            ..
        } => *maker_order_id == order_id || *taker_order_id == order_id,
        EventSummary::OrderRejected { .. }
        | EventSummary::RiskRejected { .. }
        | EventSummary::OrderCanceled { .. }
        | EventSummary::CancelRejected { .. }
        | EventSummary::OrderAmended { .. }
        | EventSummary::AmendRejected { .. } => false,
    });

    rejected && !created
}

fn command_account_id(command: &Command) -> Option<AccountId> {
    match command {
        Command::NewOrder(order) => Some(order.account_id),
        Command::CancelOrder(_) | Command::AmendOrder(_) | Command::SetMarkPrice(_) => None,
    }
}

fn status_name(status: MarketStatus) -> &'static str {
    match status {
        MarketStatus::Running => "running",
        MarketStatus::Paused => "paused",
        MarketStatus::Closed => "closed",
    }
}

fn status_from_name(status: &str) -> Result<MarketStatus, JournalError> {
    match status {
        "running" => Ok(MarketStatus::Running),
        "paused" => Ok(MarketStatus::Paused),
        "closed" => Ok(MarketStatus::Closed),
        other => Err(JournalError::InvalidStatus(other.to_string())),
    }
}

fn run_postgres<T, F>(client: &mut Client, operation: F) -> Result<T, JournalError>
where
    F: FnOnce(&mut Client) -> Result<T, JournalError>,
{
    operation(client)
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange_core::{
        InstrumentConfig, MarketConfig, NewOrder, SpotClearingConfig, SpotMarketConfig,
        SpotRiskConfig, VenueAssetPolicyConfig, VenueRuleConfig, scenario::ScenarioAccount,
    };

    fn new_order_record(
        command_seq: u64,
        order_id: u64,
        account_id: AccountId,
        side: Side,
        events: Vec<EventSummary>,
        clearing_events: Vec<ClearingEventSummary>,
    ) -> JournalExecution {
        let command = Command::NewOrder(NewOrder {
            order_id,
            account_id,
            side,
            kind: OrderKind::Limit { price_tick: 100 },
            qty: 2,
            reduce_only: false,
        });
        JournalExecution {
            room_id: "room-1".to_string(),
            command_seq,
            participant_id: Some(format!("participant-{account_id}")),
            account_id: Some(account_id),
            request_user_id: None,
            idempotency_key: None,
            request_fingerprint: None,
            command,
            execution: RoomExecutionSummary {
                room_id: "room-1".to_string(),
                instrument_id: Some("V-BTC-SPOT".to_string()),
                command_seq,
                market_time_ms: Some(command_seq * 1_000),
                status: MarketStatus::Running,
                accepted: true,
                reject_reason: None,
                clearing_event_count: clearing_events.len(),
                events,
                clearing_events,
                clearing_events_omitted: false,
            },
        }
    }

    fn spot_scenario(room_id: &str) -> ScenarioConfig {
        ScenarioConfig {
            room_id: room_id.to_string(),
            venue_preset: None,
            venue_rules: VenueRuleConfig::default(),
            venue_asset_policy: VenueAssetPolicyConfig::default(),
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
            accounts: vec![ScenarioAccount::Spot {
                account_id: 10,
                cash_balance: 1_000,
                position_qty: 10,
            }],
            seed_orders: Vec::new(),
            routed_seed_orders: Vec::new(),
        }
    }

    fn empty_room(room_id: &str) -> SimulationRoom {
        SimulationRoom::from_scenario(spot_scenario(room_id))
            .unwrap()
            .room
    }

    #[test]
    fn in_memory_room_writer_leases_fence_stale_owners() {
        let room_id = "lease-room";
        let mut store = InMemoryJournalStore::new();
        store.rooms.push(StoredRoom {
            room_id: room_id.to_string(),
            scenario: spot_scenario(room_id),
            status: MarketStatus::Running,
        });

        assert!(
            store
                .acquire_room_writer_lease(
                    "missing-room",
                    "owner-a",
                    Some("http://owner-a"),
                    Duration::from_secs(1),
                )
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .acquire_room_writer_lease(room_id, "owner-a", None, Duration::ZERO)
                .is_err()
        );

        let lease_a = store
            .acquire_room_writer_lease(
                room_id,
                "owner-a",
                Some("http://owner-a"),
                Duration::from_secs(1),
            )
            .unwrap()
            .unwrap();
        assert_eq!(lease_a.claim.fencing_token, 1);
        assert_eq!(lease_a.owner_url.as_deref(), Some("http://owner-a"));
        assert!(
            store
                .acquire_room_writer_lease(
                    room_id,
                    "owner-b",
                    Some("http://owner-b"),
                    Duration::from_secs(1),
                )
                .unwrap()
                .is_none()
        );
        let renewed_a = store
            .renew_room_writer_lease(&lease_a.claim, Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert_eq!(renewed_a.claim, lease_a.claim);

        store
            .append_room_mutation_fenced(
                &lease_a.claim,
                &PendingJournalMutation::new(
                    room_id,
                    0,
                    RoomMutation::StatusChanged {
                        status: MarketStatus::Paused,
                    },
                ),
                &[],
                &[],
                None,
            )
            .unwrap();
        assert_eq!(store.rooms[0].status, MarketStatus::Paused);
        assert!(store.release_room_writer_lease(&lease_a.claim).unwrap());

        let lease_b = store
            .acquire_room_writer_lease(
                room_id,
                "owner-b",
                Some("http://owner-b"),
                Duration::from_secs(1),
            )
            .unwrap()
            .unwrap();
        assert_eq!(lease_b.claim.fencing_token, 2);
        assert_eq!(lease_b.owner_url.as_deref(), Some("http://owner-b"));
        assert!(!store.release_room_writer_lease(&lease_a.claim).unwrap());
        assert!(matches!(
            store.append_room_mutation_fenced(
                &lease_a.claim,
                &PendingJournalMutation::new(
                    room_id,
                    0,
                    RoomMutation::StatusChanged {
                        status: MarketStatus::Running,
                    },
                ),
                &[],
                &[],
                None,
            ),
            Err(JournalError::RoomLeaseLost { .. })
        ));
        store
            .append_room_mutation_fenced(
                &lease_b.claim,
                &PendingJournalMutation::new(
                    room_id,
                    0,
                    RoomMutation::StatusChanged {
                        status: MarketStatus::Running,
                    },
                ),
                &[],
                &[],
                None,
            )
            .unwrap();
        assert_eq!(store.rooms[0].status, MarketStatus::Running);

        assert!(store.release_room_writer_lease(&lease_b.claim).unwrap());
        let short_lease = store
            .acquire_room_writer_lease(room_id, "owner-a", None, Duration::from_millis(1))
            .unwrap()
            .unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let after_expiry = store
            .acquire_room_writer_lease(room_id, "owner-b", None, Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert_eq!(
            after_expiry.claim.fencing_token,
            short_lease.claim.fencing_token + 1
        );
    }

    #[test]
    fn in_memory_room_routes_are_authorized_ordered_and_include_unowned_rooms() {
        let mut store = InMemoryJournalStore::new();
        for (room_id, user_id) in [
            ("route-c", "bob"),
            ("route-b", "alice"),
            ("route-a", "alice"),
        ] {
            store.rooms.push(StoredRoom {
                room_id: room_id.to_string(),
                scenario: spot_scenario(room_id),
                status: MarketStatus::Running,
            });
            store.room_members.insert(
                (room_id.to_string(), user_id.to_string()),
                "owner".to_string(),
            );
        }
        store
            .acquire_room_writer_lease(
                "route-a",
                "instance-a",
                Some("http://instance-a"),
                Duration::from_secs(1),
            )
            .unwrap()
            .unwrap();

        let first = store.query_room_routes("alice", None, 1).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].room_id, "route-a");
        assert_eq!(
            first[0]
                .owner
                .as_ref()
                .and_then(|lease| lease.owner_url.as_deref()),
            Some("http://instance-a")
        );

        let second = store
            .query_room_routes("alice", Some("route-a"), 10)
            .unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].room_id, "route-b");
        assert!(second[0].owner.is_none());
        assert!(
            store
                .query_room_routes("mallory", None, 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn in_memory_execution_pages_follow_durable_cursor_semantics() {
        let mut store = InMemoryJournalStore::new();
        store.executions = (0..5)
            .map(|command_seq| {
                new_order_record(
                    command_seq,
                    command_seq + 1,
                    10,
                    Side::Buy,
                    Vec::new(),
                    Vec::new(),
                )
            })
            .collect();

        let first = store.query_executions("room-1", None, true, 2).unwrap();
        assert_eq!(
            first
                .executions
                .iter()
                .map(|execution| execution.command_seq)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_eq!(first.latest_command_seq, Some(4));
        assert!(first.has_more);

        let middle = store.query_executions("room-1", Some(1), false, 2).unwrap();
        assert_eq!(
            middle
                .executions
                .iter()
                .map(|execution| execution.command_seq)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert!(middle.has_more);

        let tail = store.query_executions("room-1", None, false, 2).unwrap();
        assert_eq!(
            tail.executions
                .iter()
                .map(|execution| execution.command_seq)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
        assert!(!tail.has_more);
    }

    #[test]
    fn legacy_single_exchange_snapshots_fall_back_to_command_replay() {
        let legacy_actor = serde_json::json!({
            "room_id": "legacy-room",
            "config": {},
            "engine": {},
            "status": "Running",
            "next_command_seq": 1
        });
        assert!(deserialize_snapshot_actor(legacy_actor).unwrap().is_none());

        let malformed_current_actor = serde_json::json!({
            "room_id": "broken-room",
            "primary_venue_id": "default-venue",
            "exchanges": {}
        });
        assert!(deserialize_snapshot_actor(malformed_current_actor).is_err());
    }

    #[test]
    fn room_mutation_json_deserializes_i128_without_changing_legacy_shape() {
        let checkpoint = RoomMutation::StateCheckpoint {
            actor: Box::new(empty_room("checkpoint-i128-room")),
            complete_history: true,
        };
        let checkpoint_json = serde_json::to_value(&checkpoint).unwrap();
        assert_eq!(checkpoint_json["kind"], "state_checkpoint");
        assert!(
            serde_json::to_string(&checkpoint_json)
                .unwrap()
                .contains("\"cash_balance\":1000")
        );
        let decoded: RoomMutation = serde_json::from_value(checkpoint_json).unwrap();
        let RoomMutation::StateCheckpoint {
            actor,
            complete_history,
        } = decoded
        else {
            panic!("expected state checkpoint mutation");
        };
        assert_eq!(actor.room_id(), "checkpoint-i128-room");
        assert!(complete_history);

        let transfer_json = serde_json::json!({
            "kind": "deposit_submitted",
            "venue_id": "default-venue",
            "account_id": 10,
            "asset_id": "USD",
            "amount": 25,
            "transfer": {
                "transfer_id": 1,
                "kind": "Deposit",
                "account_id": 10,
                "asset_id": "USD",
                "amount": 25,
                "requested_at_step": 0,
                "available_after_step": 0,
                "completed_at_step": 0,
                "status": "Completed",
                "reject_reason": null
            }
        });
        let decoded: RoomMutation = serde_json::from_value(transfer_json).unwrap();
        assert!(matches!(
            decoded,
            RoomMutation::DepositSubmitted { amount: 25, .. }
        ));
    }

    #[test]
    fn released_migrations_stay_immutable_and_repairs_are_new_versions() {
        let initial = include_str!("../migrations/0001_initial_schema.sql");
        let margin_fields = include_str!("../migrations/0005_margin_projection_fields.sql");
        let legacy_room_claim = include_str!("../migrations/0006_claim_unowned_legacy_rooms.sql");
        let room_mutations = include_str!("../migrations/0007_room_mutation_journal.sql");
        let portfolio_margin_fields =
            include_str!("../migrations/0008_portfolio_margin_projection_fields.sql");
        let authoritative_market_time =
            include_str!("../migrations/0009_authoritative_market_time.sql");
        let order_request_idempotency =
            include_str!("../migrations/0010_order_request_idempotency.sql");
        let room_writer_leases = include_str!("../migrations/0011_room_writer_leases.sql");
        let room_writer_owner_url = include_str!("../migrations/0012_room_writer_owner_url.sql");
        let scheduler_and_control =
            include_str!("../migrations/0013_scheduler_and_control_idempotency.sql");

        assert!(!initial.contains("maintenance_margin"));
        assert!(!initial.contains("margin_status"));
        assert!(margin_fields.contains("ADD COLUMN IF NOT EXISTS maintenance_margin"));
        assert!(margin_fields.contains("ADD COLUMN IF NOT EXISTS margin_status"));
        assert!(legacy_room_claim.contains("'local-user', 'owner'"));
        assert!(legacy_room_claim.contains("WHERE NOT EXISTS"));
        assert!(room_mutations.contains("marketforge_room_mutations"));
        assert!(room_mutations.contains("'state_checkpoint'"));
        assert!(room_mutations.contains("snapshot.actor_json"));
        assert!(portfolio_margin_fields.contains("portfolio_initial_margin"));
        assert!(portfolio_margin_fields.contains("portfolio_maintenance_margin"));
        assert!(authoritative_market_time.contains("market_time_ms"));
        assert!(order_request_idempotency.contains("idempotency_key"));
        assert!(room_writer_leases.contains("marketforge_room_writer_leases"));
        assert!(room_writer_leases.contains("fencing_token"));
        assert!(room_writer_owner_url.contains("owner_url"));
        assert!(scheduler_and_control.contains("marketforge_control_idempotency"));
        assert_eq!(
            MIGRATIONS.last().map(|migration| migration.version),
            Some(13)
        );
    }

    #[test]
    fn in_memory_projection_queries_match_role_and_account_visibility() {
        let mut store = InMemoryJournalStore::new();
        store.set_room_member_for_test("room-1", "admin", "admin");
        store.set_room_member_for_test("room-1", "trader", "trader");
        store.set_room_member_for_test("room-1", "viewer", "spectator");
        store.set_account_owner_for_test("room-1", 20, "trader");

        let maker = new_order_record(
            1,
            100,
            10,
            Side::Sell,
            vec![
                EventSummary::OrderAccepted {
                    seq: 0,
                    order_id: 100,
                },
                EventSummary::OrderRested {
                    seq: 1,
                    order_id: 100,
                    price_tick: 100,
                    remaining_qty: 2,
                },
            ],
            Vec::new(),
        );
        let taker = new_order_record(
            2,
            200,
            20,
            Side::Buy,
            vec![
                EventSummary::OrderAccepted {
                    seq: 2,
                    order_id: 200,
                },
                EventSummary::TradePrinted {
                    seq: 3,
                    trade_id: 1,
                    maker_order_id: 100,
                    maker_account_id: 10,
                    taker_order_id: 200,
                    taker_account_id: 20,
                    price_tick: 100,
                    qty: 2,
                    taker_side: Side::Buy,
                },
                EventSummary::OrderFilled {
                    seq: 4,
                    order_id: 100,
                },
                EventSummary::OrderFilled {
                    seq: 5,
                    order_id: 200,
                },
            ],
            vec![ClearingEventSummary::SpotTradeSettled {
                trade_id: 1,
                buyer_account_id: 20,
                seller_account_id: 10,
                price_tick: 100,
                qty: 2,
                notional: 200,
                buyer_fee: 1,
                seller_fee: 1,
                buyer: crate::SpotAccountStateSummary {
                    account_id: 20,
                    cash_balance: 799,
                    position_qty: 2,
                    fees_paid: 1,
                },
                seller: crate::SpotAccountStateSummary {
                    account_id: 10,
                    cash_balance: 1_199,
                    position_qty: 8,
                    fees_paid: 1,
                },
            }],
        );
        store.append_executions(&[maker, taker], None).unwrap();

        assert!(store.user_can_access_room("viewer", "room-1").unwrap());
        assert!(!store.user_can_administer_room("viewer", "room-1").unwrap());
        assert!(store.user_can_administer_room("admin", "room-1").unwrap());
        assert_eq!(
            store
                .query_orders("admin", "room-1", None, None, 100)
                .unwrap()
                .len(),
            2
        );
        let trader_orders = store
            .query_orders("trader", "room-1", None, None, 100)
            .unwrap();
        assert_eq!(trader_orders.len(), 1);
        assert_eq!(trader_orders[0].order_id, 200);
        assert_eq!(trader_orders[0].created_market_time_ms, Some(2_000));
        assert!(
            store
                .query_orders("viewer", "room-1", None, None, 100)
                .unwrap()
                .is_empty()
        );
        let trader_trades = store
            .query_trades("trader", "room-1", None, None, 100)
            .unwrap();
        assert_eq!(trader_trades.len(), 1);
        assert_eq!(trader_trades[0].market_time_ms, Some(2_000));
        assert!(
            store
                .query_trades("viewer", "room-1", None, None, 100)
                .unwrap()
                .is_empty()
        );
        let ticks = store
            .query_market_ticks("viewer", "room-1", None, 100)
            .unwrap();
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].market_time_ms, Some(2_000));
        let ledger = store
            .query_account_ledger("trader", "room-1", None, None, 100)
            .unwrap();
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].account_id, 20);
        let positions = store
            .query_position_snapshots("trader", "room-1", None, None, 100)
            .unwrap();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].account_id, 20);
    }

    #[test]
    fn system_liquidation_ioc_projects_an_expired_terminal_order() {
        let mut store = InMemoryJournalStore::new();
        store.set_room_member_for_test("room-1", "admin", "admin");
        let order_id = 300;
        let account_id = 20;
        let command = Command::NewOrder(NewOrder {
            order_id,
            account_id,
            side: Side::Sell,
            kind: OrderKind::ImmediateOrCancel { price_tick: None },
            qty: 3,
            reduce_only: true,
        });
        let record = JournalExecution {
            room_id: "room-1".to_string(),
            command_seq: 1,
            participant_id: None,
            account_id: Some(account_id),
            request_user_id: None,
            idempotency_key: None,
            request_fingerprint: None,
            command,
            execution: RoomExecutionSummary {
                room_id: "room-1".to_string(),
                instrument_id: Some("V-BTC-PERP".to_string()),
                command_seq: 1,
                market_time_ms: Some(1_000),
                status: MarketStatus::Running,
                accepted: true,
                reject_reason: None,
                clearing_event_count: 0,
                events: vec![
                    EventSummary::OrderAccepted { seq: 0, order_id },
                    EventSummary::OrderExpired {
                        seq: 1,
                        order_id,
                        unfilled_qty: 3,
                    },
                ],
                clearing_events: Vec::new(),
                clearing_events_omitted: false,
            },
        };

        store.append_execution(&record, None).unwrap();

        let orders = store
            .query_orders("admin", "room-1", Some("V-BTC-PERP"), None, 100)
            .unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].participant_id, None);
        assert_eq!(orders[0].order_type, "reduce_only_immediate_or_cancel");
        assert_eq!(orders[0].status, "expired");
        assert_eq!(orders[0].remaining_qty, 3);
        assert!(!matches!(orders[0].status.as_str(), "submitted" | "open"));
    }

    #[test]
    fn in_memory_batch_append_rolls_back_every_record_on_projection_conflict() {
        let mut store = InMemoryJournalStore::new();
        store.set_room_member_for_test("room-1", "admin", "admin");
        let first = new_order_record(1, 100, 10, Side::Sell, Vec::new(), Vec::new());
        store.append_execution(&first, None).unwrap();

        let second = new_order_record(2, 200, 20, Side::Buy, Vec::new(), Vec::new());
        let conflicting = new_order_record(3, 100, 30, Side::Buy, Vec::new(), Vec::new());
        let error = store
            .append_executions(&[second, conflicting], None)
            .unwrap_err();

        assert!(matches!(
            error,
            JournalError::ProjectionConflict {
                entity: "order",
                ..
            }
        ));
        assert_eq!(store.load_recovery().unwrap().executions.len(), 1);
        assert_eq!(
            store
                .query_orders("admin", "room-1", None, None, 100)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn rejected_duplicate_order_is_journaled_without_overwriting_original_projection() {
        let mut store = InMemoryJournalStore::new();
        store.set_room_member_for_test("room-1", "admin", "admin");
        let first = new_order_record(
            1,
            100,
            10,
            Side::Sell,
            vec![
                EventSummary::OrderAccepted {
                    seq: 0,
                    order_id: 100,
                },
                EventSummary::OrderRested {
                    seq: 1,
                    order_id: 100,
                    price_tick: 100,
                    remaining_qty: 2,
                },
            ],
            Vec::new(),
        );
        let rejected_duplicate = new_order_record(
            2,
            100,
            20,
            Side::Buy,
            vec![EventSummary::OrderRejected {
                seq: 2,
                order_id: 100,
                reason: "DuplicateOrderId".to_string(),
            }],
            Vec::new(),
        );

        store
            .append_executions(&[first, rejected_duplicate], None)
            .unwrap();

        assert_eq!(store.load_recovery().unwrap().executions.len(), 2);
        let orders = store
            .query_orders("admin", "room-1", None, None, 100)
            .unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].account_id, 10);
        assert_eq!(orders[0].status, "open");
        assert_eq!(orders[0].created_command_seq, 1);
        assert_eq!(orders[0].updated_command_seq, 1);
        assert_eq!(orders[0].created_market_time_ms, Some(1_000));
        assert_eq!(orders[0].updated_market_time_ms, Some(1_000));
    }

    #[test]
    fn in_memory_idempotency_lookup_survives_recovery_and_rejects_duplicate_keys() {
        let mut store = InMemoryJournalStore::new();
        let first = new_order_record(1, 100, 10, Side::Sell, Vec::new(), Vec::new())
            .with_idempotency("alice", "client-order-1", "fingerprint-a");
        store.append_execution(&first, None).unwrap();

        let stored = store
            .find_idempotent_execution("alice", "room-1", "client-order-1")
            .unwrap()
            .unwrap();
        assert_eq!(stored.command_seq, 1);
        assert_eq!(stored.request_fingerprint.as_deref(), Some("fingerprint-a"));
        let recovered = store.load_recovery().unwrap();
        assert_eq!(recovered.executions[0].command_seq, stored.command_seq);
        assert_eq!(
            recovered.executions[0].idempotency_key,
            stored.idempotency_key
        );

        let duplicate = new_order_record(2, 200, 20, Side::Buy, Vec::new(), Vec::new())
            .with_idempotency("alice", "client-order-1", "fingerprint-b");
        let error = store.append_execution(&duplicate, None).unwrap_err();
        assert!(error.to_string().contains("duplicate idempotency key"));
        assert_eq!(store.load_recovery().unwrap().executions.len(), 1);
    }

    #[test]
    fn in_memory_batch_append_rolls_back_on_projection_value_overflow() {
        let mut store = InMemoryJournalStore::new();
        let first = new_order_record(1, 100, 10, Side::Sell, Vec::new(), Vec::new());
        let overflowing = new_order_record(
            2,
            200,
            20,
            Side::Buy,
            Vec::new(),
            vec![ClearingEventSummary::SpotTradeSettled {
                trade_id: 1,
                buyer_account_id: 20,
                seller_account_id: 10,
                price_tick: 100,
                qty: 1,
                notional: i128::from(i64::MAX),
                buyer_fee: 2,
                seller_fee: 0,
                buyer: crate::SpotAccountStateSummary {
                    account_id: 20,
                    cash_balance: 0,
                    position_qty: 1,
                    fees_paid: 0,
                },
                seller: crate::SpotAccountStateSummary {
                    account_id: 10,
                    cash_balance: 0,
                    position_qty: 0,
                    fees_paid: 0,
                },
            }],
        );

        let error = store
            .append_executions(&[first, overflowing], None)
            .unwrap_err();

        assert!(matches!(
            error,
            JournalError::SignedValueOutOfRange {
                field: "cash_delta",
                ..
            }
        ));
        assert!(store.load_recovery().unwrap().executions.is_empty());
    }

    #[test]
    fn in_memory_latest_snapshot_uses_last_write_for_equal_sequence() {
        let mut store = InMemoryJournalStore::new();
        let original = empty_room("room-1");
        let mut advanced = original.clone();
        advanced.advance_clock(1).unwrap();

        store
            .append_snapshot(&JournalSnapshot {
                room_id: "room-1".to_string(),
                command_seq: 7,
                actor: original,
            })
            .unwrap();
        store
            .append_snapshot(&JournalSnapshot {
                room_id: "room-1".to_string(),
                command_seq: 7,
                actor: advanced,
            })
            .unwrap();

        let recovery = store.load_recovery().unwrap();
        assert_eq!(recovery.snapshots.len(), 1);
        assert_eq!(recovery.snapshots[0].command_seq, 7);
        assert_eq!(recovery.snapshots[0].actor.clock().step(), 1);
    }

    #[test]
    fn in_memory_transfer_batch_rejects_out_of_range_amount_without_partial_write() {
        let mut store = InMemoryJournalStore::new();
        let transfer = |transfer_id, amount| {
            JournalTransfer::recorded(
                "room-1",
                VenueTransfer {
                    transfer_id,
                    kind: VenueTransferKind::Deposit,
                    account_id: 10,
                    asset_id: "USDT".to_string(),
                    amount,
                    requested_at_step: 0,
                    available_after_step: 0,
                    completed_at_step: Some(0),
                    status: VenueTransferStatus::Completed,
                    reject_reason: None,
                },
            )
        };

        let error = store
            .append_transfers(&[transfer(1, 10), transfer(2, i128::MAX)], None)
            .unwrap_err();

        assert!(matches!(
            error,
            JournalError::SignedValueOutOfRange {
                field: "amount",
                ..
            }
        ));
        assert!(store.transfers.is_empty());
    }

    #[test]
    fn checked_journal_arithmetic_reports_overflow() {
        assert!(matches!(
            checked_add_i128(i128::MAX, 1, "cash_delta"),
            Err(JournalError::ArithmeticOverflow {
                field: "cash_delta"
            })
        ));
        assert!(matches!(
            checked_neg_i128(i128::MIN, "cash_delta"),
            Err(JournalError::ArithmeticOverflow {
                field: "cash_delta"
            })
        ));
    }

    #[test]
    fn journal_read_worker_count_accepts_supported_bounds() {
        assert_eq!(parse_journal_read_workers("0").unwrap(), 0);
        assert_eq!(
            parse_journal_read_workers(&DEFAULT_POSTGRES_READ_WORKERS.to_string()).unwrap(),
            DEFAULT_POSTGRES_READ_WORKERS
        );
        assert_eq!(
            parse_journal_read_workers(&MAX_POSTGRES_READ_WORKERS.to_string()).unwrap(),
            MAX_POSTGRES_READ_WORKERS
        );
    }

    #[test]
    fn journal_read_worker_count_rejects_invalid_values() {
        for value in ["", "-1", "4.5", "many", " 4"] {
            assert!(
                parse_journal_read_workers(value).is_err(),
                "value={value:?}"
            );
        }
        assert!(parse_journal_read_workers(&(MAX_POSTGRES_READ_WORKERS + 1).to_string()).is_err());
    }

    #[test]
    fn runtime_lock_wait_accepts_supported_bounds() {
        assert_eq!(parse_runtime_lock_wait_ms("0").unwrap(), 0);
        assert_eq!(parse_runtime_lock_wait_ms("10000").unwrap(), 10_000);
        assert_eq!(
            parse_runtime_lock_wait_ms(&MAX_RUNTIME_LOCK_WAIT_MS.to_string()).unwrap(),
            MAX_RUNTIME_LOCK_WAIT_MS
        );
    }

    #[test]
    fn runtime_lock_wait_rejects_invalid_values() {
        for value in ["", "-1", "1.5", "forever", " 100"] {
            assert!(
                parse_runtime_lock_wait_ms(value).is_err(),
                "value={value:?}"
            );
        }
        assert!(parse_runtime_lock_wait_ms(&(MAX_RUNTIME_LOCK_WAIT_MS + 1).to_string()).is_err());
    }

    #[test]
    fn role_matrix_gates_account_access_and_assignment() {
        let mut store = InMemoryJournalStore::new();
        store
            .upsert_room_member("room-a", "owner", "owner")
            .unwrap();
        store
            .upsert_room_member("room-a", "instructor", "instructor")
            .unwrap();
        store
            .upsert_room_member("room-a", "trader-a", "trader")
            .unwrap();
        store
            .upsert_room_member("room-a", "trader-b", "trader")
            .unwrap();
        store
            .upsert_room_member("room-a", "spectator", "spectator")
            .unwrap();
        store
            .assign_account_owner("room-a", 10, "trader-a")
            .unwrap();
        store
            .assign_account_owner("room-a", 20, "trader-b")
            .unwrap();
        assert!(
            store
                .user_can_access_account("owner", "room-a", 10)
                .unwrap()
        );
        assert!(
            !store
                .user_can_access_account("instructor", "room-a", 10)
                .unwrap()
        );
        store
            .assign_account_owner("room-a", 10, "instructor")
            .unwrap();
        assert!(
            store
                .user_can_access_account("instructor", "room-a", 10)
                .unwrap()
        );
        assert!(
            store
                .user_can_access_account("trader-a", "room-a", 10)
                .unwrap()
        );
        assert!(
            !store
                .user_can_access_account("trader-a", "room-a", 20)
                .unwrap()
        );
        assert!(
            !store
                .user_can_access_account("trader-b", "room-a", 10)
                .unwrap()
        );
        assert!(
            store
                .assign_account_owner("room-a", 10, "spectator")
                .is_err()
        );
        assert!(
            !store
                .user_can_access_account("spectator", "room-a", 10)
                .unwrap()
        );
        store.remove_room_member("room-a", "trader-a").unwrap();
        assert!(
            !store
                .user_can_access_account("trader-a", "room-a", 10)
                .unwrap()
        );
        assert_eq!(
            store
                .user_room_role("spectator", "room-a")
                .unwrap()
                .as_deref(),
            Some("spectator")
        );
    }
}
