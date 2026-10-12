//! Fixed public-API work for account-scope optimizations. Setup, assertions,
//! serialization and final receipt hashing are outside each command timer.
use exchange_core::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{hint::black_box, time::Instant};

fn canonical(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                if key == "seen_order_ids" || key == "reduce_orders" {
                    value.as_array_mut().unwrap().sort_by_key(|v| v.as_u64());
                } else {
                    canonical(value);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(canonical),
        _ => {}
    }
}

fn order(id: u64, account: u64, side: Side) -> Command {
    Command::NewOrder(NewOrder {
        order_id: id,
        account_id: account,
        side,
        position_side: PositionSide::Both,
        kind: OrderKind::Limit { price_tick: 100 },
        qty: 1,
        reduce_only: false,
    })
}

fn percentiles(mut samples: Vec<f64>) -> Value {
    samples.sort_by(f64::total_cmp);
    let at = |q: f64| samples[(samples.len() as f64 * q).ceil() as usize - 1];
    json!({"samples": samples.len(), "median_us": at(0.5), "p95_us": at(0.95)})
}

fn run(count: u64, perp: bool, trade: bool) -> Value {
    let instrument =
        InstrumentConfig::new_for_venue("virtual", "m", "BTC", "USDT", "BTCUSDT", 1, 1).unwrap();
    let market = if perp {
        MarketConfig::Perp(PerpMarketConfig {
            instrument,
            clearing: PerpClearingConfig::default(),
            risk: PerpRiskConfig::default(),
            initial_mark_price_tick: 100,
            price_link: None,
            funding: None,
        })
    } else {
        MarketConfig::Spot(SpotMarketConfig {
            instrument,
            clearing: SpotClearingConfig::default(),
            risk: SpotRiskConfig::default(),
        })
    };
    let mut actor = ExchangeActor::new(
        "account-scope",
        ExchangeConfig::new("virtual", vec![market]).unwrap(),
    )
    .unwrap();
    for id in 1..=count {
        actor.create_account(id, 1_000_000);
    }
    if trade && !perp {
        actor
            .create_spot_account_with_position(1, 1_000_000, 1000)
            .unwrap();
    }
    let mut first = Vec::new();
    let mut second = Vec::new();
    let mut receipts = Vec::new();
    for round in 0..200 {
        let id = round * 2 + 1;
        let maker = if trade {
            order(id, 1, Side::Sell)
        } else {
            order(id, 1, Side::Buy)
        };
        let start = Instant::now();
        let placed = actor.apply_to_instrument("m", maker).unwrap();
        let elapsed_first = start.elapsed().as_secs_f64() * 1e6;
        assert!(matches!(placed.result, ActorExecutionResult::Accepted(_)));
        let next = if trade {
            order(id + 1, 2, Side::Buy)
        } else {
            Command::CancelOrder(CancelOrder { order_id: id })
        };
        let start = Instant::now();
        let result = actor.apply_to_instrument("m", next).unwrap();
        let elapsed_second = start.elapsed().as_secs_f64() * 1e6;
        let events = match &result.result {
            ActorExecutionResult::Accepted(MarketExecution::Spot(s)) => &s.events,
            ActorExecutionResult::Accepted(MarketExecution::Perp(p)) => &p.events,
            other => panic!("unexpected result: {other:?}"),
        };
        assert!(events.iter().any(|e| if trade {
            matches!(e.event, Event::TradePrinted(_))
        } else {
            matches!(e.event, Event::OrderCanceled { .. })
        }));
        black_box(&result);
        if round >= 20 {
            first.push(elapsed_first);
            second.push(elapsed_second);
        }
        receipts.push(placed);
        receipts.push(result);
    }
    assert!(actor.book_snapshot().bids.is_empty());
    assert!(actor.book_snapshot().asks.is_empty());
    let mut receipt = json!({"actor": actor, "executions": format!("{receipts:?}")});
    canonical(&mut receipt);
    json!({"accounts": count, "kind": if perp {"perp"} else {"spot"},
        "workload": if trade {"maker_taker"} else {"place_cancel"},
        "commands": 400, "trades": if trade {200} else {0},
        "first": percentiles(first), "second": percentiles(second),
        "receipt_sha256": format!("{:x}", Sha256::digest(serde_json::to_vec(&receipt).unwrap()))})
}

fn main() {
    let mut runs = Vec::new();
    for count in [1, 100, 1000, 10000, 10000, 1000, 100, 1] {
        for perp in [false, true] {
            runs.push(run(count, perp, false));
            if count >= 2 {
                runs.push(run(count, perp, true));
            }
        }
    }
    println!("{}", serde_json::to_string_pretty(&json!({"runs": runs,
        "scope": "Release core public API; 20 warmup pairs then 180 measured pairs; inactive accounts retained; growing histories identical; final state and all 400 execution receipts hashed outside timers; no HTTP, scheduler or database"})).unwrap());
}
