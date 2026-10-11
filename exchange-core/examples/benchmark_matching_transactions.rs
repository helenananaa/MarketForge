//! Release benchmark of complete fills, including command-level rollback and
//! clearing. This source also builds unchanged against the frozen old core.
use exchange_core::{
    Command, Event, NewOrder, OrderBook, OrderKind, PositionSide, Side, SpotAccountStore,
    SpotClearingConfig, SpotTradingEngine,
};
use serde_json::{Value, json};
use std::{hint::black_box, time::Instant};

fn maker(id: u64) -> NewOrder {
    NewOrder {
        order_id: id,
        account_id: 1,
        side: Side::Sell,
        position_side: PositionSide::Both,
        kind: OrderKind::Limit { price_tick: 1000 },
        qty: 1,
        reduce_only: false,
    }
}

fn taker(n: u64, qty: u64) -> NewOrder {
    NewOrder {
        order_id: n + 1,
        account_id: 2,
        side: Side::Buy,
        kind: OrderKind::Market,
        qty,
        ..maker(n + 1)
    }
}

fn maker_id(ordinal: u64, n: u64, shuffled: bool) -> u64 {
    if shuffled {
        (ordinal * 9973) % n + 1
    } else {
        ordinal + 1
    }
}

fn seed(n: u64, shuffled: bool) -> (OrderBook, SpotTradingEngine) {
    seed_with_qty(n, shuffled, 1)
}

fn seed_with_qty(n: u64, shuffled: bool, maker_qty: u64) -> (OrderBook, SpotTradingEngine) {
    let config = SpotClearingConfig::default();
    let mut book = OrderBook::new();
    let mut accounts = SpotAccountStore::new(config);
    accounts.create_account_with_position(1, 0, i128::from(n * maker_qty));
    accounts.create_account(2, 1_000_000_000);
    for ordinal in 0..n {
        let id = maker_id(ordinal, n, shuffled);
        book.place_order(NewOrder {
            qty: maker_qty,
            ..maker(id)
        });
        accounts
            .reserve_resting_order(id, 1, Side::Sell, 1000, maker_qty)
            .unwrap();
    }
    let mut wire = serde_json::to_value(SpotTradingEngine::new(config)).unwrap();
    wire["book"] = serde_json::to_value(&book).unwrap();
    wire["accounts"] = serde_json::to_value(accounts).unwrap();
    (book, serde_json::from_value(wire).unwrap())
}

fn measure<S, R>(
    mut reset: impl FnMut() -> S,
    mut op: impl FnMut(&mut S) -> R,
    validate: impl Fn(&S, &R),
) -> Value {
    let mut times = Vec::new();
    for i in 0..25 {
        let mut state = reset();
        let start = Instant::now();
        let result = black_box(op(black_box(&mut state)));
        let us = start.elapsed().as_secs_f64() * 1e6;
        validate(&state, &result);
        if i >= 5 {
            times.push(us);
        }
    }
    times.sort_by(f64::total_cmp);
    json!({"median_us": times[10], "p95_us": times[18], "samples": 20})
}

fn main() {
    let (_, mut restored) = seed(16, false);
    let mut natural = SpotTradingEngine::new(SpotClearingConfig::default());
    natural.create_account_with_position(1, 0, 16);
    natural.create_account(2, 1_000_000_000);
    for id in 1..=16 {
        natural.apply(Command::NewOrder(maker(id))).unwrap();
    }
    let a = natural.apply(Command::NewOrder(taker(16, 8))).unwrap();
    let b = restored.apply(Command::NewOrder(taker(16, 8))).unwrap();
    assert_eq!(
        a.events.iter().map(|e| &e.event).collect::<Vec<_>>(),
        b.events.iter().map(|e| &e.event).collect::<Vec<_>>()
    );
    for account in [1, 2] {
        assert_eq!(
            natural.account_snapshot(account),
            restored.account_snapshot(account)
        );
    }
    assert_eq!(natural.snapshot(), restored.snapshot());

    let mut rows = Vec::new();
    let mut partial_rows = Vec::new();
    for block in 0..3 {
        let sizes = if block % 2 == 0 {
            [100, 10_000]
        } else {
            [10_000, 100]
        };
        for n in sizes {
            for shuffled in [false, true] {
                let (book, engine) = seed(n, shuffled);
                let book_wire = serde_json::to_string(&book).unwrap();
                for fills in [1, 100, 1000].into_iter().filter(|fills| *fills <= n) {
                    let order = taker(n, fills);
                    let validate_book = |b: &OrderBook, events: &Vec<Event>| {
                        assert_eq!(
                            events
                                .iter()
                                .filter(|e| matches!(e, Event::TradePrinted(_)))
                                .count(),
                            fills as usize
                        );
                        assert_eq!(b.order_owner(maker_id(0, n, shuffled)), None);
                    };
                    let raw_shared = measure(
                        || book.clone(),
                        |b| b.place_order(order.clone()),
                        validate_book,
                    );
                    // Decode each reset independently: no live shared nodes remain
                    // in either implementation. JSON work is outside the timer.
                    let raw_unique = measure(
                        || serde_json::from_str::<OrderBook>(&book_wire).unwrap(),
                        |b| b.place_order(order.clone()),
                        validate_book,
                    );
                    let spot = measure(
                        || engine.clone(),
                        |e| e.apply(Command::NewOrder(order.clone())).unwrap(),
                        |e, result| {
                            assert_eq!(
                                result
                                    .events
                                    .iter()
                                    .filter(|e| matches!(e.event, Event::TradePrinted(_)))
                                    .count(),
                                fills as usize
                            );
                            assert_eq!(
                                e.account_snapshot(1).unwrap().position_qty,
                                i128::from(n - fills)
                            );
                            assert_eq!(
                                e.account_snapshot(2).unwrap().position_qty,
                                i128::from(fills)
                            );
                            assert_eq!(
                                e.account_snapshot(1).unwrap().cash_balance,
                                i128::from(fills * 1000)
                            );
                        },
                    );
                    rows.push(json!({"block":block, "orders":n, "shuffled_ids":shuffled, "maker_fills":fills, "book_match_shared":raw_shared, "book_match_unique":raw_unique, "spot_match":spot}));
                    eprintln!(
                        "completed block={block} orders={n} shuffled={shuffled} fills={fills}"
                    );
                }
                let (book, engine) = seed_with_qty(n, shuffled, 1000);
                let book_wire = serde_json::to_string(&book).unwrap();
                let order = taker(n, 1);
                let validate_book = |b: &OrderBook, events: &Vec<Event>| {
                    assert_eq!(
                        events
                            .iter()
                            .filter(|e| matches!(e, Event::TradePrinted(_)))
                            .count(),
                        1
                    );
                    assert_eq!(b.order_owner(maker_id(0, n, shuffled)), Some(1));
                    assert_eq!(
                        b.resting_orders_for_account(1)
                            .iter()
                            .find(|o| o.order_id == maker_id(0, n, shuffled))
                            .unwrap()
                            .remaining_qty,
                        999
                    );
                };
                let raw_shared = measure(
                    || book.clone(),
                    |b| b.place_order(order.clone()),
                    validate_book,
                );
                let raw_unique = measure(
                    || serde_json::from_str::<OrderBook>(&book_wire).unwrap(),
                    |b| b.place_order(order.clone()),
                    validate_book,
                );
                let spot = measure(
                    || engine.clone(),
                    |e| e.apply(Command::NewOrder(order.clone())).unwrap(),
                    |e, result| {
                        assert_eq!(
                            result
                                .events
                                .iter()
                                .filter(|e| matches!(e.event, Event::TradePrinted(_)))
                                .count(),
                            1
                        );
                        assert_eq!(e.order_owner(maker_id(0, n, shuffled)), Some(1));
                        assert_eq!(
                            e.account_snapshot(1).unwrap().position_qty,
                            i128::from(n * 1000 - 1)
                        );
                        assert_eq!(e.account_snapshot(2).unwrap().position_qty, 1);
                    },
                );
                partial_rows.push(json!({"block":block, "orders":n, "shuffled_ids":shuffled, "maker_qty":1000, "taker_qty":1, "book_match_shared":raw_shared, "book_match_unique":raw_unique, "spot_match":spot}));
            }
        }
    }
    println!("{}", serde_json::to_string_pretty(&json!({"scope":"Release, two spot accounts, one price, unit makers, 1/100/1000 complete fills, sequential and permuted IDs; partial rows fill 1 of a 1000-unit head maker. Empty input event history. Shared raw resets clone the seed; unique raw resets independently deserialize it; both resets are excluded. Spot apply includes its internal candidate and clearing. All event/account assertions and state/result destruction excluded. 5 warmups, 20 samples, 3 blocks. Seed validated against ordinary apply at 16 makers. No server, journal, network or durable commit timing.", "rows":rows, "partial_rows":partial_rows})).unwrap());
}
