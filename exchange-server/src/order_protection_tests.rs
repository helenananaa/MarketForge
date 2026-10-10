fn protected_request(
    price: i64,
    order_type: exchange_core::model::ProtectedOrderType,
    valid_until: Option<u64>,
    expires: Option<u64>,
) -> SubmitOrderRequest {
    SubmitOrderRequest {
        participant_id: "protected-trader".into(),
        instrument_id: None,
        account_id: 20,
        action: OrderAction::PlaceProtected {
            position_side: Default::default(),
            side: Side::Buy,
            qty: 2,
            order_type,
            price_tick: price,
            reduce_only: false,
            valid_until_market_time_ms: valid_until,
            expires_at_market_time_ms: expires,
        },
    }
}

#[tokio::test]
async fn protection_http_deadline_and_retry_preserve_the_original_receipt() {
    use exchange_core::model::ProtectedOrderType::ImmediateOrCancel;
    let app = new_app();
    assert_eq!(
        send_json(
            &app,
            Method::POST,
            "/rooms",
            None,
            spot_scenario("protected-http")
        )
        .await
        .status(),
        StatusCode::OK
    );
    let intent = protected_request(101, ImmediateOrCancel, Some(1000), None);
    let first = send_json_with_key(
        &app,
        "/rooms/protected-http/orders",
        None,
        Some("original-intent"),
        &intent,
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    let first: OrderResponse = response_json(first).await;
    assert!(first.accepted);
    assert_eq!(
        send_json(
            &app,
            Method::POST,
            "/rooms/protected-http/clock/advance",
            None,
            AdvanceClockRequest { steps: 1 }
        )
        .await
        .status(),
        StatusCode::OK
    );
    let retry: OrderResponse = response_json(
        send_json_with_key(
            &app,
            "/rooms/protected-http/orders",
            None,
            Some("original-intent"),
            &intent,
        )
        .await,
    )
    .await;
    assert_eq!(retry.command_seq, first.command_seq);
    assert!(retry.accepted);
    let stale: OrderResponse = response_json(
        send_json_with_key(
            &app,
            "/rooms/protected-http/orders",
            None,
            Some("late-new-intent"),
            &intent,
        )
        .await,
    )
    .await;
    assert!(!stale.accepted);
    assert!(
        stale
            .reject_reason
            .unwrap()
            .contains("order protection expired")
    );
    let changed = protected_request(101, ImmediateOrCancel, Some(5000), None);
    assert_eq!(
        send_json_with_key(
            &app,
            "/rooms/protected-http/orders",
            None,
            Some("original-intent"),
            &changed
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn protection_http_expiry_updates_orders_events_and_reserved_funds() {
    use exchange_core::model::ProtectedOrderType::Limit;
    let app = new_app();
    send_json(
        &app,
        Method::POST,
        "/rooms",
        None,
        spot_scenario("protected-expiry"),
    )
    .await;
    let result: OrderResponse = response_json(
        send_json_with_key(
            &app,
            "/rooms/protected-expiry/orders",
            None,
            Some("resting-intent"),
            protected_request(95, Limit, Some(1000), Some(2000)),
        )
        .await,
    )
    .await;
    assert!(result.accepted);
    let id = result
        .events
        .iter()
        .find_map(|event| match event {
            EventSummary::OrderAccepted { order_id, .. } => Some(*order_id),
            _ => None,
        })
        .unwrap();
    let clock = send_json(
        &app,
        Method::POST,
        "/rooms/protected-expiry/clock/advance",
        None,
        AdvanceClockRequest { steps: 2 },
    )
    .await;
    assert_eq!(clock.status(), StatusCode::OK);
    let orders = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/rooms/protected-expiry/orders")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let orders: serde_json::Value = response_json(orders).await;
    assert_eq!(orders["orders"][0]["status"], "expired");
    // History retains the unfilled quantity; expired orders are absent from the live book.
    assert_eq!(orders["orders"][0]["remaining_qty"], 2);
    let events = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/rooms/protected-expiry/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let events: RoomEventsResponse = response_json(events).await;
    assert!(
        events
            .executions
            .iter()
            .any(|execution| execution.market_time_ms == Some(2000)
                && execution.events.iter().any(|event| matches!(event,
            EventSummary::OrderExpired { order_id, unfilled_qty: 2, .. } if *order_id == id)))
    );
    let observed = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/rooms/protected-expiry/observe?account_id=20&instrument_id=V-BTC-SPOT")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(observed.status(), StatusCode::OK);
    let observed: serde_json::Value = response_json(observed).await;
    assert!(
        observed["observation"]["own_orders"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        observed["observation"]["own_account"]["Spot"]["reserved_cash"],
        0
    );
}

#[test]
fn protection_journal_recovery_replays_automatic_expiry_once() {
    use exchange_core::model::ProtectedOrderType::Limit;
    let scenario = spot_scenario("protected-recovery");
    let mut rooms = RoomManager::new();
    let bootstrap = rooms.create_room(scenario.clone()).unwrap();
    let mut store = journal::InMemoryJournalStore::new();
    store
        .create_room("owner", &scenario, &bootstrap, &[10, 20], &[], None)
        .unwrap();
    let request = protected_request(95, Limit, Some(1000), Some(2000));
    let execution = OrderGateway::new(&mut rooms, 1)
        .submit_action(GatewayRequest {
            participant_id: request.participant_id,
            room_id: scenario.room_id.clone(),
            instrument_id: None,
            account_id: request.account_id,
            action: request.action,
        })
        .unwrap();
    let record = JournalExecution::submitted(
        execution.participant_id,
        execution.account_id,
        execution.command,
        execution.execution,
    );
    store.append_executions(&[record], None).unwrap();
    let before_clock_cursor = rooms
        .simulation_room(&scenario.room_id)
        .unwrap()
        .next_command_seq();
    rooms.advance_clock(&scenario.room_id, 2).unwrap();
    let expiry = rooms
        .execution_history(&scenario.room_id)
        .unwrap()
        .last()
        .unwrap()
        .clone();
    let record = JournalExecution::system(command_from_actor_execution(&expiry).unwrap(), expiry);
    store
        .append_room_mutation(
            &PendingJournalMutation::new(
                &scenario.room_id,
                before_clock_cursor,
                RoomMutation::ClockAdvanced {
                    steps: 2,
                    completed_transfers: vec![],
                },
            ),
            &[record],
            &[],
            None,
        )
        .unwrap();
    let mut recovered = recover_rooms(&store.load_recovery().unwrap()).unwrap();
    assert_eq!(
        recovered.book_snapshot(&scenario.room_id).unwrap(),
        rooms.book_snapshot(&scenario.room_id).unwrap()
    );
    assert_eq!(
        recovered
            .execution_history(&scenario.room_id)
            .unwrap()
            .len(),
        2
    );
    let count = recovered
        .execution_history(&scenario.room_id)
        .unwrap()
        .len();
    recovered.advance_clock(&scenario.room_id, 1).unwrap();
    assert_eq!(
        recovered
            .execution_history(&scenario.room_id)
            .unwrap()
            .len(),
        count
    );
}

#[tokio::test]
async fn protection_unbounded_market_sweeps_multiple_levels_and_obeys_cash_limits() {
    let app = new_app();
    let mut scenario = spot_scenario("unbounded-http");
    scenario.seed_orders = [(1, 120), (2, 200)]
        .into_iter()
        .map(|(id, price)| {
            Command::NewOrder(NewOrder {
                position_side: Default::default(),
                order_id: id,
                account_id: 10,
                side: Side::Sell,
                qty: 1,
                kind: OrderKind::Limit { price_tick: price },
                reduce_only: false,
            })
        })
        .collect();
    assert_eq!(
        send_json(&app, Method::POST, "/rooms", None, scenario)
            .await
            .status(),
        StatusCode::OK
    );
    let sweep = |qty| SubmitOrderRequest {
        participant_id: "sweeper".into(),
        account_id: 20,
        instrument_id: None,
        action: OrderAction::PlaceUnboundedMarket {
            position_side: Default::default(),
            side: Side::Buy,
            qty,
            reduce_only: false,
            valid_until_market_time_ms: Some(5000),
        },
    };
    let result: OrderResponse = response_json(
        send_json(
            &app,
            Method::POST,
            "/rooms/unbounded-http/orders",
            None,
            sweep(3),
        )
        .await,
    )
    .await;
    assert!(result.accepted);
    assert_eq!(
        result
            .events
            .iter()
            .filter_map(|event| match event {
                EventSummary::TradePrinted {
                    price_tick, qty, ..
                } => Some((*price_tick, *qty)),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![(120, 1), (200, 1)]
    );
    assert!(result.events.iter().any(|event| matches!(
        event,
        EventSummary::OrderExpired {
            unfilled_qty: 1,
            ..
        }
    )));

    let maker = SubmitOrderRequest {
        participant_id: "maker".into(),
        account_id: 10,
        instrument_id: None,
        action: OrderAction::PlaceLimit {
            side: Side::Sell,
            price_tick: 800,
            qty: 1,
        },
    };
    send_json(
        &app,
        Method::POST,
        "/rooms/unbounded-http/orders",
        None,
        maker,
    )
    .await;
    let cash_reject: OrderResponse = response_json(
        send_json(
            &app,
            Method::POST,
            "/rooms/unbounded-http/orders",
            None,
            sweep(1),
        )
        .await,
    )
    .await;
    assert!(cash_reject.events.iter().any(|event| matches!(event,
        EventSummary::RiskRejected { reason, .. } if reason.contains("InsufficientCash"))));
    assert!(
        !cash_reject
            .events
            .iter()
            .any(|event| matches!(event, EventSummary::TradePrinted { .. }))
    );
}

#[tokio::test]
async fn protection_unbounded_market_deadline_is_checked_by_the_exchange() {
    let app = new_app();
    let mut scenario = spot_scenario("unbounded-deadline");
    scenario.seed_orders = vec![Command::NewOrder(NewOrder {
        position_side: Default::default(),
        order_id: 1,
        account_id: 10,
        side: Side::Sell,
        qty: 1,
        kind: OrderKind::Limit { price_tick: 100 },
        reduce_only: false,
    })];
    send_json(&app, Method::POST, "/rooms", None, scenario).await;
    send_json(
        &app,
        Method::POST,
        "/rooms/unbounded-deadline/clock/advance",
        None,
        AdvanceClockRequest { steps: 1 },
    )
    .await;
    let request = SubmitOrderRequest {
        participant_id: "sweeper".into(),
        account_id: 20,
        instrument_id: None,
        action: OrderAction::PlaceUnboundedMarket {
            position_side: Default::default(),
            side: Side::Buy,
            qty: 1,
            reduce_only: false,
            valid_until_market_time_ms: Some(1000),
        },
    };
    let stale: OrderResponse = response_json(
        send_json(
            &app,
            Method::POST,
            "/rooms/unbounded-deadline/orders",
            None,
            &request,
        )
        .await,
    )
    .await;
    assert!(!stale.accepted);
    assert!(
        stale
            .reject_reason
            .unwrap()
            .contains("order protection expired")
    );
    let mut fresh = request;
    if let OrderAction::PlaceUnboundedMarket {
        valid_until_market_time_ms,
        ..
    } = &mut fresh.action
    {
        *valid_until_market_time_ms = None;
    }
    let result: OrderResponse = response_json(
        send_json(
            &app,
            Method::POST,
            "/rooms/unbounded-deadline/orders",
            None,
            fresh,
        )
        .await,
    )
    .await;
    assert!(result.accepted);
    assert!(result.events.iter().any(|event| matches!(
        event,
        EventSummary::TradePrinted {
            price_tick: 100,
            qty: 1,
            ..
        }
    )));
}
