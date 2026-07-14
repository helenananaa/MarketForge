use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
    env,
    error::Error,
    fmt, thread,
};

use exchange_core::{
    ActorExecution, Command, MarketStatus, RoomBootstrap, ScenarioConfig, SimulationRoom,
    model::{AccountId, OrderKind, Side},
    transfer::{VenueTransfer, VenueTransferKind, VenueTransferRejectReason, VenueTransferStatus},
};
use postgres::{Client, NoTls};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ClearingEventSummary, EventSummary, RoomExecutionSummary};

const DATABASE_URL_ENV: &str = "MARKETFORGE_DATABASE_URL";
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
];

pub trait JournalStore: Send {
    fn load_recovery(&mut self) -> Result<JournalRecovery, JournalError>;

    fn create_room(
        &mut self,
        owner_user_id: &str,
        scenario: &ScenarioConfig,
        bootstrap: &RoomBootstrap,
        account_ids: &[AccountId],
        seed_records: &[JournalExecution],
        initial_snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError>;

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

    fn update_room_status(
        &mut self,
        room_id: &str,
        status: MarketStatus,
    ) -> Result<(), JournalError>;

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
    match env::var(DATABASE_URL_ENV) {
        Ok(url) if !url.trim().is_empty() => {
            Ok(Box::new(PostgresJournalStore::connect_migrated(&url)?))
        }
        _ => Ok(Box::new(InMemoryJournalStore::new())),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct JournalExecution {
    pub room_id: String,
    pub command_seq: u64,
    pub participant_id: Option<String>,
    pub account_id: Option<AccountId>,
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
            command,
            execution,
        }
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

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct JournalRecovery {
    pub rooms: Vec<JournalRoom>,
    pub executions: Vec<JournalExecution>,
    pub snapshots: Vec<JournalSnapshot>,
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
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TradeProjection {
    pub room_id: String,
    pub instrument_id: String,
    pub trade_id: i64,
    pub command_seq: i64,
    pub event_seq: i64,
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
    pub margin_status: Option<String>,
    pub fees_paid: i64,
}

#[derive(Debug, Default)]
pub struct InMemoryJournalStore {
    rooms: Vec<StoredRoom>,
    executions: Vec<JournalExecution>,
    transfers: BTreeMap<(String, u64), VenueTransfer>,
    snapshots: Vec<JournalSnapshot>,
    room_members: BTreeMap<(String, String), String>,
    account_owners: BTreeMap<(String, AccountId), String>,
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
            .insert((room_id.to_string(), account_id), user_id.to_string());
    }

    fn user_is_room_admin(&self, user_id: &str, room_id: &str) -> bool {
        self.room_members
            .get(&(room_id.to_string(), user_id.to_string()))
            .is_some_and(|role| role == "owner" || role == "admin")
    }

    fn user_owns_account(&self, user_id: &str, room_id: &str, account_id: AccountId) -> bool {
        self.account_owners
            .get(&(room_id.to_string(), account_id))
            .is_some_and(|owner| owner == user_id)
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
            snapshots: self.latest_snapshots(),
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
        }
        self.room_members.insert(
            (bootstrap.room_id.clone(), owner_user_id.to_string()),
            "owner".to_string(),
        );
        for account_id in account_ids {
            self.account_owners.insert(
                (bootstrap.room_id.clone(), *account_id),
                owner_user_id.to_string(),
            );
        }
        Ok(())
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
        if self.user_is_room_admin(user_id, room_id) {
            return Ok(true);
        }

        Ok(self.user_owns_account(user_id, room_id, account_id))
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
        let is_admin = self.user_is_room_admin(user_id, room_id);
        let mut orders = projections
            .orders
            .into_values()
            .filter(|order| {
                order.room_id == room_id
                    && instrument_id.is_none_or(|id| order.instrument_id == id)
                    && account_id.is_none_or(|id| u64::try_from(order.account_id) == Ok(id))
                    && (is_admin
                        || u64::try_from(order.account_id)
                            .is_ok_and(|id| self.user_owns_account(user_id, room_id, id)))
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
        let is_admin = self.user_is_room_admin(user_id, room_id);
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
                    && (is_admin
                        || maker_account_id
                            .is_some_and(|id| self.user_owns_account(user_id, room_id, id))
                        || taker_account_id
                            .is_some_and(|id| self.user_owns_account(user_id, room_id, id)))
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
        let is_admin = self.user_is_room_admin(user_id, room_id);
        let mut ledger = projections
            .account_ledger
            .into_iter()
            .filter(|row| {
                let row_account_id = u64::try_from(row.account_id).ok();
                row.room_id == room_id
                    && instrument_id.is_none_or(|id| row.instrument_id == id)
                    && account_id.is_none_or(|id| row_account_id == Some(id))
                    && (is_admin
                        || row_account_id
                            .is_some_and(|id| self.user_owns_account(user_id, room_id, id)))
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
        let is_admin = self.user_is_room_admin(user_id, room_id);
        let mut positions = projections
            .position_snapshots
            .into_iter()
            .filter(|row| {
                let row_account_id = u64::try_from(row.account_id).ok();
                row.room_id == room_id
                    && instrument_id.is_none_or(|id| row.instrument_id == id)
                    && account_id.is_none_or(|id| row_account_id == Some(id))
                    && (is_admin
                        || row_account_id
                            .is_some_and(|id| self.user_owns_account(user_id, room_id, id)))
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
        let is_admin = self.user_is_room_admin(user_id, room_id);
        let mut transfers = self
            .transfers
            .iter()
            .filter(|((transfer_room_id, _), transfer)| {
                transfer_room_id == room_id
                    && account_id.is_none_or(|account_id| transfer.account_id == account_id)
                    && (is_admin || self.user_owns_account(user_id, room_id, transfer.account_id))
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
}

impl PostgresJournalStore {
    pub fn connect(database_url: &str) -> Result<Self, JournalError> {
        Ok(Self {
            database_url: database_url.to_string(),
        })
    }

    pub fn connect_migrated(database_url: &str) -> Result<Self, JournalError> {
        let mut store = Self::connect(database_url)?;
        store.ensure_schema()?;
        Ok(store)
    }

    fn ensure_schema(&mut self) -> Result<(), JournalError> {
        let database_url = self.database_url.clone();
        run_postgres(database_url, run_schema_migrations)
    }

    fn insert_execution(
        tx: &mut postgres::Transaction<'_>,
        record: &JournalExecution,
    ) -> Result<(), JournalError> {
        let command_seq = i64::try_from(record.command_seq)
            .map_err(|_| JournalError::SequenceOutOfRange(record.command_seq))?;
        let clearing_event_count = i32::try_from(record.execution.clearing_event_count)
            .map_err(|_| JournalError::CountOutOfRange(record.execution.clearing_event_count))?;
        let account_id = record.account_id.map(|id| id.to_string());
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
                clearing_event_count
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
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
                    updated_command_seq
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $9, $11, $11)
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
                        taker_side
                    )
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
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
                        taker_side
                    )
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                    ON CONFLICT (room_id, command_seq, event_seq)
                    DO UPDATE SET
                        trade_id = EXCLUDED.trade_id,
                        price_tick = EXCLUDED.price_tick,
                        qty = EXCLUDED.qty,
                        taker_side = EXCLUDED.taker_side
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
                        margin_status,
                        fees_paid,
                        payload_json
                    )
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                            $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25, $26)
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
                        margin_status,
                        fees_paid,
                        payload_json
                    )
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                            $11, $12, $13, $14, $15, $16, $17, $18)
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
}

impl JournalStore for PostgresJournalStore {
    fn load_recovery(&mut self) -> Result<JournalRecovery, JournalError> {
        let database_url = self.database_url.clone();
        run_postgres(database_url, |client| {
            let rooms = client
                .query(
                    r#"
                    SELECT room_id, scenario_json, status
                    FROM marketforge_rooms
                    ORDER BY created_at, room_id
                    "#,
                    &[],
                )
                .map_err(JournalError::Postgres)?
                .into_iter()
                .map(|row| {
                    let room_id: String = row.get("room_id");
                    let scenario_json: Value = row.get("scenario_json");
                    let status: String = row.get("status");
                    Ok(JournalRoom {
                        room_id,
                        scenario: serde_json::from_value(scenario_json)
                            .map_err(JournalError::Serialize)?,
                        status: status_from_name(&status)?,
                    })
                })
                .collect::<Result<Vec<_>, JournalError>>()?;

            let executions = client
                .query(
                    r#"
                    SELECT room_id, command_seq, participant_id, account_id, command_json, execution_json
                    FROM marketforge_executions
                    ORDER BY room_id, command_seq
                    "#,
                    &[],
                )
                .map_err(JournalError::Postgres)?
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
                        command: serde_json::from_value(command_json)
                            .map_err(JournalError::Serialize)?,
                        execution: serde_json::from_value(execution_json)
                            .map_err(JournalError::Serialize)?,
                    })
                })
                .collect::<Result<Vec<_>, JournalError>>()?;

            let snapshots = client
                .query(
                    r#"
                    SELECT DISTINCT ON (room_id) room_id, command_seq, actor_json
                    FROM marketforge_room_snapshots
                    ORDER BY room_id, command_seq DESC
                    "#,
                    &[],
                )
                .map_err(JournalError::Postgres)?
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
                snapshots,
            })
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
        let database_url = self.database_url.clone();
        let owner_user_id = owner_user_id.to_string();
        let scenario = scenario.clone();
        let bootstrap = bootstrap.clone();
        let account_ids = account_ids.to_vec();
        let seed_records = seed_records.to_vec();
        let initial_snapshot = initial_snapshot.cloned();

        run_postgres(database_url, move |client| {
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
            }

            tx.commit().map_err(JournalError::Postgres)
        })
    }

    fn append_executions(
        &mut self,
        records: &[JournalExecution],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        let database_url = self.database_url.clone();
        let records = records.to_vec();
        let snapshot = snapshot.cloned();
        run_postgres(database_url, move |client| {
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
        let database_url = self.database_url.clone();
        let records = records.to_vec();
        let snapshot = snapshot.cloned();
        run_postgres(database_url, move |client| {
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
        let database_url = self.database_url.clone();
        let snapshot = snapshot.clone();
        run_postgres(database_url, move |client| {
            let mut tx = client.transaction().map_err(JournalError::Postgres)?;
            Self::insert_snapshot(&mut tx, &snapshot)?;
            tx.commit().map_err(JournalError::Postgres)
        })
    }

    fn update_room_status(
        &mut self,
        room_id: &str,
        status: MarketStatus,
    ) -> Result<(), JournalError> {
        let database_url = self.database_url.clone();
        let room_id = room_id.to_string();
        run_postgres(database_url, move |client| {
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

    fn user_can_access_room(&mut self, user_id: &str, room_id: &str) -> Result<bool, JournalError> {
        let database_url = self.database_url.clone();
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        run_postgres(database_url, move |client| {
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
        let database_url = self.database_url.clone();
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        run_postgres(database_url, move |client| {
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
        let database_url = self.database_url.clone();
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        let account_id = i64_from_u64(account_id, "account_id")?;
        run_postgres(database_url, move |client| {
            let count: i64 = client
                .query_one(
                    r#"
                    SELECT count(*)
                    FROM marketforge_room_members member
                    WHERE member.room_id = $1
                      AND member.user_id = $2
                      AND (
                          member.role IN ('owner', 'admin')
                          OR EXISTS (
                              SELECT 1
                              FROM marketforge_account_owners owner
                              WHERE owner.room_id = $1
                                AND owner.account_id = $3
                                AND owner.user_id = $2
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
        let database_url = self.database_url.clone();
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        let instrument_id = instrument_id.map(str::to_string);
        let account_id = optional_i64_account_id(account_id)?;
        let limit = bounded_query_limit(limit)?;
        run_postgres(database_url, move |client| {
            client
                .query(
                    r#"
                    SELECT room_id, instrument_id, order_id, account_id, participant_id, side, order_type,
                           limit_price_tick, original_qty, status, remaining_qty,
                           created_command_seq, updated_command_seq
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
                          OR EXISTS (
                              SELECT 1
                              FROM marketforge_account_owners owner
                              WHERE owner.room_id = marketforge_orders.room_id
                                AND owner.account_id = marketforge_orders.account_id
                                AND owner.user_id = $5
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
        let database_url = self.database_url.clone();
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        let instrument_id = instrument_id.map(str::to_string);
        let account_id = optional_i64_account_id(account_id)?;
        let limit = bounded_query_limit(limit)?;
        run_postgres(database_url, move |client| {
            client
                .query(
                    r#"
                    SELECT room_id, instrument_id, trade_id, command_seq, event_seq, maker_order_id,
                           maker_account_id, taker_order_id, taker_account_id,
                           price_tick, qty, taker_side
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
                          OR EXISTS (
                              SELECT 1
                              FROM marketforge_account_owners owner
                              WHERE owner.room_id = marketforge_trades.room_id
                                AND owner.user_id = $5
                                AND owner.account_id IN (maker_account_id, taker_account_id)
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
        let database_url = self.database_url.clone();
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        let instrument_id = instrument_id.map(str::to_string);
        let limit = bounded_query_limit(limit)?;
        run_postgres(database_url, move |client| {
            client
                .query(
                    r#"
                    SELECT room_id, instrument_id, command_seq, event_seq, trade_id, price_tick, qty, taker_side
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
        let database_url = self.database_url.clone();
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        let instrument_id = instrument_id.map(str::to_string);
        let account_id = optional_i64_account_id(account_id)?;
        let limit = bounded_query_limit(limit)?;
        run_postgres(database_url, move |client| {
            client
                .query(
                    r#"
                    SELECT room_id, instrument_id, command_seq, ledger_seq, market_kind, account_id, trade_id,
                           account_side, cash_delta, position_delta, fee, realized_pnl,
                           price_tick, qty, notional, cash_balance, position_qty,
                           avg_entry_price_tick, realized_pnl_total, unrealized_pnl,
                           equity, initial_margin, maintenance_margin, margin_status, fees_paid
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
                          OR EXISTS (
                              SELECT 1
                              FROM marketforge_account_owners owner
                              WHERE owner.room_id = marketforge_account_ledger.room_id
                                AND owner.account_id = marketforge_account_ledger.account_id
                                AND owner.user_id = $5
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
        let database_url = self.database_url.clone();
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        let instrument_id = instrument_id.map(str::to_string);
        let account_id = optional_i64_account_id(account_id)?;
        let limit = bounded_query_limit(limit)?;
        run_postgres(database_url, move |client| {
            client
                .query(
                    r#"
                    SELECT room_id, instrument_id, command_seq, ledger_seq, market_kind, account_id, trade_id,
                           cash_balance, position_qty, avg_entry_price_tick, realized_pnl,
                           unrealized_pnl, equity, initial_margin, maintenance_margin,
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
                          OR EXISTS (
                              SELECT 1
                              FROM marketforge_account_owners owner
                              WHERE owner.room_id = marketforge_position_snapshots.room_id
                                AND owner.account_id = marketforge_position_snapshots.account_id
                                AND owner.user_id = $5
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
        let database_url = self.database_url.clone();
        let user_id = user_id.to_string();
        let room_id = room_id.to_string();
        let account_id = optional_i64_account_id(account_id)?;
        let limit = bounded_query_limit(limit)?;
        run_postgres(database_url, move |client| {
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
                          OR EXISTS (
                              SELECT 1
                              FROM marketforge_account_owners owner
                              WHERE owner.room_id = marketforge_transfers.room_id
                                AND owner.account_id = marketforge_transfers.account_id
                                AND owner.user_id = $4
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
        let mut ledger_keys = BTreeSet::new();

        for record in records {
            if !execution_keys.insert((record.room_id.clone(), record.command_seq)) {
                return Err(JournalError::DuplicateExecution {
                    room_id: record.room_id.clone(),
                    command_seq: record.command_seq,
                });
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
                    margin_status: row.margin_status,
                    fees_paid: row.fees_paid,
                });
            }
        }

        Ok(())
    }
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
    WorkerPanic,
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
            Self::WorkerPanic => write!(f, "postgres journal worker panicked"),
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

fn run_postgres<T, F>(database_url: String, operation: F) -> Result<T, JournalError>
where
    T: Send + 'static,
    F: FnOnce(&mut Client) -> Result<T, JournalError> + Send + 'static,
{
    thread::spawn(move || {
        let mut client = Client::connect(&database_url, NoTls).map_err(JournalError::Postgres)?;
        operation(&mut client)
    })
    .join()
    .map_err(|_| JournalError::WorkerPanic)?
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
            command,
            execution: RoomExecutionSummary {
                room_id: "room-1".to_string(),
                instrument_id: Some("V-BTC-SPOT".to_string()),
                command_seq,
                status: MarketStatus::Running,
                accepted: true,
                reject_reason: None,
                clearing_event_count: clearing_events.len(),
                events,
                clearing_events,
            },
        }
    }

    fn empty_room(room_id: &str) -> SimulationRoom {
        SimulationRoom::from_scenario(ScenarioConfig {
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
        })
        .unwrap()
        .room
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
    fn released_migrations_stay_immutable_and_repairs_are_new_versions() {
        let initial = include_str!("../migrations/0001_initial_schema.sql");
        let margin_fields = include_str!("../migrations/0005_margin_projection_fields.sql");
        let legacy_room_claim = include_str!("../migrations/0006_claim_unowned_legacy_rooms.sql");

        assert!(!initial.contains("maintenance_margin"));
        assert!(!initial.contains("margin_status"));
        assert!(margin_fields.contains("ADD COLUMN IF NOT EXISTS maintenance_margin"));
        assert!(margin_fields.contains("ADD COLUMN IF NOT EXISTS margin_status"));
        assert!(legacy_room_claim.contains("'local-user', 'owner'"));
        assert!(legacy_room_claim.contains("WHERE NOT EXISTS"));
        assert_eq!(
            MIGRATIONS.last().map(|migration| migration.version),
            Some(6)
        );
    }

    #[test]
    fn in_memory_projection_queries_match_role_and_account_visibility() {
        let mut store = InMemoryJournalStore::new();
        store.set_room_member_for_test("room-1", "admin", "admin");
        store.set_room_member_for_test("room-1", "trader", "member");
        store.set_room_member_for_test("room-1", "viewer", "member");
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
        assert!(
            store
                .query_orders("viewer", "room-1", None, None, 100)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .query_trades("trader", "room-1", None, None, 100)
                .unwrap()
                .len(),
            1
        );
        assert!(
            store
                .query_trades("viewer", "room-1", None, None, 100)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .query_market_ticks("viewer", "room-1", None, 100)
                .unwrap()
                .len(),
            1
        );
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
        advanced.advance_clock(1);

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
}
