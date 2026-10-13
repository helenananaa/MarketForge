fn projection_room(bars: u64) -> RoomManager {
    let mut scenario = spot_scenario("projection");
    scenario.seed_orders.clear();
    let mut rooms = RoomManager::new();
    rooms.create_room(scenario).unwrap();
    for bar in 0..bars {
        projection_trade(&mut rooms, bar);
        rooms.advance_clock("projection", 1).unwrap();
    }
    rooms
}

fn projection_trade(rooms: &mut RoomManager, index: u64) {
    let (seller, buyer) = if index.is_multiple_of(2) {
        (10, 20)
    } else {
        (20, 10)
    };
    rooms
        .apply(
            "projection",
            limit(index * 2 + 1, seller, Side::Sell, 100, 1),
        )
        .unwrap();
    rooms
        .apply("projection", limit(index * 2 + 2, buyer, Side::Buy, 100, 1))
        .unwrap();
}

type ProjectionView = (crate::ParticipantObservation, (Option<[u8; 32]>, u64));

fn projection_views(wave: &NativeObservationWave) -> Vec<ProjectionView> {
    let mut batch = wave.batch();
    let mut result = Vec::new();
    for account in [10, 20, 999] {
        for interval in [1000, 2000, 7000] {
            let request = crate::bots::BotMarketDataRequest {
                interval_ms: interval,
                max_bars: 64,
            };
            let mut view = batch
                .bot_observation("V-BTC-SPOT", account, Some(request), &[])
                .unwrap();
            batch
                .enrich_data(&mut view, Some(request), &[], &[1500], true, account)
                .unwrap();
            let stamp = batch
                .bot_event_stamp(
                    "V-BTC-SPOT",
                    account,
                    Some(request),
                    &[],
                    &[1500],
                    true,
                    crate::bots::BotEventFilter {
                        closed_bars: true,
                        account: true,
                        interval_ms: None,
                    },
                )
                .unwrap();
            result.push((view, stamp));
        }
    }
    result
}

#[test]
fn observation_projection_appends_only_suffix_and_preserves_cold_views_and_old_wave() {
    for bars in [15, 16, 17, 31, 32, 33, 1000] {
        let mut rooms = projection_room(bars);
        let old = rooms
            .clone()
            .into_native_observation_wave("projection".into());
        let before = projection_views(&old);
        observation_projection::take_work();
        projection_trade(&mut rooms, bars);
        rooms.advance_clock("projection", 1).unwrap();
        let next = rooms
            .clone()
            .into_native_observation_wave_after("projection".into(), &old);
        let warm = projection_views(&next);
        let work = observation_projection::take_work();
        assert_eq!(work.cold_histories, 0, "{work:?}");
        assert_eq!(work.cold_intervals, 0, "{work:?}");
        assert_eq!(work.suffix_executions, 2, "{work:?}");
        assert_eq!(work.index_entries, 2, "{work:?}");
        assert!(work.appended_links <= 4, "{work:?}");
        let cold = rooms
            .clone()
            .into_native_observation_wave("projection".into());
        assert_eq!(warm, projection_views(&cold));
        assert_eq!(projection_views(&old), before);
        // A later, previously unseen interval takes the ordinary full build;
        // its proof still covers the full authoritative history.
        let request = Some(crate::bots::BotMarketDataRequest {
            interval_ms: 3000,
            max_bars: 2,
        });
        assert_eq!(
            next.batch()
                .bot_observation("V-BTC-SPOT", 20, request, &[])
                .unwrap(),
            cold.batch()
                .bot_observation("V-BTC-SPOT", 20, request, &[])
                .unwrap()
        );
    }
}

#[test]
fn observation_projection_clock_closes_carry_without_empty_gap_candles_or_history_scans() {
    let mut rooms = projection_room(40);
    projection_trade(&mut rooms, 40); // Current bar is still forming.
    let old = rooms
        .clone()
        .into_native_observation_wave("projection".into());
    let before = projection_views(&old);
    observation_projection::take_work();
    rooms.advance_clock("projection", 101).unwrap();
    let next = rooms
        .clone()
        .into_native_observation_wave_after("projection".into(), &old);
    let warm = projection_views(&next);
    let work = observation_projection::take_work();
    assert_eq!(
        work.cold_histories + work.cold_intervals + work.suffix_executions + work.index_entries,
        0
    );
    assert!(work.appended_links <= 4);
    assert_eq!(
        warm,
        projection_views(&rooms.into_native_observation_wave("projection".into()))
    );
    assert_eq!(projection_views(&old), before);
    assert_eq!(
        warm[0].0.bot_market_data.as_ref().unwrap().candles.len(),
        41
    );
}

#[test]
fn observation_projection_rebuilt_truncated_branched_and_restored_sources_go_cold() {
    let mut rooms = projection_room(40);
    let (receipts, _) = rooms.bot_trade_history("projection", "V-BTC-SPOT").unwrap();
    rooms.restore_bot_history("projection", receipts, true);
    let old = rooms
        .clone()
        .into_native_observation_wave("projection".into());
    projection_views(&old);
    for case in 0..6 {
        let mut changed = rooms.clone();
        match case {
            0 => {
                let records = changed
                    .execution_history_from("projection", 0)
                    .unwrap()
                    .cloned()
                    .collect::<Vec<_>>();
                changed
                    .executions
                    .insert("projection".into(), records.into());
            }
            1 => {
                let records = changed
                    .execution_history_from("projection", 0)
                    .unwrap()
                    .take(31)
                    .cloned()
                    .collect::<Vec<_>>();
                changed
                    .executions
                    .insert("projection".into(), records.into());
            }
            2 => {
                let mut records = changed
                    .execution_history_from("projection", 0)
                    .unwrap()
                    .cloned()
                    .collect::<Vec<_>>();
                records[0].market_time_ms += 1;
                changed
                    .executions
                    .insert("projection".into(), records.into());
            }
            3 => changed.restore_bot_history("projection", vec![], true),
            4 => changed.restore_bot_accounts("projection", vec![]),
            _ => {
                changed
                    .restored_bot_history
                    .get_mut("projection")
                    .unwrap()
                    .1 = false
            }
        }
        observation_projection::take_work();
        let next = changed
            .clone()
            .into_native_observation_wave_after("projection".into(), &old);
        let warm = projection_views(&next);
        assert_eq!(
            observation_projection::take_work().cold_histories,
            1,
            "case {case}"
        );
        assert_eq!(
            warm,
            projection_views(&changed.into_native_observation_wave("projection".into()))
        );
    }
}

#[test]
fn observation_projection_late_or_duplicate_receipt_rebuilds_full_proof() {
    let mut rooms = projection_room(40);
    projection_trade(&mut rooms, 40);
    let old = rooms
        .clone()
        .into_native_observation_wave("projection".into());
    projection_views(&old);
    for late in [false, true] {
        let mut changed = rooms.clone();
        let mut execution = changed
            .execution_history_from("projection", 0)
            .unwrap()
            .last()
            .unwrap()
            .clone();
        if late {
            execution.market_time_ms -= 500;
            if let ActorExecutionResult::Accepted(MarketExecution::Spot(result)) =
                &mut execution.result
            {
                for record in &mut result.events {
                    if let Event::TradePrinted(trade) = &mut record.event {
                        trade.trade_id += 1;
                    }
                }
            }
        }
        changed
            .executions
            .get_mut("projection")
            .unwrap()
            .push(execution);
        observation_projection::take_work();
        let next = changed
            .clone()
            .into_native_observation_wave_after("projection".into(), &old);
        let warm = projection_views(&next);
        assert_eq!(observation_projection::take_work().cold_histories, 1);
        assert_eq!(
            warm,
            projection_views(&changed.into_native_observation_wave("projection".into()))
        );
    }
}

#[test]
fn observation_projection_keeps_missing_private_frames_incomplete_across_append() {
    let mut rooms = projection_room(40);
    let (receipts, _) = rooms.bot_trade_history("projection", "V-BTC-SPOT").unwrap();
    rooms.restore_bot_history("projection", receipts, true);
    let old = rooms
        .clone()
        .into_native_observation_wave("projection".into());
    let before = projection_views(&old);
    assert!(
        !before[0]
            .0
            .bot_market_data
            .as_ref()
            .unwrap()
            .private_history_complete
    );
    projection_trade(&mut rooms, 40);
    rooms.advance_clock("projection", 1).unwrap();
    observation_projection::take_work();
    let next = rooms
        .clone()
        .into_native_observation_wave_after("projection".into(), &old);
    let warm = projection_views(&next);
    assert_eq!(observation_projection::take_work().cold_histories, 0);
    assert!(
        !warm[0]
            .0
            .bot_market_data
            .as_ref()
            .unwrap()
            .private_history_complete
    );
    assert_eq!(
        warm,
        projection_views(&rooms.into_native_observation_wave("projection".into()))
    );
}

#[test]
fn observation_projection_merges_forming_bar_with_mixed_aggressors_exactly() {
    let mut rooms = projection_room(40);
    projection_trade(&mut rooms, 40);
    let old = rooms
        .clone()
        .into_native_observation_wave("projection".into());
    let before = projection_views(&old);
    // Reverse maker/taker roles while keeping the existing forming interval.
    rooms
        .apply("projection", limit(83, 10, Side::Buy, 100, 1))
        .unwrap();
    rooms
        .apply("projection", limit(84, 20, Side::Sell, 100, 1))
        .unwrap();
    observation_projection::take_work();
    let forming = rooms
        .clone()
        .into_native_observation_wave_after("projection".into(), &old);
    let warm = projection_views(&forming);
    assert_eq!(observation_projection::take_work().cold_histories, 0);
    assert_eq!(
        warm,
        projection_views(
            &rooms
                .clone()
                .into_native_observation_wave("projection".into())
        )
    );
    rooms.advance_clock("projection", 1).unwrap();
    let closed = rooms
        .clone()
        .into_native_observation_wave_after("projection".into(), &forming);
    let warm = projection_views(&closed);
    assert_eq!(
        warm,
        projection_views(&rooms.into_native_observation_wave("projection".into()))
    );
    let bar = warm[0]
        .0
        .bot_market_data
        .as_ref()
        .unwrap()
        .candles
        .last()
        .unwrap();
    assert_eq!(bar.volume, 2);
    assert_eq!(bar.taker_buy_base, Some(1));
    assert_eq!(projection_views(&old), before);
}

fn projection_set_time(rooms: &mut RoomManager, time: u64) {
    let mut actor = serde_json::to_value(rooms.simulation_room("projection").unwrap()).unwrap();
    for exchange in actor["exchanges"].as_object_mut().unwrap().values_mut() {
        exchange["clock"]["market_time_ms"] = time.into();
    }
    rooms
        .rooms
        .insert("projection".into(), serde_json::from_value(actor).unwrap());
}

#[test]
fn observation_projection_time_regression_future_receipts_and_saturated_closes_go_cold() {
    let base = projection_room(40);
    for case in 0..3 {
        let mut rooms = base.clone();
        if case == 1 {
            let (receipts, _) = rooms.bot_trade_history("projection", "V-BTC-SPOT").unwrap();
            let mut future = receipts.last().unwrap().clone();
            future.trade.trade_id += 100;
            future.market_time_ms = 45_000;
            let mut second = future.clone();
            second.trade.trade_id += 1;
            second.market_time_ms += 1000;
            rooms.restore_bot_history("projection", vec![future, second], true);
        }
        if case == 2 {
            projection_set_time(&mut rooms, u64::MAX);
            projection_trade(&mut rooms, 40);
        }
        let old = rooms
            .clone()
            .into_native_observation_wave("projection".into());
        projection_views(&old);
        match case {
            0 => projection_set_time(&mut rooms, 39_000),
            1 => {
                rooms.advance_clock("projection", 1).unwrap();
            }
            _ => projection_trade(&mut rooms, 41),
        }
        observation_projection::take_work();
        let next = rooms
            .clone()
            .into_native_observation_wave_after("projection".into(), &old);
        let warm = projection_views(&next);
        assert_eq!(
            observation_projection::take_work().cold_histories,
            1,
            "case {case}"
        );
        assert_eq!(
            warm,
            projection_views(&rooms.into_native_observation_wave("projection".into()))
        );
    }
}

fn projection_perp_views(wave: &NativeObservationWave) -> Vec<ProjectionView> {
    let mut batch = wave.batch();
    [10, 20, 30]
        .into_iter()
        .map(|account| {
            let request = Some(crate::bots::BotMarketDataRequest {
                interval_ms: 1000,
                max_bars: 64,
            });
            let mut view = batch
                .bot_observation("V-USD-PERP", account, request, &[])
                .unwrap();
            batch
                .enrich_data(&mut view, request, &[], &[2000], true, account)
                .unwrap();
            let stamp = batch
                .bot_event_stamp(
                    "V-USD-PERP",
                    account,
                    request,
                    &[],
                    &[2000],
                    true,
                    crate::bots::BotEventFilter {
                        closed_bars: true,
                        account: true,
                        interval_ms: None,
                    },
                )
                .unwrap();
            (view, stamp)
        })
        .collect()
}

#[test]
fn observation_projection_perp_funding_and_liquidation_suffix_matches_cold_and_preserves_old_wave()
{
    for liquidate in [false, true] {
        // Reuse the actual funding fixture and executions from funding_tests.
        // The distressed variant pays its entire margin at the first boundary,
        // and the existing bid lets the room execute the resulting liquidation.
        let mut config = crate::funding_tests::scenario(if liquidate { 100_000 } else { 10_000 });
        if liquidate {
            config.accounts[1] = ScenarioAccount::Basic {
                account_id: 20,
                cash_balance: 1000,
            };
        }
        let mut rooms = RoomManager::new();
        rooms.create_room(config).unwrap();
        for command in [
            limit(3, 10, Side::Sell, 10000, 1),
            limit(4, 20, Side::Buy, 10000, 1),
            limit(5, 30, Side::Buy, 9999, 10),
            limit(6, 30, Side::Sell, 10001, 10),
        ] {
            let execution = rooms
                .apply_to_instrument("funded", "V-USD-PERP", command)
                .unwrap();
            assert!(matches!(
                execution.result,
                ActorExecutionResult::Accepted(MarketExecution::Perp(_))
            ));
        }
        rooms.advance_clock("funded", 1).unwrap();
        let old = rooms.clone().into_native_observation_wave("funded".into());
        // Materialize both the account-event/index caches and the event stamps
        // before appending: a stale cached account must not make this test pass.
        let before = projection_perp_views(&old);
        let start = rooms.execution_history_len("funded").unwrap();
        observation_projection::take_work();
        rooms.advance_clock("funded", 1).unwrap();
        let suffix: Vec<_> = rooms
            .execution_history_from("funded", start)
            .unwrap()
            .collect();
        assert!(suffix.iter().any(|execution| {
            execution
                .funding_settlement
                .as_ref()
                .is_some_and(|settlement| {
                    settlement.status == crate::FundingStatus::Settled
                        && settlement.total_transfer > 0
                })
        }));
        assert!(suffix.iter().any(|execution| {
            matches!(&execution.result, ActorExecutionResult::Accepted(MarketExecution::Perp(result))
                if result.clearing_events.iter().any(|event| matches!(event,
                    crate::PerpClearingEvent::FundingSettled { snapshot, .. }
                    if snapshot.account_id == 20)))
        }));
        let has_liquidation = suffix.iter().any(|execution| {
            matches!(&execution.result, ActorExecutionResult::Accepted(MarketExecution::Perp(result))
                if result.clearing_events.iter().any(|event| matches!(event,
                    crate::PerpClearingEvent::LiquidationSettled { account_id: 20, .. })))
        });
        assert_eq!(has_liquidation, liquidate);
        let has_trade = suffix.iter().any(|execution| {
            matches!(&execution.result, ActorExecutionResult::Accepted(MarketExecution::Perp(result))
                if result.events.iter().any(|record| matches!(record.event, Event::TradePrinted(_))))
        });
        assert_eq!(has_trade, liquidate);
        let suffix_len = suffix.len();
        let next = rooms
            .clone()
            .into_native_observation_wave_after("funded".into(), &old);
        let warm = projection_perp_views(&next);
        let work = observation_projection::take_work();
        assert_eq!(work.cold_histories, 0, "liquidate={liquidate}, {work:?}");
        assert_eq!(work.cold_intervals, 0, "liquidate={liquidate}, {work:?}");
        assert_eq!(work.suffix_executions, suffix_len, "{work:?}");
        assert_eq!(work.index_entries, suffix_len, "{work:?}");
        let cold = rooms.clone().into_native_observation_wave("funded".into());
        // Includes exact candles/proofs, private events, fee details,
        // completeness flags, current account/risk and event fingerprints.
        assert_eq!(warm, projection_perp_views(&cold));
        assert_eq!(projection_perp_views(&old), before);
        for (old_view, new_view) in before.iter().zip(&warm) {
            let old_data = old_view.0.bot_market_data.as_ref().unwrap();
            let new_data = new_view.0.bot_market_data.as_ref().unwrap();
            assert!(old_data.private_history_complete && new_data.private_history_complete);
            assert!(!new_data.truncated);
            assert!(
                new_data
                    .fill_details
                    .iter()
                    .all(|fill| fill.fee_paid == Some(0))
            );
            if !liquidate {
                // Funding changes ledger frames without inventing a trade or fee.
                assert_eq!(new_data.own_fills, old_data.own_fills);
                assert_eq!(new_data.fill_details, old_data.fill_details);
            }
        }
        let old_long = before[1].0.bot_market_data.as_ref().unwrap();
        let new_long = warm[1].0.bot_market_data.as_ref().unwrap();
        assert!(new_long.account_events.len() > old_long.account_events.len());
        assert_ne!(warm[1].1.0, before[1].1.0);
        let expected_pnl = if liquidate { -1000 } else { -100 };
        let expected_position = if liquidate { 0 } else { 1 };
        let Some(crate::observation::BotAccountEvent {
            account: crate::observation::BotAccountFrame::Perp(frame),
            ..
        }) = new_long.account_events.last()
        else {
            panic!("expected the long account's authoritative perp frame");
        };
        assert_eq!(frame.funding_pnl, expected_pnl);
        assert_eq!(frame.position_qty, expected_position);
        if liquidate {
            assert!(new_long.own_fills.len() > old_long.own_fills.len());
            assert!(
                warm[2]
                    .0
                    .bot_market_data
                    .as_ref()
                    .unwrap()
                    .account_events
                    .len()
                    > before[2]
                        .0
                        .bot_market_data
                        .as_ref()
                        .unwrap()
                        .account_events
                        .len()
            );
        } else {
            // Account 30 supplies quotes but has no position: its cached private
            // history is unaffected by the other accounts' funding settlement.
            assert_eq!(
                warm[2].0.bot_market_data.as_ref().unwrap().account_events,
                before[2].0.bot_market_data.as_ref().unwrap().account_events
            );
        }
    }
}
