//! Fixed public-API withdrawal/liquidation work. Fixture clone setup,
//! assertions and final state/receipt hashing are excluded from command timers.
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
fn order(id: u64, account: u64, side: Side, kind: OrderKind) -> Command {
    Command::NewOrder(NewOrder {
        order_id: id,
        account_id: account,
        side,
        position_side: PositionSide::Both,
        kind,
        qty: 10,
        reduce_only: false,
    })
}
fn fixture(count: u64, markets: usize, liquidation: bool) -> ExchangeActor {
    let config = ExchangeConfig::new(
        "virtual",
        (0..markets)
            .map(|i| {
                MarketConfig::Perp(PerpMarketConfig {
                    instrument: InstrumentConfig::new_for_venue(
                        "virtual",
                        format!("m{i}"),
                        format!("B{i}"),
                        "USDT",
                        format!("B{i}USDT"),
                        1,
                        1,
                    )
                    .unwrap(),
                    clearing: PerpClearingConfig {
                        leverage: 10,
                        maintenance_margin_ppm: 50_000,
                        ..PerpClearingConfig::default()
                    },
                    risk: PerpRiskConfig::default(),
                    initial_mark_price_tick: if liquidation { 80 } else { 100 },
                    price_link: None,
                    funding: None,
                })
            })
            .collect(),
    )
    .unwrap();
    let mut actor = ExchangeActor::new("probe", config).unwrap();
    for id in 1..=count {
        actor.create_account(id, if liquidation && id == 2 { 200 } else { 10_000 });
    }
    if liquidation {
        for command in [
            order(1, 1, Side::Sell, OrderKind::Limit { price_tick: 100 }),
            order(2, 2, Side::Buy, OrderKind::Market),
            order(3, 3, Side::Buy, OrderKind::Limit { price_tick: 80 }),
        ] {
            let execution = actor.apply_to_instrument("m0", command).unwrap();
            assert!(
                matches!(execution.result, ActorExecutionResult::Accepted(_)),
                "{execution:?}"
            );
        }
    } else {
        // Warm the reconciliation roots without changing the flat fixture.
        actor
            .apply_to_instrument(
                "m0",
                order(1, 1, Side::Buy, OrderKind::Limit { price_tick: 100 }),
            )
            .unwrap();
        actor
            .apply_to_instrument("m0", Command::CancelOrder(CancelOrder { order_id: 1 }))
            .unwrap();
    }
    actor
}
fn run(count: u64, markets: usize, workload: &str) -> Value {
    let liquidation = workload == "liquidation";
    let base = fixture(count, markets, liquidation);
    let mut times = Vec::new();
    let mut receipt = None;
    let mut final_actor = None;
    for round in 0..33 {
        let mut actor = base.clone(); // fixed work, no growing transfer/history effects
        let start = Instant::now();
        let result = if liquidation {
            let execution = actor.liquidate_account("m0", 2, 4).unwrap();
            let elapsed = start.elapsed().as_secs_f64() * 1e6;
            assert!(
                matches!(execution.result, ActorExecutionResult::Accepted(_)),
                "{execution:?}"
            );
            assert!(matches!(
                actor.account_snapshot_for("m0", 2).unwrap(),
                Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                    position_qty: 0,
                    ..
                }))
            ));
            (elapsed, format!("{execution:?}"))
        } else {
            let transfer = actor
                .submit_venue_withdrawal(
                    count,
                    "USDT",
                    if workload == "withdrawal_rejected" {
                        20_000
                    } else {
                        1
                    },
                )
                .unwrap();
            let elapsed = start.elapsed().as_secs_f64() * 1e6;
            assert_eq!(
                transfer.status == VenueTransferStatus::Rejected,
                workload == "withdrawal_rejected"
            );
            (elapsed, format!("{transfer:?}"))
        };
        black_box(&actor);
        if let Some(prior) = &receipt {
            assert_eq!(prior, &result.1);
        } else {
            receipt = Some(result.1);
        }
        if round >= 3 {
            times.push(result.0);
        }
        final_actor = Some(actor);
    }
    let mut state = json!({"actor": final_actor, "receipt": receipt});
    canonical(&mut state);
    times.sort_by(f64::total_cmp);
    json!({"accounts": count, "markets": markets, "workload": workload,
        "median_us": times[14], "p95_us": times[28], "samples": times.len(),
        "state_receipt_sha256": format!("{:x}", Sha256::digest(serde_json::to_vec(&state).unwrap()))})
}
fn main() {
    let mut runs = Vec::new();
    for count in [3, 100, 1000, 10000, 10000, 1000, 100, 3] {
        for markets in [1, 4] {
            for workload in ["withdrawal_rejected", "withdrawal_accepted", "liquidation"] {
                runs.push(run(count, markets, workload));
            }
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"runs": runs})).unwrap()
    );
}
