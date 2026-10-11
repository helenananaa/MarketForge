//! Durable, bounded query projection batches. This worker never owns actor state.
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

const PROJECTOR_LOCK: i32 = i32::from_be_bytes(*b"PRJ1");
const BATCH_SIZE: i64 = 256;
const QUERY_WAIT: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct QueryProjectionMetrics {
    pending: AtomicU64,
    batches: AtomicU64,
    records: AtomicU64,
    errors: AtomicU64,
}

impl QueryProjectionMetrics {
    pub(crate) fn snapshot(&self) -> [u64; 4] {
        [&self.pending, &self.batches, &self.records, &self.errors]
            .map(|value| value.load(Ordering::Relaxed))
    }
}

pub(super) struct Projector {
    pub(super) metrics: Arc<QueryProjectionMetrics>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Projector {
    pub(super) fn start(url: String) -> Result<Self, JournalError> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let metrics = Arc::new(QueryProjectionMetrics::default());
        let monitor = metrics.clone();
        let worker = thread::Builder::new()
            .name("marketforge-query-projector".into())
            .spawn(move || {
                let mut connection = None;
                let mut reported = false;
                while !stopping.load(Ordering::Acquire) {
                    let result = (|| {
                        if connection.as_ref().is_none_or(Client::is_closed) {
                            let mut client =
                                Client::connect(&url, NoTls).map_err(JournalError::Postgres)?;
                            client
                                .batch_execute("SET statement_timeout='5s'; SET lock_timeout='2s'")
                                .map_err(JournalError::Postgres)?;
                            connection = Some(client);
                        }
                        project_batch(connection.as_mut().expect("connected"), &monitor)
                    })();
                    match result {
                        Ok(count) => {
                            reported = false;
                            if count > 0 {
                                monitor.batches.fetch_add(1, Ordering::Relaxed);
                                monitor.records.fetch_add(count as u64, Ordering::Relaxed);
                            }
                            if count == 0 {
                                thread::sleep(Duration::from_millis(10));
                            }
                        }
                        Err(error) => {
                            monitor.errors.fetch_add(1, Ordering::Relaxed);
                            if !reported {
                                eprintln!("query projector will retry durable backlog: {error}");
                            }
                            reported = true;
                            thread::sleep(Duration::from_millis(100));
                        }
                    }
                }
            })
            .map_err(|error| JournalError::Recovery(format!("start query projector: {error}")))?;
        Ok(Self {
            metrics,
            stop,
            worker: Some(worker),
        })
    }
}

impl Drop for Projector {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Keep the old numeric/serialization and identity checks on the commit path.
/// Query rows cannot supply these checks once they may lag behind the journal.
pub(super) fn validate_and_claim(
    tx: &mut CachedTransaction<'_>,
    records: &[JournalExecution],
) -> Result<(), JournalError> {
    let mut profile = crate::write_profile::Profile::new("pg_claim");
    let projected = MemoryProjections::from_executions(records)?;
    profile.mark("projection_validation");
    let mut rooms = BTreeMap::<&str, i64>::new();
    let mut claims = BTreeMap::new();
    for record in records {
        *rooms.entry(&record.room_id).or_default() += 1;
        let instrument = record
            .execution
            .instrument_id
            .as_deref()
            .unwrap_or("legacy-primary");
        if let Some(order) = record.command.new_order() {
            claims
                .entry((
                    record.room_id.clone(),
                    instrument.to_string(),
                    i64_from_u64(order.order_id, "order_id")?,
                ))
                .or_insert(new_order_did_not_create_order(record, order.order_id));
        }
    }
    let capacity: Vec<_> = rooms
        .into_iter()
        .map(|(room_id, count)| serde_json::json!({"room_id":room_id,"count":count}))
        .collect();
    let overloaded: bool = tx.query_one(
        "SELECT EXISTS(SELECT 1 FROM jsonb_to_recordset($1) AS incoming(room_id TEXT,count BIGINT) \
         WHERE incoming.count>65536 OR EXISTS (SELECT 1 FROM marketforge_projection_queue q \
         WHERE q.room_id=incoming.room_id OFFSET GREATEST(65536-incoming.count,0) LIMIT 1))",
        &[&Value::Array(capacity)],
    ).map_err(JournalError::Postgres)?.get(0);
    if overloaded {
        return Err(JournalError::Recovery(
            "query projection backlog limit reached; retry after catch-up".into(),
        ));
    }
    profile.mark("capacity");
    if !claims.is_empty() {
        let rows: Vec<_> = claims.keys().map(|(room_id,instrument_id,order_id)|serde_json::json!({"room_id":room_id,"instrument_id":instrument_id,"order_id":order_id})).collect();
        let inserted: BTreeSet<(String,String,i64)> = tx.query(
            "INSERT INTO marketforge_order_keys(room_id,instrument_id,order_id) \
             SELECT room_id,instrument_id,order_id FROM jsonb_populate_recordset(NULL::marketforge_order_keys,$1) \
             ON CONFLICT DO NOTHING RETURNING room_id,instrument_id,order_id",
            &[&Value::Array(rows)],
        ).map_err(JournalError::Postgres)?.into_iter().map(|row|(row.get(0),row.get(1),row.get(2))).collect();
        for ((room_id, instrument_id, id), allow_duplicate) in claims {
            if !allow_duplicate && !inserted.contains(&(room_id.clone(), instrument_id.clone(), id))
            {
                return Err(JournalError::ProjectionConflict {
                    entity: "order",
                    room_id,
                    instrument_id,
                    id,
                });
            }
        }
    }
    profile.mark("order_keys");
    if !projected.trades.is_empty() {
        let rows: Vec<_> = projected.trades.values().map(|trade|serde_json::json!({"room_id":trade.room_id,"instrument_id":trade.instrument_id,"trade_id":trade.trade_id})).collect();
        tx.execute(
            "INSERT INTO marketforge_trade_keys(room_id,instrument_id,trade_id) \
             SELECT room_id,instrument_id,trade_id FROM jsonb_populate_recordset(NULL::marketforge_trade_keys,$1)",
            &[&Value::Array(rows)],
        ).map_err(JournalError::Postgres)?;
    }
    Ok(())
}

/// Capture only already-committed work at query entry. New writes do not extend
/// the barrier forever, and authorization still runs in the original query.
pub(super) fn wait_for_room(client: &mut Client, room: &str) -> Result<(), JournalError> {
    let row = client
        .query_one(
            "SELECT (SELECT MAX(queue_id) FROM marketforge_projection_queue WHERE room_id=$1), \
                    (SELECT MAX(queue_id) FROM marketforge_transfer_projection_queue WHERE room_id=$1)",
            &[&room],
        )
        .map_err(JournalError::Postgres)?;
    let target: Option<i64> = row.get(0);
    let transfer_target: Option<i64> = row.get(1);
    if target.is_none() && transfer_target.is_none() {
        return Ok(());
    }
    let started = Instant::now();
    loop {
        let pending: bool = client.query_one(
            "SELECT EXISTS(SELECT 1 FROM marketforge_projection_queue WHERE room_id=$1 AND queue_id<=$2) \
             OR EXISTS(SELECT 1 FROM marketforge_transfer_projection_queue WHERE room_id=$1 AND queue_id<=$3)",
            &[&room, &target, &transfer_target],
        ).map_err(JournalError::Postgres)?.get(0);
        if !pending {
            return Ok(());
        }
        if started.elapsed() >= QUERY_WAIT {
            return Err(JournalError::Recovery(format!(
                "query projection for room {room} has not reached the committed journal"
            )));
        }
        thread::sleep(Duration::from_millis(2));
    }
}

pub(super) fn project_batch(
    client: &mut Client,
    metrics: &QueryProjectionMetrics,
) -> Result<usize, JournalError> {
    let mut tx = client.transaction().map_err(JournalError::Postgres)?;
    // One global projection writer across single-active and room-leased servers.
    // A crashed worker releases this lock and rolls back rows plus progress.
    let locked: bool = tx
        .query_one(
            "SELECT pg_try_advisory_xact_lock($1,$2)",
            &[&POSTGRES_RUNTIME_LOCK_NAMESPACE, &PROJECTOR_LOCK],
        )
        .map_err(JournalError::Postgres)?
        .get(0);
    if !locked {
        return Ok(0);
    }
    let transfer_count = project_transfers(&mut tx)?;
    let transfer_pending: i64 = if transfer_count >= BATCH_SIZE as usize {
        tx.query_one(
            "SELECT COUNT(*) FROM marketforge_transfer_projection_queue",
            &[],
        )
        .map_err(JournalError::Postgres)?
        .get(0)
    } else {
        0
    };
    let rows = tx.query(
        "SELECT q.queue_id,e.room_id,e.command_seq,e.participant_id,e.account_id,e.command_json,e.execution_json \
         FROM (SELECT * FROM marketforge_projection_queue ORDER BY queue_id LIMIT $1) q \
         JOIN marketforge_executions e USING(room_id,command_seq) ORDER BY q.queue_id",
        &[&BATCH_SIZE],
    ).map_err(JournalError::Postgres)?;
    let pending: i64 = if rows.len() < BATCH_SIZE as usize {
        rows.len() as i64
    } else {
        tx.query_one("SELECT COUNT(*) FROM marketforge_projection_queue", &[])
            .map_err(JournalError::Postgres)?
            .get(0)
    };
    metrics
        .pending
        .store((pending + transfer_pending) as u64, Ordering::Relaxed);
    if rows.is_empty() {
        tx.commit().map_err(JournalError::Postgres)?;
        return Ok(transfer_count);
    }
    let mut records = Vec::with_capacity(rows.len());
    let mut queue_ids = Vec::with_capacity(rows.len());
    for row in rows {
        queue_ids.push(row.get::<_, i64>("queue_id"));
        let account: Option<String> = row.get("account_id");
        records.push(JournalExecution {
            room_id: row.get("room_id"),
            command_seq: row.get::<_, i64>("command_seq") as u64,
            participant_id: row.get("participant_id"),
            account_id: account
                .map(|id| id.parse().map_err(|_| JournalError::InvalidAccountId(id)))
                .transpose()?,
            // Idempotency is checked by the canonical transaction, not projected.
            request_user_id: None,
            idempotency_key: None,
            request_fingerprint: None,
            quota_user_step: None,
            command: serde_json::from_value(row.get("command_json"))
                .map_err(JournalError::Serialize)?,
            execution: serde_json::from_value(row.get("execution_json"))
                .map_err(JournalError::Serialize)?,
        });
    }
    // Always replay a room's commands in their canonical order, independent of
    // allocation gaps in the outbox's global sequence.
    records.sort_by(|a, b| (&a.room_id, a.command_seq).cmp(&(&b.room_id, b.command_seq)));
    let mut keys = BTreeSet::new();
    for record in &records {
        let instrument = record
            .execution
            .instrument_id
            .as_deref()
            .unwrap_or("legacy-primary");
        for id in record
            .command
            .new_order()
            .map(|order| order.order_id)
            .into_iter()
            .chain(
                record
                    .execution
                    .events
                    .iter()
                    .filter_map(|e| order_status_update(e).map(|v| v.0)),
            )
        {
            keys.insert((
                record.room_id.clone(),
                instrument.to_string(),
                i64_from_u64(id, "order_id")?,
            ));
        }
    }
    let keys: Vec<_> = keys.into_iter().map(|(room_id,instrument_id,order_id)|
        serde_json::json!({"room_id":room_id,"instrument_id":instrument_id,"order_id":order_id})).collect();
    let mut projected = MemoryProjections::default();
    for row in tx.query(
        "SELECT to_jsonb(o) FROM marketforge_orders o JOIN \
         jsonb_to_recordset($1) AS k(room_id TEXT,instrument_id TEXT,order_id BIGINT) USING(room_id,instrument_id,order_id)",
        &[&serde_json::to_value(keys).map_err(JournalError::Serialize)?],
    ).map_err(JournalError::Postgres)? {
        let order: OrderProjection = serde_json::from_value(row.get(0)).map_err(JournalError::Serialize)?;
        projected.orders.insert((order.room_id.clone(),order.instrument_id.clone(),order.order_id),order);
    }
    projected.append(&records)?;
    upsert_rows(
        &mut tx,
        "marketforge_orders",
        &serialize_rows(projected.orders.values())?,
        "room_id,instrument_id,order_id,account_id,participant_id,side,order_type,limit_price_tick,original_qty,status,remaining_qty,created_command_seq,updated_command_seq,created_market_time_ms,updated_market_time_ms",
        "room_id,instrument_id,order_id",
        "status,remaining_qty,limit_price_tick,updated_command_seq,updated_market_time_ms",
    )?;
    upsert_rows(
        &mut tx,
        "marketforge_trades",
        &serialize_rows(projected.trades.values())?,
        "room_id,instrument_id,trade_id,command_seq,event_seq,maker_order_id,maker_account_id,taker_order_id,taker_account_id,price_tick,qty,taker_side,market_time_ms",
        "room_id,instrument_id,trade_id",
        "",
    )?;
    upsert_rows(
        &mut tx,
        "marketforge_market_ticks",
        &serialize_rows(projected.market_ticks.values())?,
        "room_id,instrument_id,command_seq,event_seq,trade_id,price_tick,qty,taker_side,market_time_ms",
        "room_id,command_seq,event_seq",
        "trade_id,price_tick,qty,taker_side,market_time_ms",
    )?;
    let mut events = BTreeMap::new();
    let mut ledger = Vec::new();
    let mut positions = Vec::new();
    let mut progress = BTreeMap::new();
    for record in &records {
        let instrument = record
            .execution
            .instrument_id
            .as_deref()
            .unwrap_or("legacy-primary");
        for event in &record.execution.events {
            let row = serde_json::json!({"room_id":record.room_id,"instrument_id":instrument,
                "command_seq":record.command_seq,"event_seq":event_seq(event),"event_type":event_type(event),
                "order_id":event_order_id(event),"reason":event_reason(event),"price_tick":event_price_tick(event),
                "qty":event_qty(event),"remaining_qty":event_remaining_qty(event),"payload_json":event});
            events.insert(
                (record.room_id.clone(), record.command_seq, event_seq(event)),
                row,
            );
        }
        for (index, event) in record.execution.clearing_events.iter().enumerate() {
            for row in clearing_ledger_rows(index, event)? {
                let mut value = serde_json::to_value(&row).map_err(JournalError::Serialize)?;
                let object = value.as_object_mut().expect("ledger object");
                object.insert("room_id".into(), record.room_id.clone().into());
                object.insert("instrument_id".into(), instrument.into());
                object.insert("command_seq".into(), record.command_seq.into());
                ledger.push(value.clone());
                // Position rows expose cumulative PnL, not this fill's delta.
                value["realized_pnl"] = serde_json::to_value(row.realized_pnl_total)
                    .map_err(JournalError::Serialize)?;
                positions.push(value);
            }
        }
        progress.insert(record.room_id.clone(), record.command_seq);
    }
    upsert_rows(
        &mut tx,
        "marketforge_order_events",
        &events.into_values().collect::<Vec<_>>(),
        "room_id,instrument_id,command_seq,event_seq,event_type,order_id,reason,price_tick,qty,remaining_qty,payload_json",
        "room_id,command_seq,event_seq",
        "event_type,order_id,reason,price_tick,qty,remaining_qty,payload_json",
    )?;
    let ledger_columns = "room_id,instrument_id,command_seq,ledger_seq,market_kind,account_id,trade_id,account_side,cash_delta,position_delta,fee,realized_pnl,price_tick,qty,notional,cash_balance,position_qty,avg_entry_price_tick,realized_pnl_total,unrealized_pnl,equity,initial_margin,maintenance_margin,portfolio_initial_margin,portfolio_maintenance_margin,margin_status,fees_paid,payload_json";
    upsert_rows(
        &mut tx,
        "marketforge_account_ledger",
        &ledger,
        ledger_columns,
        "room_id,command_seq,ledger_seq",
        "",
    )?;
    upsert_rows(
        &mut tx,
        "marketforge_position_snapshots",
        &positions,
        "room_id,instrument_id,command_seq,ledger_seq,market_kind,account_id,trade_id,cash_balance,position_qty,avg_entry_price_tick,realized_pnl,unrealized_pnl,equity,initial_margin,maintenance_margin,portfolio_initial_margin,portfolio_maintenance_margin,margin_status,fees_paid,payload_json",
        "room_id,command_seq,ledger_seq",
        "",
    )?;
    let progress: Vec<_> = progress.into_iter().map(|(room_id,command_seq)|serde_json::json!({"room_id":room_id,"command_seq":command_seq})).collect();
    upsert_rows(
        &mut tx,
        "marketforge_projection_progress",
        &progress,
        "room_id,command_seq",
        "room_id",
        "command_seq",
    )?;
    tx.execute(
        "DELETE FROM marketforge_projection_queue WHERE queue_id=ANY($1)",
        &[&queue_ids],
    )
    .map_err(JournalError::Postgres)?;
    tx.commit().map_err(JournalError::Postgres)?;
    metrics.pending.store(
        ((pending + transfer_pending) as u64).saturating_sub(queue_ids.len() as u64),
        Ordering::Relaxed,
    );
    Ok(queue_ids.len() + transfer_count)
}

fn project_transfers(tx: &mut CachedTransaction<'_>) -> Result<usize, JournalError> {
    let rows = tx
        .query(
            "SELECT q.queue_id,to_jsonb(e) AS event FROM \
         (SELECT * FROM marketforge_transfer_projection_queue ORDER BY queue_id LIMIT $1) q \
         JOIN marketforge_transfer_events e USING(room_id,transfer_id,event_seq) \
         ORDER BY q.room_id,q.transfer_id,q.event_seq",
            &[&BATCH_SIZE],
        )
        .map_err(JournalError::Postgres)?;
    if rows.is_empty() {
        return Ok(0);
    }
    let mut latest = BTreeMap::new();
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row.get::<_, i64>("queue_id"));
        let value: Value = row.get("event");
        latest.insert(
            (
                value["room_id"].as_str().unwrap().to_string(),
                value["transfer_id"].as_i64().unwrap(),
            ),
            value,
        );
    }
    upsert_rows(
        tx,
        "marketforge_transfers",
        &latest.into_values().collect::<Vec<_>>(),
        "room_id,transfer_id,kind,account_id,asset_id,amount,requested_at_step,available_after_step,completed_at_step,status,reject_reason,payload_json",
        "room_id,transfer_id",
        "kind,account_id,asset_id,amount,requested_at_step,available_after_step,completed_at_step,status,reject_reason,payload_json",
    )?;
    tx.execute(
        "DELETE FROM marketforge_transfer_projection_queue WHERE queue_id=ANY($1)",
        &[&ids],
    )
    .map_err(JournalError::Postgres)?;
    Ok(ids.len())
}

fn serialize_rows<'a, T: Serialize + 'a>(
    rows: impl Iterator<Item = &'a T>,
) -> Result<Vec<Value>, JournalError> {
    rows.map(|row| serde_json::to_value(row).map_err(JournalError::Serialize))
        .collect()
}

/// All identifiers come from static call sites. Values stay bound parameters;
/// each table receives one set-based statement rather than one per event.
fn upsert_rows(
    tx: &mut CachedTransaction<'_>,
    table: &str,
    rows: &[Value],
    columns: &str,
    keys: &str,
    updates: &str,
) -> Result<(), JournalError> {
    if rows.is_empty() {
        return Ok(());
    }
    let conflict = if updates.is_empty() {
        "DO NOTHING".into()
    } else {
        let assignments = updates
            .split(',')
            .map(|name| format!("{name}=EXCLUDED.{name}"))
            .collect::<Vec<_>>()
            .join(",");
        let timestamp = if matches!(
            table,
            "marketforge_orders" | "marketforge_projection_progress" | "marketforge_transfers"
        ) {
            ",updated_at=now()"
        } else {
            ""
        };
        format!("DO UPDATE SET {assignments}{timestamp}")
    };
    let sql = format!(
        "INSERT INTO {table}({columns}) SELECT {columns} FROM jsonb_populate_recordset(NULL::{table},$1) ON CONFLICT({keys}) {conflict}"
    );
    tx.execute(
        &sql,
        &[&serde_json::to_value(rows).map_err(JournalError::Serialize)?],
    )
    .map_err(JournalError::Postgres)?;
    Ok(())
}
