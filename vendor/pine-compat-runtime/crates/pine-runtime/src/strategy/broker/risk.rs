use std::collections::BTreeSet;

use super::pending_entries::{PendingEntryDirection, PendingEntryKind};
use super::types::StrategyCommandOrigin;

/// Broker policy configured by `strategy.risk.*` calls. 22b accepts
/// `strategy.risk.allow_entry_in`; 22c accepts `strategy.risk.max_position_size`;
/// 22d accepts `strategy.risk.max_drawdown`. 22e owns the intraday window key
/// and reset/baseline state. 22f accepts `strategy.risk.max_intraday_loss` and
/// `strategy.risk.max_intraday_filled_orders`. 22g accepts
/// `strategy.risk.max_cons_loss_days`. Other risk calls stay rejected.
#[derive(Debug, Clone, Default, PartialEq)]
#[allow(dead_code)]
pub(crate) struct StrategyRiskRules {
    pub allow_entry_direction: Option<RiskEntryDirection>,
    pub max_drawdown: Option<f64>,
    pub max_drawdown_type: Option<RiskDrawdownType>,
    pub max_drawdown_alert_message: Option<String>,
    pub max_intraday_loss: Option<f64>,
    pub max_intraday_loss_type: Option<RiskDrawdownType>,
    pub max_intraday_loss_alert_message: Option<String>,
    pub max_position_size: Option<f64>,
    pub max_intraday_filled_orders: Option<u32>,
    pub max_intraday_filled_orders_alert_message: Option<String>,
    pub max_cons_loss_days: Option<u32>,
    pub max_cons_loss_days_alert_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RiskDrawdownType {
    Cash,
    PercentOfEquity,
}

impl RiskDrawdownType {
    pub(crate) fn from_strategy_constant(value: &str) -> Option<Self> {
        match value {
            "strategy.cash" => Some(Self::Cash),
            "strategy.percent_of_equity" => Some(Self::PercentOfEquity),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RiskEntryDirection {
    Long,
    Short,
    All,
}

impl RiskEntryDirection {
    pub(crate) fn from_strategy_constant(value: &str) -> Option<Self> {
        match value {
            "strategy.direction.long" => Some(Self::Long),
            "strategy.direction.short" => Some(Self::Short),
            "strategy.direction.all" => Some(Self::All),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[allow(dead_code)]
pub(crate) enum RiskRuleKind {
    AllowEntryIn,
    MaxDrawdown,
    MaxIntradayLoss,
    MaxPositionSize,
    MaxIntradayFilledOrders,
    MaxConsLossDays,
}

impl RiskRuleKind {
    pub(crate) fn is_window_scoped(self) -> bool {
        matches!(self, Self::MaxIntradayLoss | Self::MaxIntradayFilledOrders)
    }

    fn is_permanent_stop(self) -> bool {
        matches!(self, Self::MaxDrawdown | Self::MaxConsLossDays)
    }
}

/// Triggered and windowed risk state, stored separately from configuration.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct StrategyRiskState {
    #[allow(dead_code)]
    pub tripped_rules: BTreeSet<RiskRuleKind>,
    pub blocked_order_placement: bool,
    pub intraday_filled_orders: u32,
    #[allow(dead_code)]
    pub intraday_equity_baseline: Option<f64>,
    pub window_key: Option<RiskWindowKey>,
    pub trading_day_key: Option<RiskWindowKey>,
    pub window_realized_pnl: f64,
    pub consecutive_loss_days: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RiskWindowKey {
    Utc(i64),
    Host(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RiskAdmission {
    Allow,
    Reject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntryDirectionAdmission {
    Allow,
    CloseOnly,
    Reject,
}

impl StrategyRiskState {
    pub(crate) fn trip(&mut self, rule: RiskRuleKind) {
        self.tripped_rules.insert(rule);
        self.blocked_order_placement = true;
    }
}

const MILLIS_PER_UTC_DAY: i64 = 86_400_000;
const SECONDS_PER_UTC_DAY: i64 = 86_400;

pub(crate) fn trading_day_key(time_ms: i64) -> i64 {
    time_ms.div_euclid(MILLIS_PER_UTC_DAY)
}

/// Host-neutral intraday window key. Chart timeframes at or below 1D use the
/// UTC day of `time_ms`. Timeframes higher than 1D use the bar timestamp so
/// one chart bar is one window. Non-positive `timeframe_seconds` fail closed
/// to the UTC-day key; this runtime has no session calendar.
pub(crate) fn intraday_window_key(time_ms: i64, timeframe_seconds: i64) -> i64 {
    if timeframe_seconds > SECONDS_PER_UTC_DAY {
        time_ms
    } else {
        trading_day_key(time_ms)
    }
}

impl super::BrokerState {
    pub(crate) fn set_allow_entry_in(&mut self, value: &str) {
        let Some(direction) = RiskEntryDirection::from_strategy_constant(value) else {
            return;
        };
        self.record_risk_rule_call(
            RiskRuleKind::AllowEntryIn,
            Some(direction),
            None,
            None,
            None,
            None,
        );
    }

    pub(crate) fn set_max_drawdown(
        &mut self,
        value: f64,
        type_name: &str,
        alert_message: Option<String>,
    ) {
        let Some(kind) = RiskDrawdownType::from_strategy_constant(type_name) else {
            return;
        };
        if !value.is_finite() || value <= 0.0 {
            return;
        }
        if kind == RiskDrawdownType::PercentOfEquity && value > 100.0 {
            return;
        }
        self.risk_rules.max_drawdown = Some(value);
        self.risk_rules.max_drawdown_type = Some(kind);
        self.risk_rules.max_drawdown_alert_message = alert_message;
    }

    pub(crate) fn set_max_position_size(&mut self, contracts: f64) {
        if !contracts.is_finite() || contracts <= 0.0 {
            return;
        }
        self.record_risk_rule_call(
            RiskRuleKind::MaxPositionSize,
            None,
            None,
            None,
            Some(contracts),
            None,
        );
    }

    pub(crate) fn set_max_intraday_loss(
        &mut self,
        value: f64,
        type_name: &str,
        alert_message: Option<String>,
    ) {
        let Some(kind) = RiskDrawdownType::from_strategy_constant(type_name) else {
            return;
        };
        if !value.is_finite() || value <= 0.0 {
            return;
        }
        if kind == RiskDrawdownType::PercentOfEquity && value > 100.0 {
            return;
        }
        self.risk_rules.max_intraday_loss = Some(value);
        self.risk_rules.max_intraday_loss_type = Some(kind);
        self.risk_rules.max_intraday_loss_alert_message = alert_message;
    }

    pub(crate) fn set_max_intraday_filled_orders(
        &mut self,
        count: f64,
        alert_message: Option<String>,
    ) {
        let Some(limit) = positive_filled_order_limit(count) else {
            return;
        };
        self.risk_rules.max_intraday_filled_orders = Some(limit);
        self.risk_rules.max_intraday_filled_orders_alert_message = alert_message;
    }

    pub(crate) fn set_max_cons_loss_days(&mut self, count: f64, alert_message: Option<String>) {
        let Some(limit) = positive_filled_order_limit(count) else {
            return;
        };
        self.risk_rules.max_cons_loss_days = Some(limit);
        self.risk_rules.max_cons_loss_days_alert_message = alert_message;
    }

    #[allow(dead_code)]
    pub(crate) fn record_risk_rule_call(
        &mut self,
        kind: RiskRuleKind,
        allow_entry_direction: Option<RiskEntryDirection>,
        max_drawdown: Option<f64>,
        max_intraday_loss: Option<f64>,
        max_position_size: Option<f64>,
        max_intraday_filled_orders: Option<u32>,
    ) {
        match kind {
            RiskRuleKind::AllowEntryIn => {
                self.risk_rules.allow_entry_direction = allow_entry_direction;
                self.apply_allow_entry_in_to_pending_entries();
            }
            RiskRuleKind::MaxDrawdown => {
                self.risk_rules.max_drawdown = max_drawdown;
                if max_drawdown.is_some() && self.risk_rules.max_drawdown_type.is_none() {
                    self.risk_rules.max_drawdown_type = Some(RiskDrawdownType::Cash);
                }
            }
            RiskRuleKind::MaxIntradayLoss => self.risk_rules.max_intraday_loss = max_intraday_loss,
            RiskRuleKind::MaxPositionSize => {
                self.risk_rules.max_position_size = max_position_size;
                self.apply_max_position_size_to_pending_entries();
            }
            RiskRuleKind::MaxIntradayFilledOrders => {
                self.risk_rules.max_intraday_filled_orders = max_intraday_filled_orders;
            }
            RiskRuleKind::MaxConsLossDays => {}
        }
    }

    #[allow(dead_code)]
    pub(crate) fn trip_risk_rule(&mut self, kind: RiskRuleKind) {
        self.risk_state.trip(kind);
    }

    pub(crate) fn check_risk_before_order(&self) -> RiskAdmission {
        if self.risk_state.blocked_order_placement {
            RiskAdmission::Reject
        } else {
            RiskAdmission::Allow
        }
    }

    pub(super) fn entry_direction_admission(
        &self,
        direction: PendingEntryDirection,
    ) -> EntryDirectionAdmission {
        if self.risk_state.blocked_order_placement {
            return EntryDirectionAdmission::Reject;
        }
        let Some(allowed) = self.risk_rules.allow_entry_direction else {
            return EntryDirectionAdmission::Allow;
        };
        match (allowed, direction) {
            (RiskEntryDirection::All, _)
            | (RiskEntryDirection::Long, PendingEntryDirection::Long)
            | (RiskEntryDirection::Short, PendingEntryDirection::Short) => {
                EntryDirectionAdmission::Allow
            }
            (RiskEntryDirection::Long, PendingEntryDirection::Short) => {
                if self.position_size > 0.0 {
                    EntryDirectionAdmission::CloseOnly
                } else {
                    EntryDirectionAdmission::Reject
                }
            }
            (RiskEntryDirection::Short, PendingEntryDirection::Long) => {
                if self.position_size < 0.0 {
                    EntryDirectionAdmission::CloseOnly
                } else {
                    EntryDirectionAdmission::Reject
                }
            }
        }
    }

    pub(super) fn gate_strategy_entry(
        &self,
        direction: PendingEntryDirection,
        kind: PendingEntryKind,
    ) -> Option<PendingEntryKind> {
        match self.entry_direction_admission(direction) {
            EntryDirectionAdmission::Reject => None,
            EntryDirectionAdmission::CloseOnly => Some(PendingEntryKind::Market),
            EntryDirectionAdmission::Allow => Some(kind),
        }
    }

    pub(super) fn strategy_entry_room(&self, direction: PendingEntryDirection) -> Option<f64> {
        let max = self.risk_rules.max_position_size?;
        if !max.is_finite() || max <= 0.0 {
            return Some(0.0);
        }
        let opposite = match direction {
            PendingEntryDirection::Long => self.position_size < 0.0,
            PendingEntryDirection::Short => self.position_size > 0.0,
        };
        if opposite {
            Some(max)
        } else {
            Some((max - self.position_size.abs()).max(0.0))
        }
    }

    pub(super) fn clamp_strategy_entry_qty(
        &self,
        direction: PendingEntryDirection,
        qty: f64,
    ) -> Option<f64> {
        let Some(room) = self.strategy_entry_room(direction) else {
            return Some(qty);
        };
        if room <= 0.0 {
            None
        } else if qty.is_finite() && qty > 0.0 {
            Some(qty.min(room))
        } else {
            Some(qty)
        }
    }

    fn apply_max_position_size_to_pending_entries(&mut self) {
        let mut cancel = Vec::new();
        let mut reduce = Vec::new();
        for pending in self.order_book.entries().iter() {
            if pending.origin != StrategyCommandOrigin::Entry {
                continue;
            }
            match self.clamp_strategy_entry_qty(pending.direction, pending.quantity) {
                None => cancel.push(pending.key),
                Some(qty) if qty != pending.quantity => reduce.push((pending.key, qty)),
                Some(_) => {}
            }
        }
        for key in cancel {
            self.order_book.entries_mut().remove_by_key(key);
        }
        for (key, qty) in reduce {
            if let Some(pending) = self.order_book.entries_mut().find_mut_by_key(key) {
                pending.quantity = qty;
            }
        }
    }

    fn apply_allow_entry_in_to_pending_entries(&mut self) {
        let long = self.entry_direction_admission(PendingEntryDirection::Long);
        let short = self.entry_direction_admission(PendingEntryDirection::Short);
        let mut cancel = Vec::new();
        let mut convert = Vec::new();
        for pending in self.order_book.entries().iter() {
            if pending.origin != StrategyCommandOrigin::Entry {
                continue;
            }
            let admission = match pending.direction {
                PendingEntryDirection::Long => long,
                PendingEntryDirection::Short => short,
            };
            match admission {
                EntryDirectionAdmission::Allow => {}
                EntryDirectionAdmission::CloseOnly => convert.push(pending.key),
                EntryDirectionAdmission::Reject => cancel.push(pending.key),
            }
        }
        for key in cancel {
            self.order_book.entries_mut().remove_by_key(key);
        }
        for key in convert {
            if let Some(pending) = self.order_book.entries_mut().find_mut_by_key(key) {
                pending.kind = PendingEntryKind::Market;
            }
        }
    }

    fn peak_equity_for_drawdown(&self) -> f64 {
        self.max_equity_before_open_trade
            .max(self.open_trade_max_equity_before_entry.unwrap_or(0.0))
            .max(self.initial_capital)
    }

    fn current_risk_drawdown_amount(&self, mark: f64) -> f64 {
        let equity = self.equity_value(mark);
        let peak = self.peak_equity_for_drawdown();
        let mark_drawdown = if peak.is_finite() && equity.is_finite() {
            (peak - equity).max(0.0)
        } else {
            0.0
        };
        self.max_drawdown().max(mark_drawdown)
    }

    fn max_drawdown_rule_tripped(&self, mark: f64) -> bool {
        let Some(limit) = self.risk_rules.max_drawdown else {
            return false;
        };
        let Some(kind) = self.risk_rules.max_drawdown_type else {
            return false;
        };
        match kind {
            RiskDrawdownType::Cash => self.current_risk_drawdown_amount(mark) >= limit,
            RiskDrawdownType::PercentOfEquity => {
                let peak = self.peak_equity_for_drawdown();
                if !peak.is_finite() || peak <= 0.0 {
                    return true;
                }
                let equity = self.equity_value(mark);
                if !equity.is_finite() || equity <= 0.0 {
                    return true;
                }
                let percent = self.current_risk_drawdown_amount(mark) / peak * 100.0;
                percent >= limit
            }
        }
    }

    fn enforce_risk_stop(
        &mut self,
        kind: RiskRuleKind,
        alert_message: Option<String>,
        bar_index: usize,
        time: i64,
        mark: f64,
    ) {
        if let Some(message) = alert_message {
            self.next_close_metadata.alert_message = Some(message);
        }
        self.risk_state.trip(kind);
        self.cancel_all_pending_orders();
        self.close_all_position(bar_index, time, mark);
    }

    pub(crate) fn evaluate_max_drawdown(&mut self, bar_index: usize, time: i64, mark: f64) {
        if self.risk_state.blocked_order_placement {
            return;
        }
        if !self.max_drawdown_rule_tripped(mark) {
            return;
        }
        self.enforce_risk_stop(
            RiskRuleKind::MaxDrawdown,
            self.risk_rules.max_drawdown_alert_message.clone(),
            bar_index,
            time,
            mark,
        );
    }

    fn bump_intraday_equity_max(&mut self, mark: f64) {
        let equity = self.equity_value(mark);
        if !equity.is_finite() {
            return;
        }
        match self.risk_state.intraday_equity_baseline {
            Some(max_equity) if equity > max_equity => {
                self.risk_state.intraday_equity_baseline = Some(equity);
            }
            None => self.risk_state.intraday_equity_baseline = Some(equity),
            Some(_) => {}
        }
    }

    fn current_intraday_loss(&self, mark: f64) -> Option<f64> {
        let baseline = self.risk_state.intraday_equity_baseline?;
        if !baseline.is_finite() {
            return None;
        }
        let equity = self.equity_value(mark);
        if !equity.is_finite() {
            return None;
        }
        Some((baseline - equity).max(0.0))
    }

    fn max_intraday_loss_rule_tripped(&self, mark: f64) -> bool {
        let Some(limit) = self.risk_rules.max_intraday_loss else {
            return false;
        };
        let Some(kind) = self.risk_rules.max_intraday_loss_type else {
            return false;
        };
        match kind {
            RiskDrawdownType::Cash => self
                .current_intraday_loss(mark)
                .is_some_and(|loss| loss >= limit),
            RiskDrawdownType::PercentOfEquity => {
                let equity = self.equity_value(mark);
                if !equity.is_finite() || equity <= 0.0 {
                    return true;
                }
                let Some(baseline) = self.risk_state.intraday_equity_baseline else {
                    return false;
                };
                if !baseline.is_finite() || baseline <= 0.0 {
                    return true;
                }
                let loss = (baseline - equity).max(0.0);
                loss / baseline * 100.0 >= limit
            }
        }
    }

    pub(crate) fn evaluate_max_intraday_loss(&mut self, bar_index: usize, time: i64, mark: f64) {
        if self.risk_state.blocked_order_placement {
            return;
        }
        self.bump_intraday_equity_max(mark);
        if !self.max_intraday_loss_rule_tripped(mark) {
            return;
        }
        self.enforce_risk_stop(
            RiskRuleKind::MaxIntradayLoss,
            self.risk_rules.max_intraday_loss_alert_message.clone(),
            bar_index,
            time,
            mark,
        );
    }

    pub(crate) fn flatten_if_risk_blocked(&mut self, bar_index: usize, time: i64, mark: f64) {
        if !self.risk_state.blocked_order_placement {
            return;
        }
        if self.position_size.abs() == 0.0 {
            return;
        }
        self.cancel_all_pending_orders();
        self.close_all_position(bar_index, time, mark);
    }

    pub(crate) fn evaluate_risk_equity_stops(&mut self, bar_index: usize, time: i64, mark: f64) {
        self.evaluate_max_drawdown(bar_index, time, mark);
        self.evaluate_max_intraday_loss(bar_index, time, mark);
        self.flatten_if_risk_blocked(bar_index, time, mark);
    }

    pub(crate) fn check_risk_after_fill(&mut self, _bar_index: usize, _time: i64, _mark: f64) {
        if self.risk_state.blocked_order_placement {
            return;
        }
        self.risk_state.intraday_filled_orders =
            self.risk_state.intraday_filled_orders.saturating_add(1);
        if let Some(limit) = self.risk_rules.max_intraday_filled_orders
            && self.risk_state.intraday_filled_orders >= limit
        {
            if let Some(message) = self
                .risk_rules
                .max_intraday_filled_orders_alert_message
                .clone()
            {
                self.next_close_metadata.alert_message = Some(message);
            }
            self.risk_state.trip(RiskRuleKind::MaxIntradayFilledOrders);
            self.cancel_all_pending_orders();
        }
    }

    pub(crate) fn record_window_realized_pnl(&mut self, profit: f64) {
        if profit.is_finite() {
            self.risk_state.window_realized_pnl += profit;
        }
    }

    fn finalize_completed_loss_window(&mut self) {
        if self
            .risk_state
            .tripped_rules
            .iter()
            .any(|kind| kind.is_permanent_stop())
        {
            return;
        }
        if self.risk_state.window_realized_pnl < 0.0 {
            self.risk_state.consecutive_loss_days =
                self.risk_state.consecutive_loss_days.saturating_add(1);
        } else {
            self.risk_state.consecutive_loss_days = 0;
        }
    }

    #[cfg(test)]
    pub(crate) fn reset_intraday_risk_state(
        &mut self,
        bar_index: usize,
        time_ms: i64,
        timeframe_seconds: i64,
        equity: f64,
    ) {
        self.reset_intraday_window(bar_index, time_ms, timeframe_seconds, equity, equity);
    }

    #[cfg(test)]
    pub(crate) fn reset_intraday_window(
        &mut self,
        bar_index: usize,
        time_ms: i64,
        timeframe_seconds: i64,
        equity: f64,
        mark: f64,
    ) {
        self.reset_risk_windows(bar_index, time_ms, timeframe_seconds, equity, mark, None);
    }

    pub(crate) fn reset_risk_windows(
        &mut self,
        bar_index: usize,
        time_ms: i64,
        timeframe_seconds: i64,
        equity: f64,
        mark: f64,
        host: Option<&crate::SessionWindowIds>,
    ) {
        let (window, trading_day) = match host {
            Some(ids) => (
                RiskWindowKey::Host(ids.window_id.clone()),
                RiskWindowKey::Host(ids.trading_day_id.clone()),
            ),
            None => {
                let utc = RiskWindowKey::Utc(intraday_window_key(time_ms, timeframe_seconds));
                (utc.clone(), utc)
            }
        };
        let window_changed = self.risk_state.window_key.as_ref() != Some(&window);
        let day_changed = self.risk_state.trading_day_key.as_ref() != Some(&trading_day);
        if !window_changed && !day_changed {
            return;
        }
        if day_changed {
            if self.risk_state.trading_day_key.is_some() {
                self.finalize_completed_loss_window();
            }
            self.risk_state.trading_day_key = Some(trading_day);
            self.risk_state.window_realized_pnl = 0.0;
        }
        if window_changed {
            self.risk_state.window_key = Some(window);
            self.risk_state.intraday_filled_orders = 0;
            self.risk_state.intraday_equity_baseline = if equity.is_finite() {
                Some(equity)
            } else {
                None
            };
            self.risk_state
                .tripped_rules
                .retain(|kind| !kind.is_window_scoped());
            self.risk_state.blocked_order_placement = !self.risk_state.tripped_rules.is_empty();
        }
        if self.risk_state.blocked_order_placement {
            return;
        }
        if !day_changed {
            return;
        }
        let Some(limit) = self.risk_rules.max_cons_loss_days else {
            return;
        };
        if self.risk_state.consecutive_loss_days >= limit {
            self.enforce_risk_stop(
                RiskRuleKind::MaxConsLossDays,
                self.risk_rules.max_cons_loss_days_alert_message.clone(),
                bar_index,
                time_ms,
                mark,
            );
        }
    }

    pub(crate) fn check_risk_before_forced_close(&self) -> RiskAdmission {
        self.check_risk_before_order()
    }

    #[allow(dead_code)]
    pub(crate) fn risk_rules(&self) -> &StrategyRiskRules {
        &self.risk_rules
    }

    #[allow(dead_code)]
    pub(crate) fn risk_state(&self) -> &StrategyRiskState {
        &self.risk_state
    }
}

fn positive_filled_order_limit(count: f64) -> Option<u32> {
    if !count.is_finite() || count <= 0.0 || count != count.trunc() {
        return None;
    }
    u32::try_from(count as i64).ok().filter(|limit| *limit > 0)
}
