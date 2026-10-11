mod market_events_tests {
    use super::*;

    async fn post(app: &axum::Router, uri: &str, body: serde_json::Value) {
        let response = send_json(app, Method::POST, uri, None, body).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
    }

    async fn get(
        app: &axum::Router,
        uri: &str,
        user: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let mut request = Request::builder().uri(uri);
        if let Some(user) = user {
            request = request.header(USER_ID_HEADER, user);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap_or_default())
    }

    #[tokio::test]
    async fn market_events_partial_fills_filter_privacy_pagination_and_recovery() {
        let journal = journal::SharedInMemoryJournalStore::new();
        let app = recovering_app(Box::new(journal.clone()));
        let scenario = perp_liquidation_scenario_with_mark("event-liquidation", 100);
        post(&app, "/rooms", serde_json::to_value(scenario).unwrap()).await;
        for (account, side, price, qty) in [
            (10, "Sell", 100, 10),
            (20, "Buy", 100, 10),
            (30, "Buy", 70, 4),
        ] {
            post(
                &app,
                "/rooms/event-liquidation/orders",
                serde_json::json!({"participant_id":"test","account_id":account,
                "action":{"PlaceLimit":{"side":side,"price_tick":price,"qty":qty}}}),
            )
            .await;
        }
        assign_trader(&app, "event-liquidation", "reader", 10).await;
        post(
            &app,
            "/rooms/event-liquidation/instruments/V-BTC-PERP/mark-price",
            serde_json::json!({"price_tick":70}),
        )
        .await;
        let base = "/rooms/event-liquidation/instruments/V-BTC-PERP/market-events";
        let (status, all) = get(
            &app,
            &format!("{base}?from_start=true&event_type=liquidation&min_notional=280&side=Sell"),
            Some("reader"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(all["events"].as_array().unwrap().len(), 1);
        let event = &all["events"][0]["event"];
        assert_eq!(event["type"], "LiquidationTrade");
        assert_eq!(event["qty"], 4);
        assert_eq!(event["notional"], "280");
        for forbidden in [
            "account_id",
            "order_id",
            "clearing_events",
            "cash_balance",
            "wallet_updates",
        ] {
            assert!(!all.to_string().contains(forbidden));
        }
        let (_, empty) = get(
            &app,
            &format!("{base}?from_start=true&event_type=liquidation&min_notional=281"),
            Some("reader"),
        )
        .await;
        assert_eq!(empty["events"], serde_json::json!([]));
        assert_eq!(
            empty["next_after_command_seq"],
            all["next_after_command_seq"]
        );
        let (_, wrong_side) = get(
            &app,
            &format!("{base}?from_start=true&event_type=liquidation&side=Buy"),
            Some("reader"),
        )
        .await;
        assert_eq!(wrong_side["events"], serde_json::json!([]));
        let analysis = "/rooms/event-liquidation/instruments/V-BTC-PERP";
        let (status, flow) = get(
            &app,
            &format!("{analysis}/order-flow?window_ms=30000&large_trade_min_notional=281"),
            Some("reader"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(flow["complete"], true, "{flow}");
        assert_eq!(flow["metrics"]["trade_count"], 2);
        assert_eq!(flow["metrics"]["quote_volume"], "1280");
        assert_eq!(flow["metrics"]["delta_qty"], "6");
        assert_eq!(flow["metrics"]["liquidation_notional"], "280");
        assert_eq!(flow["metrics"]["long_liquidation_notional"], "280");
        assert_eq!(flow["metrics"]["short_liquidation_notional"], "0");
        assert_eq!(flow["metrics"]["large_trade_count"], 1);
        assert_eq!(
            flow["metrics"]["vwap_tick"],
            serde_json::json!({"numerator":"1280","denominator":"14"})
        );
        assert_eq!(flow["buckets"][0]["cvd_qty"], "6");
        assert_eq!(flow["volume_profile"].as_array().unwrap().len(), 2);
        assert_eq!(flow["volume_profile"][0]["delta_qty"], "-4");
        assert_eq!(flow["volume_profile"][0]["price_from_tick"], 70);
        assert!(!flow.to_string().contains("account_id"));
        assert_eq!(
            get(&app, &format!("{analysis}/window-stats"), Some("outsider"))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        let (_, capped) = get(
            &app,
            &format!("{analysis}/window-stats?scan_limit=1"),
            Some("reader"),
        )
        .await;
        assert_eq!(capped["complete"], false);
        assert_eq!(capped["truncated"], true);
        for query in [
            "window_ms=0",
            "scan_limit=0",
            "window_ms=512&bucket_ms=1",
            "book_levels=101",
            "price_group_tick=0",
            "account_id=20",
            "end_market_time_ms=9999",
        ] {
            assert_eq!(
                get(
                    &app,
                    &format!("{analysis}/window-stats?{query}"),
                    Some("reader")
                )
                .await
                .0,
                StatusCode::BAD_REQUEST,
                "{query}"
            );
        }
        let mut after = 0;
        let mut collected = Vec::new();
        let mut empty_pages = 0;
        loop {
            let (status, page) = get(
                &app,
                &format!("{base}?after_command_seq={after}&event_type=liquidation&limit=1"),
                Some("reader"),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            if page["events"].as_array().unwrap().is_empty() {
                empty_pages += 1;
            }
            collected.extend(page["events"].as_array().unwrap().iter().cloned());
            after = page["next_after_command_seq"].as_u64().unwrap();
            if page["has_more"] == false {
                break;
            }
        }
        assert!(empty_pages > 0);
        assert_eq!(serde_json::json!(collected), all["events"]);
        assert_eq!(
            get(&app, base, Some("outsider")).await.0,
            StatusCode::FORBIDDEN
        );
        let recovered = recovering_app(Box::new(journal));
        let (_, replay) = get(
            &recovered,
            &format!("{base}?from_start=true&event_type=liquidation"),
            Some("reader"),
        )
        .await;
        assert_eq!(replay["events"], all["events"]);
        let (_, tail) = get(
            &recovered,
            &format!("{base}?after_command_seq={after}&event_type=liquidation"),
            Some("reader"),
        )
        .await;
        assert_eq!(tail["events"], serde_json::json!([]));
        assert_eq!(tail["next_after_command_seq"], after);
        let (_, recovered_flow) = get(
            &recovered,
            &format!("{analysis}/order-flow?large_trade_min_notional=281"),
            Some("reader"),
        )
        .await;
        assert_eq!(recovered_flow["metrics"], flow["metrics"]);
        post(
            &recovered,
            "/rooms/event-liquidation/clock/advance",
            serde_json::json!({"steps":2}),
        )
        .await;
        let (_, expired) = get(
            &recovered,
            &format!("{analysis}/window-stats?window_ms=1"),
            Some("reader"),
        )
        .await;
        assert_eq!(expired["metrics"]["liquidation_notional"], "0");
        let (_, historical) = get(
            &recovered,
            &format!("{analysis}/order-flow?end_market_time_ms=0"),
            Some("reader"),
        )
        .await;
        assert_eq!(historical["metrics"]["liquidation_notional"], "280");
        assert_eq!(historical["book"], serde_json::Value::Null);
        assert!(historical["metrics"].get("book_imbalance_ppm").is_none());
    }

    #[tokio::test]
    async fn market_events_signed_funding_time_range_and_price_history() {
        let app = new_app();
        post(
            &app,
            "/rooms",
            serde_json::to_value(funding_scenario("event-funding", -10000, 100000)).unwrap(),
        )
        .await;
        post(
            &app,
            "/rooms/event-funding/clock/advance",
            serde_json::json!({"steps":4}),
        )
        .await;
        let base = "/rooms/event-funding/instruments/V-BTC-PERP/market-events";
        let (status, history) = get(&app, &format!("{base}?from_start=true&event_type=funding&max_rate_ppm=-10000&start_market_time_ms=2000&end_market_time_ms=2000"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(history["events"].as_array().unwrap().len(), 1, "{history}");
        let event = &history["events"][0]["event"];
        assert_eq!(event["rate_ppm"], -10000);
        assert_eq!(event["funding_time_ms"], 2000);
        assert_eq!(event["total_transfer"], "100");
        let (_, stats) = get(&app, "/rooms/event-funding/instruments/V-BTC-PERP/window-stats?end_market_time_ms=2000&window_ms=1", None).await;
        assert_eq!(stats["complete"], true);
        assert_eq!(stats["metrics"]["funding_receipt_count"], 1);
        assert_eq!(stats["metrics"]["funding_settled_count"], 1);
        assert_eq!(stats["metrics"]["funding_total_transfer"], "100");
        assert_eq!(stats["metrics"]["last_funding_rate_ppm"], -10000);
        assert!(!history.to_string().contains("account_id"));
        let (_, excluded) = get(
            &app,
            &format!("{base}?from_start=true&event_type=funding&min_rate_ppm=0"),
            None,
        )
        .await;
        assert_eq!(excluded["events"], serde_json::json!([]));
        let (_, prices) = get(
            &app,
            &format!("{base}?from_start=true&event_type=price_update"),
            None,
        )
        .await;
        assert!(!prices["events"].as_array().unwrap().is_empty());
        assert!(
            prices["events"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["event"]["instrument_id"] == "V-BTC-PERP")
        );
        for query in [
            "event_type=oops",
            "event_type=funding&min_notional=1",
            "event_type=liquidation&min_notional=-1",
            "event_type=liquidation&min_notional=170141183460469231731687303715884105728",
            "event_type=funding&min_rate_ppm=2&max_rate_ppm=1",
            "from_start=true&after_command_seq=0",
            "limit=0",
            "limit=501",
            "start_market_time_ms=2&end_market_time_ms=1",
            "account_id=20",
        ] {
            assert_eq!(
                get(&app, &format!("{base}?{query}"), None).await.0,
                StatusCode::BAD_REQUEST,
                "{query}"
            );
        }
        assert_eq!(
            get(
                &app,
                "/rooms/event-funding/instruments/OTHER/market-events",
                None
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            get(&app, &format!("{base}?after_command_seq=999999"), None)
                .await
                .0,
            StatusCode::CONFLICT
        );
    }
}
