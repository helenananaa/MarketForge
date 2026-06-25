use std::{cmp::Reverse, collections::BTreeMap, env, error::Error, fmt, thread};

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

    fn append_execution(
        &mut self,
        record: &JournalExecution,
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError>;

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
}

impl JournalStore for InMemoryJournalStore {
    fn load_recovery(&mut self) -> Result<JournalRecovery, JournalError> {
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
            executions: self.executions.clone(),
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

    fn append_execution(
        &mut self,
        record: &JournalExecution,
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        self.executions.push(record.clone());
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
        self.snapshots.push(snapshot.clone());
        Ok(())
    }

    fn user_can_access_room(&mut self, user_id: &str, room_id: &str) -> Result<bool, JournalError> {
        Ok(self
            .room_members
            .contains_key(&(room_id.to_string(), user_id.to_string())))
    }

    fn user_can_access_account(
        &mut self,
        user_id: &str,
        room_id: &str,
        account_id: AccountId,
    ) -> Result<bool, JournalError> {
        if self
            .room_members
            .get(&(room_id.to_string(), user_id.to_string()))
            .is_some_and(|role| role == "owner" || role == "admin")
        {
            return Ok(true);
        }

        Ok(self
            .account_owners
            .get(&(room_id.to_string(), account_id))
            .is_some_and(|owner| owner == user_id))
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
            if current.is_none_or(|current| snapshot.command_seq > current.command_seq) {
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

        if let Command::NewOrder(order) = &record.command {
            let order_id = i64_from_u64(order.order_id, "order_id")?;
            let account_id = i64_from_u64(order.account_id, "account_id")?;
            let original_qty = i64_from_u64(order.qty, "qty")?;
            let (order_type, limit_price_tick) = match order.kind {
                OrderKind::Limit { price_tick } => ("limit", Some(price_tick)),
                OrderKind::Market => ("market", None),
            };

            tx.execute(
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
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'submitted', $9, $10, $10)
                ON CONFLICT (room_id, instrument_id, order_id)
                DO UPDATE SET
                    account_id = EXCLUDED.account_id,
                    participant_id = EXCLUDED.participant_id,
                    side = EXCLUDED.side,
                    order_type = EXCLUDED.order_type,
                    limit_price_tick = EXCLUDED.limit_price_tick,
                    original_qty = EXCLUDED.original_qty,
                    status = EXCLUDED.status,
                    remaining_qty = EXCLUDED.remaining_qty,
                    updated_command_seq = EXCLUDED.updated_command_seq,
                    updated_at = now()
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
                    &command_seq,
                ],
            )
            .map_err(JournalError::Postgres)?;
        }

        for event in &record.execution.events {
            Self::insert_order_event(tx, record, event)?;
            Self::apply_order_event_projection(tx, record, event)?;
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
                    ON CONFLICT (room_id, instrument_id, trade_id)
                    DO UPDATE SET
                        command_seq = EXCLUDED.command_seq,
                        event_seq = EXCLUDED.event_seq,
                        maker_order_id = EXCLUDED.maker_order_id,
                        maker_account_id = EXCLUDED.maker_account_id,
                        taker_order_id = EXCLUDED.taker_order_id,
                        taker_account_id = EXCLUDED.taker_account_id,
                        price_tick = EXCLUDED.price_tick,
                        qty = EXCLUDED.qty,
                        taker_side = EXCLUDED.taker_side
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
        let Some((order_id, status, remaining_qty)) = order_status_update(event) else {
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
                updated_command_seq = $6,
                updated_at = now()
            WHERE room_id = $1 AND instrument_id = $2 AND order_id = $3
            "#,
            &[
                &record.room_id,
                &instrument_id,
                &order_id,
                &status,
                &remaining_qty,
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
                        fees_paid,
                        payload_json
                    )
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                            $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24)
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
                        fees_paid,
                        payload_json
                    )
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                            $11, $12, $13, $14, $15, $16)
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
                    Ok(JournalSnapshot {
                        room_id: row.get("room_id"),
                        command_seq: u64::try_from(command_seq)
                            .map_err(|_| JournalError::InvalidSequence(command_seq))?,
                        actor: serde_json::from_value(actor_json)
                            .map_err(JournalError::Serialize)?,
                    })
                })
                .collect::<Result<Vec<_>, JournalError>>()?;

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

    fn append_execution(
        &mut self,
        record: &JournalExecution,
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        let database_url = self.database_url.clone();
        let record = record.clone();
        let snapshot = snapshot.cloned();
        run_postgres(database_url, move |client| {
            let mut tx = client.transaction().map_err(JournalError::Postgres)?;
            Self::insert_execution(&mut tx, &record)?;
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
                           equity, initial_margin, fees_paid
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
                           unrealized_pnl, equity, initial_margin, fees_paid
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

fn ledger_seq(clearing_index: usize, leg: u64) -> Result<i64, JournalError> {
    let index =
        u64::try_from(clearing_index).map_err(|_| JournalError::CountOutOfRange(clearing_index))?;
    let seq = index
        .checked_mul(2)
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
                        -(i128::from(*notional) + i128::from(*buyer_fee)),
                        "cash_delta",
                    )?,
                    position_delta: i64_from_i128(qty_i128, "position_delta")?,
                    fee: *buyer_fee,
                    realized_pnl: 0,
                    price_tick: *price_tick,
                    qty: qty_i64,
                    notional: *notional,
                    cash_balance: buyer.cash_balance,
                    position_qty: buyer.position_qty,
                    avg_entry_price_tick: None,
                    realized_pnl_total: None,
                    unrealized_pnl: None,
                    equity: spot_equity_at_price(
                        buyer.cash_balance,
                        buyer.position_qty,
                        *price_tick,
                    )?,
                    initial_margin: None,
                    fees_paid: buyer.fees_paid,
                    payload_json: payload_json.clone(),
                },
                LedgerProjection {
                    ledger_seq: ledger_seq(clearing_index, 1)?,
                    market_kind: "spot",
                    account_id: i64_from_u64(*seller_account_id, "seller_account_id")?,
                    trade_id,
                    account_side: "sell",
                    cash_delta: i64_from_i128(
                        i128::from(*notional) - i128::from(*seller_fee),
                        "cash_delta",
                    )?,
                    position_delta: i64_from_i128(-qty_i128, "position_delta")?,
                    fee: *seller_fee,
                    realized_pnl: 0,
                    price_tick: *price_tick,
                    qty: qty_i64,
                    notional: *notional,
                    cash_balance: seller.cash_balance,
                    position_qty: seller.position_qty,
                    avg_entry_price_tick: None,
                    realized_pnl_total: None,
                    unrealized_pnl: None,
                    equity: spot_equity_at_price(
                        seller.cash_balance,
                        seller.position_qty,
                        *price_tick,
                    )?,
                    initial_margin: None,
                    fees_paid: seller.fees_paid,
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
                        i128::from(*buyer_realized_pnl) - i128::from(*buyer_fee),
                        "cash_delta",
                    )?,
                    position_delta: i64_from_i128(qty_i128, "position_delta")?,
                    fee: *buyer_fee,
                    realized_pnl: *buyer_realized_pnl,
                    price_tick: *price_tick,
                    qty: qty_i64,
                    notional: *notional,
                    cash_balance: buyer.cash_balance,
                    position_qty: buyer.position_qty,
                    avg_entry_price_tick: Some(buyer.avg_entry_price_tick),
                    realized_pnl_total: Some(buyer.realized_pnl),
                    unrealized_pnl: Some(buyer.unrealized_pnl),
                    equity: Some(buyer.equity),
                    initial_margin: Some(buyer.initial_margin),
                    fees_paid: buyer.fees_paid,
                    payload_json: payload_json.clone(),
                },
                LedgerProjection {
                    ledger_seq: ledger_seq(clearing_index, 1)?,
                    market_kind: "perp",
                    account_id: i64_from_u64(*seller_account_id, "seller_account_id")?,
                    trade_id,
                    account_side: "sell",
                    cash_delta: i64_from_i128(
                        i128::from(*seller_realized_pnl) - i128::from(*seller_fee),
                        "cash_delta",
                    )?,
                    position_delta: i64_from_i128(-qty_i128, "position_delta")?,
                    fee: *seller_fee,
                    realized_pnl: *seller_realized_pnl,
                    price_tick: *price_tick,
                    qty: qty_i64,
                    notional: *notional,
                    cash_balance: seller.cash_balance,
                    position_qty: seller.position_qty,
                    avg_entry_price_tick: Some(seller.avg_entry_price_tick),
                    realized_pnl_total: Some(seller.realized_pnl),
                    unrealized_pnl: Some(seller.unrealized_pnl),
                    equity: Some(seller.equity),
                    initial_margin: Some(seller.initial_margin),
                    fees_paid: seller.fees_paid,
                    payload_json,
                },
            ])
        }
    }
}

fn spot_equity_at_price(
    cash_balance: i64,
    position_qty: i64,
    price_tick: i64,
) -> Result<Option<i64>, JournalError> {
    if price_tick < 0 {
        return Ok(None);
    }
    let equity = i128::from(cash_balance) + i128::from(position_qty) * i128::from(price_tick);
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
        | EventSummary::CancelRejected { seq, .. } => *seq,
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
        | EventSummary::CancelRejected { order_id, .. } => Some(*order_id),
        EventSummary::TradePrinted { .. } => None,
    }
}

fn event_reason(event: &EventSummary) -> Option<String> {
    match event {
        EventSummary::OrderRejected { reason, .. }
        | EventSummary::RiskRejected { reason, .. }
        | EventSummary::CancelRejected { reason, .. } => Some(reason.clone()),
        _ => None,
    }
}

fn event_price_tick(event: &EventSummary) -> Option<i64> {
    match event {
        EventSummary::TradePrinted { price_tick, .. }
        | EventSummary::OrderRested { price_tick, .. } => Some(*price_tick),
        _ => None,
    }
}

fn event_qty(event: &EventSummary) -> Option<u64> {
    match event {
        EventSummary::TradePrinted { qty, .. } => Some(*qty),
        EventSummary::OrderExpired { unfilled_qty, .. } => Some(*unfilled_qty),
        _ => None,
    }
}

fn event_remaining_qty(event: &EventSummary) -> Option<u64> {
    match event {
        EventSummary::OrderPartiallyFilled { remaining_qty, .. }
        | EventSummary::OrderRested { remaining_qty, .. }
        | EventSummary::OrderCanceled { remaining_qty, .. } => Some(*remaining_qty),
        EventSummary::OrderFilled { .. } => Some(0),
        _ => None,
    }
}

fn order_status_update(event: &EventSummary) -> Option<(u64, &'static str, Option<u64>)> {
    match event {
        EventSummary::OrderAccepted { order_id, .. } => Some((*order_id, "accepted", None)),
        EventSummary::OrderRejected { order_id, .. }
        | EventSummary::RiskRejected { order_id, .. } => Some((*order_id, "rejected", None)),
        EventSummary::OrderPartiallyFilled {
            order_id,
            remaining_qty,
            ..
        } => Some((*order_id, "partially_filled", Some(*remaining_qty))),
        EventSummary::OrderFilled { order_id, .. } => Some((*order_id, "filled", Some(0))),
        EventSummary::OrderRested {
            order_id,
            remaining_qty,
            ..
        } => Some((*order_id, "open", Some(*remaining_qty))),
        EventSummary::OrderExpired {
            order_id,
            unfilled_qty,
            ..
        } => Some((*order_id, "expired", Some(*unfilled_qty))),
        EventSummary::OrderCanceled {
            order_id,
            remaining_qty,
            ..
        } => Some((*order_id, "canceled", Some(*remaining_qty))),
        EventSummary::TradePrinted { .. } | EventSummary::CancelRejected { .. } => None,
    }
}

fn command_account_id(command: &Command) -> Option<AccountId> {
    match command {
        Command::NewOrder(order) => Some(order.account_id),
        Command::CancelOrder(_) => None,
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
