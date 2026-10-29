//! Isolate a restored maker's quote-reference feedback without any new trades.
use exchange_core::*;
use serde_json::{Value, json};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let recipe: Value = serde_json::from_str(&std::fs::read_to_string(&args[1])?)?;
    let receipt: Value = serde_json::from_str(&std::fs::read_to_string(&args[2])?)?;
    let template: AgentTemplate = serde_json::from_value(
        recipe["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| {
                a["Plugin"]["plugin_id"] == "AdaptiveLiquidityMaker"
                    && a["Plugin"]["participant"]["instrument_id"] == "V-BTC-SPOT"
            })
            .unwrap()
            .clone(),
    )?;
    let AgentTemplate::Plugin(p) = &template else {
        unreachable!()
    };
    let participant = &p.participant;
    let state: PersistedAgentKindState = serde_json::from_value(
        receipt["states"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == participant.participant_id)
            .unwrap()["state"]
            .clone(),
    )?;
    let registry = BotRegistry::with_builtins();
    let mut results = vec![];
    for offset in [0_i64, p.config["inventory_cap"].as_i64().unwrap() / 4] {
        let mut scenario = recipe["scenario"].clone();
        scenario["seed_orders"] = json!([]);
        scenario["routed_seed_orders"] = json!([]);
        let position = p.config["inventory_target"].as_i64().unwrap() + offset;
        for a in scenario["accounts"].as_array_mut().unwrap() {
            let inner = a.as_object().unwrap().values().next().unwrap();
            if inner["account_id"] == participant.account_id {
                *a = json!({"Spot":{"account_id":participant.account_id,
                    "cash_balance":inner["cash_balance"],"position_qty":position}});
            }
        }
        let mut rooms = RoomManager::new();
        rooms
            .create_room(serde_json::from_value(scenario)?)
            .map_err(|e| format!("{e:?}"))?;
        let mut advance = receipt["simulation_time_ms"].as_u64().unwrap() / 1000;
        while advance > 0 {
            let step = advance.min(10000);
            rooms
                .advance_clock(&participant.room_id, step)
                .map_err(|e| format!("{e:?}"))?;
            advance -= step;
        }
        let mut bot = registry.create(&template, &state)?;
        let mut next_id = 100000;
        let mut traces = vec![];
        let mut trades = 0;
        for _ in 0..120 {
            rooms
                .advance_clock(&participant.room_id, 1)
                .map_err(|e| format!("{e:?}"))?;
            let obs = rooms
                .participant_observation(&participant.room_id, "V-BTC-SPOT", participant.account_id)
                .map_err(|e| format!("{e:?}"))?;
            for action in bot.decide(&obs)? {
                let mut gateway = OrderGateway::new_scheduler(&mut rooms, next_id);
                let execution = gateway
                    .submit_action(GatewayRequest {
                        participant_id: participant.participant_id.clone(),
                        room_id: participant.room_id.clone(),
                        instrument_id: Some("V-BTC-SPOT".into()),
                        account_id: participant.account_id,
                        action: action.clone(),
                    })
                    .map_err(|e| format!("{e:?}"))?;
                next_id = gateway.next_order_id();
                bot.execution_feedback(&action, &execution);
                if let ActorExecutionResult::Accepted(MarketExecution::Spot(r)) =
                    &execution.execution.result
                {
                    trades += r
                        .events
                        .iter()
                        .filter(|e| matches!(&e.event, Event::TradePrinted(_)))
                        .count();
                }
            }
            let book = rooms
                .book_snapshot_for(&participant.room_id, "V-BTC-SPOT")
                .map_err(|e| format!("{e:?}"))?;
            if let (Some(b), Some(a)) = (book.bids.first(), book.asks.first()) {
                traces.push(
                    json!({"time_ms":obs.market_time_ms,"bid":b.price_tick,"ask":a.price_tick,
                    "mid":(b.price_tick as f64+a.price_tick as f64)/2.}),
                );
            }
        }
        assert_eq!(trades, 0, "quote-only experiment must not contain trades");
        let first = traces.first().ok_or("no two-sided book")?["mid"]
            .as_f64()
            .unwrap();
        let last = traces.last().unwrap()["mid"].as_f64().unwrap();
        results.push(json!({"inventory_offset":offset,"inventory":position,"trades":trades,
            "first_mid":first,"last_mid":last,"displacement_bps":(last/first-1.)*10000.,"trace":traces}));
    }
    println!(
        "{}",
        json!({"participant":participant.participant_id,"plugin_config":p.config,
        "restored_state_from":args[2],"duration_seconds":120,"branches":results})
    );
    Ok(())
}
