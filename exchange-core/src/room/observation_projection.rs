//! Derived observation history shared structurally between immutable waves.
//! This is never an authority or an archive: uncertain ancestry takes the cold
//! path, and every older wave keeps its own unchanged persistent roots.
use super::*;

impl RoomManager {
    /// Reuse only history whose complete ancestry and restored inputs are known.
    /// Public views and event stamps always belong to the new frozen room.
    pub fn into_native_observation_wave_after(
        self,
        room_id: String,
        previous: &NativeObservationWave,
    ) -> NativeObservationWave {
        let append = self.observation_append_start(&room_id, previous);
        let wave = self.into_native_observation_wave(room_id);
        let Some(start) = append else { return wave };
        // Release the directory before taking any cell lock. Older wave readers
        // may still be preparing a cell; no nested directory/cell lock is needed.
        let cells: Vec<_> = previous
            .history
            .lock()
            .unwrap()
            .iter()
            .map(|(instrument, history)| (instrument.clone(), history.clone()))
            .collect();
        for (instrument, history) in cells {
            // Clone persistent roots, never a full receipt/candle/prefix buffer.
            #[cfg(feature = "bench-observation-timing")]
            let history = crate::observation_timing::lock(
                &history,
                crate::observation_timing::Site::WaveClone,
            )
            .clone();
            #[cfg(not(feature = "bench-observation-timing"))]
            let history = history.lock().unwrap().clone();
            if let Some(history) =
                history.append_wave(&wave.rooms, &wave.room_id, &instrument, start)
            {
                wave.history
                    .lock()
                    .unwrap()
                    .insert(instrument, Arc::new(Mutex::new(history)));
            }
        }
        wave
    }

    fn observation_append_start(
        &self,
        room: &str,
        previous: &NativeObservationWave,
    ) -> Option<usize> {
        if room != previous.room_id {
            return None;
        }
        let before = &previous.rooms;
        fn same<T>(a: Option<&Arc<T>>, b: Option<&Arc<T>>) -> bool {
            match (a, b) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
        }
        if !same(
            self.restored_bot_accounts.get(room),
            before.restored_bot_accounts.get(room),
        ) {
            return None;
        }
        match (
            self.restored_bot_history.get(room),
            before.restored_bot_history.get(room),
        ) {
            (None, None) => {}
            (Some((a, ac)), Some((b, bc))) if ac == bc && Arc::ptr_eq(a, b) => {}
            _ => return None,
        }
        if self.clock(room).ok()?.market_time_ms() < before.clock(room).ok()?.market_time_ms() {
            return None;
        }
        match (self.executions.get(room), before.executions.get(room)) {
            (Some(after), Some(before)) if after.shares_append_prefix(before) => Some(before.len()),
            (Some(_), None) | (None, None) => Some(0),
            _ => None,
        }
    }
}

impl ObservationHistory {
    pub(super) fn window_proof(
        &self,
        interval: u64,
        start: usize,
    ) -> Option<crate::CandleHistoryProof> {
        let prefixes = self.candle_prefixes.get(&interval)?;
        Some(crate::CandleHistoryProof {
            version: crate::CANDLE_HISTORY_PROOF_VERSION,
            prefix_bars: start as u64,
            prefix_sha256: crate::candle_history_hex(prefixes.get(start)?),
        })
    }

    fn append_wave(
        mut self,
        rooms: &RoomManager,
        room: &str,
        instrument: &str,
        start: usize,
    ) -> Option<Self> {
        let now = rooms.clock(room).ok()?.market_time_ms();
        if !self.append_eligible || now < self.market_time_ms {
            return None;
        }
        // The source still names a live instrument; public/configuration inputs
        // are rebuilt by the new wave independently of this history projection.
        let actor = rooms.simulation_room(room).ok()?;
        actor.venue_id_for_instrument(instrument)?;
        let mut receipts = Vec::new();
        for execution in rooms.execution_history_from(room, start).ok()? {
            #[cfg(test)]
            record_work(|w| w.suffix_executions += 1);
            if execution.instrument_id == instrument {
                receipts.extend(execution_receipts(execution));
            }
        }
        let mut last = self
            .receipts
            .back()
            .map(|r| (r.trade.trade_id, r.market_time_ms));
        for receipt in &receipts {
            // A late trade may revise an already finalized bar even if its time
            // is after the last trade. Never use that as an append proof.
            if receipt.market_time_ms < self.market_time_ms
                || receipt.market_time_ms > now
                || last.is_some_and(|(id, time)| {
                    receipt.trade.trade_id <= id || receipt.market_time_ms < time
                })
            {
                return None;
            }
            last = Some((receipt.trade.trade_id, receipt.market_time_ms));
        }
        let timed: Vec<_> = receipts
            .iter()
            .map(|r| crate::TimedTrade::from_trade(r.market_time_ms, &r.trade))
            .collect();
        for (&interval, candles) in &mut self.candles {
            // Also covers saturated u64::MAX closes: a same-time append must
            // not rewrite a candle already authenticated by the old wave.
            if let (Some(closed), Some(trade)) = (candles.back(), timed.first())
                && (trade.market_time_ms / interval) * interval <= closed.open_time_ms
            {
                return None;
            }
            let mut additions = crate::aggregate_candles(&timed, interval, now).ok()?;
            let mut carry = self.forming.get(&interval).cloned().flatten();
            if let (Some(old), Some(first)) = (carry.as_mut(), additions.first())
                && old.open_time_ms == first.open_time_ms
            {
                merge_candle(old, first);
                additions.remove(0);
            }
            self.forming.insert(interval, None);
            for mut candle in carry.into_iter().chain(additions) {
                candle.is_final = now >= candle.close_time_ms;
                if candle.is_final {
                    if let Some(prefixes) = self.candle_prefixes.get_mut(&interval) {
                        let digest = crate::candle_history_append(prefixes.back()?, &candle);
                        prefixes.push_back(digest);
                        #[cfg(test)]
                        record_work(|w| w.appended_links += 1);
                    }
                    candles.push_back(candle);
                } else {
                    if self.forming[&interval].is_some() {
                        return None;
                    }
                    self.forming.insert(interval, Some(candle));
                }
            }
        }
        for receipt in receipts {
            let index = self.receipts.len();
            let trade = &receipt.trade;
            self.volume = self
                .volume
                .and_then(|v| v.checked_add(u128::from(trade.qty)));
            for (side, account) in [trade.maker_account_id, trade.taker_account_id]
                .into_iter()
                .enumerate()
            {
                if side == 1 && trade.maker_account_id == trade.taker_account_id {
                    continue;
                }
                let own = self.accounts.entry(account).or_default();
                own.volume = own.volume.saturating_add(u128::from(trade.qty));
                let buyer = if trade.taker_side == crate::Side::Buy {
                    trade.taker_account_id
                } else {
                    trade.maker_account_id
                };
                own.incomplete_fees |= if account == buyer {
                    receipt.buyer_fee.is_none()
                } else {
                    receipt.seller_fee.is_none()
                };
                own.receipt_indices.push_back(index);
            }
            self.receipts.push_back(receipt);
        }
        if let Some(index) = &mut self.account_execution_index {
            // Unchanged accounts keep their exact, bounded frame cache. Changed
            // accounts use the existing keyed merge/conflict/overflow logic.
            for (account, added) in rooms
                .bot_account_execution_index(room, instrument, start)
                .ok()?
            {
                index.entry(account).or_default().extend(added);
                if let Some(events) = &mut self.account_events {
                    events.remove(&account);
                }
            }
        }
        self.market_time_ms = now;
        self.candle_stamps.clear();
        Some(self)
    }
}

fn merge_candle(old: &mut crate::Candle, next: &crate::Candle) {
    old.high_tick = old.high_tick.max(next.high_tick);
    old.low_tick = old.low_tick.min(next.low_tick);
    old.close_tick = next.close_tick;
    old.volume = old.volume.saturating_add(next.volume);
    old.quote_volume += next.quote_volume;
    old.trades = old.trades.saturating_add(next.trades);
    old.taker_buy_base = Some(
        old.taker_buy_base
            .unwrap_or(0)
            .saturating_add(next.taker_buy_base.unwrap_or(0)),
    );
    old.taker_buy_quote =
        Some(old.taker_buy_quote.unwrap_or(0) + next.taker_buy_quote.unwrap_or(0));
}

fn execution_receipts(execution: &ActorExecution) -> Vec<crate::observation::BotTradeReceipt> {
    use crate::{Event, PerpClearingEvent, SpotClearingEvent};
    let mut fees = BTreeMap::new();
    let events = match &execution.result {
        ActorExecutionResult::Accepted(MarketExecution::Spot(result)) => {
            for event in &result.clearing_events {
                let SpotClearingEvent::TradeSettled {
                    trade_id,
                    buyer_fee,
                    seller_fee,
                    ..
                } = event;
                fees.insert(*trade_id, (*buyer_fee, *seller_fee));
            }
            &result.events
        }
        ActorExecutionResult::Accepted(MarketExecution::Perp(result)) => {
            for event in &result.clearing_events {
                if let PerpClearingEvent::TradeSettled {
                    trade_id,
                    buyer_fee,
                    seller_fee,
                    ..
                } = event
                {
                    fees.insert(*trade_id, (*buyer_fee, *seller_fee));
                }
            }
            &result.events
        }
        ActorExecutionResult::Rejected(_) => return Vec::new(),
    };
    events
        .iter()
        .filter_map(|record| {
            let Event::TradePrinted(trade) = &record.event else {
                return None;
            };
            let fee = fees.get(&trade.trade_id);
            Some(crate::observation::BotTradeReceipt {
                instrument_id: execution.instrument_id.clone(),
                market_time_ms: execution.market_time_ms,
                trade: trade.clone(),
                buyer_fee: fee.map(|v| v.0),
                seller_fee: fee.map(|v| v.1),
            })
        })
        .collect()
}

#[cfg(test)]
#[derive(Clone, Copy, Default, Debug)]
pub(super) struct Work {
    pub cold_histories: usize,
    pub cold_intervals: usize,
    pub index_entries: usize,
    pub suffix_executions: usize,
    pub appended_links: usize,
}
#[cfg(test)]
thread_local! { static WORK: std::cell::Cell<Work> = std::cell::Cell::new(Work::default()); }
#[cfg(test)]
pub(super) fn record_work(f: impl FnOnce(&mut Work)) {
    WORK.with(|cell| {
        let mut work = cell.get();
        f(&mut work);
        cell.set(work);
    });
}
#[cfg(test)]
pub(super) fn take_work() -> Work {
    WORK.with(|cell| cell.replace(Work::default()))
}
