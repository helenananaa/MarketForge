use std::env;
use std::process;

use exchange_core::{OrderAction, Side};
use exchange_server::{HttpTradingClient, StartAgentsRequest, SubmitOrderRequest};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let mut base_url =
        env::var("MARKETFORGE_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:57305".to_string());
    let mut bearer = env::var("MARKETFORGE_BEARER_TOKEN").ok();
    let mut user_id = env::var("MARKETFORGE_USER_ID").ok();
    let mut idempotency = None::<String>;
    let mut trusted = Vec::new();
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--base-url" => {
                base_url = args.get(i + 1).ok_or("--base-url needs a value")?.clone();
                i += 2;
            }
            "--bearer" => {
                bearer = Some(args.get(i + 1).ok_or("--bearer needs a value")?.clone());
                i += 2;
            }
            "--user-id" => {
                user_id = Some(args.get(i + 1).ok_or("--user-id needs a value")?.clone());
                i += 2;
            }
            "--idempotency-key" => {
                idempotency = Some(
                    args.get(i + 1)
                        .ok_or("--idempotency-key needs a value")?
                        .clone(),
                );
                i += 2;
            }
            "--trust-owner" => {
                trusted.push(
                    args.get(i + 1)
                        .ok_or("--trust-owner needs a value")?
                        .clone(),
                );
                i += 2;
            }
            other => {
                rest.push(other.to_string());
                i += 1;
            }
        }
    }
    let mut client = if let Some(token) = bearer {
        HttpTradingClient::with_bearer_token(&base_url, token)
    } else if let Some(user_id) = user_id {
        HttpTradingClient::with_user_id(&base_url, user_id)
    } else {
        HttpTradingClient::new(&base_url)
    };
    for owner in trusted {
        client
            .trust_owner_url(&owner)
            .map_err(|error| error.to_string())?;
    }
    match rest
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["room", "list"] => print_json(&client.list_rooms().map_err(|e| e.to_string())?),
        ["room", "create", path] => {
            let body = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
            let request: exchange_server::CreateRoomRequest =
                serde_json::from_str(&body).map_err(|e| e.to_string())?;
            print_json(
                &client
                    .create_room_with_agents(&request)
                    .map_err(|e| e.to_string())?,
            )
        }
        ["room", "pause", room_id] => {
            print_json(&client.pause_room(room_id).map_err(|e| e.to_string())?)
        }
        ["room", "resume", room_id] => {
            print_json(&client.resume_room(room_id).map_err(|e| e.to_string())?)
        }
        ["room", "close", room_id] => {
            print_json(&client.close_room(room_id).map_err(|e| e.to_string())?)
        }
        ["clock", "get", room_id] => {
            print_json(&client.room_clock(room_id).map_err(|e| e.to_string())?)
        }
        ["clock", "advance", room_id, steps] => {
            let steps = steps.parse::<u64>().map_err(|e| e.to_string())?;
            print_json(
                &client
                    .advance_room_clock(room_id, steps)
                    .map_err(|e| e.to_string())?,
            )
        }
        ["ticker", room_id] => print_json(&client.room_ticker(room_id).map_err(|e| e.to_string())?),
        ["candles", room_id, interval] => {
            let interval = interval.parse::<u64>().map_err(|e| e.to_string())?;
            print_json(
                &client
                    .room_candles(room_id, interval)
                    .map_err(|e| e.to_string())?,
            )
        }
        ["account", "list", room_id] => {
            print_json(&client.room_accounts(room_id).map_err(|e| e.to_string())?)
        }
        ["bot", "list"] => print_json(&client.list_bots().map_err(|e| e.to_string())?),
        ["agent", "status", room_id] => {
            print_json(&client.agent_status(room_id).map_err(|e| e.to_string())?)
        }
        ["agent", "start", room_id, path] => {
            let body = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
            let request: StartAgentsRequest =
                serde_json::from_str(&body).map_err(|e| e.to_string())?;
            print_json(
                &client
                    .start_agents(room_id, &request)
                    .map_err(|e| e.to_string())?,
            )
        }
        ["agent", "stop", room_id] => {
            print_json(&client.stop_agents(room_id).map_err(|e| e.to_string())?)
        }
        ["order", "submit", room_id, account_id, side, price, qty] => {
            let account_id = account_id.parse::<u64>().map_err(|e| e.to_string())?;
            let price_tick = price.parse::<i64>().map_err(|e| e.to_string())?;
            let qty = qty.parse::<u64>().map_err(|e| e.to_string())?;
            let side = match *side {
                "buy" => Side::Buy,
                "sell" => Side::Sell,
                other => return Err(format!("side must be buy or sell, got {other}")),
            };
            let request = SubmitOrderRequest {
                participant_id: "cli".to_string(),
                instrument_id: None,
                account_id,
                action: OrderAction::PlaceLimit {
                    side,
                    price_tick,
                    qty,
                },
            };
            let response = if let Some(key) = idempotency.as_deref() {
                client
                    .submit_order_idempotent(room_id, key, &request)
                    .map_err(|e| e.to_string())?
            } else {
                client
                    .submit_order(room_id, &request)
                    .map_err(|e| e.to_string())?
            };
            print_json(&response)
        }
        ["training", "start", path] => {
            let body = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
            let request: exchange_server::StartTrainingRequest =
                serde_json::from_str(&body).map_err(|e| e.to_string())?;
            print_json(&client.start_training(&request).map_err(|e| e.to_string())?)
        }
        ["training", "status", run_id] => {
            print_json(&client.training_status(run_id).map_err(|e| e.to_string())?)
        }
        ["training", "abort", run_id] => {
            print_json(&client.abort_training(run_id).map_err(|e| e.to_string())?)
        }
        ["training", "result", run_id] => {
            print_json(&client.training_result(run_id).map_err(|e| e.to_string())?)
        }
        ["training", "report", run_id] => {
            print_json(&client.training_report(run_id).map_err(|e| e.to_string())?)
        }
        ["replay", room_id] => print_json(
            &client
                .replay_room(room_id, None)
                .map_err(|e| e.to_string())?,
        ),
        ["replay", room_id, seq] => {
            let seq = seq.parse::<u64>().map_err(|e| e.to_string())?;
            print_json(
                &client
                    .replay_room(room_id, Some(seq))
                    .map_err(|e| e.to_string())?,
            )
        }
        ["order", "cancel", room_id, account_id, order_id] => {
            let account_id = account_id.parse::<u64>().map_err(|e| e.to_string())?;
            let order_id = order_id.parse::<u64>().map_err(|e| e.to_string())?;
            let request = SubmitOrderRequest {
                participant_id: "cli".to_string(),
                instrument_id: None,
                account_id,
                action: OrderAction::Cancel { order_id },
            };
            print_json(
                &client
                    .submit_order(room_id, &request)
                    .map_err(|e| e.to_string())?,
            )
        }
        ["member", "add", room_id, user_id, role] => print_json(
            &client
                .upsert_room_member(room_id, user_id, role)
                .map_err(|e| e.to_string())?,
        ),
        ["member", "remove", room_id, user_id] => print_json(
            &client
                .remove_room_member(room_id, user_id)
                .map_err(|e| e.to_string())?,
        ),
        ["account", "assign", room_id, account_id, user_id] => {
            let account_id = account_id.parse::<u64>().map_err(|e| e.to_string())?;
            print_json(
                &client
                    .assign_account_owner(room_id, account_id, user_id)
                    .map_err(|e| e.to_string())?,
            )
        }
        ["observe", room_id, account_id] => {
            let account_id = account_id.parse::<u64>().map_err(|e| e.to_string())?;
            print_json(
                &client
                    .observe_room(room_id, account_id, None)
                    .map_err(|e| e.to_string())?,
            )
        }
        _ => {
            eprintln!(
                "usage: marketforge [--base-url URL] [--bearer TOKEN] [--user-id ID] [--idempotency-key KEY] [--trust-owner URL]
  room list|create FILE|pause ID|resume ID|close ID
  clock get ID|advance ID STEPS
  ticker ID
  candles ID INTERVAL_MS
  account list ID|assign ROOM ACCOUNT USER
  member add ROOM USER ROLE|remove ROOM USER
  observe ROOM ACCOUNT
  bot list
  agent status ID|start ID FILE|stop ID
  order submit ID ACCOUNT buy|sell PRICE QTY
  order cancel ID ACCOUNT ORDER_ID
  training start FILE|status ID|abort ID|result ID|report ID
  replay ROOM [SEQ]"
            );
            Err("invalid command".to_string())
        }
    }
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|e| e.to_string())?
    );
    Ok(())
}
