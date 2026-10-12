fn event_stamp_for(
    manager: &RoomManager,
    account: AccountId,
    request: crate::bots::BotMarketDataRequest,
    intervals: &[u64],
) -> [u8; 32] {
    let wave = manager
        .clone()
        .into_native_observation_wave("event-filter".into());
    wave.batch()
        .bot_event_stamp(
            "V-BTC-SPOT",
            account,
            Some(request),
            &[],
            intervals,
            true,
            crate::bots::BotEventFilter {
                closed_bars: true,
                account: true,
                interval_ms: None,
            },
        )
        .unwrap()
        .0
        .unwrap()
}

#[test]
fn event_prefilter_ignores_empty_clock_ticks_and_observes_same_bar_private_feedback() {
    let mut manager = RoomManager::new();
    manager.create_room(spot_scenario("event-filter")).unwrap();
    let request = crate::bots::BotMarketDataRequest {
        interval_ms: 10_000,
        max_bars: 512,
    };
    let initial = event_stamp_for(&manager, 20, request, &[]);
    manager.advance_clock("event-filter", 1).unwrap();
    assert_eq!(initial, event_stamp_for(&manager, 20, request, &[]));
    let outsider = event_stamp_for(&manager, 999, request, &[]);
    // An unfilled private order must wake only its owner even before bar close.
    manager
        .apply("event-filter", limit(2, 20, Side::Buy, 90, 1))
        .unwrap();
    let ordered = event_stamp_for(&manager, 20, request, &[]);
    assert_ne!(initial, ordered);
    assert_eq!(outsider, event_stamp_for(&manager, 999, request, &[]));
    manager
        .apply("event-filter", limit(3, 20, Side::Buy, 100, 1))
        .unwrap();
    assert_ne!(ordered, event_stamp_for(&manager, 20, request, &[]));
    // A public trade still forming its bar does not wake an uninvolved account.
    assert_eq!(outsider, event_stamp_for(&manager, 999, request, &[]));
    manager.advance_clock("event-filter", 9).unwrap();
    assert_ne!(outsider, event_stamp_for(&manager, 999, request, &[]));
    let closed = event_stamp_for(&manager, 20, request, &[]);
    manager.advance_clock("event-filter", 1).unwrap();
    assert_eq!(closed, event_stamp_for(&manager, 20, request, &[]));
}

#[test]
fn event_prefilter_observes_full_history_corrections_fees_and_completeness() {
    let mut manager = RoomManager::new();
    manager.create_room(spot_scenario("event-filter")).unwrap();
    manager
        .apply("event-filter", limit(2, 20, Side::Buy, 100, 1))
        .unwrap();
    let mut receipts = manager
        .bot_trade_history("event-filter", "V-BTC-SPOT")
        .unwrap()
        .0;
    let mut old = receipts[0].clone();
    old.trade.trade_id += 100;
    old.market_time_ms = 10_000;
    receipts.push(old);
    manager.restore_bot_history("event-filter", receipts.clone(), true);
    manager.advance_clock("event-filter", 30).unwrap();
    let request = crate::bots::BotMarketDataRequest {
        interval_ms: 10_000,
        max_bars: 512,
    };
    let baseline = event_stamp_for(&manager, 20, request, &[]);
    // Keep the tail/count unchanged: a last-bar-only cursor would miss this.
    receipts[0].trade.price_tick += 1;
    manager.restore_bot_history("event-filter", receipts.clone(), true);
    assert_ne!(baseline, event_stamp_for(&manager, 20, request, &[]));
    receipts[0].trade.price_tick -= 1;
    receipts[1].buyer_fee = Some(7);
    manager.restore_bot_history("event-filter", receipts.clone(), true);
    assert_ne!(baseline, event_stamp_for(&manager, 20, request, &[]));
    receipts[1].buyer_fee = None;
    manager.restore_bot_history("event-filter", receipts, false);
    assert_ne!(baseline, event_stamp_for(&manager, 20, request, &[]));
    assert_ne!(
        baseline,
        event_stamp_for(
            &manager,
            20,
            crate::bots::BotMarketDataRequest {
                max_bars: 1,
                ..request
            },
            &[]
        )
    );
}

#[test]
fn event_prefilter_additional_interval_closes_before_primary_and_timer_wakes_without_bars() {
    let mut manager = RoomManager::new();
    manager.create_room(spot_scenario("event-filter")).unwrap();
    manager
        .apply("event-filter", limit(2, 20, Side::Buy, 100, 1))
        .unwrap();
    let request = crate::bots::BotMarketDataRequest {
        interval_ms: 10_000,
        max_bars: 512,
    };
    let primary = event_stamp_for(&manager, 999, request, &[]);
    let multi = event_stamp_for(&manager, 999, request, &[5000]);
    manager.advance_clock("event-filter", 5).unwrap();
    assert_eq!(primary, event_stamp_for(&manager, 999, request, &[]));
    assert_ne!(multi, event_stamp_for(&manager, 999, request, &[5000]));
    let timer_stamp = |manager: &RoomManager| {
        manager
            .clone()
            .into_native_observation_wave("event-filter".into())
            .batch()
            .bot_event_stamp(
                "V-BTC-SPOT",
                999,
                None,
                &[],
                &[],
                false,
                crate::bots::BotEventFilter {
                    closed_bars: false,
                    account: false,
                    interval_ms: Some(10_000),
                },
            )
            .unwrap()
            .0
    };
    let before = timer_stamp(&manager);
    manager.advance_clock("event-filter", 4).unwrap();
    assert_eq!(before, timer_stamp(&manager));
    manager.advance_clock("event-filter", 1).unwrap();
    assert_ne!(before, timer_stamp(&manager));
}

#[test]
fn shared_account_event_scan_preserves_order_isolation_and_recovered_prefix_replacement() {
    let mut manager = RoomManager::new();
    manager.create_room(spot_scenario("event-filter")).unwrap();
    manager
        .apply("event-filter", limit(2, 20, Side::Buy, 100, 1))
        .unwrap();
    let current = manager
        .bot_account_events("event-filter", "V-BTC-SPOT", 20)
        .unwrap();
    assert_eq!(current.len(), 1);
    let mut replaced = current[0].clone();
    if let crate::observation::BotAccountFrame::Spot { cash_balance, .. } = &mut replaced.account {
        *cash_balance = -999;
    }
    let mut prefix = replaced.clone();
    prefix.command_seq = 0;
    manager.restore_bot_accounts("event-filter", vec![prefix.clone(), replaced]);
    let wave = manager
        .clone()
        .into_native_observation_wave("event-filter".into());
    let mut batch = wave.batch();
    let request = Some(crate::bots::BotMarketDataRequest {
        interval_ms: 1000,
        max_bars: 512,
    });
    for account in [20, 10, 999, 20] {
        let expected = manager
            .bot_account_events("event-filter", "V-BTC-SPOT", account)
            .unwrap();
        let mut view = batch
            .bot_observation("V-BTC-SPOT", account, request, &[])
            .unwrap();
        batch
            .enrich_data(&mut view, request, &[], &[], true, account)
            .unwrap();
        assert_eq!(view.bot_market_data.unwrap().account_events, expected);
    }
    let result = manager
        .bot_account_events("event-filter", "V-BTC-SPOT", 20)
        .unwrap();
    assert_eq!(result, vec![prefix, current[0].clone()]);
    assert_eq!(wave.history.lock().unwrap().len(), 1);
}

#[test]
fn indexed_execution_reads_preserve_shared_chunk_and_tail_boundaries() {
    let mut history = History::default();
    history.extend(0..100);
    let frozen = history.clone();
    history.extend(100..135);
    for index in 0..140 {
        assert_eq!(history.at(index), (index < 135).then_some(&index));
        assert_eq!(frozen.at(index), (index < 100).then_some(&index));
    }
}

#[test]
fn event_prefilter_observes_related_perp_price_and_conditional_order_changes() {
    let mut scenario = spot_scenario("event-filter");
    scenario
        .extra_markets
        .push(pending_perp_liquidation_scenario("peer").market);
    let mut manager = RoomManager::new();
    manager.create_room(scenario).unwrap();
    let stamp = |manager: &RoomManager| {
        manager
            .clone()
            .into_native_observation_wave("event-filter".into())
            .batch()
            .bot_event_stamp(
                "V-BTC-SPOT",
                20,
                Some(crate::bots::BotMarketDataRequest {
                    interval_ms: 10_000,
                    max_bars: 512,
                }),
                &["V-BTC-PERP".into()],
                &[5000],
                true,
                crate::bots::BotEventFilter {
                    closed_bars: true,
                    account: true,
                    interval_ms: None,
                },
            )
            .unwrap()
            .0
    };
    let initial = stamp(&manager);
    manager
        .apply_to_instrument(
            "event-filter",
            "V-BTC-PERP",
            Command::SetMarkPrice(SetMarkPrice { price_tick: 101 }),
        )
        .unwrap();
    let price = stamp(&manager);
    assert_ne!(initial, price);
    manager
        .apply_to_instrument(
            "event-filter",
            "V-BTC-PERP",
            Command::SetConditionalOrder {
                account_id: 20,
                key: "pending-entry".into(),
                spec: Some(Box::new(crate::conditional_orders::ConditionalOrderSpec {
                    side: Side::Buy,
                    position_side: crate::PositionSide::Both,
                    qty: 1,
                    trigger_price_tick: 110,
                    above: true,
                    trigger: crate::ProtectionTrigger::Last,
                    limit_price_tick: Some(111),
                    protection: None,
                })),
            },
        )
        .unwrap();
    assert_ne!(price, stamp(&manager));
}

#[test]
#[ignore = "fixed immutable-wave Release comparison; run alone"]
fn event_prefilter_fixed_work_benchmark() {
    use std::hint::black_box;
    use std::time::Instant;
    let mut manager = RoomManager::new();
    manager.create_room(spot_scenario("event-filter")).unwrap();
    manager
        .apply("event-filter", limit(2, 20, Side::Buy, 100, 1))
        .unwrap();
    let receipt = manager
        .bot_trade_history("event-filter", "V-BTC-SPOT")
        .unwrap()
        .0[0]
        .clone();
    let receipts = (0..384)
        .map(|index| {
            let mut receipt = receipt.clone();
            receipt.trade.trade_id = index + 100;
            receipt.market_time_ms = index * 1000;
            receipt
        })
        .collect();
    manager.restore_bot_history("event-filter", receipts, true);
    manager.advance_clock("event-filter", 384).unwrap();
    let wave = manager.into_native_observation_wave("event-filter".into());
    let mut batch = wave.batch();
    let request = Some(crate::bots::BotMarketDataRequest {
        interval_ms: 1000,
        max_bars: 512,
    });
    let filter = crate::bots::BotEventFilter {
        closed_bars: true,
        account: true,
        interval_ms: None,
    };
    for account in 50_000..51_000 {
        let mut view = batch
            .bot_observation("V-BTC-SPOT", account, request, &[])
            .unwrap();
        batch
            .enrich_data(&mut view, request, &[], &[], true, account)
            .unwrap();
        assert!(!view.bot_market_data.as_ref().unwrap().truncated);
        batch
            .bot_event_stamp("V-BTC-SPOT", account, request, &[], &[], true, filter)
            .unwrap();
    }
    let mut rows = vec![];
    for round in 0..5 {
        let modes = if round % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        };
        let mut row = [0u128; 2];
        for early in modes {
            let begun = Instant::now();
            for _ in 0..5 {
                for account in 50_000..51_000 {
                    if early {
                        black_box(
                            batch
                                .bot_event_stamp(
                                    "V-BTC-SPOT",
                                    account,
                                    request,
                                    &[],
                                    &[],
                                    true,
                                    filter,
                                )
                                .unwrap(),
                        );
                    } else {
                        let mut view = batch
                            .bot_observation("V-BTC-SPOT", account, request, &[])
                            .unwrap();
                        batch
                            .enrich_data(&mut view, request, &[], &[], true, account)
                            .unwrap();
                        // Include the full-history hash required by the old trigger.
                        let mut digest = EventHashWriter(Sha256::new());
                        assert!(digest.value(&view.bot_market_data));
                        black_box(digest.0.finalize());
                    }
                }
            }
            row[usize::from(early)] = begun.elapsed().as_nanos();
        }
        rows.push(row);
    }
    println!(
        "{}",
        serde_json::json!({"scope":"5000 idle host checks per mode, 1000 accounts, 384 real-content closed bars; immutable wave; excludes runtime, queue, copies of persisted state and commit", "rounds_full_then_prefilter_ns": rows})
    );
}
