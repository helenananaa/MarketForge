//! Async HTTP load generator. The server is an independently started process.
//! Each task owns exclusive account(s); the barrier fixes the concurrent start.
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Instant};
use tokio::sync::Barrier;

async fn worker(
    client: reqwest::Client,
    base: String,
    route: Value,
    workload: String,
    cycles: usize,
    barrier: Arc<Barrier>,
    origin: Arc<Instant>,
) -> (Vec<Value>, Option<String>) {
    let mut rows = Vec::new();
    let worker = route["worker"].as_u64().unwrap();
    let a = route["accounts"][0].as_u64().unwrap();
    let b = route["accounts"][1].as_u64().unwrap();
    let room = route["room"].as_str().unwrap();
    let path = format!("{base}/rooms/{room}/orders");
    // Establish the connection before admitting any measured command.
    if let Err(error) = client.get(format!("{base}/health/live")).send().await {
        return (rows, Some(error.to_string()));
    }
    barrier.wait().await;
    let outcome: Result<(), String> = async {
        for _ in 0..cycles {
            let mut resting_id = 0;
            let actions = if workload == "rest_cancel" { 2 } else { 4 };
            for step in 0..actions {
                let (account, action, expected) = if workload == "rest_cancel" {
                    if step == 0 {
                        (a, json!({"PlaceLimit":{"side":"Buy","price_tick":1,"qty":1}}), "OrderRested")
                    } else {
                        (a, json!({"Cancel":{"order_id":resting_id}}), "OrderCanceled")
                    }
                } else {
                    let (seller, buyer) = if step < 2 { (a, b) } else { (b, a) };
                    if step % 2 == 0 {
                        (seller, json!({"PlaceLimit":{"side":"Sell","price_tick":100,"qty":1}}), "OrderRested")
                    } else {
                        (buyer, json!({"PlaceImmediateOrCancel":{"side":"Buy","price_tick":100,"qty":1}}), "OrderFilled")
                    }
                };
                let operation = action.as_object().unwrap().keys().next().unwrap().clone();
                let started = Instant::now();
                let response = client.post(&path).json(&json!({
                    "participant_id": format!("bench-{worker}"), "account_id": account,
                    "instrument_id": route["instrument"], "action": action,
                })).send().await.map_err(|error| error.to_string())?;
                let status = response.status();
                let receipt: Value = response.json().await.map_err(|error| error.to_string())?;
                let finished = Instant::now();
                if !status.is_success() {
                    return Err(format!("HTTP {status}: {receipt}"));
                }
                rows.push(json!({
                    "worker": worker, "room": room, "instrument": route["instrument"],
                    "operation": operation, "phase": "measure",
                    "started_seconds": started.duration_since(*origin).as_secs_f64(),
                    "finished_seconds": finished.duration_since(*origin).as_secs_f64(),
                    "latency_ms": finished.duration_since(started).as_secs_f64() * 1000.0,
                    "command_seq": receipt["command_seq"], "accepted": receipt["accepted"],
                    "events": receipt["events"], "clearing_event_count": receipt["clearing_event_count"],
                }));
                let events = receipt["events"].as_array().ok_or("missing receipt events")?;
                if receipt["accepted"] != true || !events.iter().any(|event| event["type"] == expected) {
                    return Err(format!("unexpected receipt {receipt}"));
                }
                if expected == "OrderRested" {
                    resting_id = events.iter().find(|event| event["type"] == expected)
                        .and_then(|event| event["order_id"].as_u64()).ok_or("missing order id")?;
                }
            }
        }
        Ok(())
    }.await;
    (rows, outcome.err())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 6 {
        return Err(
            "usage: benchmark_room_http BASE_URL ROUTES_JSON WORKLOAD CYCLES OUTPUT_JSON".into(),
        );
    }
    let routes: Vec<Value> = serde_json::from_slice(&std::fs::read(&args[2])?)?;
    let cycles: usize = args[4].parse()?;
    if routes.is_empty()
        || routes.len() > 64
        || cycles == 0
        || cycles > 8192
        || !cycles.is_multiple_of(routes.len())
    {
        return Err("bounded cycles must divide the route count".into());
    }
    if !["rest_cancel", "roundtrip"].contains(&args[3].as_str()) {
        return Err("unknown workload".into());
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let origin = Arc::new(Instant::now());
    let barrier = Arc::new(Barrier::new(routes.len() + 1));
    let mut tasks = Vec::new();
    for route in &routes {
        tasks.push(tokio::spawn(worker(
            client.clone(),
            args[1].clone(),
            route.clone(),
            args[3].clone(),
            cycles / routes.len(),
            barrier.clone(),
            origin.clone(),
        )));
    }
    barrier.wait().await;
    let begun = Instant::now();
    let mut rows = Vec::new();
    let mut errors = Vec::new();
    for task in tasks {
        let (requests, error) = task.await?;
        rows.extend(requests);
        if let Some(error) = error {
            errors.push(error);
        }
    }
    let elapsed = begun.elapsed().as_secs_f64();
    std::fs::write(
        PathBuf::from(&args[5]),
        serde_json::to_vec(&json!({
            "elapsed_seconds":elapsed, "requests":rows, "errors":errors,
            "load_generator":"Rust Tokio/reqwest async tasks, persistent HTTP connections",
        }))?,
    )?;
    if !errors.is_empty() {
        return Err("one or more measured workers failed".into());
    }
    println!("commands={} elapsed_seconds={elapsed:.6}", rows.len());
    Ok(())
}
