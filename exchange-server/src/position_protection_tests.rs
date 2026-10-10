fn native_protection_spec(
    tp: Option<i64>,
    sl: Option<i64>,
) -> exchange_core::PositionProtectionSpec {
    exchange_core::PositionProtectionSpec {
        take_profit_tick: tp,
        stop_loss_tick: sl,
        trigger: exchange_core::ProtectionTrigger::Mark,
        ..Default::default()
    }
}
fn native_order(id: u64, account: u64, side: Side, price: i64, qty: u64) -> Command {
    Command::NewOrder(NewOrder {
        order_id: id,
        account_id: account,
        side,
        position_side: Default::default(),
        qty,
        reduce_only: false,
        kind: OrderKind::Limit { price_tick: price },
    })
}
fn native_room(name: &str) -> RoomManager {
    let mut rooms = RoomManager::new();
    rooms
        .create_room(perp_liquidation_scenario_with_mark(name, 100))
        .unwrap();
    rooms
        .apply(name, native_order(1, 10, Side::Sell, 100, 5))
        .unwrap();
    rooms
}
fn native_bracket(name: &str, rooms: &mut RoomManager, kind: OrderKind) -> ActorExecution {
    rooms
        .apply(
            name,
            Command::NewOrderWithProtection {
                order: NewOrder {
                    order_id: 2,
                    account_id: 20,
                    side: Side::Buy,
                    position_side: Default::default(),
                    qty: 5,
                    reduce_only: false,
                    kind,
                },
                protection: Box::new(native_protection_spec(Some(110), Some(90))),
            },
        )
        .unwrap()
}
fn native_position(name: &str, rooms: &RoomManager) -> i128 {
    let Some(exchange_core::AccountSnapshot::Perp(a)) = rooms.account_snapshot(name, 20).unwrap()
    else {
        panic!("missing account");
    };
    a.position_qty
}
#[test]
fn position_protection_partial_exit_latches_and_retries_after_price_recovers() {
    let name = "native-partial";
    let mut r = native_room(name);
    native_bracket(name, &mut r, OrderKind::Market);
    r.apply(name, native_order(3, 30, Side::Buy, 90, 2))
        .unwrap();
    r.apply(name, Command::SetMarkPrice(SetMarkPrice { price_tick: 90 }))
        .unwrap();
    assert_eq!(native_position(name, &r), 3);
    let protection = &r.room(name).unwrap().position_protections("V-BTC-PERP", 20)[0];
    assert_eq!(protection.status, "triggered");
    assert_eq!(protection.triggered_by.as_deref(), Some("stop_loss"));
    r.apply(
        name,
        Command::SetMarkPrice(SetMarkPrice { price_tick: 100 }),
    )
    .unwrap();
    r.apply(name, native_order(4, 30, Side::Buy, 95, 3))
        .unwrap();
    assert_eq!(native_position(name, &r), 0);
    let p = &r.room(name).unwrap().position_protections("V-BTC-PERP", 20)[0];
    assert_eq!(p.status, "completed");
    r.apply(name, native_order(5, 10, Side::Sell, 100, 1))
        .unwrap();
    r.apply(
        name,
        Command::NewOrder(NewOrder {
            order_id: 6,
            account_id: 20,
            side: Side::Buy,
            position_side: Default::default(),
            qty: 1,
            reduce_only: false,
            kind: OrderKind::Market,
        }),
    )
    .unwrap();
    r.apply(name, Command::SetMarkPrice(SetMarkPrice { price_tick: 80 }))
        .unwrap();
    assert_eq!(
        native_position(name, &r),
        1,
        "completed protection must not close a later position"
    );
}
#[test]
fn position_protection_invalid_bracket_is_atomic_and_does_not_open() {
    let name = "native-invalid";
    let mut r = native_room(name);
    let e = r
        .apply(
            name,
            Command::NewOrderWithProtection {
                order: NewOrder {
                    order_id: 2,
                    account_id: 20,
                    side: Side::Buy,
                    position_side: Default::default(),
                    qty: 5,
                    reduce_only: false,
                    kind: OrderKind::Market,
                },
                protection: Box::new(native_protection_spec(Some(95), Some(90))),
            },
        )
        .unwrap();
    assert!(matches!(e.result, ActorExecutionResult::Rejected(_)));
    assert_eq!(native_position(name, &r), 0);
    assert!(
        r.room(name)
            .unwrap()
            .position_protections("V-BTC-PERP", 20)
            .is_empty()
    );
}
#[test]
fn position_protection_take_profit_and_snapshot_recovery() {
    let name = "native-recover";
    let mut r = native_room(name);
    native_bracket(name, &mut r, OrderKind::Market);
    let snapshot = serde_json::to_value(r.simulation_room(name).unwrap()).unwrap();
    let mut restored = RoomManager::new();
    restored
        .restore_simulation_room(serde_json::from_value(snapshot).unwrap(), Vec::new())
        .unwrap();
    restored
        .apply(name, native_order(3, 30, Side::Buy, 109, 5))
        .unwrap();
    restored
        .apply(
            name,
            Command::SetMarkPrice(SetMarkPrice { price_tick: 110 }),
        )
        .unwrap();
    assert_eq!(native_position(name, &restored), 0);
    let p = &restored
        .room(name)
        .unwrap()
        .position_protections("V-BTC-PERP", 20)[0];
    assert_eq!(p.triggered_by.as_deref(), Some("take_profit"));
}
#[test]
fn position_protection_pending_entry_cancel_and_manual_update() {
    let name = "native-pending";
    let mut r = native_room(name);
    native_bracket(name, &mut r, OrderKind::Limit { price_tick: 95 });
    assert_eq!(
        r.room(name).unwrap().position_protections("V-BTC-PERP", 20)[0].status,
        "awaiting_fill"
    );
    r.apply(
        name,
        Command::CancelOrder(exchange_core::CancelOrder { order_id: 2 }),
    )
    .unwrap();
    assert_eq!(
        r.room(name).unwrap().position_protections("V-BTC-PERP", 20)[0].status,
        "completed"
    );
    r.apply(
        name,
        Command::NewOrder(NewOrder {
            order_id: 3,
            account_id: 20,
            side: Side::Buy,
            position_side: Default::default(),
            qty: 2,
            reduce_only: false,
            kind: OrderKind::Market,
        }),
    )
    .unwrap();
    r.apply(
        name,
        Command::SetPositionProtection {
            account_id: 20,
            position_side: Default::default(),
            protection: Some(Box::new(native_protection_spec(None, Some(92)))),
        },
    )
    .unwrap();
    assert_eq!(
        r.room(name).unwrap().position_protections("V-BTC-PERP", 20)[0]
            .spec
            .stop_loss_tick,
        Some(92)
    );
    r.apply(
        name,
        Command::SetPositionProtection {
            account_id: 20,
            position_side: Default::default(),
            protection: None,
        },
    )
    .unwrap();
    assert!(
        r.room(name)
            .unwrap()
            .position_protections("V-BTC-PERP", 20)
            .is_empty()
    );
}
#[tokio::test]
async fn position_protection_http_idempotency_risk_events_and_ownership() {
    let app = new_app();
    let name = "native-http";
    send_json(
        &app,
        Method::POST,
        "/rooms",
        None,
        perp_liquidation_scenario_with_mark(name, 100),
    )
    .await;
    let seller = SubmitOrderRequest {
        participant_id: "maker".into(),
        account_id: 10,
        instrument_id: None,
        action: OrderAction::PlaceLimit {
            side: Side::Sell,
            price_tick: 100,
            qty: 5,
        },
    };
    send_json(
        &app,
        Method::POST,
        "/rooms/native-http/orders",
        None,
        seller,
    )
    .await;
    let entry = SubmitOrderRequest {
        participant_id: "native".into(),
        account_id: 20,
        instrument_id: None,
        action: OrderAction::PlaceBracket {
            side: Side::Buy,
            position_side: Default::default(),
            qty: 5,
            price_tick: None,
            protection: native_protection_spec(Some(110), Some(90)),
        },
    };
    let first: OrderResponse = response_json(
        send_json_with_key(
            &app,
            "/rooms/native-http/orders",
            None,
            Some("bracket"),
            &entry,
        )
        .await,
    )
    .await;
    assert!(first.accepted);
    let retry: OrderResponse = response_json(
        send_json_with_key(
            &app,
            "/rooms/native-http/orders",
            None,
            Some("bracket"),
            &entry,
        )
        .await,
    )
    .await;
    assert_eq!(first.command_seq, retry.command_seq);
    let risk: serde_json::Value = response_json(
        send_json(
            &app,
            Method::GET,
            "/rooms/native-http/observe?account_id=20",
            None,
            (),
        )
        .await,
    )
    .await;
    assert!(risk["observation"]["risk"]["margin_buffer"].is_number());
    assert_eq!(
        risk["observation"]["position_protections"][0]["status"],
        "armed"
    );
    let page: serde_json::Value = response_json(
        send_json(
            &app,
            Method::GET,
            "/rooms/native-http/instruments/V-BTC-PERP/risk-events?account_id=20&from_start=true",
            None,
            (),
        )
        .await,
    )
    .await;
    assert!(page["next_after_command_seq"].is_number());
    let denied = send_json(
        &app,
        Method::GET,
        "/rooms/native-http/instruments/V-BTC-PERP/risk-events?account_id=20",
        Some("outsider"),
        (),
    )
    .await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
}
#[test]
fn position_protection_journal_replay_regenerates_exits_once() {
    let name = "native-journal";
    let mut r = native_room(name);
    native_bracket(name, &mut r, OrderKind::Market);
    r.apply(name, native_order(3, 30, Side::Buy, 89, 5))
        .unwrap();
    r.apply(name, Command::SetMarkPrice(SetMarkPrice { price_tick: 90 }))
        .unwrap();
    let history = r
        .execution_history_from(name, 0)
        .unwrap()
        .cloned()
        .collect::<Vec<_>>();
    let mut replay = RoomManager::new();
    replay
        .create_room(perp_liquidation_scenario_with_mark(name, 100))
        .unwrap();
    for e in &history {
        let cmd = command_from_actor_execution(e).unwrap();
        if cmd
            .new_order()
            .is_some_and(|o| o.order_id >= 8_000_000_000_000_000_000)
        {
            continue;
        }
        replay.apply(name, cmd).unwrap();
    }
    assert_eq!(native_position(name, &replay), 0);
    assert_eq!(
        replay
            .execution_history_from(name, 0)
            .unwrap()
            .cloned()
            .collect::<Vec<_>>(),
        history
    );
}

#[test]
fn position_protection_actual_journal_recovery_matches_actor() {
    let name = "native-full-recovery";
    let scenario = perp_liquidation_scenario_with_mark(name, 100);
    let mut r = native_room(name);
    native_bracket(name, &mut r, OrderKind::Market);
    r.apply(name, native_order(3, 30, Side::Buy, 89, 5))
        .unwrap();
    r.apply(name, Command::SetMarkPrice(SetMarkPrice { price_tick: 90 }))
        .unwrap();
    let records = r
        .execution_history_from(name, 0)
        .unwrap()
        .cloned()
        .map(|e| JournalExecution::system(command_from_actor_execution(&e).unwrap(), e))
        .collect();
    let recovery = JournalRecovery {
        runtime_checkpoints: Vec::new(),
        next_order_id: None,
        last_market_ticks: Vec::new(),
        rooms: vec![journal::JournalRoom {
            room_id: name.into(),
            scenario,
            status: MarketStatus::Running,
        }],
        executions: records,
        mutations: Vec::new(),
        snapshots: Vec::new(),
    };
    assert_eq!(next_order_id_from_recovery(&recovery).unwrap(), 4);
    let recovered = recover_rooms(&recovery).unwrap();
    fn canonical(mut v: serde_json::Value) -> serde_json::Value {
        match &mut v {
            serde_json::Value::Object(o) => {
                for (key, val) in o.iter_mut() {
                    if key == "seen_order_ids" {
                        val.as_array_mut().unwrap().sort_by_key(|v| v.as_u64());
                    } else {
                        *val = canonical(val.take());
                    }
                }
            }
            serde_json::Value::Array(a) => {
                for v in a {
                    *v = canonical(v.take());
                }
            }
            _ => {}
        }
        v
    }
    assert_eq!(
        canonical(serde_json::to_value(recovered.simulation_room(name).unwrap()).unwrap()),
        canonical(serde_json::to_value(r.simulation_room(name).unwrap()).unwrap())
    );
}

#[test]
fn position_protection_last_price_checks_all_fills_and_batch_observations_are_private() {
    let name = "native-last";
    let mut r = native_room(name);
    native_bracket(name, &mut r, OrderKind::Market); // prints the initial last price 100
    r.apply(
        name,
        Command::SetPositionProtection {
            account_id: 20,
            position_side: Default::default(),
            protection: Some(Box::new(exchange_core::PositionProtectionSpec {
                take_profit_tick: Some(110),
                stop_loss_tick: Some(90),
                trigger: exchange_core::ProtectionTrigger::Last,
                ..Default::default()
            })),
        },
    )
    .unwrap();
    {
        let mut batch = r.observation_batch(name);
        for account in [20, 30, 10, 20] {
            let view = batch
                .bot_observation("V-BTC-PERP", account, None, &[])
                .unwrap();
            assert_eq!(
                view,
                r.bot_observation(name, "V-BTC-PERP", account, None)
                    .unwrap()
            );
            assert!(
                view.position_protections
                    .iter()
                    .all(|p| p.account_id == account)
            );
        }
    }
    r.apply(name, native_order(3, 30, Side::Buy, 111, 1))
        .unwrap();
    r.apply(name, native_order(4, 30, Side::Buy, 100, 5))
        .unwrap();
    // Two fills in the same sell command: 111 then 100. The recovered final
    // price cannot erase the take-profit crossing at 111.
    r.apply(
        name,
        Command::NewOrder(NewOrder {
            order_id: 5,
            account_id: 10,
            side: Side::Sell,
            position_side: Default::default(),
            qty: 2,
            reduce_only: false,
            kind: OrderKind::Market,
        }),
    )
    .unwrap();
    let p = &r.room(name).unwrap().position_protections("V-BTC-PERP", 20)[0];
    assert_eq!(p.triggered_by.as_deref(), Some("take_profit"));
    assert_eq!(p.trigger_price_tick, Some(111));
    assert_eq!(native_position(name, &r), 1);
}

#[test]
fn position_protection_short_hedge_exit_preserves_other_leg() {
    let name = "native-hedge";
    let mut scenario = perp_liquidation_scenario_with_mark(name, 100);
    if let MarketConfig::Perp(ref mut config) = scenario.market {
        config.clearing.position_mode = exchange_core::PositionMode::Hedge;
    }
    let mut r = RoomManager::new();
    r.create_room(scenario).unwrap();
    let mut place = |id, account, side, leg, qty, kind| {
        r.apply(
            name,
            Command::NewOrder(NewOrder {
                order_id: id,
                account_id: account,
                side,
                position_side: leg,
                qty,
                kind,
                reduce_only: false,
            }),
        )
        .unwrap()
    };
    place(
        1,
        10,
        Side::Sell,
        exchange_core::PositionSide::Short,
        2,
        OrderKind::Limit { price_tick: 100 },
    );
    place(
        2,
        20,
        Side::Buy,
        exchange_core::PositionSide::Long,
        2,
        OrderKind::Market,
    );
    place(
        3,
        30,
        Side::Buy,
        exchange_core::PositionSide::Long,
        3,
        OrderKind::Limit { price_tick: 100 },
    );
    r.apply(
        name,
        Command::NewOrderWithProtection {
            order: NewOrder {
                order_id: 4,
                account_id: 20,
                side: Side::Sell,
                position_side: exchange_core::PositionSide::Short,
                qty: 3,
                kind: OrderKind::Market,
                reduce_only: false,
            },
            protection: Box::new(native_protection_spec(Some(90), Some(110))),
        },
    )
    .unwrap();
    r.apply(
        name,
        Command::NewOrder(NewOrder {
            order_id: 5,
            account_id: 10,
            side: Side::Sell,
            position_side: exchange_core::PositionSide::Short,
            qty: 3,
            kind: OrderKind::Limit { price_tick: 110 },
            reduce_only: false,
        }),
    )
    .unwrap();
    r.apply(
        name,
        Command::SetMarkPrice(SetMarkPrice { price_tick: 110 }),
    )
    .unwrap();
    let Some(exchange_core::AccountSnapshot::Perp(a)) = r.account_snapshot(name, 20).unwrap()
    else {
        panic!()
    };
    let legs = a.hedge_positions.unwrap();
    assert_eq!(legs.long.qty, 2);
    assert_eq!(legs.short.qty, 0);
}

#[test]
fn position_protection_slices_limits_and_clock_retries_without_funding() {
    let name = "native-slices";
    let mut scenario = perp_liquidation_scenario_with_mark(name, 100);
    if let MarketConfig::Perp(ref mut c) = scenario.market {
        c.risk.max_order_qty = Some(2);
        c.risk.max_order_notional = Some(200);
    }
    let mut r = RoomManager::new();
    r.create_room(scenario).unwrap();
    for (i, qty) in [2, 2, 1].into_iter().enumerate() {
        r.apply(
            name,
            native_order(10 + i as u64 * 2, 10, Side::Sell, 100, qty),
        )
        .unwrap();
        r.apply(
            name,
            Command::NewOrderWithProtection {
                order: NewOrder {
                    order_id: 11 + i as u64 * 2,
                    account_id: 20,
                    side: Side::Buy,
                    position_side: Default::default(),
                    qty,
                    kind: OrderKind::Market,
                    reduce_only: false,
                },
                protection: Box::new(native_protection_spec(Some(110), Some(90))),
            },
        )
        .unwrap();
        r.apply(name, native_order(20 + i as u64, 30, Side::Buy, 90, qty))
            .unwrap();
    }
    assert_eq!(native_position(name, &r), 5);
    r.apply(name, Command::SetMarkPrice(SetMarkPrice { price_tick: 90 }))
        .unwrap();
    assert_eq!(native_position(name, &r), 3);
    r.advance_clock(name, 2).unwrap();
    assert_eq!(native_position(name, &r), 0);
}

#[test]
fn position_protection_liquidation_completes_protection_before_reopening() {
    let name = "native-after-liquidation";
    let mut r = native_room(name);
    r.apply(
        name,
        Command::NewOrderWithProtection {
            order: NewOrder {
                order_id: 2,
                account_id: 20,
                side: Side::Buy,
                position_side: Default::default(),
                qty: 5,
                kind: OrderKind::Market,
                reduce_only: false,
            },
            protection: Box::new(native_protection_spec(Some(110), Some(50))),
        },
    )
    .unwrap();
    r.apply(name, native_order(3, 30, Side::Buy, 60, 5))
        .unwrap();
    r.apply(name, Command::SetMarkPrice(SetMarkPrice { price_tick: 60 }))
        .unwrap();
    assert_eq!(native_position(name, &r), 0);
    assert_eq!(
        r.room(name).unwrap().position_protections("V-BTC-PERP", 20)[0].status,
        "completed"
    );
}

fn native_reduce(id: u64, price: i64, qty: u64) -> Command {
    Command::NewOrder(NewOrder {
        order_id: id,
        account_id: 20,
        side: Side::Sell,
        position_side: Default::default(),
        qty,
        reduce_only: true,
        kind: OrderKind::Limit { price_tick: price },
    })
}
fn native_set_spec(name: &str, r: &mut RoomManager, spec: exchange_core::PositionProtectionSpec) {
    r.apply(
        name,
        Command::SetPositionProtection {
            account_id: 20,
            position_side: Default::default(),
            protection: Some(Box::new(spec)),
        },
    )
    .unwrap();
}
#[test]
fn resting_reduce_only_fok_uses_executable_depth_and_never_reverses_position() {
    let name = "reduce-resting";
    let mut r = native_room(name);
    native_bracket(name, &mut r, OrderKind::Market);
    r.apply(name, native_reduce(3, 110, 5)).unwrap();
    r.apply(name, native_reduce(4, 111, 5)).unwrap();
    let receipt = r
        .apply(
            name,
            Command::NewOrder(NewOrder {
                order_id: 5,
                account_id: 30,
                side: Side::Buy,
                position_side: Default::default(),
                qty: 10,
                reduce_only: false,
                kind: OrderKind::FillOrKill {
                    price_tick: Some(115),
                },
            }),
        )
        .unwrap();
    let ActorExecutionResult::Accepted(exchange_core::MarketExecution::Perp(result)) =
        receipt.result
    else {
        panic!("command rejected");
    };
    assert!(
        !result
            .events
            .iter()
            .any(|e| matches!(e.event, exchange_core::Event::TradePrinted(_)))
    );
    assert_eq!(native_position(name, &r), 5);
    r.apply(
        name,
        Command::NewOrder(NewOrder {
            order_id: 6,
            account_id: 30,
            side: Side::Buy,
            position_side: Default::default(),
            qty: 10,
            reduce_only: false,
            kind: OrderKind::Market,
        }),
    )
    .unwrap();
    assert_eq!(native_position(name, &r), 0);
    assert_eq!(
        r.room(name)
            .unwrap()
            .order_owner_for("V-BTC-PERP", 4)
            .unwrap(),
        None
    );
}
#[test]
fn trailing_stop_tracks_favorable_mark_across_snapshot() {
    let name = "trailing-recover";
    let mut r = native_room(name);
    native_bracket(name, &mut r, OrderKind::Market);
    native_set_spec(
        name,
        &mut r,
        exchange_core::PositionProtectionSpec {
            trailing_distance_tick: Some(10),
            ..Default::default()
        },
    );
    r.apply(
        name,
        Command::SetMarkPrice(SetMarkPrice { price_tick: 120 }),
    )
    .unwrap();
    let snapshot = serde_json::to_value(r.simulation_room(name).unwrap()).unwrap();
    let mut restored = RoomManager::new();
    restored
        .restore_simulation_room(serde_json::from_value(snapshot).unwrap(), Vec::new())
        .unwrap();
    restored
        .apply(name, native_order(3, 30, Side::Buy, 111, 5))
        .unwrap();
    restored
        .apply(
            name,
            Command::SetMarkPrice(SetMarkPrice { price_tick: 111 }),
        )
        .unwrap();
    assert_eq!(native_position(name, &restored), 5);
    restored
        .apply(
            name,
            Command::SetMarkPrice(SetMarkPrice { price_tick: 110 }),
        )
        .unwrap();
    assert_eq!(native_position(name, &restored), 0);
    assert_eq!(
        restored
            .room(name)
            .unwrap()
            .position_protections("V-BTC-PERP", 20)[0]
            .triggered_by
            .as_deref(),
        Some("trailing_stop")
    );
}
#[test]
fn native_take_profit_ladder_rearms_and_stop_closes_remaining_position() {
    let name = "profit-ladder";
    let mut r = native_room(name);
    native_bracket(name, &mut r, OrderKind::Market);
    native_set_spec(
        name,
        &mut r,
        exchange_core::PositionProtectionSpec {
            take_profit_steps: vec![
                exchange_core::position_protection::TakeProfitStep {
                    price_tick: 110,
                    qty: 2,
                },
                exchange_core::position_protection::TakeProfitStep {
                    price_tick: 120,
                    qty: 1,
                },
            ],
            stop_loss_tick: Some(90),
            ..Default::default()
        },
    );
    r.apply(name, native_order(3, 30, Side::Buy, 105, 5))
        .unwrap();
    r.apply(
        name,
        Command::SetMarkPrice(SetMarkPrice { price_tick: 110 }),
    )
    .unwrap();
    assert_eq!(native_position(name, &r), 3);
    r.advance_clock(name, 0).unwrap();
    assert_eq!(native_position(name, &r), 3);
    r.apply(
        name,
        Command::SetMarkPrice(SetMarkPrice { price_tick: 120 }),
    )
    .unwrap();
    assert_eq!(native_position(name, &r), 2);
    r.apply(name, Command::SetMarkPrice(SetMarkPrice { price_tick: 90 }))
        .unwrap();
    assert_eq!(native_position(name, &r), 0);
}
#[test]
fn stop_limit_survives_recovery_and_clear_cancels_its_resting_child() {
    let name = "stop-limit-clear";
    let mut r = native_room(name);
    native_bracket(name, &mut r, OrderKind::Market);
    native_set_spec(
        name,
        &mut r,
        exchange_core::PositionProtectionSpec {
            stop_loss_tick: Some(90),
            exit_price_tick: Some(92),
            ..Default::default()
        },
    );
    r.apply(name, Command::SetMarkPrice(SetMarkPrice { price_tick: 90 }))
        .unwrap();
    let p = r.room(name).unwrap().position_protections("V-BTC-PERP", 20)[0].clone();
    let id = p.last_exit_order_id.unwrap();
    assert_eq!(
        r.room(name)
            .unwrap()
            .order_owner_for("V-BTC-PERP", id)
            .unwrap(),
        Some(20)
    );
    let snapshot = serde_json::to_value(r.simulation_room(name).unwrap()).unwrap();
    let mut restored = RoomManager::new();
    restored
        .restore_simulation_room(serde_json::from_value(snapshot).unwrap(), Vec::new())
        .unwrap();
    restored
        .apply(
            name,
            Command::SetPositionProtection {
                account_id: 20,
                position_side: Default::default(),
                protection: None,
            },
        )
        .unwrap();
    assert_eq!(
        restored
            .room(name)
            .unwrap()
            .order_owner_for("V-BTC-PERP", id)
            .unwrap(),
        None
    );
    restored
        .apply(name, native_order(3, 30, Side::Buy, 95, 5))
        .unwrap();
    assert_eq!(native_position(name, &restored), 5);
}
#[test]
fn conditional_entry_fires_once_after_snapshot_and_cancel_removes_child() {
    let name = "conditional-recover";
    let mut r = native_room(name);
    r.apply(
        name,
        Command::SetConditionalOrder {
            account_id: 20,
            key: "entry".into(),
            spec: Some(Box::new(
                exchange_core::conditional_orders::ConditionalOrderSpec {
                    side: Side::Buy,
                    position_side: Default::default(),
                    qty: 2,
                    trigger_price_tick: 110,
                    above: true,
                    trigger: Default::default(),
                    limit_price_tick: Some(95),
                    protection: None,
                },
            )),
        },
    )
    .unwrap();
    let snapshot = serde_json::to_value(r.simulation_room(name).unwrap()).unwrap();
    let mut restored = RoomManager::new();
    restored
        .restore_simulation_room(serde_json::from_value(snapshot).unwrap(), Vec::new())
        .unwrap();
    restored
        .apply(
            name,
            Command::SetMarkPrice(SetMarkPrice { price_tick: 110 }),
        )
        .unwrap();
    // Exchange-owned child is visible in scoped observation, despite its large ID.
    let p = restored
        .room(name)
        .unwrap()
        .conditional_orders("V-BTC-PERP", 20)[0]
        .clone();
    let id = p.submitted_order_id.unwrap();
    assert_eq!(p.status, "submitted");
    restored.advance_clock(name, 0).unwrap();
    assert_eq!(
        restored
            .room(name)
            .unwrap()
            .conditional_orders("V-BTC-PERP", 20)[0]
            .submitted_order_id,
        Some(id)
    );
    restored
        .apply(
            name,
            Command::SetConditionalOrder {
                account_id: 20,
                key: "entry".into(),
                spec: None,
            },
        )
        .unwrap();
    assert_eq!(
        restored
            .room(name)
            .unwrap()
            .order_owner_for("V-BTC-PERP", id)
            .unwrap(),
        None
    );
}
