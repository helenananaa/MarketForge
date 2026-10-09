use crate::*;
use serde_json::json;

fn scenario() -> ScenarioConfig {
    serde_json::from_value(json!({
        "room_id": "linked", "market": {"Spot": {
            "instrument": {"symbol": "V-USD-SPOT", "base_asset": "V", "quote_asset": "USD", "tick_size": 1, "lot_size": 1},
            "clearing": {"maker_fee_ppm": 0, "taker_fee_ppm": 0}, "risk": {"allow_short": false}
        }}, "extra_markets": [{"Perp": {
            "instrument": {"symbol": "V-USD-PERP", "base_asset": "V", "quote_asset": "USD", "tick_size": 1, "lot_size": 1},
            "clearing": {"maker_fee_ppm": 0, "taker_fee_ppm": 0, "leverage": 10}, "risk": {},
            "initial_mark_price_tick": 100,
            "price_link": {"spot_instrument_id": "V-USD-SPOT", "max_age_ms": 2000}
        }}], "accounts": [
            {"Spot": {"account_id": 10, "cash_balance": 100000, "position_qty": 100}},
            {"Basic": {"account_id": 20, "cash_balance": 10}},
            {"Basic": {"account_id": 30, "cash_balance": 2000}}
        ], "seed_orders": []
    })).unwrap()
}

fn limit(id: u64, account: u64, side: Side, price: i64, qty: u64) -> Command {
    Command::NewOrder(NewOrder {
        position_side: crate::model::PositionSide::Both,
        order_id: id,
        account_id: account,
        side,
        kind: OrderKind::Limit { price_tick: price },
        qty,
        reduce_only: false,
    })
}

fn accepted(execution: &ActorExecution) {
    assert!(
        matches!(execution.result, ActorExecutionResult::Accepted(_)),
        "{execution:?}"
    );
}

fn seed(exchange: &mut ExchangeActor) {
    accepted(&exchange.apply(limit(1, 10, Side::Buy, 99, 10)));
    accepted(&exchange.apply(limit(2, 10, Side::Sell, 101, 10)));
}

fn price(exchange: &ExchangeActor) -> PerpPriceSnapshot {
    exchange.perp_price_snapshot("V-USD-PERP").unwrap().unwrap()
}

#[test]
fn price_link_validates_kind_identity_venue_and_config() {
    for change in ["missing", "perp", "asset", "venue", "age"] {
        let mut s = scenario();
        let MarketConfig::Perp(perp) = &mut s.extra_markets[0] else {
            unreachable!()
        };
        match change {
            "missing" => perp.price_link.as_mut().unwrap().spot_instrument_id = "absent".into(),
            "perp" => perp.price_link.as_mut().unwrap().spot_instrument_id = "V-USD-PERP".into(),
            "asset" => perp.instrument.base_asset = "OTHER".into(),
            "venue" => perp.instrument.venue_id = "other-venue".into(),
            "age" => perp.price_link.as_mut().unwrap().max_age_ms = 0,
            _ => unreachable!(),
        }
        assert!(SimulationRoom::from_scenario(s).is_err(), "{change}");
    }
    let mut legacy = serde_json::to_value(scenario()).unwrap();
    legacy["extra_markets"][0]["Perp"]
        .as_object_mut()
        .unwrap()
        .remove("price_link");
    let legacy: ScenarioConfig = serde_json::from_value(legacy).unwrap();
    let exchange = legacy.bootstrap().unwrap().exchange;
    assert_eq!(exchange.perp_price_snapshot("V-USD-PERP").unwrap(), None);
}

#[test]
fn price_link_requires_a_price_and_preserves_independent_perp_book() {
    let mut exchange = scenario().bootstrap().unwrap().exchange;
    assert_eq!(price(&exchange).status, PriceLinkStatus::AwaitingPrice);
    let unpriced = exchange
        .apply_to_instrument("V-USD-PERP", limit(5, 30, Side::Buy, 150, 1))
        .unwrap();
    assert_eq!(
        unpriced.result,
        ActorExecutionResult::Rejected(ActorRejectReason::PriceLinkNotReady)
    );
    seed(&mut exchange);
    let snapshot = price(&exchange);
    assert_eq!(snapshot.index_price_tick, Some(100));
    assert_eq!(snapshot.mark_price_tick, 100);
    assert_eq!(snapshot.source, Some(IndexPriceSource::SpotMid));
    accepted(
        &exchange
            .apply_to_instrument("V-USD-PERP", limit(6, 30, Side::Buy, 150, 1))
            .unwrap(),
    );
    assert_eq!(
        exchange.book_snapshot_for("V-USD-PERP").unwrap().bids[0].price_tick,
        150
    );
    assert_eq!(price(&exchange).mark_price_tick, 100);
    let manual = exchange
        .apply_to_instrument(
            "V-USD-PERP",
            Command::SetMarkPrice(SetMarkPrice { price_tick: 200 }),
        )
        .unwrap();
    assert_eq!(
        manual.result,
        ActorExecutionResult::Rejected(ActorRejectReason::LinkedMarkPriceManaged)
    );
}

#[test]
fn price_link_stale_and_unavailable_freeze_mark_and_allow_cancellation() {
    let mut exchange = scenario().bootstrap().unwrap().exchange;
    seed(&mut exchange);
    accepted(
        &exchange
            .apply_to_instrument("V-USD-PERP", limit(5, 30, Side::Buy, 100, 1))
            .unwrap(),
    );
    exchange.advance_clock(3).unwrap();
    assert_eq!(price(&exchange).status, PriceLinkStatus::Stale);
    assert_eq!(price(&exchange).mark_price_tick, 100);
    let rejected = exchange
        .apply_to_instrument("V-USD-PERP", limit(6, 30, Side::Buy, 100, 1))
        .unwrap();
    assert_eq!(
        rejected.result,
        ActorExecutionResult::Rejected(ActorRejectReason::PriceLinkNotReady)
    );
    accepted(
        &exchange
            .apply_to_instrument(
                "V-USD-PERP",
                Command::CancelOrder(CancelOrder { order_id: 5 }),
            )
            .unwrap(),
    );
    // A rejected/no-op command does not make an old quote fresh.
    exchange.apply(Command::CancelOrder(CancelOrder { order_id: 999 }));
    assert_eq!(price(&exchange).status, PriceLinkStatus::Stale);
    accepted(&exchange.apply(Command::CancelOrder(CancelOrder { order_id: 1 })));
    assert_eq!(price(&exchange).status, PriceLinkStatus::Unavailable);
    assert_eq!(price(&exchange).mark_price_tick, 100);
    accepted(&exchange.apply(limit(7, 10, Side::Buy, 95, 10)));
    assert_eq!(price(&exchange).status, PriceLinkStatus::Live);
    assert_eq!(price(&exchange).mark_price_tick, 98);
}

#[test]
fn price_link_trade_fallback_persists_timestamp_and_expires() {
    let mut exchange = scenario().bootstrap().unwrap().exchange;
    seed(&mut exchange);
    accepted(&exchange.apply(limit(5, 30, Side::Buy, 101, 10)));
    assert_eq!(price(&exchange).source, Some(IndexPriceSource::SpotTrade));
    assert_eq!(price(&exchange).mark_price_tick, 101);
    exchange.advance_clock(3).unwrap();
    exchange.apply(limit(6, 10, Side::Buy, 99, 1));
    assert_eq!(price(&exchange).status, PriceLinkStatus::Unavailable);
    assert_eq!(price(&exchange).source_time_ms, Some(0));
    assert_eq!(price(&exchange).mark_price_tick, 101);
}

#[test]
fn price_link_rounds_mark_to_perp_grid_and_updates_multiple_contracts() {
    let mut s = scenario();
    let MarketConfig::Perp(perp) = &mut s.extra_markets[0] else {
        unreachable!()
    };
    perp.instrument.tick_size = 3;
    let mut second = perp.clone();
    second.instrument.instrument_id = "V-USD-PERP-2".into();
    second.instrument.symbol = "V-USD-PERP-2".into();
    second.instrument.tick_size = 1;
    s.extra_markets.push(MarketConfig::Perp(second));
    let mut exchange = s.bootstrap().unwrap().exchange;
    exchange.apply(limit(1, 10, Side::Buy, 99, 10));
    let update = exchange.apply(limit(2, 10, Side::Sell, 101, 10));
    assert_eq!(update.price_updates.len(), 2);
    assert_eq!(price(&exchange).index_price_tick, Some(100));
    assert_eq!(price(&exchange).mark_price_tick, 99);
    assert_eq!(
        exchange
            .perp_price_snapshot("V-USD-PERP-2")
            .unwrap()
            .unwrap()
            .mark_price_tick,
        100
    );
    assert_eq!(crate::price_link::mark_on_grid(i64::MAX, 3), i64::MAX - 1);
}

#[test]
fn price_link_rejected_spot_order_does_not_change_price_or_receipts() {
    let mut exchange = scenario().bootstrap().unwrap().exchange;
    seed(&mut exchange);
    let before = price(&exchange);
    let result = exchange.apply(limit(5, 20, Side::Buy, 120, 1000));
    assert!(result.price_updates.is_empty());
    assert_eq!(price(&exchange), before);
}

#[test]
fn price_link_snapshot_and_replay_reproduce_derived_state() {
    let mut exchange = scenario().bootstrap().unwrap().exchange;
    seed(&mut exchange);
    exchange.advance_clock(1).unwrap();
    let json = serde_json::to_value(&exchange).unwrap();
    let mut restored: ExchangeActor = serde_json::from_value(json).unwrap();
    restored.normalize_after_restore().unwrap();
    let command = limit(5, 30, Side::Buy, 101, 10);
    assert_eq!(
        exchange.apply(command.clone()),
        restored.apply(command.clone())
    );
    assert_eq!(price(&exchange), price(&restored));
    let mut replay = scenario().bootstrap().unwrap().exchange;
    seed(&mut replay);
    replay.advance_clock(1).unwrap();
    replay.apply(command);
    // The matching engine stores seen order IDs in a HashSet, whose JSON
    // iteration order is not semantic state.
    fn canonicalize(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(fields) => {
                if let Some(ids) = fields
                    .get_mut("seen_order_ids")
                    .and_then(|v| v.as_array_mut())
                {
                    ids.sort_by_key(|id| id.as_u64());
                }
                for value in fields.values_mut() {
                    canonicalize(value);
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    canonicalize(value);
                }
            }
            _ => {}
        }
    }
    let mut actual = serde_json::to_value(exchange).unwrap();
    let mut expected = serde_json::to_value(replay).unwrap();
    canonicalize(&mut actual);
    canonicalize(&mut expected);
    assert_eq!(actual, expected);
}

#[test]
fn price_link_spot_drop_drives_pnl_auto_liquidation_and_stale_reduce_only() {
    let mut rooms = RoomManager::new();
    let mut s = scenario();
    s.seed_orders = vec![
        limit(1, 10, Side::Buy, 99, 10),
        limit(2, 10, Side::Sell, 101, 10),
    ];
    rooms.create_room(s).unwrap();
    accepted(
        &rooms
            .apply_to_instrument("linked", "V-USD-PERP", limit(3, 10, Side::Sell, 100, 1))
            .unwrap(),
    );
    accepted(
        &rooms
            .apply_to_instrument("linked", "V-USD-PERP", limit(4, 20, Side::Buy, 100, 1))
            .unwrap(),
    );
    accepted(
        &rooms
            .apply_to_instrument("linked", "V-USD-PERP", limit(5, 10, Side::Buy, 80, 10))
            .unwrap(),
    );
    let AccountSnapshot::Perp(long) = rooms
        .account_snapshot_for("linked", "V-USD-PERP", 20)
        .unwrap()
        .unwrap()
    else {
        unreachable!()
    };
    assert_eq!(long.position_qty, 1);
    rooms.advance_clock("linked", 3).unwrap();
    let mut reduce = limit(6, 20, Side::Sell, 200, 1);
    if let Command::NewOrder(order) = &mut reduce {
        order.reduce_only = true;
    }
    accepted(
        &rooms
            .apply_to_instrument("linked", "V-USD-PERP", reduce)
            .unwrap(),
    );
    accepted(
        &rooms
            .apply("linked", Command::CancelOrder(CancelOrder { order_id: 1 }))
            .unwrap(),
    );
    accepted(
        &rooms
            .apply("linked", Command::CancelOrder(CancelOrder { order_id: 2 }))
            .unwrap(),
    );
    accepted(
        &rooms
            .apply("linked", limit(7, 10, Side::Buy, 79, 10))
            .unwrap(),
    );
    let drop = rooms
        .apply("linked", limit(8, 10, Side::Sell, 81, 10))
        .unwrap();
    accepted(&drop);
    assert_eq!(drop.price_updates[0].mark_price_tick, 80);
    let AccountSnapshot::Perp(long) = rooms
        .account_snapshot_for("linked", "V-USD-PERP", 20)
        .unwrap()
        .unwrap()
    else {
        unreachable!()
    };
    assert_eq!(long.position_qty, 0);
    assert!(rooms.execution_history("linked").unwrap().iter().any(|execution| {
        matches!(&execution.result, ActorExecutionResult::Accepted(MarketExecution::Perp(perp)) if perp.clearing_events.iter().any(|event| matches!(event, PerpClearingEvent::LiquidationSettled { account_id: 20, .. })))
    }));
}
