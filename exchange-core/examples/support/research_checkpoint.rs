//! Research-only checkpoint. Matching state, accepted execution history and bot
//! states are restored; actor-level rejected attempts stay in the aggregate audit.
use super::*;
use serde_json::{Value, json};
type Error = Box<dyn std::error::Error>;

fn export_execution(e: &ActorExecution) -> Option<Value> {
    let (kind, command, events, clearing) = match &e.result {
        ActorExecutionResult::Accepted(MarketExecution::Spot(r)) => (
            "spot",
            json!(r.command),
            json!(r.events),
            json!(r.clearing_events),
        ),
        ActorExecutionResult::Accepted(MarketExecution::Perp(r)) => (
            "perp",
            json!(r.command),
            json!(r.events),
            json!(r.clearing_events),
        ),
        ActorExecutionResult::Rejected(_) => return None,
    };
    Some(
        json!({"kind":kind,"command":command,"events":events,"clearing":clearing,
        "room_id":e.room_id,"instrument_id":e.instrument_id,"command_seq":e.command_seq,
        "market_time_ms":e.market_time_ms,"status":e.status,"price_updates":e.price_updates,
        "funding_settlement":e.funding_settlement,"book_delta":e.book_delta,
        "is_liquidation":e.is_liquidation,"wallet_updates":e.wallet_updates,"margin_updates":e.margin_updates}),
    )
}
fn import_execution(v: &Value) -> Result<ActorExecution, Error> {
    let result = match v["kind"]
        .as_str()
        .ok_or("checkpoint execution kind missing")?
    {
        "spot" => MarketExecution::Spot(SpotTradingExecution {
            command: serde_json::from_value(v["command"].clone())?,
            events: serde_json::from_value(v["events"].clone())?,
            clearing_events: serde_json::from_value(v["clearing"].clone())?,
        }),
        "perp" => MarketExecution::Perp(PerpTradingExecution {
            command: serde_json::from_value(v["command"].clone())?,
            events: serde_json::from_value(v["events"].clone())?,
            clearing_events: serde_json::from_value(v["clearing"].clone())?,
        }),
        _ => return Err("unknown checkpoint execution kind".into()),
    };
    Ok(ActorExecution {
        rejected_command: None,
        room_id: serde_json::from_value(v["room_id"].clone())?,
        instrument_id: serde_json::from_value(v["instrument_id"].clone())?,
        command_seq: serde_json::from_value(v["command_seq"].clone())?,
        market_time_ms: serde_json::from_value(v["market_time_ms"].clone())?,
        status: serde_json::from_value(v["status"].clone())?,
        result: ActorExecutionResult::Accepted(result),
        price_updates: serde_json::from_value(v["price_updates"].clone())?,
        funding_settlement: serde_json::from_value(v["funding_settlement"].clone())?,
        book_delta: serde_json::from_value(v["book_delta"].clone())?,
        is_liquidation: serde_json::from_value(v["is_liquidation"].clone())?,
        wallet_updates: serde_json::from_value(v["wallet_updates"].clone())?,
        margin_updates: serde_json::from_value(v["margin_updates"].clone())?,
    })
}
fn merge(total: &mut Value, part: Value) {
    if total.is_null() {
        *total = part;
        return;
    }
    for key in ["trades", "fills", "rejections", "actions_per_bot"] {
        for (id, count) in part[key].as_object().unwrap() {
            let old = total[key][id].as_u64().unwrap_or(0);
            total[key][id] = json!(old + count.as_u64().unwrap());
        }
    }
    for key in ["trade_receipts", "book_snapshots"] {
        total[key]
            .as_array_mut()
            .unwrap()
            .extend(part[key].as_array().unwrap().iter().cloned());
    }
    total["steps"] = json!(total["steps"].as_u64().unwrap() + part["steps"].as_u64().unwrap());
    for key in ["simulation_time_ms", "assets_after", "states"] {
        total[key] = part[key].clone();
    }
}
pub fn stopped() -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::env::var("MF_RESEARCH_DEADLINE_UNIX")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .is_some_and(|end| now >= end)
        || std::env::var("MF_RESEARCH_STOP_FILE")
            .ok()
            .is_some_and(|p| std::path::Path::new(&p).exists())
}
fn write_atomic(path: &std::path::Path, value: &Value) -> Result<(), Error> {
    use std::io::Write;
    let temporary = path.with_extension("checkpoint.tmp");
    let mut file = std::fs::File::create(&temporary)?;
    let mut writer = std::io::BufWriter::new(&mut file);
    serde_json::to_writer(&mut writer, value)?;
    writer.flush()?;
    drop(writer);
    file.sync_all()?;
    // MoveFileEx(REPLACE_EXISTING) is provided by rename on Windows.
    std::fs::rename(temporary, path)?;
    Ok(())
}
#[allow(clippy::too_many_arguments)]
pub fn run(
    registry: &BotRegistry,
    templates: &[AgentTemplate],
    room: &str,
    rooms: &mut RoomManager,
    bots: &mut [Box<dyn ScheduledBot>],
    order_id: &mut u64,
    steps: u64,
    seconds: u64,
    recipe: &str,
    path: &std::path::Path,
) -> Result<Value, Error> {
    let mut total = Value::Null;
    let build_id =
        std::env::var("MF_RESEARCH_BUILD_ID").unwrap_or_else(|_| "manual-validation".into());
    if path.exists() {
        let saved: Value = serde_json::from_reader(std::fs::File::open(path)?)?;
        if saved["schema"] != 1
            || saved["recipe"] != recipe
            || saved["seconds"] != seconds
            || saved["build_id"] != build_id
        {
            return Err("checkpoint identity mismatch".into());
        }
        let restored = serde_json::from_value(saved["room"].clone())?;
        let history = saved["history"]
            .as_array()
            .ok_or("checkpoint history missing")?
            .iter()
            .map(import_execution)
            .collect::<Result<Vec<_>, _>>()?;
        let mut recovered = RoomManager::new();
        recovered
            .restore_simulation_room(restored, history)
            .map_err(|e| format!("{e:?}"))?;
        *rooms = recovered;
        *order_id = saved["order_id"]
            .as_u64()
            .ok_or("checkpoint order cursor missing")?;
        total = saved["result"].clone();
        if total["states"]
            .as_array()
            .ok_or("checkpoint states missing")?
            .len()
            != templates.len()
        {
            return Err("checkpoint bot count mismatch".into());
        }
        for ((bot, template), state) in bots.iter_mut().zip(templates).zip(
            total["states"]
                .as_array()
                .ok_or("checkpoint states missing")?,
        ) {
            if state["id"] != template.participant_id() {
                return Err("checkpoint bot identity mismatch".into());
            }
            *bot = registry.create(template, &serde_json::from_value(state["state"].clone())?)?;
        }
        eprintln!("resumed_step={}", total["steps"]);
    }
    let mut done = total["steps"].as_u64().unwrap_or(0);
    while done < steps && !stopped() {
        let part = super::run_waves(
            registry,
            templates,
            room,
            rooms,
            bots,
            order_id,
            (steps - done).min(50),
            seconds,
        )?;
        merge(&mut total, part);
        done = total["steps"].as_u64().unwrap();
        let history = rooms
            .execution_history_from(room, 0)
            .map_err(|e| format!("{e:?}"))?
            .filter_map(export_execution)
            .collect::<Vec<_>>();
        write_atomic(
            path,
            &json!({"schema":1,"build_id":build_id,"recipe":recipe,"seconds":seconds,
            "order_id":order_id,"room":rooms.simulation_room(room).map_err(|e|format!("{e:?}"))?,
            "history":history,"result":total}),
        )?;
        eprintln!("checkpoint_step={done}/{steps} path={}", path.display());
    }
    if total.is_null() {
        return Err("deadline arrived before first wave".into());
    }
    total["requested_steps"] = json!(steps);
    total["run_complete"] = json!(done == steps);
    total["checkpoint"] = json!(path.to_string_lossy());
    Ok(total)
}
