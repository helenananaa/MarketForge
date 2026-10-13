// Included in room::tests so the independent cold API and existing market
// fixtures can exercise exactly the same account and history inputs.
thread_local! {
    static OBSERVATION_MATERIALIZE_CHECKS: std::cell::RefCell<Option<Vec<&'static str>>> =
        const { std::cell::RefCell::new(None) };
}

pub(super) fn observation_materialize_check_unlocked(
    history: &Mutex<ObservationHistory>,
    stage: &'static str,
) {
    OBSERVATION_MATERIALIZE_CHECKS.with(|checks| {
        if let Some(checks) = &mut *checks.borrow_mut() {
            assert!(
                history.try_lock().is_ok(),
                "{stage} still holds history lock"
            );
            checks.push(stage);
        }
    });
}

fn materialize_cold_reference(
    rooms: &RoomManager,
    account: AccountId,
    interval_ms: u64,
) -> crate::ParticipantObservation {
    let request = crate::bots::BotMarketDataRequest {
        interval_ms,
        max_bars: 64,
    };
    // This path rebuilds trades/candles directly from authoritative room
    // history, independently of RoomObservationBatch's persistent projection.
    let mut view = rooms
        .bot_observation("projection", "V-BTC-SPOT", account, Some(request))
        .unwrap();
    let events = rooms
        .bot_account_events("projection", "V-BTC-SPOT", account)
        .unwrap();
    let data = view.bot_market_data.as_mut().unwrap();
    assert!(events.len() <= 4096);
    data.account_events = events;
    for interval_ms in [2000, 7000] {
        let stream = rooms
            .bot_observation(
                "projection",
                "V-BTC-SPOT",
                account,
                Some(crate::bots::BotMarketDataRequest {
                    interval_ms,
                    ..request
                }),
            )
            .unwrap()
            .bot_market_data
            .unwrap();
        data.streams.push(crate::observation::BotDataStream {
            instrument_id: "V-BTC-SPOT".into(),
            interval_ms,
            candles: stream.candles,
            history_proof: stream.history_proof,
            truncated: stream.truncated,
        });
    }
    view
}

fn materialize_wave_view(
    wave: &NativeObservationWave,
    account: AccountId,
    interval_ms: u64,
) -> crate::ParticipantObservation {
    let request = Some(crate::bots::BotMarketDataRequest {
        interval_ms,
        max_bars: 64,
    });
    let mut batch = wave.batch();
    let mut view = batch
        .bot_observation("V-BTC-SPOT", account, request, &[])
        .unwrap();
    batch
        .enrich_data(&mut view, request, &[], &[2000, 7000], true, account)
        .unwrap();
    view
}

#[test]
fn observation_materialize_vectors_begin_after_history_lock_release() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            OBSERVATION_MATERIALIZE_CHECKS.with(|checks| *checks.borrow_mut() = None);
        }
    }
    let rooms = projection_room(72);
    let expected = materialize_cold_reference(&rooms, 20, 1000);
    let wave = rooms.into_native_observation_wave("projection".into());
    let _reset = Reset;
    OBSERVATION_MATERIALIZE_CHECKS.with(|checks| *checks.borrow_mut() = Some(vec![]));
    let actual = materialize_wave_view(&wave, 20, 1000);
    assert_eq!(actual, expected);
    let data = actual.bot_market_data.as_ref().unwrap();
    assert_eq!(data.candles.len(), 64);
    assert_eq!(data.own_fills.len(), 72);
    assert!(!data.account_events.is_empty());
    assert_eq!(data.history_proof.as_ref().unwrap().prefix_bars, 8);
    assert!(data.private_history_complete);
    OBSERVATION_MATERIALIZE_CHECKS.with(|checks| {
        assert_eq!(
            checks.borrow().as_deref().unwrap(),
            &[
                "market_data",
                "account_events",
                "market_data",
                "market_data"
            ]
        );
    });
}

#[test]
fn observation_materialize_concurrent_accounts_match_cold_and_preserve_old_wave() {
    let mut rooms = projection_room(72);
    let old = rooms
        .clone()
        .into_native_observation_wave("projection".into());
    let accounts = [10, 20, 999];
    let intervals = [1000, 3000];
    let expected_old: Vec<_> = accounts
        .into_iter()
        .flat_map(|account| intervals.map(|interval| (account, interval)))
        .map(|(account, interval)| materialize_cold_reference(&rooms, account, interval))
        .collect();
    // Seed only the first interval; other readers race on a still-cold interval
    // and account event cache, while the older wave remains independently live.
    assert_eq!(materialize_wave_view(&old, 10, 1000), expected_old[0]);
    projection_trade(&mut rooms, 72);
    rooms.advance_clock("projection", 1).unwrap();
    let next = rooms
        .clone()
        .into_native_observation_wave_after("projection".into(), &old);
    let expected_next: Vec<_> = accounts
        .into_iter()
        .flat_map(|account| intervals.map(|interval| (account, interval)))
        .map(|(account, interval)| materialize_cold_reference(&rooms, account, interval))
        .collect();
    assert_ne!(expected_old, expected_next);
    let barrier = std::sync::Barrier::new(12);
    std::thread::scope(|scope| {
        let mut jobs = Vec::new();
        for (wave, expected) in [(&old, &expected_old), (&next, &expected_next)] {
            for (index, (account, interval)) in accounts
                .into_iter()
                .flat_map(|account| intervals.map(|interval| (account, interval)))
                .enumerate()
            {
                let barrier = &barrier;
                jobs.push(scope.spawn(move || {
                    barrier.wait();
                    for _ in 0..2 {
                        let actual = materialize_wave_view(wave, account, interval);
                        assert_eq!(actual, expected[index]);
                        assert_eq!(
                            serde_json::to_vec(&actual).unwrap(),
                            serde_json::to_vec(&expected[index]).unwrap()
                        );
                        let data = actual.bot_market_data.as_ref().unwrap();
                        assert!(data.history_proof.is_some());
                        assert!(data.private_history_complete);
                        assert_eq!(data.fill_details.len(), data.own_fills.len());
                        if account == 999 {
                            assert!(data.own_fills.is_empty());
                            assert!(data.account_events.is_empty());
                        }
                    }
                }));
            }
        }
        for job in jobs {
            job.join().unwrap();
        }
    });
}
