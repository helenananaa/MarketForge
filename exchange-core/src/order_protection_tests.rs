fn protected_order(
    id: u64,
    side: Side,
    price: i64,
    qty: u64,
    order_type: crate::model::ProtectedOrderType,
    valid_until: Option<u64>,
    expires: Option<u64>,
) -> Command {
    Command::NewOrder(NewOrder {
        position_side: crate::model::PositionSide::Both,
        order_id: id,
        account_id: 20,
        side,
        qty,
        reduce_only: false,
        kind: OrderKind::Protected {
            order_type,
            price_tick: price,
            valid_until_market_time_ms: valid_until,
            expires_at_market_time_ms: expires,
        },
    })
}

fn spot_events(execution: &crate::ActorExecution) -> Vec<Event> {
    match &execution.result {
        ActorExecutionResult::Accepted(MarketExecution::Spot(result)) => result
            .events
            .iter()
            .map(|record| record.event.clone())
            .collect(),
        other => panic!("unexpected result: {other:?}"),
    }
}

#[test]
fn protection_rejects_old_decisions_at_the_exchange_deadline() {
    use crate::model::ProtectedOrderType::ImmediateOrCancel;
    let mut rooms = RoomManager::new();
    rooms.create_room(spot_scenario("deadline")).unwrap();
    let old_book = rooms.book_snapshot("deadline").unwrap();
    rooms.advance_clock("deadline", 1).unwrap();
    let stale = rooms
        .apply(
            "deadline",
            protected_order(2, Side::Buy, 101, 1, ImmediateOrCancel, Some(1000), None),
        )
        .unwrap();
    assert!(matches!(
        stale.result,
        ActorExecutionResult::Rejected(ActorRejectReason::OrderProtectionExpired {
            deadline_market_time_ms: 1000,
            market_time_ms: 1000
        })
    ));
    assert_eq!(rooms.book_snapshot("deadline").unwrap(), old_book);
    let fresh = rooms
        .apply(
            "deadline",
            protected_order(3, Side::Buy, 101, 1, ImmediateOrCancel, Some(1001), None),
        )
        .unwrap();
    assert!(
        spot_events(&fresh)
            .iter()
            .any(|event| matches!(event, Event::TradePrinted(_)))
    );
}

#[test]
fn protection_price_bound_survives_price_moving_while_deciding() {
    use crate::model::ProtectedOrderType::ImmediateOrCancel;
    let mut rooms = RoomManager::new();
    rooms.create_room(spot_scenario("moving")).unwrap();
    assert_eq!(
        rooms.book_snapshot("moving").unwrap().asks[0].price_tick,
        100
    );
    rooms
        .apply("moving", Command::CancelOrder(CancelOrder { order_id: 1 }))
        .unwrap();
    rooms
        .apply("moving", limit(2, 10, Side::Sell, 120, 5))
        .unwrap();
    let result = rooms
        .apply(
            "moving",
            protected_order(3, Side::Buy, 101, 2, ImmediateOrCancel, None, None),
        )
        .unwrap();
    let events = spot_events(&result);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::TradePrinted(_)))
    );
    assert!(events.contains(&Event::OrderExpired {
        order_id: 3,
        unfilled_qty: 2
    }));
    assert_eq!(
        rooms.book_snapshot("moving").unwrap().asks[0].price_tick,
        120
    );
}

#[test]
fn protection_ioc_can_partially_fill_without_crossing_the_price_bound() {
    use crate::model::ProtectedOrderType::ImmediateOrCancel;
    let mut scenario = spot_scenario("partial");
    scenario.seed_orders = vec![
        limit(1, 10, Side::Sell, 100, 1),
        limit(2, 10, Side::Sell, 105, 4),
    ];
    let mut rooms = RoomManager::new();
    rooms.create_room(scenario).unwrap();
    let result = rooms
        .apply(
            "partial",
            protected_order(3, Side::Buy, 101, 3, ImmediateOrCancel, Some(5000), None),
        )
        .unwrap();
    let events = spot_events(&result);
    assert_eq!(
        events
            .iter()
            .filter_map(|event| match event {
                Event::TradePrinted(trade) => Some((trade.price_tick, trade.qty)),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![(100, 1)]
    );
    assert!(events.contains(&Event::OrderExpired {
        order_id: 3,
        unfilled_qty: 2
    }));
    assert!(rooms.book_snapshot("partial").unwrap().bids.is_empty());
}

#[test]
fn protection_expiry_releases_partial_resting_orders_and_is_replayable() {
    use crate::model::ProtectedOrderType::Limit;
    let mut scenario = spot_scenario("expiry");
    scenario.seed_orders = vec![limit(1, 10, Side::Sell, 100, 1)];
    let mut rooms = RoomManager::new();
    rooms.create_room(scenario).unwrap();
    rooms
        .apply(
            "expiry",
            protected_order(2, Side::Buy, 101, 3, Limit, Some(1000), Some(2000)),
        )
        .unwrap();
    rooms.advance_clock("expiry", 1).unwrap();
    assert_eq!(rooms.book_snapshot("expiry").unwrap().bids[0].qty, 2);
    let saved = serde_json::to_string(rooms.simulation_room("expiry").unwrap()).unwrap();
    let mut restored = RoomManager::new();
    restored
        .restore_simulation_room(serde_json::from_str(&saved).unwrap(), vec![])
        .unwrap();
    for manager in [&mut rooms, &mut restored] {
        manager.advance_clock("expiry", 1).unwrap();
        assert!(manager.book_snapshot("expiry").unwrap().bids.is_empty());
        let observation = manager
            .participant_observation("expiry", "V-BTC-SPOT", 20)
            .unwrap();
        let Some(crate::AccountSnapshot::Spot(account)) = observation.own_account else {
            panic!()
        };
        assert_eq!(account.reserved_cash, 0);
        assert_eq!(account.position_qty, 1);
        let last = manager.execution_history("expiry").unwrap().last().unwrap();
        assert_eq!(last.market_time_ms, 2000);
        assert!(spot_events(last).contains(&Event::OrderExpired {
            order_id: 2,
            unfilled_qty: 2
        }));
        let count = manager.execution_history("expiry").unwrap().len();
        manager.advance_clock("expiry", 1).unwrap();
        assert_eq!(manager.execution_history("expiry").unwrap().len(), count);
    }
    let commands = rooms
        .execution_history("expiry")
        .unwrap()
        .iter()
        .flat_map(|execution| match &execution.result {
            ActorExecutionResult::Accepted(MarketExecution::Spot(result)) => {
                vec![result.command.clone()]
            }
            _ => vec![],
        })
        .collect::<Vec<_>>();
    let replayed = crate::ReplayEngine::replay(&commands);
    assert_eq!(
        replayed.final_snapshot,
        rooms.book_snapshot("expiry").unwrap()
    );
    assert!(replayed.events.iter().any(|record| record.event
        == Event::OrderExpired {
            order_id: 2,
            unfilled_qty: 2
        }));
}

#[test]
fn protection_long_lived_limits_and_post_only_keep_their_semantics() {
    use crate::model::ProtectedOrderType::{ImmediateOrCancel, Limit, PostOnly};
    let mut rooms = RoomManager::new();
    rooms.create_room(spot_scenario("long-lived")).unwrap();
    let rejected = rooms
        .apply(
            "long-lived",
            protected_order(2, Side::Buy, 101, 1, PostOnly, None, Some(5000)),
        )
        .unwrap();
    assert!(spot_events(&rejected).iter().any(|event| matches!(
        event,
        Event::OrderRejected {
            reason: crate::RejectReason::PostOnlyWouldTakeLiquidity,
            ..
        }
    )));
    rooms
        .apply(
            "long-lived",
            protected_order(3, Side::Buy, 95, 1, Limit, None, None),
        )
        .unwrap();
    rooms.advance_clock("long-lived", 10).unwrap();
    assert_eq!(
        rooms.book_snapshot("long-lived").unwrap().bids[0].price_tick,
        95
    );
    let invalid = rooms
        .apply(
            "long-lived",
            protected_order(4, Side::Buy, 101, 1, ImmediateOrCancel, None, Some(20000)),
        )
        .unwrap();
    assert!(matches!(
        invalid.result,
        ActorExecutionResult::Rejected(ActorRejectReason::InvalidOrderProtection)
    ));
}

#[test]
fn protection_expired_sell_cannot_fill_on_the_next_step() {
    use crate::model::ProtectedOrderType::Limit;
    let mut scenario = spot_scenario("sell-expiry");
    scenario.seed_orders.clear();
    let mut rooms = RoomManager::new();
    rooms.create_room(scenario).unwrap();
    let mut sell = protected_order(1, Side::Sell, 100, 2, Limit, None, Some(1000));
    if let Command::NewOrder(order) = &mut sell {
        order.account_id = 10;
    }
    rooms.apply("sell-expiry", sell).unwrap();
    rooms.pause_room("sell-expiry").unwrap();
    rooms.advance_clock("sell-expiry", 1).unwrap();
    assert!(rooms.book_snapshot("sell-expiry").unwrap().asks.is_empty());
    let observation = rooms
        .participant_observation("sell-expiry", "V-BTC-SPOT", 10)
        .unwrap();
    let Some(crate::AccountSnapshot::Spot(account)) = observation.own_account else {
        panic!()
    };
    assert_eq!(account.reserved_position, 0);
}

#[test]
fn protection_reduce_only_respects_minimum_sell_price_and_current_position() {
    use crate::model::ProtectedOrderType::ImmediateOrCancel;
    let mut scenario = pending_perp_liquidation_scenario("reduce-protection");
    scenario.seed_orders = vec![
        limit(1, 10, Side::Sell, 100, 2),
        limit(2, 20, Side::Buy, 100, 2),
        limit(3, 30, Side::Buy, 98, 1),
        limit(4, 30, Side::Buy, 95, 1),
    ];
    let mut rooms = RoomManager::new();
    rooms.create_room(scenario).unwrap();
    let close = |id, price, qty| {
        let mut command = protected_order(
            id,
            Side::Sell,
            price,
            qty,
            ImmediateOrCancel,
            Some(5000),
            None,
        );
        if let Command::NewOrder(order) = &mut command {
            order.reduce_only = true;
        }
        command
    };
    let no_fill = rooms.apply("reduce-protection", close(5, 99, 2)).unwrap();
    let ActorExecutionResult::Accepted(MarketExecution::Perp(no_fill)) = no_fill.result else {
        panic!()
    };
    assert!(
        !no_fill
            .events
            .iter()
            .any(|event| matches!(event.event, Event::TradePrinted(_)))
    );
    let partial = rooms.apply("reduce-protection", close(6, 98, 2)).unwrap();
    let ActorExecutionResult::Accepted(MarketExecution::Perp(partial)) = partial.result else {
        panic!()
    };
    assert!(partial.events.iter().any(|event| matches!(&event.event,
        Event::TradePrinted(trade) if trade.price_tick == 98 && trade.qty == 1)));
    assert!(partial.events.iter().any(|event| event.event
        == Event::OrderExpired {
            order_id: 6,
            unfilled_qty: 1
        }));
    let too_large = rooms.apply("reduce-protection", close(7, 95, 2)).unwrap();
    let ActorExecutionResult::Accepted(MarketExecution::Perp(too_large)) = too_large.result else {
        panic!()
    };
    assert!(too_large.events.iter().any(|event| matches!(
        event.event,
        Event::RiskRejected {
            reason: crate::model::RiskRejectReason::ReduceOnlyExceedsPosition,
            ..
        }
    )));
    let observation = rooms
        .participant_observation("reduce-protection", "V-BTC-PERP", 20)
        .unwrap();
    let Some(crate::AccountSnapshot::Perp(account)) = observation.own_account else {
        panic!()
    };
    assert_eq!(account.position_qty, 1);
}

#[test]
fn protection_perp_expiry_releases_reserved_margin() {
    use crate::model::ProtectedOrderType::Limit;
    let mut rooms = RoomManager::new();
    rooms
        .create_room(pending_perp_liquidation_scenario("perp-expiry"))
        .unwrap();
    rooms
        .apply(
            "perp-expiry",
            protected_order(1, Side::Buy, 95, 2, Limit, None, Some(1000)),
        )
        .unwrap();
    let observation = rooms
        .participant_observation("perp-expiry", "V-BTC-PERP", 20)
        .unwrap();
    let Some(crate::AccountSnapshot::Perp(account)) = observation.own_account else {
        panic!()
    };
    assert!(account.reserved_margin > 0);
    rooms.advance_clock("perp-expiry", 1).unwrap();
    let observation = rooms
        .participant_observation("perp-expiry", "V-BTC-PERP", 20)
        .unwrap();
    let Some(crate::AccountSnapshot::Perp(account)) = observation.own_account else {
        panic!()
    };
    assert_eq!(account.reserved_margin, 0);
    assert!(observation.own_orders.is_empty());
}

#[test]
fn protection_unbounded_reduce_only_sweeps_but_cannot_reverse_the_position() {
    use crate::TradingApi;
    let mut scenario = pending_perp_liquidation_scenario("unbounded-reduce");
    scenario.seed_orders = vec![
        limit(1, 10, Side::Sell, 100, 2),
        limit(2, 20, Side::Buy, 100, 2),
        limit(3, 30, Side::Buy, 98, 1),
        limit(4, 30, Side::Buy, 95, 1),
    ];
    let mut rooms = RoomManager::new();
    rooms.create_room(scenario).unwrap();
    let close = |rooms: &mut RoomManager, id, qty| {
        crate::OrderGateway::new(rooms, id)
            .submit_action(crate::GatewayRequest {
                participant_id: "sweeper".into(),
                room_id: "unbounded-reduce".into(),
                instrument_id: None,
                account_id: 20,
                action: crate::OrderAction::PlaceUnboundedMarket {
                    position_side: Default::default(),
                    side: Side::Sell,
                    qty,
                    reduce_only: true,
                    valid_until_market_time_ms: Some(5000),
                },
            })
            .unwrap()
            .execution
    };
    let too_large = close(&mut rooms, 5, 3);
    let ActorExecutionResult::Accepted(MarketExecution::Perp(too_large)) = too_large.result else {
        panic!()
    };
    assert!(too_large.events.iter().any(|event| matches!(
        event.event,
        Event::RiskRejected {
            reason: crate::model::RiskRejectReason::ReduceOnlyExceedsPosition,
            ..
        }
    )));
    let sweep = close(&mut rooms, 6, 2);
    let ActorExecutionResult::Accepted(MarketExecution::Perp(sweep)) = sweep.result else {
        panic!()
    };
    assert_eq!(
        sweep
            .events
            .iter()
            .filter_map(|event| match &event.event {
                Event::TradePrinted(trade) => Some(trade.price_tick),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![98, 95]
    );
    let observation = rooms
        .participant_observation("unbounded-reduce", "V-BTC-PERP", 20)
        .unwrap();
    let Some(crate::AccountSnapshot::Perp(account)) = observation.own_account else {
        panic!()
    };
    assert_eq!(account.position_qty, 0);
    let flat = close(&mut rooms, 7, 1);
    let ActorExecutionResult::Accepted(MarketExecution::Perp(flat)) = flat.result else {
        panic!()
    };
    assert!(flat.events.iter().any(|event| matches!(
        event.event,
        Event::RiskRejected {
            reason: crate::model::RiskRejectReason::ReduceOnlyWouldIncreasePosition,
            ..
        }
    )));
}
