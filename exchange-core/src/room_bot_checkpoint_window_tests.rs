fn checkpoint_window_fixture() -> RoomManager {
    let mut rooms = restored_history_room();
    let receipts = rooms.restored_bot_history["cached-history"].0.as_ref().clone();
    rooms.restore_bot_history("cached-history", receipts, true);
    rooms
}

fn committed_window_cursor(rooms: &RoomManager, interval_ms: u64, count: usize) -> crate::bots::BotHistoryCursor {
    let mut batch = rooms.observation_batch("cached-history");
    let history = batch.history_for("V-BTC-SPOT").unwrap();
    let mut history = history.lock().unwrap();
    history.prepare_candles(interval_ms, rooms.clock("cached-history").unwrap().market_time_ms()).unwrap();
    crate::bots::BotHistoryCursor {
        instrument_id: "V-BTC-SPOT".into(), interval_ms, count: count as u64,
        digest: history.candle_prefixes[&interval_ms][count],
    }
}

#[test]
fn checkpoint_window_reduces_manifest_2049_and_expands_for_every_pending_bar() {
    let rooms = checkpoint_window_fixture();
    let recent = committed_window_cursor(&rooms, 1000, 4199);
    let delayed = committed_window_cursor(&rooms, 1000, 4120);
    let mut batch = rooms.observation_batch("cached-history");
    let legacy_request = crate::bots::BotMarketDataRequest { interval_ms: 1000, max_bars: 2049 };
    let legacy = batch.bot_observation("V-BTC-SPOT", 30, Some(legacy_request), &[])
        .unwrap().bot_market_data.unwrap();
    assert_eq!(legacy.candles.len(), 2049, "exercise the actual installed manifest size");
    for (cursor, expected) in [(recent, 64), (delayed, 80)] {
        let maximum = batch.checkpoint_window_max_bars(std::slice::from_ref(&cursor)).unwrap();
        assert_eq!(maximum, expected);
        let request = crate::bots::BotMarketDataRequest { max_bars: maximum, ..legacy_request };
        request.validate().unwrap();
        let data = batch.bot_observation("V-BTC-SPOT", 30, Some(request), &[])
            .unwrap().bot_market_data.unwrap();
        assert_eq!(data.candles.len(), expected);
        assert_eq!(data.candles.last(), legacy.candles.last());
        let proof = data.history_proof.unwrap();
        assert!(proof.prefix_bars <= cursor.count, "the complete pending range must be visible");
        let verified = data.candles[..(cursor.count - proof.prefix_bars) as usize].iter()
            .fold(proof.prefix_digest().unwrap(), |digest, candle| crate::candle_history_append(&digest, candle));
        assert_eq!(verified, cursor.digest);
    }
    assert_eq!(batch.history["V-BTC-SPOT"].lock().unwrap().candle_prefixes.len(), 1);
}

#[test]
fn checkpoint_window_uses_the_slowest_stream_and_obeys_the_existing_cap() {
    let rooms = checkpoint_window_fixture();
    let primary = committed_window_cursor(&rooms, 1000, 4199);
    let additional = committed_window_cursor(&rooms, 2000, 2000);
    let at_cap = committed_window_cursor(&rooms, 1000, 104);
    let beyond_cap = committed_window_cursor(&rooms, 1000, 103);
    let mut batch = rooms.observation_batch("cached-history");
    assert_eq!(batch.checkpoint_window_max_bars(&[primary, additional]).unwrap(), 100);
    let max_bars = batch.checkpoint_window_max_bars(&[at_cap]).unwrap();
    assert_eq!(max_bars, crate::bots::MAX_BOT_MARKET_DATA_BARS);
    crate::bots::BotMarketDataRequest { interval_ms: 1000, max_bars }.validate().unwrap();
    let error = batch.checkpoint_window_max_bars(&[beyond_cap]).unwrap_err();
    assert_eq!(error.0, "checkpoint candle catch-up exceeds bot market data bounds");
}

#[test]
fn checkpoint_window_refuses_old_corrections_missing_proof_and_future_cursors() {
    let mut rooms = checkpoint_window_fixture();
    let cursor = committed_window_cursor(&rooms, 1000, 4199);
    let request = Some(crate::bots::BotMarketDataRequest { interval_ms: 1000, max_bars: 64 });
    let before = rooms.bot_observation("cached-history", "V-BTC-SPOT", 30, request)
        .unwrap().bot_market_data.unwrap().candles;
    let mut corrected = rooms.restored_bot_history["cached-history"].0.as_ref().clone();
    corrected[0].trade.price_tick += 1;
    rooms.restore_bot_history("cached-history", corrected.clone(), true);
    let after = rooms.bot_observation("cached-history", "V-BTC-SPOT", 30, request)
        .unwrap().bot_market_data.unwrap().candles;
    assert_eq!(before, after, "a 64-bar overlap would miss this correction");
    assert!(rooms.observation_batch("cached-history").checkpoint_window_max_bars(std::slice::from_ref(&cursor))
        .unwrap_err().0.contains("prefix changed"));
    rooms.restore_bot_history("cached-history", corrected, false);
    assert!(rooms.observation_batch("cached-history").checkpoint_window_max_bars(std::slice::from_ref(&cursor))
        .unwrap_err().0.contains("complete authoritative history"));
    let rooms = checkpoint_window_fixture();
    let mut future = cursor;
    future.count = 4201;
    assert!(rooms.observation_batch("cached-history").checkpoint_window_max_bars(&[future])
        .unwrap_err().0.contains("prefix changed or disappeared"));
}
