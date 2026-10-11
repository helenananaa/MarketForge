use exchange_core::{
    CancelOrder, Command, Event, NewOrder, OrderBook, OrderKind, PositionSide, Side,
    SpotAccountStore, SpotClearingConfig, SpotTradingEngine,
};
use serde_json::{Value, json};
use std::{hint::black_box, time::Instant};

fn order(id: u64, price: i64) -> NewOrder {
    NewOrder {
        order_id: id,
        account_id: 1,
        side: Side::Buy,
        position_side: PositionSide::Both,
        kind: OrderKind::Limit { price_tick: price },
        qty: 1,
        reduce_only: false,
    }
}

fn canonical_book(mut wire: Value) -> Value {
    for key in ["seen_order_ids", "reduce_orders"] {
        wire[key]
            .as_array_mut()
            .unwrap()
            .sort_by_key(|v| v.as_u64().unwrap());
    }
    wire
}

fn seed(n: u64, levels: u64) -> (OrderBook, SpotAccountStore, SpotTradingEngine) {
    let config = SpotClearingConfig::default();
    let mut book = OrderBook::new();
    let mut accounts = SpotAccountStore::new(config);
    accounts.create_account(1, 1_000_000_000_000);
    for id in 1..=n {
        let price = 1000 + (id % levels) as i64;
        assert!(
            book.place_order(order(id, price))
                .iter()
                .any(|e| matches!(e, Event::OrderRested { .. }))
        );
        accounts
            .reserve_resting_order(id, 1, Side::Buy, price, 1)
            .unwrap();
    }
    let mut wire = serde_json::to_value(SpotTradingEngine::new(config)).unwrap();
    wire["book"] = serde_json::to_value(&book).unwrap();
    wire["accounts"] = serde_json::to_value(&accounts).unwrap();
    let engine = serde_json::from_value(wire).unwrap();
    (book, accounts, engine)
}

// Reset, serialization, assertions and destruction of the remaining state are
// outside the operation timer. apply includes its internal clone/old-state drop.
fn measure<S: Clone, R>(
    seed: &S,
    mut op: impl FnMut(&mut S) -> R,
    validate: impl Fn(&S, &R),
    reps: usize,
) -> Value {
    measure_prepared(seed, |_| {}, &mut op, validate, reps)
}

fn measure_prepared<S: Clone, R>(
    seed: &S,
    mut prepare: impl FnMut(&mut S),
    mut op: impl FnMut(&mut S) -> R,
    validate: impl Fn(&S, &R),
    reps: usize,
) -> Value {
    let mut times = Vec::with_capacity(reps);
    for i in 0..reps + 10 {
        let mut state = seed.clone();
        prepare(&mut state);
        let started = Instant::now();
        let result = black_box(op(black_box(&mut state)));
        let us = started.elapsed().as_secs_f64() * 1e6;
        validate(&state, &result);
        if i >= 10 {
            times.push(us);
        }
    }
    times.sort_by(f64::total_cmp);
    json!({"median_us": times[reps / 2], "p95_us": times[(reps - 1) * 95 / 100], "samples": reps})
}

fn main() {
    // Compare bulk-restored seed with ordinary apply at small size.
    let (book, accounts, restored) = seed(16, 4);
    let mut natural = SpotTradingEngine::new(SpotClearingConfig::default());
    natural.create_account(1, 1_000_000_000_000);
    for id in 1..=16 {
        natural
            .apply(Command::NewOrder(order(id, 1000 + (id % 4) as i64)))
            .unwrap();
    }
    let natural_wire = serde_json::to_value(&natural).unwrap();
    assert_eq!(
        canonical_book(natural_wire["book"].clone()),
        canonical_book(serde_json::to_value(&book).unwrap())
    );
    assert_eq!(
        natural_wire["accounts"],
        serde_json::to_value(&accounts).unwrap()
    );
    for cmd in [
        Command::CancelOrder(CancelOrder { order_id: 16 }),
        Command::NewOrder(order(17, 1000)),
    ] {
        let mut a = natural.clone();
        let mut b = restored.clone();
        let ra = a.apply(cmd.clone()).unwrap();
        let rb = b.apply(cmd).unwrap();
        assert_eq!(
            ra.events.iter().map(|e| &e.event).collect::<Vec<_>>(),
            rb.events.iter().map(|e| &e.event).collect::<Vec<_>>()
        );
        let a = serde_json::to_value(a).unwrap();
        let b = serde_json::to_value(b).unwrap();
        assert_eq!(
            canonical_book(a["book"].clone()),
            canonical_book(b["book"].clone())
        );
        assert_eq!(a["accounts"], b["accounts"]);
    }
    let mut rows = Vec::new();
    for block in 0..3 {
        let sizes = if block % 2 == 0 {
            vec![100, 1000, 10_000, 100_000]
        } else {
            vec![100_000, 10_000, 1000, 100]
        };
        for n in sizes {
            for levels in [1, 100] {
                let (book, accounts, engine) = seed(n, levels);
                let base_balance = engine.account_snapshot(1).unwrap();
                let price = 1000 + (n % levels) as i64;
                let cancel = CancelOrder { order_id: n };
                let add = order(n + 1, price);
                let no_validate = |_: &OrderBook, _: &OrderBook| {};
                let clone = measure(&book, |b| b.clone(), no_validate, 80);
                // Clone gives queues tight capacity. Create spare capacity outside
                // the timer to measure ordinary amortized raw insertion separately.
                let raw_add = measure_prepared(
                    &book,
                    |b| {
                        b.place_order(order(n + 2, price));
                        b.cancel_order(CancelOrder { order_id: n + 2 });
                    },
                    |b| b.place_order(add.clone()),
                    |b, e| {
                        assert_eq!(b.order_owner(n + 1), Some(1));
                        assert!(e.iter().any(|e| matches!(e, Event::OrderRested { .. })));
                    },
                    80,
                );
                let raw_cancel = measure(
                    &book,
                    |b| b.cancel_order(cancel.clone()),
                    |b, e| {
                        assert_eq!(b.order_owner(n), None);
                        assert!(matches!(e.as_slice(), [Event::OrderCanceled { .. }]));
                    },
                    80,
                );
                let release = measure(
                    &accounts,
                    |a| a.release_order_reservation(n),
                    |a, r| {
                        assert!(r.is_ok());
                        assert_eq!(
                            a.account_snapshot(1).unwrap().reserved_cash,
                            base_balance.reserved_cash - i128::from(price)
                        );
                    },
                    80,
                );
                let engine_clone = measure(&engine, |e| e.clone(), |_, _| {}, 80);
                let engine_add = measure(
                    &engine,
                    |e| e.apply(Command::NewOrder(add.clone())).unwrap(),
                    |e, r| {
                        assert_eq!(e.order_owner(n + 1), Some(1));
                        assert!(
                            r.events
                                .iter()
                                .any(|e| matches!(e.event, Event::OrderRested { .. }))
                        );
                        assert_eq!(
                            e.account_snapshot(1).unwrap().reserved_cash,
                            base_balance.reserved_cash + i128::from(price)
                        );
                    },
                    80,
                );
                let engine_cancel = measure(
                    &engine,
                    |e| e.apply(Command::CancelOrder(cancel.clone())).unwrap(),
                    |e, r| {
                        assert_eq!(e.order_owner(n), None);
                        assert!(matches!(r.events[0].event, Event::OrderCanceled { .. }));
                        assert_eq!(
                            e.account_snapshot(1).unwrap().reserved_cash,
                            base_balance.reserved_cash - i128::from(price)
                        );
                    },
                    80,
                );
                assert_eq!(engine.account_snapshot(1).unwrap(), base_balance);
                let row = json!({"block":block, "orders":n, "levels":levels, "book_clone":clone,
                    "book_add":raw_add,"book_cancel_tail":raw_cancel, "reservation_release_cow":release,
                    "engine_clone":engine_clone, "spot_add":engine_add,"spot_cancel_tail":engine_cancel});
                eprintln!("completed block={block} orders={n} levels={levels}");
                rows.push(row);
            }
        }
    }
    // Fixed 100 live orders; vary retained historical IDs independently.
    let mut historical = Vec::new();
    for seen in [100, 10_000, 100_000] {
        let mut book = OrderBook::new();
        for id in 1..=seen {
            book.place_order(order(id, 1000));
            if id > 100 {
                book.cancel_order(CancelOrder { order_id: id });
            }
        }
        historical.push(json!({"live_orders":100,"seen_ids":seen,"book_clone":measure(&book,|b| b.clone(),|_,_|{},80)}));
    }
    println!("{}",serde_json::to_string_pretty(&json!({"scope":"Release local microbenchmark of current dirty checkout. One funded account, no fills, empty history, no server/journal/network. Seed setup and external reset excluded; apply's internal clone and old-state drop included. 10 warmups, 80 samples, 3 blocks; ascending/descending sizes. Raw operations use an externally cloned reset: legacy queues are unshared, persistent nodes share roots with the seed. Raw add is prepared by add/cancel outside the timer (spare queue capacity in the legacy build). Account release and engine apply measured with live originals sharing account tables, as transaction candidates do. Seed book/accounts and order events checked against ordinary apply at n=16, canonicalizing unordered HashSet serialization.","rows":rows,"historical_ids":historical})).unwrap());
}
