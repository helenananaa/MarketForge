//! Read-only reconstruction used by the storage capacity and archive tools.
use std::time::Instant;

use serde_json::{Value, json};

use crate::journal::{JournalError, JournalRecovery};

/// Rebuilds room, scheduler and training state using the server's actual recovery
/// code. Comparing `state` checks more than matching row counts or file digests.
pub fn audit_recovery(recovery: &JournalRecovery) -> Result<Value, JournalError> {
    inspect_recovery(recovery, true)
}

pub fn audit_runtime_recovery(recovery: &JournalRecovery) -> Result<Value, JournalError> {
    inspect_recovery(recovery, false)
}

fn inspect_recovery(recovery: &JournalRecovery, full_replay: bool) -> Result<Value, JournalError> {
    let started = Instant::now();
    let rooms = if full_replay {
        crate::recover_rooms_for_full_replay(recovery)?
    } else {
        crate::recover_rooms(recovery)?
    };
    let mut actors = Vec::new();
    let mut tickers = serde_json::Map::new();
    for room_id in rooms.room_ids() {
        let instrument_id = rooms
            .room(room_id)
            .map_err(|error| JournalError::Recovery(format!("{error:?}")))?
            .primary_instrument_id();
        tickers.insert(
            room_id.to_string(),
            serde_json::to_value(
                rooms
                    .ticker(room_id, instrument_id)
                    .map_err(|error| JournalError::Recovery(format!("{error:?}")))?,
            )
            .map_err(JournalError::Serialize)?,
        );
        actors.push(
            serde_json::to_value(
                rooms
                    .simulation_room(room_id)
                    .map_err(|error| JournalError::Recovery(format!("{error:?}")))?,
            )
            .map_err(JournalError::Serialize)?,
        );
    }
    let mut state = json!({
        "rooms": actors,
        "tickers": tickers,
        "next_order_id": crate::next_order_id_from_recovery(recovery)?,
        "schedulers": crate::scheduler_states_from_recovery(recovery),
        "training_runs": crate::training_runs_from_recovery(recovery),
    });
    // Plugin state is arbitrary JSON: never normalize its arrays by field name.
    normalize_sets(&mut state["rooms"]);
    Ok(json!({
        "loaded_commands": recovery.executions.len(),
        "loaded_mutations": recovery.mutations.len() + recovery.runtime_checkpoints.len(),
        "snapshot_rows_loaded": recovery.snapshots.len(),
        "replay_ms": started.elapsed().as_secs_f64() * 1000.0,
        "state": state,
    }))
}

// OrderBook serializes this HashSet as an array; iteration order is unrelated to
// state identity. Preserve the order of every actual command/event/queue array.
fn normalize_sets(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            if let Some(Value::Array(ids)) = fields.get_mut("seen_order_ids") {
                ids.sort_by_key(|id| id.as_u64());
            }
            for field in fields.values_mut() {
                normalize_sets(field);
            }
        }
        Value::Array(values) => values.iter_mut().for_each(normalize_sets),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{
        JournalExecution, JournalSnapshot, JournalStore, PendingJournalMutation,
        PostgresJournalStore, RoomMutation,
    };
    use exchange_core::{Command, NewOrder, OrderKind, RoomManager, ScenarioConfig, Side};

    #[test]
    fn postgres_checkpoint_preserves_history_candles_order_cursor_and_tail_replay() {
        let Some(dsn) = std::env::var("MARKETFORGE_TEST_DATABASE_URL").ok() else {
            assert_ne!(
                std::env::var("MARKETFORGE_REQUIRE_POSTGRES_TESTS")
                    .ok()
                    .as_deref(),
                Some("1"),
                "forced PostgreSQL storage test needs a database"
            );
            return;
        };
        let room_id = format!(
            "storage-recovery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let scenario: ScenarioConfig = serde_json::from_value(json!({
            "room_id": room_id,
            "market": {"Spot": {
                "instrument": {"instrument_id":"V-BTC-SPOT","venue_id":"default-venue","symbol":"V-BTC-SPOT","base_asset":"V","quote_asset":"BTC","tick_size":1,"lot_size":1},
                "clearing":{"maker_fee_ppm":0,"taker_fee_ppm":0},"risk":{"allow_short":true}
            }},
            "accounts":[{"Spot":{"account_id":10,"cash_balance":1000000,"position_qty":100}}, {"Spot":{"account_id":20,"cash_balance":1000000,"position_qty":0}}],
            "seed_orders":[]
        })).unwrap();
        let mut rooms = RoomManager::new();
        let bootstrap = rooms.create_room(scenario.clone()).unwrap();
        let mut store = PostgresJournalStore::connect_migrated(&dsn).unwrap();
        store
            .create_room("owner", &scenario, &bootstrap, &[10, 20], &[], None)
            .unwrap();
        for (id, account_id, side, price) in [
            (1, 10, Side::Sell, 101),
            (2, 20, Side::Buy, 101),
            (3, 10, Side::Sell, 102),
            (4, 20, Side::Buy, 102),
        ] {
            let command = Command::NewOrder(NewOrder {
                order_id: id,
                account_id,
                side,
                kind: OrderKind::Limit { price_tick: price },
                qty: 1,
                reduce_only: false,
            });
            let execution = rooms.apply(&room_id, command.clone()).unwrap();
            store
                .append_execution(
                    &JournalExecution::submitted("p".to_string(), account_id, command, execution),
                    None,
                )
                .unwrap();
        }
        // Latest training/bot state may precede the checkpoint and must survive.
        let scheduler = exchange_core::SchedulerState::new(
            &room_id,
            Vec::new(),
            exchange_core::SchedulerMode::Manual,
        );
        let cursor = rooms.simulation_room(&room_id).unwrap().next_command_seq();
        for run_name in ["first", "second"] {
            let run_id = format!("{room_id}-{run_name}");
            let spec = exchange_core::TrainingSpec::low_slippage_buy(
                run_id,
                scenario.clone(),
                Vec::new(),
                20,
                1,
                10,
                100,
            )
            .unwrap();
            let mut run = exchange_core::TrainingRun::new(spec);
            store
                .append_room_mutation(
                    &PendingJournalMutation::new(
                        &room_id,
                        cursor,
                        RoomMutation::TrainingProgress {
                            run: Box::new(run.clone()),
                        },
                    ),
                    &[],
                    &[],
                    None,
                )
                .unwrap();
            run.steps_elapsed = 1;
            store
                .append_room_mutation(
                    &PendingJournalMutation::new(
                        &room_id,
                        cursor,
                        RoomMutation::TrainingProgress { run: Box::new(run) },
                    ),
                    &[],
                    &[],
                    None,
                )
                .unwrap();
        }
        store
            .append_room_mutation(
                &PendingJournalMutation::new(
                    &room_id,
                    cursor,
                    RoomMutation::SchedulerProgress {
                        clock_steps: 0,
                        state: scheduler,
                        training: None,
                    },
                ),
                &[],
                &[],
                None,
            )
            .unwrap();
        let snapshot = JournalSnapshot {
            room_id: room_id.clone(),
            command_seq: cursor - 1,
            actor: rooms.simulation_room(&room_id).unwrap().clone(),
        };
        store.append_snapshot(&snapshot).unwrap();
        let tail = Command::NewOrder(NewOrder {
            order_id: 100,
            account_id: 20,
            side: Side::Buy,
            kind: OrderKind::Limit { price_tick: 90 },
            qty: 1,
            reduce_only: false,
        });
        let execution = rooms.apply(&room_id, tail.clone()).unwrap();
        store
            .append_execution(
                &JournalExecution::submitted("p".to_string(), 20, tail, execution),
                None,
            )
            .unwrap();
        let full = store.load_room_replay(&room_id).unwrap();
        let fast = store.load_room_recovery(&room_id).unwrap();
        assert_eq!(full.executions.len(), 5);
        assert_eq!(fast.executions.len(), 1);
        assert_eq!(
            audit_recovery(&full).unwrap()["state"],
            audit_runtime_recovery(&fast).unwrap()["state"]
        );
        assert_eq!(crate::next_order_id_from_recovery(&fast).unwrap(), 101);
        let expected = rooms.candles(&room_id, "V-BTC-SPOT", 1000).unwrap();
        assert_eq!(
            store
                .query_candles("owner", &room_id, "V-BTC-SPOT", 1000, 0, None)
                .unwrap()
                .unwrap(),
            expected
        );
        let mut finalized = expected.clone();
        for candle in &mut finalized {
            candle.is_final = true;
        }
        assert_eq!(
            store
                .query_candles("owner", &room_id, "V-BTC-SPOT", 1000, 1000, None)
                .unwrap()
                .unwrap(),
            finalized
        );
        assert!(
            store
                .query_candles("other-user", &room_id, "V-BTC-SPOT", 1000, 0, None)
                .unwrap()
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .query_candles("owner", &room_id, "V-BTC-SPOT", 1000, 0, Some(0))
                .unwrap()
                .unwrap()
                .is_empty()
        );
        // A snapshot-only final command must not let the API reuse an older ID.
        store
            .append_snapshot(&JournalSnapshot {
                room_id: room_id.clone(),
                command_seq: 4,
                actor: rooms.simulation_room(&room_id).unwrap().clone(),
            })
            .unwrap();
        let fast = store.load_room_recovery(&room_id).unwrap();
        assert!(fast.executions.is_empty());
        assert_eq!(crate::next_order_id_from_recovery(&fast).unwrap(), 101);
        assert_eq!(
            audit_recovery(&full).unwrap()["state"],
            audit_runtime_recovery(&fast).unwrap()["state"]
        );
        // Training metadata can be committed after a checkpoint while referring
        // to the cursor before the automatic command included in that snapshot.
        let late_metadata = full
            .mutations
            .iter()
            .rev()
            .find(|record| matches!(record.mutation, RoomMutation::TrainingProgress { .. }))
            .unwrap();
        store
            .append_room_mutation(
                &PendingJournalMutation::new(
                    &room_id,
                    rooms.simulation_room(&room_id).unwrap().next_command_seq() - 1,
                    late_metadata.mutation.clone(),
                ),
                &[],
                &[],
                None,
            )
            .unwrap();
        assert_eq!(
            audit_recovery(&full).unwrap()["state"],
            audit_runtime_recovery(&store.load_room_recovery(&room_id).unwrap()).unwrap()["state"]
        );
        // An obsolete candidate must roll back both its snapshot and head update.
        assert!(store.append_snapshot(&snapshot).is_err());
        assert_eq!(
            audit_runtime_recovery(&store.load_room_recovery(&room_id).unwrap()).unwrap()["state"],
            audit_recovery(&full).unwrap()["state"]
        );
        let mut client = postgres::Client::connect(&dsn, postgres::NoTls).unwrap();
        client.execute("UPDATE marketforge_recovery_heads SET checkpoint_command_cursor=checkpoint_command_cursor+1 WHERE room_id=$1",&[&room_id]).unwrap();
        assert!(store.load_room_recovery(&room_id).is_err());
        client.execute("UPDATE marketforge_recovery_heads SET checkpoint_command_cursor=checkpoint_command_cursor-1 WHERE room_id=$1",&[&room_id]).unwrap();
        client
            .execute(
                "DELETE FROM marketforge_room_snapshots WHERE room_id=$1",
                &[&room_id],
            )
            .unwrap();
        let fallback = store.load_room_recovery(&room_id).unwrap();
        assert_eq!(fallback.executions.len(), 5);
        assert_eq!(
            audit_runtime_recovery(&fallback).unwrap()["state"],
            audit_recovery(&full).unwrap()["state"]
        );
        client
            .execute(
                "UPDATE marketforge_market_ticks SET market_time_ms=NULL WHERE room_id=$1",
                &[&room_id],
            )
            .unwrap();
        assert_eq!(
            store
                .query_candles("owner", &room_id, "V-BTC-SPOT", 1000, 0, None)
                .unwrap()
                .unwrap(),
            expected
        );
        // Legacy replay must use the same captured query clock as SQL aggregation.
        assert_eq!(
            store
                .query_candles("owner", &room_id, "V-BTC-SPOT", 1000, 1000, None)
                .unwrap()
                .unwrap(),
            finalized
        );
    }
}
