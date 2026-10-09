fn funding_scenario(room_id: &str, rate: i32, long_cash: i128) -> ScenarioConfig {
    let mut scenario = linked_scenario(room_id);
    let MarketConfig::Perp(perp) = &mut scenario.extra_markets[0] else {
        unreachable!()
    };
    perp.price_link.as_mut().unwrap().max_age_ms = 20000;
    perp.initial_mark_price_tick = 10000;
    perp.clearing.leverage = 10;
    perp.clearing.maker_fee_ppm = 0;
    perp.clearing.taker_fee_ppm = 0;
    perp.funding = Some(exchange_core::FundingConfig {
        interval_ms: 2000,
        base_rate_ppm: rate,
        max_rate_ppm: 200000,
        min_coverage_ppm: 900000,
    });
    for account in &mut scenario.accounts {
        match account {
            ScenarioAccount::Spot { cash_balance, .. } => *cash_balance = 10000000,
            ScenarioAccount::Basic {
                cash_balance,
                account_id: 20,
            } => *cash_balance = long_cash,
            _ => {}
        }
    }
    for command in &mut scenario.seed_orders {
        if let Command::NewOrder(order) = command {
            order.kind = OrderKind::Limit {
                price_tick: if order.side == Side::Buy { 9999 } else { 10001 },
            };
        }
    }
    scenario.routed_seed_orders = [
        (200, 10, Side::Sell, 10000, 1),
        (201, 20, Side::Buy, 10000, 1),
        (202, 10, Side::Buy, 9999, 10),
        (203, 10, Side::Sell, 10001, 10),
    ]
    .into_iter()
    .map(
        |(id, account, side, price, qty)| exchange_core::ScenarioSeedOrder {
            instrument_id: Some("V-BTC-PERP".into()),
            command: Command::NewOrder(NewOrder {
                position_side: Default::default(),
                order_id: id,
                account_id: account,
                side,
                kind: OrderKind::Limit { price_tick: price },
                qty,
                reduce_only: false,
            }),
        },
    )
    .collect();
    scenario
}

#[tokio::test]
async fn funding_http_clock_idempotency_updates_account_ledger_and_public_prices() {
    let app = new_app();
    let scenario = funding_scenario("funding-http", 10000, 100000);
    assert_eq!(
        send_json(&app, Method::POST, "/rooms", None, scenario)
            .await
            .status(),
        StatusCode::OK
    );
    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/funding-http/clock/advance")
                    .header("content-type", "application/json")
                    .header("idempotency-key", "funding-cycle-one")
                    .body(Body::from(r#"{"steps":2}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["clock"]["market_time_ms"], 2000);
    }
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/rooms/funding-http/instruments/V-BTC-PERP/view")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        value["perp_price"]["funding"]["last_settlement"]["rate_ppm"],
        10000
    );
    assert_eq!(value["perp_price"]["funding"]["next_funding_time_ms"], 4000);
    let account = value["accounts"]["Perp"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["account_id"] == 20)
        .unwrap();
    assert_eq!(account["funding_pnl"], -100);
    assert_eq!(account["cash_balance"], 99900);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/rooms/funding-http/ledger?account_id=20")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let funding: Vec<_> = value["ledger"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["account_side"] == "funding")
        .collect();
    assert_eq!(funding.len(), 1);
    assert_eq!(funding[0]["cash_delta"], -100);
    assert_eq!(funding[0]["fee"], 0);
}

#[test]
fn funding_journal_recovery_restores_partial_period_liquidation_and_rejects_tampering() {
    let room_id = "funding-recovery";
    let scenario = funding_scenario(room_id, 100000, 1000);
    let mut rooms = RoomManager::new();
    let bootstrap = rooms.create_room(scenario.clone()).unwrap();
    let mut records: Vec<_> = scenario
        .seed_commands()
        .into_iter()
        .zip(bootstrap.seed_executions)
        .map(|(command, execution)| JournalExecution::seed(command, execution))
        .collect();
    let seed_cursor = rooms.simulation_room(room_id).unwrap().next_command_seq();
    rooms.advance_clock(room_id, 1).unwrap();
    let mid_actor = rooms.simulation_room(room_id).unwrap().clone();
    let mut mutations = vec![JournalMutation {
        room_id: room_id.into(),
        mutation_seq: 1,
        command_cursor: seed_cursor,
        schema_version: journal::ROOM_MUTATION_SCHEMA_VERSION,
        mutation: RoomMutation::ClockAdvanced {
            steps: 1,
            completed_transfers: Vec::new(),
        },
    }];
    let previous = rooms.execution_history(room_id).unwrap().len();
    rooms.advance_clock(room_id, 3).unwrap();
    mutations.push(JournalMutation {
        room_id: room_id.into(),
        mutation_seq: 2,
        command_cursor: seed_cursor,
        schema_version: journal::ROOM_MUTATION_SCHEMA_VERSION,
        mutation: RoomMutation::ClockAdvanced {
            steps: 3,
            completed_transfers: Vec::new(),
        },
    });
    records.extend(
        rooms.execution_history(room_id).unwrap()[previous..]
            .iter()
            .map(|execution| {
                JournalExecution::system(
                    command_from_actor_execution(execution).unwrap(),
                    execution.clone(),
                )
            }),
    );
    let recovery = JournalRecovery {
        rooms: vec![journal::JournalRoom {
            room_id: room_id.into(),
            scenario,
            status: MarketStatus::Running,
        }],
        executions: records,
        mutations,
        ..Default::default()
    };
    let restored = recover_rooms(&recovery).unwrap();
    assert_eq!(
        restored
            .account_snapshots_for(room_id, "V-BTC-PERP")
            .unwrap(),
        rooms.account_snapshots_for(room_id, "V-BTC-PERP").unwrap()
    );
    let exchange_core::AccountSnapshot::Perp(long) = restored
        .account_snapshot_for(room_id, "V-BTC-PERP", 20)
        .unwrap()
        .unwrap()
    else {
        panic!("perp")
    };
    assert_eq!(long.funding_pnl, -1000);
    assert_eq!(long.position_qty, 0);
    let mut suffix = recovery.clone();
    suffix.mutations[0].mutation = RoomMutation::StateCheckpoint {
        actor: Box::new(mid_actor),
        complete_history: true,
    };
    let restored = recover_rooms(&suffix).unwrap();
    assert_eq!(
        restored
            .account_snapshots_for(room_id, "V-BTC-PERP")
            .unwrap(),
        rooms.account_snapshots_for(room_id, "V-BTC-PERP").unwrap()
    );
    let mut tampered = recovery;
    let record = tampered
        .executions
        .iter_mut()
        .find(|record| record.execution.funding_settlement.is_some())
        .unwrap();
    record
        .execution
        .funding_settlement
        .as_mut()
        .unwrap()
        .total_transfer += 1;
    assert!(recover_rooms(&tampered).is_err());
}

#[tokio::test]
async fn funding_streams_keep_account_cashflows_private_and_deliver_to_the_owner() {
    let scenario = funding_scenario("funding-stream", 10000, 100000);
    let mut rooms = RoomManager::new();
    rooms.create_room(scenario).unwrap();
    rooms.advance_clock("funding-stream", 2).unwrap();
    let execution = rooms
        .execution_history("funding-stream")
        .unwrap()
        .iter()
        .find(|execution| execution.funding_settlement.is_some())
        .unwrap()
        .clone();
    let summary = RoomExecutionSummary::from_execution(execution);
    let mut store = journal::InMemoryJournalStore::new();
    store.set_room_member_for_test("funding-stream", "trader", "trader");
    store.set_account_owner_for_test("funding-stream", 20, "trader");
    let journal = JournalCoordinator::new(Box::new(store));
    let public = scoped_event_from_execution(&summary, StreamScope::Public, "trader", &journal, 1)
        .await
        .unwrap();
    assert!(public.payload.get("clearing_events").is_none());
    assert_eq!(public.payload["funding_settlement"]["total_transfer"], 100);
    let private =
        scoped_event_from_execution(&summary, StreamScope::Private, "trader", &journal, 1)
            .await
            .unwrap();
    let events = private.payload["clearing_events"].as_array().unwrap();
    assert!(!events.is_empty());
    assert!(events.iter().all(|event| event["account_id"] == 20));
    assert_eq!(events[0]["cash_delta"], -100);
    assert!(
        scoped_event_from_execution(&summary, StreamScope::Private, "other", &journal, 1)
            .await
            .is_none()
    );
}
