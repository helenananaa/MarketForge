fn linked_scenario(room_id: &str) -> ScenarioConfig {
    let mut scenario = spot_perp_scenario(room_id);
    let MarketConfig::Perp(perp) = &mut scenario.extra_markets[0] else {
        unreachable!()
    };
    perp.price_link = Some(exchange_core::PerpPriceLinkConfig {
        spot_instrument_id: "V-BTC-SPOT".into(),
        max_age_ms: 2000,
    });
    scenario.routed_seed_orders.clear();
    for account in &mut scenario.accounts {
        if let ScenarioAccount::Basic {
            account_id: 20,
            cash_balance,
        } = account
        {
            *cash_balance = 2000;
        }
    }
    scenario.seed_orders = vec![
        Command::NewOrder(NewOrder {
            order_id: 100,
            account_id: 10,
            side: Side::Buy,
            kind: OrderKind::Limit { price_tick: 99 },
            qty: 10,
            reduce_only: false,
        }),
        Command::NewOrder(NewOrder {
            order_id: 101,
            account_id: 10,
            side: Side::Sell,
            kind: OrderKind::Limit { price_tick: 101 },
            qty: 10,
            reduce_only: false,
        }),
    ];
    scenario
}

#[tokio::test]
async fn price_link_http_exposes_prices_in_view_ticker_observation_and_receipts() {
    let app = new_app();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/rooms")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&linked_scenario("linked-http")).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    for uri in [
        "/rooms/linked-http/instruments/V-BTC-PERP/view",
        "/rooms/linked-http/instruments/V-BTC-PERP/ticker",
        "/rooms/linked-http/observe?instrument_id=V-BTC-PERP&account_id=20",
    ] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let value = value.get("observation").unwrap_or(&value);
        assert_eq!(
            value["perp_price"]["index_price_tick"], 100,
            "{uri}: {value}"
        );
        assert_eq!(value["perp_price"]["mark_price_tick"], 100);
        assert_eq!(value["perp_price"]["status"], "live");
    }
    let response = app.clone().oneshot(Request::builder().method(Method::POST).uri("/rooms/linked-http/instruments/V-BTC-SPOT/orders")
        .header("content-type", "application/json").body(Body::from(serde_json::json!({"participant_id": "index-trader", "account_id": 10, "action": {"Cancel": {"order_id": 100}}}).to_string())).unwrap()).await.unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let response: OrderResponse = serde_json::from_slice(&body).unwrap();
    assert!(response.accepted);
    assert_eq!(
        response.price_updates[0].status,
        exchange_core::PriceLinkStatus::Unavailable
    );
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/rooms/linked-http/instruments/V-BTC-PERP/mark-price")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"price_tick":200}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let response: RoomExecutionSummary = serde_json::from_slice(&body).unwrap();
    assert!(!response.accepted);
    assert!(
        response
            .reject_reason
            .unwrap()
            .contains("managed by its spot index")
    );
}

#[test]
fn price_link_journal_recovery_verifies_derived_receipts_and_snapshot_suffix() {
    let room_id = "linked-recovery";
    let scenario = linked_scenario(room_id);
    let mut rooms = RoomManager::new();
    let bootstrap = rooms.create_room(scenario.clone()).unwrap();
    let mut records: Vec<_> = scenario
        .seed_commands()
        .into_iter()
        .zip(bootstrap.seed_executions)
        .map(|(command, execution)| JournalExecution::seed(command, execution))
        .collect();
    let snapshot = current_room_snapshot(&rooms, room_id, 1).unwrap();
    let command = Command::NewOrder(NewOrder {
        order_id: 102,
        account_id: 20,
        side: Side::Buy,
        kind: OrderKind::Limit { price_tick: 101 },
        qty: 10,
        reduce_only: false,
    });
    let execution = rooms
        .apply_to_instrument(room_id, "V-BTC-SPOT", command.clone())
        .unwrap();
    assert!(!execution.price_updates.is_empty());
    records.push(JournalExecution::submitted(
        "spot-buyer".into(),
        20,
        command,
        execution,
    ));
    let recovery = JournalRecovery {
        rooms: vec![journal::JournalRoom {
            room_id: room_id.into(),
            scenario,
            status: MarketStatus::Running,
        }],
        executions: records,
        ..JournalRecovery::default()
    };
    let expected = rooms
        .simulation_room(room_id)
        .unwrap()
        .perp_price_snapshot("V-BTC-PERP")
        .unwrap();
    let restored = recover_rooms(&recovery).unwrap();
    assert_eq!(
        restored
            .simulation_room(room_id)
            .unwrap()
            .perp_price_snapshot("V-BTC-PERP")
            .unwrap(),
        expected
    );
    let mut suffix = recovery.clone();
    suffix.snapshots.push(snapshot);
    let restored = recover_rooms(&suffix).unwrap();
    assert_eq!(
        restored
            .simulation_room(room_id)
            .unwrap()
            .perp_price_snapshot("V-BTC-PERP")
            .unwrap(),
        expected
    );
    let mut tampered = recovery;
    tampered
        .executions
        .last_mut()
        .unwrap()
        .execution
        .price_updates[0]
        .mark_price_tick = 777;
    assert!(recover_rooms(&tampered).is_err());
}

#[test]
fn price_link_journal_recovery_replays_spot_triggered_liquidation() {
    let room_id = "linked-liquidation-recovery";
    let mut scenario = linked_scenario(room_id);
    for account in &mut scenario.accounts {
        match account {
            ScenarioAccount::Spot {
                account_id: 10,
                cash_balance,
                ..
            } => *cash_balance = 100000,
            ScenarioAccount::Basic {
                account_id: 20,
                cash_balance,
            } => *cash_balance = 10,
            _ => {}
        }
    }
    let mut rooms = RoomManager::new();
    let bootstrap = rooms.create_room(scenario.clone()).unwrap();
    let mut records: Vec<_> = scenario
        .seed_commands()
        .into_iter()
        .zip(bootstrap.seed_executions)
        .map(|(command, execution)| JournalExecution::seed(command, execution))
        .collect();
    let limit = |id, account, side, price, qty| {
        Command::NewOrder(NewOrder {
            order_id: id,
            account_id: account,
            side,
            kind: OrderKind::Limit { price_tick: price },
            qty,
            reduce_only: false,
        })
    };
    for (instrument, command) in [
        ("V-BTC-PERP", limit(200, 10, Side::Sell, 100, 1)),
        ("V-BTC-PERP", limit(201, 20, Side::Buy, 100, 1)),
        ("V-BTC-PERP", limit(202, 10, Side::Buy, 80, 10)),
        (
            "V-BTC-SPOT",
            Command::CancelOrder(CancelOrder { order_id: 100 }),
        ),
        (
            "V-BTC-SPOT",
            Command::CancelOrder(CancelOrder { order_id: 101 }),
        ),
        ("V-BTC-SPOT", limit(203, 10, Side::Buy, 79, 10)),
        ("V-BTC-SPOT", limit(204, 10, Side::Sell, 81, 10)),
    ] {
        let before = rooms.execution_history(room_id).unwrap().len();
        let execution = rooms
            .apply_to_instrument(room_id, instrument, command.clone())
            .unwrap();
        assert!(
            matches!(execution.result, ActorExecutionResult::Accepted(_)),
            "{execution:?}"
        );
        let account = match &command {
            Command::NewOrder(order) => order.account_id,
            _ => 10,
        };
        records.push(JournalExecution::submitted(
            "operator".into(),
            account,
            command,
            execution,
        ));
        for automatic in rooms
            .execution_history(room_id)
            .unwrap()
            .iter()
            .skip(before + 1)
        {
            records.push(JournalExecution::system(
                command_from_actor_execution(automatic).unwrap(),
                automatic.clone(),
            ));
        }
    }
    assert!(
        records.iter().any(|record| record
            .execution
            .clearing_events
            .iter()
            .any(|event| matches!(
                event,
                ClearingEventSummary::PerpLiquidationSettled { account_id: 20, .. }
            )))
    );
    let recovery = JournalRecovery {
        rooms: vec![journal::JournalRoom {
            room_id: room_id.into(),
            scenario,
            status: MarketStatus::Running,
        }],
        executions: records,
        ..JournalRecovery::default()
    };
    let restored = recover_rooms(&recovery).unwrap();
    assert_eq!(
        restored
            .account_snapshots_for(room_id, "V-BTC-PERP")
            .unwrap(),
        rooms.account_snapshots_for(room_id, "V-BTC-PERP").unwrap()
    );
    assert_eq!(
        restored.book_snapshot_for(room_id, "V-BTC-PERP").unwrap(),
        rooms.book_snapshot_for(room_id, "V-BTC-PERP").unwrap()
    );
    assert_eq!(
        restored
            .simulation_room(room_id)
            .unwrap()
            .perp_price_snapshot("V-BTC-PERP")
            .unwrap()
            .unwrap()
            .mark_price_tick,
        80
    );
}
