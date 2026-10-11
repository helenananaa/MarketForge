mod event_output_tests {
    use super::*;

    async fn open_stream(
        app: &axum::Router,
        room: &str,
        scope: &str,
        query: &str,
        user: Option<&str>,
    ) -> axum::http::Response<Body> {
        let mut request = Request::builder().uri(format!("/rooms/{room}/stream/{scope}{query}"));
        if let Some(user) = user {
            request = request.header(USER_ID_HEADER, user);
        }
        app.clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    async fn post(app: &axum::Router, uri: &str, value: serde_json::Value) -> serde_json::Value {
        let response = send_json(app, Method::POST, uri, None, value).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        json_body(
            axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
    }

    fn place(account: u64, side: &str, price: i64, qty: u64) -> serde_json::Value {
        serde_json::json!({"participant_id":"stream-test","account_id":account,
            "action":{"PlaceLimit":{"side":side,"price_tick":price,"qty":qty}}})
    }

    fn apply_depth(book: &mut BookSnapshot, delta: &serde_json::Value) {
        let delta: BookSnapshot = serde_json::from_value(delta.clone()).unwrap();
        for (levels, changes, reverse) in [
            (&mut book.bids, delta.bids, true),
            (&mut book.asks, delta.asks, false),
        ] {
            for change in changes {
                levels.retain(|level| level.price_tick != change.price_tick);
                if change.qty != 0 {
                    levels.push(change);
                }
            }
            levels.sort_by_key(|level| level.price_tick);
            if reverse {
                levels.reverse();
            }
        }
    }

    #[tokio::test]
    async fn public_complete_tape_and_depth_reconstruct_authority_without_private_ids() {
        let journal = journal::SharedInMemoryJournalStore::new();
        let app = recovering_app(Box::new(journal.clone()));
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms",
                None,
                spot_scenario("output-book")
            )
            .await
            .status(),
            StatusCode::OK
        );
        let opened = open_stream(&app, "output-book", "public", "", None).await;
        let mut stream = opened.into_body().into_data_stream();
        let initial = collect_sse_from_stream(&mut stream, 1).await;
        let mut book: BookSnapshot =
            serde_json::from_value(initial[0]["payload"]["book"].clone()).unwrap();
        let first = post(&app, "/rooms/output-book/orders", place(10, "Sell", 100, 3)).await;
        let second = post(&app, "/rooms/output-book/orders", place(10, "Sell", 101, 2)).await;
        let buy = post(&app, "/rooms/output-book/orders", place(20, "Buy", 101, 4)).await;
        let order = first_order_id(&second).unwrap();
        post(&app, "/rooms/output-book/orders", serde_json::json!({"participant_id":"stream-test","account_id":10,"action":{"Amend":{"order_id":order,"price_tick":102,"qty":1}}})).await;
        post(&app, "/rooms/output-book/orders", serde_json::json!({"participant_id":"stream-test","account_id":10,"action":{"Cancel":{"order_id":order}}})).await;
        let frames = collect_sse_from_stream(&mut stream, 5).await;
        assert_eq!(frames.len(), 5);
        let mut trades = Vec::new();
        for frame in &frames {
            let payload = &frame["payload"];
            assert!(payload["book_delta_available"].as_bool().unwrap());
            apply_depth(&mut book, &payload["book_delta"]);
            trades.extend(payload["trades"].as_array().unwrap().iter().cloned());
            let encoded = payload.to_string();
            for forbidden in [
                "account_id",
                "order_id",
                "cash_balance",
                "clearing_events",
                "wallet_updates",
            ] {
                assert!(!encoded.contains(forbidden), "{encoded}");
            }
        }
        assert_eq!(trades.len(), 2);
        let filled = buy["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["type"] == "TradePrinted")
            .collect::<Vec<_>>();
        assert_eq!(filled.len(), trades.len());
        for (actual, public) in filled.iter().zip(&trades) {
            for key in ["trade_id", "price_tick", "qty", "taker_side"] {
                assert_eq!(actual[key], public[key]);
            }
        }
        let authority = get_json(&app, "/rooms/output-book/book", None).await;
        assert_eq!(serde_json::to_value(&book).unwrap(), authority);
        let cursor = first["command_seq"].as_u64().unwrap();
        let recovered = recovering_app(Box::new(journal));
        let replay = collect_sse_events(
            open_stream(
                &recovered,
                "output-book",
                "public",
                &format!("?after_command_seq={cursor}"),
                None,
            )
            .await,
            4,
        )
        .await;
        assert_eq!(replay.len(), 4);
        for (live, replayed) in frames[1..].iter().zip(&replay) {
            assert_eq!(live["payload"], replayed["payload"]);
        }
    }

    #[tokio::test]
    async fn private_trade_wallets_and_reservations_never_include_counterparty_accounts() {
        let app = new_app();
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms",
                None,
                spot_scenario("output-private")
            )
            .await
            .status(),
            StatusCode::OK
        );
        assign_trader(&app, "output-private", "alice", 20).await;
        let opened = open_stream(&app, "output-private", "private", "", Some("alice")).await;
        let mut stream = opened.into_body().into_data_stream();
        let snapshot = collect_sse_from_stream(&mut stream, 1).await;
        assert!(
            snapshot[0]["payload"]["account_states"]
                .as_array()
                .unwrap()
                .iter()
                .all(|a| a["account"]["account_id"] == 20)
        );
        post(
            &app,
            "/rooms/output-private/orders",
            place(20, "Buy", 90, 1),
        )
        .await;
        post(
            &app,
            "/rooms/output-private/orders",
            place(10, "Sell", 90, 1),
        )
        .await;
        let frames = collect_sse_from_stream(&mut stream, 2).await;
        assert_eq!(frames.len(), 2);
        let rest = &frames[0]["payload"]["wallet_changes"][0];
        assert_eq!(rest["account_id"], 20);
        assert!(
            rest["balances"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["reserved"] == "90")
        );
        assert_eq!(frames[1]["payload"]["trades"][0]["account_id"], 20);
        let clearing = &frames[1]["payload"]["clearing_events"];
        assert!(
            clearing
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v["account_id"] == 20)
        );
        let encoded = frames[1]["payload"].to_string();
        for forbidden in [
            "buyer_account_id",
            "seller_account_id",
            "maker_account_id",
            "taker_account_id",
            "\"account_id\":10",
        ] {
            assert!(!encoded.contains(forbidden), "{encoded}");
        }
    }

    #[tokio::test]
    async fn transfer_wallet_cursor_survives_delays_pagination_and_restart() {
        let journal = journal::SharedInMemoryJournalStore::new();
        let app = recovering_app(Box::new(journal.clone()));
        let mut scenario = spot_scenario("output-wallet");
        scenario.venue_rules.transfers.deposit_delay_steps = 2;
        scenario.initial_portfolios.push(ScenarioPortfolio {
            account_id: 20,
            balances: BTreeMap::from([("BTC".into(), 500)]),
        });
        assert_eq!(
            send_json(&app, Method::POST, "/rooms", None, scenario)
                .await
                .status(),
            StatusCode::OK
        );
        assign_trader(&app, "output-wallet", "alice", 20).await;
        let opened = open_stream(
            &app,
            "output-wallet",
            "private",
            "?wallet_cursor=0",
            Some("alice"),
        )
        .await;
        let mut stream = opened.into_body().into_data_stream();
        collect_sse_from_stream(&mut stream, 1).await;
        post(
            &app,
            "/rooms/output-wallet/transfers/deposit",
            serde_json::json!({"account_id":20,"asset_id":"BTC","amount":100}),
        )
        .await;
        let pending = collect_sse_from_stream(&mut stream, 1).await;
        assert_eq!(pending[0]["kind"], "wallet");
        assert_eq!(pending[0]["payload"]["events"][0]["status"], "Pending");
        assert_eq!(
            pending[0]["payload"]["events"][0]["portfolio_balance"]["reserved"],
            "100"
        );
        post(
            &app,
            "/rooms/output-wallet/clock/advance",
            serde_json::json!({"steps":2}),
        )
        .await;
        let completed = collect_sse_from_stream(&mut stream, 1).await;
        assert_eq!(completed[0]["payload"]["events"][0]["status"], "Completed");
        assert_eq!(
            completed[0]["payload"]["events"][0]["portfolio_balance"]["total"],
            "400"
        );
        let page = get_json(
            &app,
            "/rooms/output-wallet/wallet-events?wallet_cursor=0&limit=1",
            Some("alice"),
        )
        .await;
        assert_eq!(page["next_wallet_cursor"], 1);
        assert_eq!(page["has_more"], true);
        let recovered = recovering_app(Box::new(journal));
        let replay = get_json(
            &recovered,
            "/rooms/output-wallet/wallet-events?wallet_cursor=1",
            Some("alice"),
        )
        .await;
        assert_eq!(replay["events"], completed[0]["payload"]["events"]);
        assign_trader(&recovered, "output-wallet", "bob", 10).await;
        let other = get_json(
            &recovered,
            "/rooms/output-wallet/wallet-events?wallet_cursor=0",
            Some("bob"),
        )
        .await;
        assert_eq!(other["events"], serde_json::json!([]));
        assert_eq!(other["next_wallet_cursor"], 2);
    }

    #[tokio::test]
    async fn pure_margin_and_adl_recipient_events_are_visible_only_to_the_affected_owner() {
        let mut rooms = RoomManager::new();
        rooms
            .create_room(perp_liquidation_scenario_with_mark("output-risk", 100))
            .unwrap();
        let account = match rooms
            .account_snapshots_for("output-risk", "V-BTC-PERP")
            .unwrap()
        {
            AccountSnapshots::Perp(a) => PerpAccountStateSummary::from_snapshot(
                a.into_iter().find(|a| a.account_id == 20).unwrap(),
            ),
            _ => unreachable!(),
        };
        let mut summary = synthetic_execution("output-risk", 2);
        summary.instrument_id = Some("V-BTC-PERP".into());
        summary
            .clearing_events
            .push(ClearingEventSummary::PerpMarginStatusChanged {
                account_id: 20,
                previous_status: "healthy".into(),
                new_status: "margin_call".into(),
                mark_price_tick: 100,
                account: account.clone(),
            });
        let mut store = journal::InMemoryJournalStore::new();
        store.set_room_member_for_test("output-risk", "alice", "trader");
        store.set_account_owner_for_test("output-risk", 20, "alice");
        let journal = JournalCoordinator::new(Box::new(store));
        let projected =
            scoped_event_from_execution(&summary, StreamScope::Private, "alice", &journal, 1)
                .await
                .unwrap();
        assert_eq!(
            projected.payload["clearing_events"][0]["type"],
            "PerpMarginStatusChanged"
        );
        summary.clearing_events = vec![ClearingEventSummary::PerpLiquidationSettled {
            account_id: 10,
            order_id: 99,
            liquidation_notional: 100,
            liquidation_fee: 1,
            insurance_fund_payment: 0,
            auto_deleveraging_loss: 5,
            auto_deleveraging_allocations: vec![PerpAutoDeleveragingAllocationSummary {
                position_side: Default::default(),
                account_id: 20,
                position_delta: -1,
                price_tick: 100,
                qty: 1,
                realized_pnl: 0,
                loss: 5,
                account,
            }],
            socialized_loss: 0,
            socialized_loss_allocations: vec![],
            bad_debt: 0,
            insurance_fund_balance: 0,
            account: match rooms
                .account_snapshots_for("output-risk", "V-BTC-PERP")
                .unwrap()
            {
                AccountSnapshots::Perp(a) => PerpAccountStateSummary::from_snapshot(
                    a.into_iter().find(|a| a.account_id == 10).unwrap(),
                ),
                _ => unreachable!(),
            },
        }];
        let projected =
            scoped_event_from_execution(&summary, StreamScope::Private, "alice", &journal, 2)
                .await
                .unwrap();
        assert_eq!(
            projected.payload["clearing_events"][0]["type"],
            "PerpAutoDeleveraged"
        );
        assert_eq!(
            projected.payload["clearing_events"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(!projected.payload.to_string().contains("\"account_id\":10"));
        assert!(
            scoped_event_from_execution(&summary, StreamScope::Private, "bob", &journal, 2)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn real_partial_liquidation_and_adl_reach_public_and_private_streams() {
        let app = new_app();
        let mut scenario = perp_liquidation_scenario_with_mark("output-liquidation", 100);
        let MarketConfig::Perp(config) = &mut scenario.market else {
            unreachable!()
        };
        config.clearing.auto_deleveraging_enabled = true;
        assert_eq!(
            send_json(&app, Method::POST, "/rooms", None, scenario)
                .await
                .status(),
            StatusCode::OK
        );
        post(
            &app,
            "/rooms/output-liquidation/orders",
            place(10, "Sell", 100, 10),
        )
        .await;
        post(
            &app,
            "/rooms/output-liquidation/orders",
            place(20, "Buy", 100, 10),
        )
        .await;
        post(
            &app,
            "/rooms/output-liquidation/orders",
            place(30, "Buy", 70, 4),
        )
        .await;
        assign_trader(&app, "output-liquidation", "long-owner", 20).await;
        assign_trader(&app, "output-liquidation", "short-owner", 10).await;
        let public = open_stream(&app, "output-liquidation", "public", "", None).await;
        let private = open_stream(
            &app,
            "output-liquidation",
            "private",
            "",
            Some("short-owner"),
        )
        .await;
        let mut public = public.into_body().into_data_stream();
        let mut private = private.into_body().into_data_stream();
        collect_sse_from_stream(&mut public, 1).await;
        collect_sse_from_stream(&mut private, 1).await;
        post(
            &app,
            "/rooms/output-liquidation/instruments/V-BTC-PERP/mark-price",
            serde_json::json!({"price_tick":70}),
        )
        .await;
        let partial = collect_sse_from_stream(&mut public, 8).await;
        let fills = partial
            .iter()
            .flat_map(|e| e["payload"]["liquidations"].as_array().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(fills.len(), 1, "{partial:?}");
        assert_eq!(fills[0]["qty"], 4);
        assert_eq!(fills[0]["side"], "Sell");
        post(
            &app,
            "/rooms/output-liquidation/orders",
            place(30, "Buy", 70, 6),
        )
        .await;
        let finished = collect_sse_from_stream(&mut public, 8).await;
        let fills = finished
            .iter()
            .flat_map(|e| e["payload"]["liquidations"].as_array().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(fills.len(), 1, "{finished:?}");
        assert_eq!(fills[0]["qty"], 6);
        let events = collect_sse_from_stream(&mut private, 8).await;
        let accounting = events
            .iter()
            .flat_map(|e| e["payload"]["clearing_events"].as_array().unwrap())
            .collect::<Vec<_>>();
        let adl = accounting
            .iter()
            .find(|e| e["type"] == "PerpAutoDeleveraged")
            .expect("actual ADL notification");
        assert_eq!(adl["account_id"], 10);
        assert!(adl["loss"].as_i64().unwrap() > 0);
        let history = get_json(&app,"/rooms/output-liquidation/instruments/V-BTC-PERP/risk-events?account_id=10&from_start=true",Some("short-owner")).await;
        assert!(
            history["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["event"]["type"] == "PerpAutoDeleveraged")
        );
        assert!(!history.to_string().contains("\"account_id\":20"));
    }

    #[tokio::test]
    async fn funding_sse_balances_match_authority_and_replay_after_restart() {
        let journal = journal::SharedInMemoryJournalStore::new();
        let app = recovering_app(Box::new(journal.clone()));
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms",
                None,
                funding_scenario("output-funding", 10000, 100000)
            )
            .await
            .status(),
            StatusCode::OK
        );
        assign_trader(&app, "output-funding", "alice", 20).await;
        let private = open_stream(&app, "output-funding", "private", "", Some("alice")).await;
        let mut stream = private.into_body().into_data_stream();
        let snapshot = collect_sse_from_stream(&mut stream, 1).await;
        let cursor = snapshot[0]["payload"]["cursor"]["command_seq"]
            .as_u64()
            .unwrap();
        assert_eq!(
            snapshot[0]["payload"]["markets"].as_array().unwrap().len(),
            2
        );
        post(
            &app,
            "/rooms/output-funding/clock/advance",
            serde_json::json!({"steps":2}),
        )
        .await;
        let live = collect_sse_from_stream(&mut stream, 5).await;
        let funded = live
            .iter()
            .find(|e| e["payload"]["funding_settlement"].is_object())
            .unwrap();
        assert_eq!(funded["payload"]["clearing_events"][0]["cash_delta"], -100);
        assert!(
            funded["payload"]["wallet_changes"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["account_id"] == 20)
        );
        let account = get_json(
            &app,
            "/rooms/output-funding/observe?instrument_id=V-BTC-PERP&account_id=20",
            Some("alice"),
        )
        .await;
        let receipt = &funded["payload"]["clearing_events"][0]["account"];
        assert_eq!(receipt["funding_pnl"], -100);
        assert!(account.to_string().contains("\"funding_pnl\":-100"));
        let recovered = recovering_app(Box::new(journal));
        let replay = collect_sse_events(
            open_stream(
                &recovered,
                "output-funding",
                "private",
                &format!("?after_command_seq={cursor}"),
                Some("alice"),
            )
            .await,
            5,
        )
        .await;
        assert_eq!(live.len(), replay.len());
        for (a, b) in live.iter().zip(&replay) {
            assert_eq!(a["payload"], b["payload"]);
        }
    }

    #[tokio::test]
    async fn public_resume_pages_beyond_cache_and_future_cursor_fails_closed() {
        let count = ROOM_EVENT_CACHE_CAPACITY + 603;
        let (app, _) = paged_event_app(count);
        let frames = collect_sse_events(
            open_stream(&app, "paged-room", "public", "?after_command_seq=0", None).await,
            count - 1,
        )
        .await;
        assert_eq!(frames.len(), count - 1);
        for (index, frame) in frames.iter().enumerate() {
            assert_eq!(frame["command_seq"], index as u64 + 1);
        }
        let future = open_stream(
            &app,
            "paged-room",
            "public",
            &format!("?after_command_seq={count}"),
            None,
        )
        .await;
        assert_eq!(future.status(), StatusCode::CONFLICT);
        let wallet = open_stream(&app, "paged-room", "private", "?wallet_cursor=1", None).await;
        assert_eq!(wallet.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn account_revocation_blocks_an_already_queued_private_snapshot() {
        let app = new_app();
        assert_eq!(
            send_json(
                &app,
                Method::POST,
                "/rooms",
                None,
                spot_scenario("output-revoke")
            )
            .await
            .status(),
            StatusCode::OK
        );
        assign_trader(&app, "output-revoke", "alice", 20).await;
        let queued = open_stream(&app, "output-revoke", "private", "", Some("alice")).await;
        post(
            &app,
            "/rooms/output-revoke/members",
            serde_json::json!({"user_id":"alice","role":"spectator"}),
        )
        .await;
        let mut stream = queued.into_body().into_data_stream();
        let bytes = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.contains("resync_required"), "{text}");
        assert!(!text.contains("cash_balance"), "{text}");
        assert!(stream.next().await.is_none());
    }
    #[tokio::test]
    async fn healthy_margin_updates_are_complete_and_wallets_only_report_changes() {
        let app = new_app();
        post(
            &app,
            "/rooms",
            serde_json::to_value(perp_liquidation_scenario_with_mark("output-healthy", 100))
                .unwrap(),
        )
        .await;
        post(
            &app,
            "/rooms/output-healthy/orders",
            place(10, "Sell", 100, 10),
        )
        .await;
        post(
            &app,
            "/rooms/output-healthy/orders",
            place(20, "Buy", 100, 10),
        )
        .await;
        assign_trader(&app, "output-healthy", "alice", 20).await;
        let mut stream = open_stream(&app, "output-healthy", "private", "", Some("alice"))
            .await
            .into_body()
            .into_data_stream();
        collect_sse_from_stream(&mut stream, 1).await;
        post(
            &app,
            "/rooms/output-healthy/instruments/V-BTC-PERP/mark-price",
            serde_json::json!({"price_tick":99}),
        )
        .await;
        let frames = collect_sse_from_stream(&mut stream, 1).await;
        let accounting = frames[0]["payload"]["clearing_events"].as_array().unwrap();
        let margin = accounting
            .iter()
            .find(|e| e["type"] == "PerpMarginUpdated")
            .unwrap();
        assert_eq!(margin["account"]["margin_status"], "healthy");
        assert_eq!(margin["account"]["unrealized_pnl"], -10);
        assert_eq!(margin["account"]["equity"], 190);
        assert!(
            !accounting
                .iter()
                .any(|e| e["type"] == "PerpMarginStatusChanged")
        );
        assert_eq!(
            frames[0]["payload"]["wallet_changes"],
            serde_json::json!([])
        );
        let history = get_json(&app,"/rooms/output-healthy/instruments/V-BTC-PERP/risk-events?account_id=20&from_start=true",Some("alice")).await;
        assert!(
            history["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["event"]["type"] == "PerpMarginUpdated"
                    && e["event"]["account"]["equity"] == 190)
        );
    }

    #[tokio::test]
    async fn postgres_event_receipts_and_wallet_cursor_survive_recovery() {
        let Some(url) = postgres_test_database_url() else {
            return;
        };
        let room = format!("output-pg-{}", uuid_for_test());
        let first_url = url.clone();
        let app = tokio::task::spawn_blocking(move || {
            recovering_app(Box::new(
                PostgresJournalStore::connect_migrated(&first_url).unwrap(),
            ))
        })
        .await
        .unwrap();
        let mut scenario = spot_scenario(&room);
        scenario.initial_portfolios.push(ScenarioPortfolio {
            account_id: 20,
            balances: BTreeMap::from([("BTC".into(), 500)]),
        });
        post(&app, "/rooms", serde_json::to_value(scenario).unwrap()).await;
        let first = post(
            &app,
            &format!("/rooms/{room}/orders"),
            place(10, "Sell", 100, 3),
        )
        .await;
        post(
            &app,
            &format!("/rooms/{room}/orders"),
            place(20, "Buy", 100, 2),
        )
        .await;
        post(
            &app,
            &format!("/rooms/{room}/transfers/deposit"),
            serde_json::json!({"account_id":20,"asset_id":"BTC","amount":100}),
        )
        .await;
        let query = format!("?after_command_seq={}", first["command_seq"]);
        let before =
            collect_sse_events(open_stream(&app, &room, "public", &query, None).await, 1).await;
        let wallets = get_json(&app, &format!("/rooms/{room}/wallet-events"), None).await;
        assert_eq!(wallets["events"].as_array().unwrap().len(), 1);
        drop(app);
        let recovered = tokio::task::spawn_blocking(move || {
            recovering_app(Box::new(
                PostgresJournalStore::connect_migrated(&url).unwrap(),
            ))
        })
        .await
        .unwrap();
        let after = collect_sse_events(
            open_stream(&recovered, &room, "public", &query, None).await,
            1,
        )
        .await;
        assert_eq!(before[0]["payload"], after[0]["payload"]);
        assert_eq!(
            wallets,
            get_json(&recovered, &format!("/rooms/{room}/wallet-events"), None).await
        );
        assert!(
            after[0]["payload"]["book_delta_available"]
                .as_bool()
                .unwrap()
        );
    }

    fn uuid_for_test() -> String {
        format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    #[test]
    fn margin_and_wallet_receipts_preserve_large_money_and_detect_tampering() {
        let mut rooms = RoomManager::new();
        rooms
            .create_room(perp_liquidation_scenario_with_mark("output-money", 100))
            .unwrap();
        let AccountSnapshots::Perp(accounts) = rooms
            .account_snapshots_for("output-money", "V-BTC-PERP")
            .unwrap()
        else {
            unreachable!()
        };
        let mut account = accounts.into_iter().find(|a| a.account_id == 20).unwrap();
        account.cash_balance = 1_i128 << 100;
        account.equity = account.cash_balance;
        let mut summary = synthetic_execution("output-money", 2);
        summary
            .margin_updates
            .push(exchange_core::actor::ExecutionMarginUpdate {
                instrument_id: "V-BTC-PERP".into(),
                account,
            });
        summary
            .wallet_updates
            .push(exchange_core::actor::ExecutionWalletUpdate {
                account_id: 20,
                venue_id: "default-venue".into(),
                balances: vec![exchange_core::actor::ExecutionWalletBalance {
                    asset_id: "BTC".into(),
                    total: (1_i128 << 100).to_string(),
                    reserved: "0".into(),
                    available: (1_i128 << 100).to_string(),
                }],
            });
        let encoded = serde_json::to_value(&summary).unwrap();
        assert_eq!(
            encoded["margin_updates"][0]["account"]["cash_balance"],
            (1_i128 << 100).to_string()
        );
        let recovered: RoomExecutionSummary = serde_json::from_value(encoded).unwrap();
        assert!(execution_summary_matches(&summary, &recovered));
        let mut tampered = recovered.clone();
        tampered.margin_updates[0].account.equity -= 1;
        assert!(!execution_summary_matches(&summary, &tampered));
        let mut tampered = recovered;
        tampered.wallet_updates[0].balances[0].total = "0".into();
        assert!(!execution_summary_matches(&summary, &tampered));
    }
}
