use super::{BrokerState, EntryPathTick};
use crate::RuntimeError;
use crate::runtime::strategy_path::{HistoricalPathKind, PathLeg};

impl BrokerState {
    /// Consume orders already present before script execution. The scheduler
    /// supplies the qualified market-order price for a one-sided range update;
    /// price-condition orders still use the observed close without interpolation.
    pub(crate) fn process_realtime_tick(
        &mut self,
        bar_index: usize,
        time: i64,
        price: f64,
        market_price: f64,
        historical_same_bar_fills: bool,
    ) -> Result<bool, RuntimeError> {
        let before = self.public_fill_event_count();
        self.fill_pending_market_closes(bar_index, time, market_price);
        self.fill_same_bar_market_closes(bar_index, time, market_price);
        self.fill_pending_market_entries(bar_index, time, market_price);
        self.fill_same_bar_market_entries(bar_index, time, market_price);
        self.order_book
            .entries_mut()
            .set_allow_same_bar_price_fills(true);
        self.order_book
            .exits_mut()
            .set_allow_same_bar_price_fills(true);
        let tick = EntryPathTick {
            bar_index,
            time,
            leg: PathLeg::point(price),
            path_kind: HistoricalPathKind::OpenHighLowClose,
            mark: price,
            long_blocked_at_path_start: self.same_side_long_entry_blocked(),
            short_blocked_at_path_start: self.same_side_short_entry_blocked(),
        };
        let mut exhausted = true;
        for _ in 0..10_000 {
            if self.take_next_realtime_event(tick).is_none() {
                exhausted = false;
                break;
            }
        }
        self.order_book
            .entries_mut()
            .set_allow_same_bar_price_fills(historical_same_bar_fills);
        self.order_book
            .exits_mut()
            .set_allow_same_bar_price_fills(historical_same_bar_fills);
        if exhausted {
            return Err(RuntimeError {
                message: format!("strategy realtime event loop made no progress: bar {bar_index}"),
            });
        }
        self.update_open_trade_extremes(price, price);
        self.evaluate_risk_equity_stops(bar_index, time, price);
        self.evaluate_margin_call_long(bar_index, time, price);
        self.evaluate_margin_call_short(bar_index, time, price);
        self.flatten_if_risk_blocked(bar_index, time, price);
        Ok(self.public_fill_event_count() > before)
    }
}
