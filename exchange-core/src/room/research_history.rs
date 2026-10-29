//! Explicit research-only compaction after complete executions have been archived.
use super::*;

impl RoomManager {
    /// Discard duplicated execution/engine archives, retaining exact bot inputs.
    /// The caller must first persist *all* execution records and release older
    /// observation waves. Production schedulers never call this archive API.
    /// Returns the number of manager records removed. Market/order/event cursors,
    /// balances, prices and active trigger event suffixes are preserved.
    pub fn compact_archived_research_history(
        &mut self,
        room_id: &str,
    ) -> Result<usize, RoomManagerError> {
        let instruments = self.simulation_room(room_id)?.instrument_ids();
        let mut receipts = Vec::new();
        let mut accounts = Vec::new();
        let mut complete = true;
        if let Some((restored, _)) = self.restored_bot_history.get(room_id) {
            let frames: BTreeSet<_> = self
                .restored_bot_accounts
                .get(room_id)
                .into_iter()
                .flat_map(|events| events.iter())
                .map(|e| (e.instrument_id.as_str(), e.account.account_id()))
                .collect();
            if restored.iter().any(|r| {
                [r.trade.maker_account_id, r.trade.taker_account_id]
                    .iter()
                    .any(|id| !frames.contains(&(r.instrument_id.as_str(), *id)))
            }) {
                return Err(RoomManagerError::ResearchHistoryIncomplete);
            }
        }
        for instrument in &instruments {
            let (mut trades, exact) = self.bot_trade_history(room_id, instrument)?;
            if !exact {
                return Err(RoomManagerError::ResearchHistoryIncomplete);
            }
            complete &= exact;
            receipts.append(&mut trades);
            let mut incomplete = BTreeSet::new();
            accounts.extend(
                self.bot_account_event_map(room_id, instrument, None, None, Some(&mut incomplete))?
                    .into_values()
                    .flatten(),
            );
            if !incomplete.is_empty() {
                return Err(RoomManagerError::ResearchHistoryIncomplete);
            }
        }
        // Public recent-trade fallback reads the global reverse order. The
        // per-market order is trade-id order, exactly as in bot_trade_history.
        receipts.sort_by(|a, b| {
            a.instrument_id
                .cmp(&b.instrument_id)
                .then(a.trade.trade_id.cmp(&b.trade.trade_id))
        });
        let removed = self.execution_history_len(room_id)?;
        self.restore_bot_history(room_id, receipts, complete);
        self.restore_bot_accounts(room_id, accounts);
        self.executions
            .insert(room_id.to_owned(), History::default());
        self.simulation_room_mut(room_id)?
            .compact_archived_history();
        Ok(removed)
    }
}
