#[tokio::test]
async fn hedge_http_orders_private_receipts_and_history_keep_both_legs() {
    use exchange_core::model::ProtectedOrderType;
    use exchange_core::{PositionMode, PositionSide};
    let app = new_app();
    let mut scenario = perp_liquidation_scenario_with_mark("hedge-http", 100);
    if let MarketConfig::Perp(config) = &mut scenario.market {
        config.clearing.position_mode = PositionMode::Hedge;
    }
    assert_eq!(
        send_json(&app, Method::POST, "/rooms", None, scenario)
            .await
            .status(),
        StatusCode::OK
    );
    for (account_id, side, leg, kind) in [
        (
            10,
            Side::Sell,
            PositionSide::Short,
            ProtectedOrderType::Limit,
        ),
        (
            20,
            Side::Buy,
            PositionSide::Long,
            ProtectedOrderType::ImmediateOrCancel,
        ),
        (30, Side::Buy, PositionSide::Long, ProtectedOrderType::Limit),
        (
            20,
            Side::Sell,
            PositionSide::Short,
            ProtectedOrderType::ImmediateOrCancel,
        ),
    ] {
        let request = SubmitOrderRequest {
            participant_id: "hedge-user".into(),
            instrument_id: None,
            account_id,
            action: OrderAction::PlaceProtected {
                side,
                position_side: leg,
                qty: 5,
                order_type: kind,
                price_tick: 100,
                reduce_only: false,
                valid_until_market_time_ms: None,
                expires_at_market_time_ms: None,
            },
        };
        let response: OrderResponse = response_json(
            send_json(
                &app,
                Method::POST,
                "/rooms/hedge-http/orders",
                None,
                request,
            )
            .await,
        )
        .await;
        assert!(response.accepted, "{response:?}");
        assert!(
            !response
                .events
                .iter()
                .any(|e| matches!(e, EventSummary::RiskRejected { .. })),
            "{response:?}"
        );
    }
    let positions: RoomPositionsResponse = response_json(
        app.clone()
            .oneshot(
                Request::builder()
                    .uri("/rooms/hedge-http/positions?account_id=20")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    let last = positions.positions.first().unwrap();
    assert_eq!(last.position_qty, 0);
    let p = last.hedge_positions.as_ref().unwrap();
    assert_eq!((p.long.qty, p.short.qty), (5, 5));
    assert_eq!(last.initial_margin, Some(100));
    let events: RoomEventsResponse = response_json(
        app.clone()
            .oneshot(
                Request::builder()
                    .uri("/rooms/hedge-http/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert!(events.executions.iter().flat_map(|e| &e.clearing_events).any(|event| matches!(event,
        ClearingEventSummary::PerpTradeSettled { seller, .. } if seller.account_id == 20
            && seller.hedge_positions.as_ref().is_some_and(|p| p.long.qty == 5 && p.short.qty == 5))));
    let request = SubmitOrderRequest {
        participant_id: "hedge-user".into(),
        instrument_id: None,
        account_id: 20,
        action: OrderAction::PlaceProtected {
            side: Side::Sell,
            position_side: PositionSide::Long,
            qty: 6,
            order_type: ProtectedOrderType::ImmediateOrCancel,
            price_tick: 100,
            reduce_only: true,
            valid_until_market_time_ms: None,
            expires_at_market_time_ms: None,
        },
    };
    let response: OrderResponse = response_json(
        send_json(
            &app,
            Method::POST,
            "/rooms/hedge-http/orders",
            None,
            request,
        )
        .await,
    )
    .await;
    assert!(response.events.iter().any(|e| matches!(e, EventSummary::RiskRejected { reason, .. } if reason == "ReduceOnlyExceedsPosition")));
}

#[test]
fn hedge_journal_recovery_keeps_legs_and_resting_order_selectors() {
    use exchange_core::{PositionMode, PositionSide};
    let mut scenario = perp_liquidation_scenario_with_mark("hedge-recovery", 100);
    if let MarketConfig::Perp(config) = &mut scenario.market {
        config.clearing.position_mode = PositionMode::Hedge;
    }
    let mut rooms = RoomManager::new();
    let bootstrap = rooms.create_room(scenario.clone()).unwrap();
    let mut store = journal::InMemoryJournalStore::new();
    store
        .create_room("owner", &scenario, &bootstrap, &[10, 20, 30], &[], None)
        .unwrap();
    for (id, account_id, side, position_side, kind) in [
        (
            1,
            10,
            Side::Sell,
            PositionSide::Short,
            OrderKind::Limit { price_tick: 100 },
        ),
        (2, 20, Side::Buy, PositionSide::Long, OrderKind::Market),
        (
            3,
            30,
            Side::Buy,
            PositionSide::Long,
            OrderKind::Limit { price_tick: 100 },
        ),
        (4, 20, Side::Sell, PositionSide::Short, OrderKind::Market),
        (
            5,
            20,
            Side::Sell,
            PositionSide::Short,
            OrderKind::Limit { price_tick: 101 },
        ),
    ] {
        let command = Command::NewOrder(NewOrder {
            order_id: id,
            account_id,
            side,
            position_side,
            kind,
            qty: 5,
            reduce_only: false,
        });
        let execution = rooms.apply("hedge-recovery", command.clone()).unwrap();
        store
            .append_executions(
                &[JournalExecution::submitted(
                    "hedge-user".into(),
                    account_id,
                    command,
                    execution,
                )],
                None,
            )
            .unwrap();
    }
    let recovered = recover_rooms(&store.load_recovery().unwrap()).unwrap();
    assert_eq!(
        recovered
            .account_snapshot_for("hedge-recovery", "V-BTC-PERP", 20)
            .unwrap(),
        rooms
            .account_snapshot_for("hedge-recovery", "V-BTC-PERP", 20)
            .unwrap()
    );
    let observation = recovered
        .participant_observation("hedge-recovery", "V-BTC-PERP", 20)
        .unwrap();
    assert_eq!(observation.own_orders[0].position_side, PositionSide::Short);
}
