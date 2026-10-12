impl super::BrokerState {
    /// Borrow public broker histories without copying event records.
    #[must_use]
    pub fn result_view(&self) -> crate::StrategyResultView<'_> {
        crate::StrategyResultView {
            orders: crate::HistoryView::persistent(self.orders.history(), 0),
            trades: crate::HistoryView::persistent(self.trades.history(), 0),
            position: crate::HistoryView::persistent(self.position.history(), 0),
            equity: crate::HistoryView::persistent(self.equity.history(), 0),
            alerts: crate::HistoryView::persistent(self.order_fill_alerts.history(), 0),
            diagnostics: &self.diagnostics,
        }
    }
}
