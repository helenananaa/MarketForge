//! Fixed public-API work, usable unchanged against the frozen predecessor.
//! Positions are explicit restored fixtures; setup/warmup/serialization are not
//! timed. Functional differential tests separately use real fills and recovery.
use exchange_core::*;
use serde_json::{Value, json};
use std::{hint::black_box, path::PathBuf, time::Instant};

// Retain returned values without adding a per-command allocation to the timer.
#[allow(clippy::large_enum_variant)]
enum Receipt {
    Execution(ActorExecution),
    Clock(Vec<VenueTransfer>),
}

fn canonical(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                if key == "seen_order_ids" {
                    value.as_array_mut().unwrap().sort_by_key(|id| id.as_u64());
                } else {
                    canonical(value);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(canonical),
        _ => {}
    }
}

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    assert_eq!(
        args.len(),
        7,
        "accounts active markets hedge mode steps output-directory"
    );
    let accounts: u64 = args[0].parse().unwrap();
    let active: u64 = args[1].parse().unwrap();
    let markets: usize = args[2].parse().unwrap();
    let hedge: bool = args[3].parse().unwrap();
    let mode = &args[4];
    let steps: u64 = args[5].parse().unwrap();
    let output = PathBuf::from(&args[6]);
    assert!(accounts >= active && active.is_multiple_of(2) && markets > 0);
    std::fs::create_dir_all(&output).unwrap();
    let setup = Instant::now();
    let configs = (0..markets)
        .map(|n| {
            MarketConfig::Perp(PerpMarketConfig {
                instrument: InstrumentConfig::new_for_venue(
                    "virtual",
                    format!("m{n}"),
                    format!("B{n}"),
                    "USDT",
                    format!("M{n}"),
                    1,
                    1,
                )
                .unwrap(),
                clearing: PerpClearingConfig {
                    leverage: 10,
                    position_mode: if hedge {
                        PositionMode::Hedge
                    } else {
                        PositionMode::OneWay
                    },
                    ..PerpClearingConfig::default()
                },
                risk: PerpRiskConfig::default(),
                initial_mark_price_tick: 100,
                price_link: None,
                funding: None,
            })
        })
        .collect();
    let mut actor = ExchangeActor::new(
        "bench-risk",
        ExchangeConfig::new("virtual", configs).unwrap(),
    )
    .unwrap();
    for id in 1..=accounts {
        actor.create_account(
            id,
            if mode == "liquidation" && id <= active && id.is_multiple_of(2) {
                markets as i128 * 100
            } else {
                1_000_000
            },
        );
    }
    let mut wire = serde_json::to_value(&actor).unwrap();
    for n in 0..markets {
        let entries = wire["markets"][format!("m{n}")]["engine"]["Perp"]["accounts"]["accounts"]
            .as_object_mut()
            .unwrap();
        for id in 1..=active {
            let a = entries.get_mut(&id.to_string()).unwrap();
            if hedge {
                a["position_qty"] = json!(0);
                a["avg_entry_price_tick"] = json!(0);
                for leg in ["long", "short"] {
                    a["hedge_positions"][leg]["qty"] = json!(1);
                    a["hedge_positions"][leg]["avg_entry_price_tick"] = json!(100);
                }
            } else {
                a["position_qty"] = json!(if id.is_multiple_of(2) {
                    if mode == "liquidation" { 10 } else { 2 }
                } else {
                    if mode == "liquidation" { -10 } else { -2 }
                });
                a["avg_entry_price_tick"] = json!(100);
            }
        }
    }
    actor = serde_json::from_value(wire).unwrap();
    for _ in 0..2 {
        for n in 0..markets {
            let execution = actor
                .apply_to_instrument(
                    &format!("m{n}"),
                    Command::SetMarkPrice(SetMarkPrice { price_tick: 100 }),
                )
                .unwrap();
            assert!(matches!(
                execution.result,
                ActorExecutionResult::Accepted(_)
            ));
        }
    }
    let mut rooms = RoomManager::new();
    rooms.restore_exchange_room(actor, Vec::new()).unwrap();
    let setup_ms = setup.elapsed().as_secs_f64() * 1000.;
    let phases = exchange_core::performance::snapshot();
    let start = Instant::now();
    let mut receipts = Vec::new();
    for n in 0..steps {
        if mode == "clock" || (mode == "liquidation" && n > 0) {
            receipts.push(Receipt::Clock(
                black_box(rooms.advance_clock("bench-risk", 1)).unwrap(),
            ));
        } else {
            let instrument = format!("m{}", (n as usize / 2) % markets);
            let command = match mode.as_str() {
                "price" => Command::SetMarkPrice(SetMarkPrice {
                    price_tick: 90 + (n % 21) as i64,
                }),
                "same-price" => Command::SetMarkPrice(SetMarkPrice { price_tick: 100 }),
                "liquidation" => Command::SetMarkPrice(SetMarkPrice { price_tick: 80 }),
                "orders" => Command::NewOrder(NewOrder {
                    order_id: n + 1,
                    account_id: n % active + 1,
                    side: if n.is_multiple_of(2) {
                        Side::Sell
                    } else {
                        Side::Buy
                    },
                    position_side: PositionSide::Both,
                    kind: OrderKind::Limit { price_tick: 100 },
                    qty: 1,
                    reduce_only: false,
                }),
                other => panic!("unknown workload {other}"),
            };
            let execution =
                black_box(rooms.apply_to_instrument("bench-risk", &instrument, command));
            assert!(execution.is_ok(), "{execution:?}");
            receipts.push(Receipt::Execution(execution.unwrap()));
        }
    }
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.;
    let after = exchange_core::performance::snapshot();
    let mut state = serde_json::to_value(rooms.simulation_room("bench-risk").unwrap()).unwrap();
    canonical(&mut state);
    std::fs::write(
        output.join("state.json"),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
    let receipts = receipts
        .iter()
        .map(|receipt| match receipt {
            Receipt::Execution(execution) => format!("{execution:?}"),
            Receipt::Clock(transfers) => format!("{transfers:?}"),
        })
        .collect::<Vec<_>>();
    let history = format!(
        "{:?}\n{:?}\n{:?}",
        receipts,
        rooms.execution_history("bench-risk").unwrap(),
        rooms.pending_liquidations("bench-risk").unwrap()
    );
    let trades = history.matches("TradePrinted").count();
    let risk_rejections = history.matches("RiskRejected").count();
    std::fs::write(output.join("executions.txt"), history).unwrap();
    let metrics = json!({"accounts":accounts,"active":active,"markets":markets,"hedge":hedge,"mode":mode,"steps":steps,
        "setup_ms":setup_ms,"elapsed_ms":elapsed_ms,"trades":trades,"risk_rejections":risk_rejections,
        "phase_deltas":(0..4).map(|i| json!({"name":exchange_core::performance::PHASES[i],"calls":after[i].0-phases[i].0,"micros":after[i].1-phases[i].1})).collect::<Vec<_>>(),
        "pending":rooms.pending_liquidation_count("bench-risk").unwrap(),"history_len":rooms.execution_history_len("bench-risk").unwrap()});
    std::fs::write(
        output.join("metrics.json"),
        serde_json::to_vec_pretty(&metrics).unwrap(),
    )
    .unwrap();
    println!("{metrics}");
}
