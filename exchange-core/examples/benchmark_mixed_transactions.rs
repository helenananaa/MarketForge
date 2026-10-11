//! Deterministic sustained command streams with growing engine histories.
//! Also builds unchanged against the frozen pre-optimization core.
use exchange_core::model::AmendOrder;
use exchange_core::{
    CancelOrder, Command, Event, NewOrder, OrderBook, OrderKind, PerpAccountStore,
    PerpClearingConfig, PerpTradingEngine, PositionSide, Side, SpotAccountStore,
    SpotClearingConfig, SpotTradingEngine,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, hint::black_box, time::Instant};

#[derive(Clone)]
struct Live {
    orders: BTreeMap<u64, NewOrder>,
    ids: Vec<u64>,
    offsets: BTreeMap<u64, usize>,
}

impl Live {
    fn remove(&mut self, id: u64) {
        if let Some(offset) = self.offsets.remove(&id) {
            self.ids.swap_remove(offset);
            if offset < self.ids.len() {
                self.offsets.insert(self.ids[offset], offset);
            }
            self.orders.remove(&id);
        }
    }

    fn update(&mut self, cmd: &Command, events: &[Event]) {
        for event in events {
            match event {
                Event::OrderRested { order_id, .. } => {
                    let Command::NewOrder(order) = cmd else {
                        unreachable!()
                    };
                    self.offsets.insert(*order_id, self.ids.len());
                    self.ids.push(*order_id);
                    self.orders.insert(*order_id, order.clone());
                }
                Event::OrderCanceled { order_id, .. } | Event::OrderFilled { order_id } => {
                    self.remove(*order_id)
                }
                Event::OrderPartiallyFilled {
                    order_id,
                    remaining_qty,
                } => {
                    if let Some(order) = self.orders.get_mut(order_id) {
                        order.qty = *remaining_qty;
                    }
                }
                Event::OrderAmended {
                    order_id,
                    new_qty,
                    new_price_tick,
                    ..
                } => {
                    let order = self.orders.get_mut(order_id).unwrap();
                    order.qty = *new_qty;
                    order.kind = OrderKind::Limit {
                        price_tick: *new_price_tick,
                    };
                }
                Event::OrderRejected { .. }
                | Event::CancelRejected { .. }
                | Event::AmendRejected { .. }
                | Event::RiskRejected { .. } => {
                    panic!("invalid generated command: {cmd:?}: {events:?}")
                }
                _ => {}
            }
        }
    }
}

fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn initial_order(id: u64, accounts: u64, levels: u64) -> NewOrder {
    let side = if id.is_multiple_of(2) {
        Side::Buy
    } else {
        Side::Sell
    };
    let offset = ((id / 2) % levels) as i64;
    NewOrder {
        order_id: id,
        account_id: (id * 9973) % accounts + 1,
        side,
        position_side: PositionSide::Both,
        kind: OrderKind::Limit {
            price_tick: if side == Side::Buy {
                990 - offset
            } else {
                1010 + offset
            },
        },
        qty: 4,
        reduce_only: false,
    }
}

struct Scenario {
    book: String,
    spot: String,
    perp: String,
    trace: Vec<(Command, Vec<Event>)>,
    counts: BTreeMap<&'static str, usize>,
    trades: usize,
    accounts: u64,
    orders: u64,
    levels_per_side: u64,
    final_book: exchange_core::BookSnapshot,
}

fn scenario(orders: u64, accounts: u64, levels: u64, cycles: u64) -> Scenario {
    let spot_config = SpotClearingConfig::default();
    let perp_config = PerpClearingConfig::default();
    let mut book = OrderBook::new();
    let mut spot = SpotAccountStore::new(spot_config);
    let mut perp = PerpAccountStore::new(perp_config, 1000).unwrap();
    for id in 1..=accounts {
        spot.create_account_with_position(id, 1_000_000_000_000, 10_000_000);
        perp.create_account(id, 1_000_000_000_000);
    }
    let mut live = Live {
        orders: BTreeMap::new(),
        ids: Vec::new(),
        offsets: BTreeMap::new(),
    };
    for id in 1..=orders {
        let order = initial_order(id, accounts, levels);
        let OrderKind::Limit { price_tick } = order.kind else {
            unreachable!()
        };
        let events = book.place_order(order.clone());
        live.update(&Command::NewOrder(order.clone()), &events);
        spot.reserve_resting_order(id, order.account_id, order.side, price_tick, 4)
            .unwrap();
        perp.reserve_resting_order(id, order.account_id, order.side, price_tick, 4)
            .unwrap();
    }
    let book_wire = serde_json::to_string(&book).unwrap();
    let mut spot_wire = serde_json::to_value(SpotTradingEngine::new(spot_config)).unwrap();
    spot_wire["book"] = serde_json::to_value(&book).unwrap();
    spot_wire["accounts"] = serde_json::to_value(&spot).unwrap();
    let mut perp_wire =
        serde_json::to_value(PerpTradingEngine::new(perp_config, 1000).unwrap()).unwrap();
    perp_wire["book"] = serde_json::to_value(&book).unwrap();
    perp_wire["accounts"] = serde_json::to_value(&perp).unwrap();
    let mut trace = Vec::new();
    let mut rng = 20261010;
    let mut new_id = orders + 1;
    let push = |cmd: Command,
                book: &mut OrderBook,
                live: &mut Live,
                trace: &mut Vec<(Command, Vec<Event>)>| {
        let events = book.apply(cmd.clone());
        live.update(&cmd, &events);
        trace.push((cmd, events));
    };
    for cycle in 0..cycles {
        let id = live.ids[(next(&mut rng) as usize) % live.ids.len()];
        let mut replacement = live.orders[&id].clone();
        push(
            Command::CancelOrder(CancelOrder { order_id: id }),
            &mut book,
            &mut live,
            &mut trace,
        );
        replacement.order_id = new_id;
        new_id += 1;
        push(
            Command::NewOrder(replacement),
            &mut book,
            &mut live,
            &mut trace,
        );
        let id = live.ids[(next(&mut rng) as usize) % live.ids.len()];
        let current = &live.orders[&id];
        let OrderKind::Limit { price_tick } = current.kind else {
            unreachable!()
        };
        let next_price = if current.side == Side::Buy {
            price_tick - 1
        } else {
            price_tick + 1
        };
        push(
            Command::AmendOrder(AmendOrder {
                order_id: id,
                price_tick: (current.qty == 1).then_some(next_price),
                qty: (current.qty > 1).then_some(current.qty - 1),
            }),
            &mut book,
            &mut live,
            &mut trace,
        );
        if cycle % 4 == 0 {
            let before = live.orders.clone();
            let side = if cycle % 8 == 0 {
                Side::Buy
            } else {
                Side::Sell
            };
            let qty = if cycle % 16 == 0 { 32 } else { 1 };
            let order = NewOrder {
                order_id: new_id,
                account_id: next(&mut rng) % accounts + 1,
                side,
                kind: OrderKind::Market,
                qty,
                ..initial_order(new_id, accounts, levels)
            };
            new_id += 1;
            push(Command::NewOrder(order), &mut book, &mut live, &mut trace);
            let events = &trace.last().unwrap().1;
            let filled: BTreeMap<_, _> = events
                .iter()
                .filter_map(|event| match event {
                    Event::TradePrinted(t) => {
                        Some((t.maker_order_id, before[&t.maker_order_id].clone()))
                    }
                    _ => None,
                })
                .collect();
            for (id, mut old) in filled {
                if live.orders.contains_key(&id) {
                    push(
                        Command::CancelOrder(CancelOrder { order_id: id }),
                        &mut book,
                        &mut live,
                        &mut trace,
                    );
                }
                old.order_id = new_id;
                new_id += 1;
                push(Command::NewOrder(old), &mut book, &mut live, &mut trace);
            }
        }
    }
    let mut counts = BTreeMap::new();
    let mut trades = 0;
    for (cmd, events) in &trace {
        *counts.entry(category(cmd)).or_insert(0) += 1;
        trades += events
            .iter()
            .filter(|e| matches!(e, Event::TradePrinted(_)))
            .count();
    }
    Scenario {
        book: book_wire,
        spot: serde_json::to_string(&spot_wire).unwrap(),
        perp: serde_json::to_string(&perp_wire).unwrap(),
        trace,
        counts,
        trades,
        accounts,
        orders,
        levels_per_side: levels,
        final_book: book.snapshot(),
    }
}

fn category(cmd: &Command) -> &'static str {
    match cmd {
        Command::NewOrder(NewOrder {
            kind: OrderKind::Market,
            ..
        }) => "market",
        Command::NewOrder(_) => "limit",
        Command::CancelOrder(_) => "cancel",
        Command::AmendOrder(_) => "amend",
        _ => unreachable!(),
    }
}

fn stats(mut values: Vec<f64>) -> Value {
    values.sort_by(f64::total_cmp);
    let percentile = |p: usize| values[(values.len() * p).div_ceil(100).saturating_sub(1)];
    json!({"count":values.len(), "p50_us":percentile(50), "p95_us":percentile(95), "p99_us":percentile(99), "max_us":values.last(), "total_apply_us":values.iter().sum::<f64>()})
}

fn measure<S, R>(
    scenario: &Scenario,
    mut reset: impl FnMut() -> S,
    mut apply: impl FnMut(&mut S, Command) -> R,
    events: impl Fn(&R) -> Vec<&Event>,
    finish: impl Fn(&S),
) -> Value {
    let mut blocks = Vec::new();
    for block in 0..5 {
        let mut state = reset();
        let mut times = Vec::with_capacity(scenario.trace.len());
        let mut categories: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
        for (cmd, expected) in &scenario.trace {
            let cmd_owned = cmd.clone();
            let start = Instant::now();
            let result = black_box(apply(black_box(&mut state), cmd_owned));
            let us = start.elapsed().as_secs_f64() * 1e6;
            assert_eq!(events(&result), expected.iter().collect::<Vec<_>>());
            times.push(us);
            categories.entry(category(cmd)).or_default().push(us);
        }
        finish(&state);
        if block > 0 {
            let total = times.iter().sum::<f64>();
            blocks.push(json!({"block":block, "overall":stats(times), "apply_commands_per_second":scenario.trace.len() as f64 * 1e6 / total, "operations":categories.into_iter().map(|(kind, times)| (kind, stats(times))).collect::<BTreeMap<_, _>>()}));
        }
    }
    json!({"blocks":blocks})
}

fn run(s: &Scenario, mode: &str) -> Value {
    match mode {
        "raw" => measure(
            s,
            || serde_json::from_str::<OrderBook>(&s.book).unwrap(),
            |book, cmd| book.apply(cmd),
            |events| events.iter().collect(),
            |book| assert_eq!(book.snapshot(), s.final_book),
        ),
        "spot" => measure(
            s,
            || serde_json::from_str::<SpotTradingEngine>(&s.spot).unwrap(),
            |engine, cmd| engine.apply(cmd).unwrap(),
            |r| r.events.iter().map(|e| &e.event).collect(),
            |engine| {
                assert_eq!(engine.snapshot(), s.final_book);
                assert_eq!(engine.command_log().len(), s.trace.len());
                let accounts = engine.account_snapshots();
                assert_eq!(
                    accounts.iter().map(|a| a.position_qty).sum::<i128>(),
                    i128::from(s.accounts) * 10_000_000
                );
                assert_eq!(
                    accounts.iter().map(|a| a.cash_balance).sum::<i128>(),
                    i128::from(s.accounts) * 1_000_000_000_000
                );
            },
        ),
        "perp" => measure(
            s,
            || serde_json::from_str::<PerpTradingEngine>(&s.perp).unwrap(),
            |engine, cmd| engine.apply(cmd).unwrap(),
            |r| r.events.iter().map(|e| &e.event).collect(),
            |engine| {
                assert_eq!(engine.snapshot(), s.final_book);
                assert_eq!(engine.command_log().len(), s.trace.len());
                assert_eq!(
                    engine
                        .account_snapshots()
                        .iter()
                        .map(|a| a.position_qty)
                        .sum::<i128>(),
                    0
                );
            },
        ),
        _ => panic!("unknown mode {mode}"),
    }
}

#[inline(never)]
fn profile_raw(s: &Scenario, seconds: u64) {
    let start = Instant::now();
    while start.elapsed().as_secs() < seconds {
        let mut book: OrderBook = serde_json::from_str(&s.book).unwrap();
        for (cmd, _) in &s.trace {
            black_box(book.apply(cmd.clone()));
        }
    }
}

#[inline(never)]
fn profile_spot(s: &Scenario, seconds: u64) {
    let start = Instant::now();
    while start.elapsed().as_secs() < seconds {
        let mut engine: SpotTradingEngine = serde_json::from_str(&s.spot).unwrap();
        for (cmd, _) in &s.trace {
            black_box(engine.apply(cmd.clone()).unwrap());
        }
    }
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|x| x == "--export") {
        let s = scenario(100, 8, 10, 400);
        println!("{}", serde_json::to_string(&json!({"accounts":s.accounts,"orders":s.orders,"levels_per_side":s.levels_per_side,"seed_orders":(1..=s.orders).map(|id|Command::NewOrder(initial_order(id,s.accounts,s.levels_per_side))).collect::<Vec<_>>(),"trace":s.trace,"final_book":s.final_book})).unwrap());
        return;
    }
    if args.get(1).is_some_and(|x| x == "--profile") {
        let mode = args.get(2).map(String::as_str).unwrap_or("spot");
        let seconds = args.get(3).and_then(|x| x.parse().ok()).unwrap_or(15);
        let s = scenario(10000, 64, 100, 2000);
        match mode {
            "raw" => profile_raw(&s, seconds),
            "spot" => profile_spot(&s, seconds),
            _ => panic!("unsupported profile mode"),
        }
        return;
    }
    let mut rows = Vec::new();
    for (orders, accounts, levels) in [(100, 8, 10), (10000, 64, 100)] {
        let s = scenario(orders, accounts, levels, 1000);
        for mode in ["raw", "spot", "perp"] {
            let result = run(&s, mode);
            rows.push(json!({"mode":mode,"initial_orders":s.orders,"accounts":s.accounts,"initial_levels_per_side":s.levels_per_side,"final_bid_levels":s.final_book.bids.len(),"final_ask_levels":s.final_book.asks.len(),"commands":s.trace.len(),"command_mix":s.counts,"trades":s.trades,"latency":result}));
            eprintln!("completed mode={mode} orders={orders}");
        }
    }
    println!("{}", serde_json::to_string_pretty(&json!({"scope":"Release, deterministic continuing command streams (seed 20261010), 8/64 funded accounts, 10/100 levels per side, 100/10000 live orders. Cancel+replace, quantity reductions and outward price amendments (levels may expand), partial market fills and 32-unit sweeps with replenishment. Every command's events verified; spot aggregate cash/position and perp net-position conservation checked. Initial histories empty and grow through the stream. One complete warm-up and four recorded blocks per scenario. Independent JSON reset, command clone, event assertions, final checks and result destruction excluded; engine apply includes candidate creation, book/risk/clearing/history append and old-state drop. Service-time throughput excludes validation and timer bookkeeping; p99 is apply service time without arrival queue or HTTP/journal/network. No production SLO qualification.","rows":rows})).unwrap());
}
