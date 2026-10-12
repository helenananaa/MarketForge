use pine_ir::{
    DEFAULT_STRATEGY_INITIAL_CAPITAL, StrategyCloseEntriesRule, StrategyCommission,
    StrategyMarginSetting,
};

use super::{
    BrokerState, StrategyExitMetadata, StrategyOrderMetadata, ledger::TradeLedger,
    order_book::OrderBook,
};
use crate::{StrategyOrderFillAlertOutput, StrategyResult};

impl Default for BrokerState {
    fn default() -> Self {
        Self::new(DEFAULT_STRATEGY_INITIAL_CAPITAL)
    }
}

impl BrokerState {
    #[cfg(test)]
    #[must_use]
    pub(crate) fn snapshot(&self) -> Self {
        self.clone()
    }

    #[cfg(test)]
    pub(crate) fn restore(&mut self, snapshot: Self) {
        *self = snapshot;
    }

    #[must_use]
    pub fn new(initial_capital: f64) -> Self {
        Self::new_with_commission(initial_capital, None)
    }

    #[must_use]
    pub fn new_with_cash_per_contract_commission(
        initial_capital: f64,
        commission_per_contract: f64,
    ) -> Self {
        Self::new_with_commission(
            initial_capital,
            Some(StrategyCommission::CashPerContract(commission_per_contract)),
        )
    }

    #[must_use]
    pub fn new_with_commission(
        initial_capital: f64,
        commission: Option<StrategyCommission>,
    ) -> Self {
        Self::new_with_commission_and_slippage(initial_capital, commission, 0.0)
    }

    #[must_use]
    pub fn new_with_commission_and_slippage(
        initial_capital: f64,
        commission: Option<StrategyCommission>,
        slippage_price_offset: f64,
    ) -> Self {
        Self::new_with_commission_slippage_and_limit_verification(
            initial_capital,
            commission,
            slippage_price_offset,
            0.0,
        )
    }

    #[must_use]
    pub fn new_with_commission_slippage_and_limit_verification(
        initial_capital: f64,
        commission: Option<StrategyCommission>,
        slippage_price_offset: f64,
        limit_verification_price_offset: f64,
    ) -> Self {
        Self::new_with_account_settings(
            initial_capital,
            commission,
            slippage_price_offset,
            limit_verification_price_offset,
            StrategyMarginSetting::default(),
            StrategyMarginSetting::default(),
        )
    }

    #[must_use]
    pub fn new_with_account_settings(
        initial_capital: f64,
        commission: Option<StrategyCommission>,
        slippage_price_offset: f64,
        limit_verification_price_offset: f64,
        margin_long: StrategyMarginSetting,
        margin_short: StrategyMarginSetting,
    ) -> Self {
        Self::new_with_account_settings_and_pyramiding(
            initial_capital,
            commission,
            slippage_price_offset,
            limit_verification_price_offset,
            margin_long,
            margin_short,
            1,
        )
    }

    #[must_use]
    pub fn new_with_account_settings_and_pyramiding(
        initial_capital: f64,
        commission: Option<StrategyCommission>,
        slippage_price_offset: f64,
        limit_verification_price_offset: f64,
        margin_long: StrategyMarginSetting,
        margin_short: StrategyMarginSetting,
        pyramiding_limit: usize,
    ) -> Self {
        Self {
            initial_capital,
            commission,
            pyramiding_limit,
            close_entries_rule: StrategyCloseEntriesRule::Fifo,
            margin_long,
            margin_short,
            open_entry_commission: 0.0,
            quantity_scale: 1,
            configured_quantity_scale: None,
            price_tick: None,
            slippage_price_offset,
            limit_verification_price_offset,
            cash: initial_capital,
            realized_profit_sum: 0.0,
            position_size: 0.0,
            avg_price: 0.0,
            next_close_metadata: StrategyOrderMetadata::default(),
            next_exit_metadata: StrategyExitMetadata::default(),
            next_exit_oca_name: None,
            entry_id: None,
            position_entry_name: None,
            entry_bar_index: None,
            entry_time: None,
            open_trade_max_high: None,
            open_trade_min_low: None,
            open_trade_equity_on_entry: None,
            open_trade_min_equity_before_entry: None,
            open_trade_max_equity_before_entry: None,
            min_equity_before_open_trade: initial_capital,
            max_equity_before_open_trade: initial_capital,
            max_runup: 0.0,
            max_runup_percent: 0.0,
            max_drawdown: 0.0,
            max_drawdown_percent: 0.0,
            max_contracts_held_long: 0.0,
            max_contracts_held_short: 0.0,
            orders: Default::default(),
            order_fill_alerts: Default::default(),
            trades: Default::default(),
            closed_trade_metrics: Default::default(),
            position: Default::default(),
            equity: Default::default(),
            diagnostics: Vec::new(),
            order_book: OrderBook::new(),
            trade_ledger: TradeLedger::default(),
            risk_rules: super::risk::StrategyRiskRules::default(),
            risk_state: super::risk::StrategyRiskState::default(),
            event_generation: 0,
        }
    }

    #[must_use]
    pub(crate) fn with_close_entries_rule(
        mut self,
        close_entries_rule: StrategyCloseEntriesRule,
    ) -> Self {
        self.close_entries_rule = close_entries_rule;
        self
    }

    pub(crate) fn with_calc_on_order_fills(mut self, calc_on_order_fills: bool) -> Self {
        self.order_book
            .entries_mut()
            .set_allow_same_bar_price_fills(calc_on_order_fills);
        self.order_book
            .exits_mut()
            .set_allow_same_bar_price_fills(calc_on_order_fills);
        self
    }

    pub(crate) fn with_quantity_scale(mut self, scale: u32) -> Self {
        debug_assert!(scale > 0);
        self.quantity_scale = scale;
        self
    }

    pub(crate) fn with_configured_quantity_scale(mut self, scale: Option<u32>) -> Self {
        self.configured_quantity_scale = scale;
        self
    }

    pub(crate) fn with_price_tick(mut self, tick: f64) -> Self {
        debug_assert!(tick.is_finite() && tick > 0.0);
        self.price_tick = Some(tick);
        self
    }

    fn commission_for_fill(&self, qty: f64, price: f64) -> f64 {
        match self.commission {
            Some(StrategyCommission::CashPerContract(value)) => qty * value,
            Some(StrategyCommission::CashPerOrder(value)) => value,
            Some(StrategyCommission::Percent(value)) => qty * price * (value / 100.0),
            None => 0.0,
        }
    }

    pub(super) fn entry_commission_for_fill(&self, qty: f64, price: f64) -> f64 {
        self.commission_for_fill(qty, price)
    }

    pub(super) fn exit_commission_for_fill(&self, qty: f64, price: f64) -> f64 {
        self.commission_for_fill(qty, price)
    }

    pub(crate) fn with_next_exit_metadata<T>(
        &mut self,
        metadata: StrategyExitMetadata,
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let previous = std::mem::replace(&mut self.next_exit_metadata, metadata);
        let result = f(self);
        self.next_exit_metadata = previous;
        result
    }

    pub(super) fn take_next_exit_metadata(&mut self) -> StrategyExitMetadata {
        std::mem::take(&mut self.next_exit_metadata)
    }

    pub(crate) fn set_next_exit_oca_name(&mut self, name: Option<String>) {
        self.next_exit_oca_name = name.filter(|name| !name.is_empty());
    }

    pub(super) fn current_exit_oca_name(&self) -> Option<&str> {
        self.next_exit_oca_name.as_deref()
    }

    pub(crate) fn with_next_close_metadata<T>(
        &mut self,
        metadata: StrategyOrderMetadata,
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let previous = std::mem::replace(&mut self.next_close_metadata, metadata);
        let result = f(self);
        self.next_close_metadata = previous;
        result
    }

    pub(super) fn take_next_close_metadata(&mut self) -> StrategyOrderMetadata {
        std::mem::take(&mut self.next_close_metadata)
    }

    pub(super) fn entry_commission_for_closed_quantity(&self, qty: f64) -> f64 {
        let open_qty = self.position_size.abs();
        if !qty.is_finite() || qty <= 0.0 || open_qty <= 0.0 {
            0.0
        } else if qty >= open_qty {
            self.open_entry_commission
        } else {
            self.open_entry_commission * (qty / open_qty)
        }
    }

    pub(super) fn long_entry_fill_price(&self, price: f64) -> f64 {
        self.snap_fill_price(price) + self.slippage_price_offset
    }

    pub(super) fn short_entry_fill_price(&self, price: f64) -> f64 {
        self.snap_fill_price(price) - self.slippage_price_offset
    }

    pub(super) fn short_exit_fill_price(&self, price: f64) -> f64 {
        self.snap_fill_price(price) + self.slippage_price_offset
    }

    pub(super) fn long_exit_fill_price(&self, price: f64) -> f64 {
        self.snap_fill_price(price) - self.slippage_price_offset
    }

    fn snap_fill_price(&self, price: f64) -> f64 {
        let Some(tick) = self.price_tick else {
            return price;
        };
        if !price.is_finite() {
            return price;
        }
        let ticks = price / tick;
        if (ticks - ticks.round()).abs() <= 1e-8 {
            return price;
        }
        (ticks + 0.5).floor() * tick
    }

    #[allow(dead_code)]
    pub(super) fn long_limit_exit_is_verified(&self, limit_price: f64, high: f64) -> bool {
        high >= limit_price + self.limit_verification_price_offset
    }

    pub(crate) fn public_fill_event_count(&self) -> usize {
        self.orders.len() + self.trades.len()
    }

    #[must_use]
    pub fn result(&self) -> StrategyResult {
        StrategyResult {
            orders: self.orders.to_vec(),
            trades: self.trades.to_vec(),
            position: self.position.to_vec(),
            equity: self.equity.to_vec(),
            alerts: self.fill_alerts_from(0),
            diagnostics: self.diagnostics.clone(),
        }
    }

    pub(crate) fn order_len(&self) -> usize {
        self.orders.len()
    }

    pub(crate) fn trade_len(&self) -> usize {
        self.trades.len()
    }

    pub(crate) fn position_len(&self) -> usize {
        self.position.len()
    }

    pub(crate) fn equity_len(&self) -> usize {
        self.equity.len()
    }

    pub(crate) fn order_tail(&self, start: usize) -> Vec<crate::StrategyOrderEvent> {
        self.orders.tail(start)
    }

    pub(crate) fn trade_tail(&self, start: usize) -> Vec<crate::StrategyTrade> {
        self.trades.tail(start)
    }

    pub(crate) fn position_tail(&self, start: usize) -> Vec<crate::StrategyPositionSnapshot> {
        self.position.tail(start)
    }

    pub(crate) fn equity_tail(&self, start: usize) -> Vec<crate::StrategyEquitySnapshot> {
        self.equity.tail(start)
    }

    pub(crate) fn fill_alert_len(&self) -> usize {
        self.order_fill_alerts.len()
    }

    pub(crate) fn diagnostics_slice(&self) -> &[crate::RuntimeDiagnostic] {
        &self.diagnostics
    }

    pub(crate) fn fill_alerts_from(&self, start: usize) -> Vec<StrategyOrderFillAlertOutput> {
        self.order_fill_alerts
            .iter()
            .skip(start)
            .map(|event| StrategyOrderFillAlertOutput {
                id: event.id.clone(),
                bar_index: event.bar_index,
                time: event.time,
                direction: event.direction.clone(),
                qty: event.qty,
                price: event.price,
                entry_id: event.entry_id.clone(),
                exit_id: event.exit_id.clone(),
                message: event.message.clone(),
            })
            .collect()
    }
}
