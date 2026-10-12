use super::{
    BrokerState, StrategyExitMetadata, StrategyOrderFillAlertEvent, StrategyOrderMetadata,
    closed_trades::{AllocatedEntryFill, ClosedTradeFill},
    ledger::TradeDirection,
    pending_exits::{PendingExit, PendingExitTrigger},
};
use crate::{RuntimeDiagnostic, StrategyOrderEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrategyExitFillAlertKind {
    Profit,
    Loss,
    Trailing,
    Generic,
}

fn exit_comment(
    metadata: &StrategyExitMetadata,
    kind: StrategyExitFillAlertKind,
) -> Option<String> {
    let specific = match kind {
        StrategyExitFillAlertKind::Profit => metadata.comment_profit.as_ref(),
        StrategyExitFillAlertKind::Loss => metadata.comment_loss.as_ref(),
        StrategyExitFillAlertKind::Trailing => metadata.comment_trailing.as_ref(),
        StrategyExitFillAlertKind::Generic => None,
    };
    specific.or(metadata.comment.as_ref()).cloned()
}

fn pending_exit_alert_kind(
    trigger: &PendingExitTrigger,
    raw_exit_price: f64,
) -> StrategyExitFillAlertKind {
    match trigger {
        PendingExitTrigger::Limit(_) => StrategyExitFillAlertKind::Profit,
        PendingExitTrigger::Stop(_) => StrategyExitFillAlertKind::Loss,
        PendingExitTrigger::Trailing(_) => StrategyExitFillAlertKind::Trailing,
        PendingExitTrigger::Bracket { downside, upside } if raw_exit_price == *downside => {
            StrategyExitFillAlertKind::Loss
        }
        PendingExitTrigger::Bracket {
            downside: _,
            upside,
        } if raw_exit_price == *upside => StrategyExitFillAlertKind::Profit,
        PendingExitTrigger::Bracket { .. } => StrategyExitFillAlertKind::Generic,
    }
}

fn pending_exit_is_limit_fill(
    trigger: &PendingExitTrigger,
    direction: TradeDirection,
    raw_exit_price: f64,
) -> bool {
    match trigger {
        PendingExitTrigger::Limit(_) => true,
        PendingExitTrigger::Bracket { upside, .. } => match direction {
            TradeDirection::Long => raw_exit_price >= *upside,
            TradeDirection::Short => raw_exit_price <= *upside,
        },
        PendingExitTrigger::Stop(_) | PendingExitTrigger::Trailing(_) => false,
    }
}

fn exit_alert_message(metadata: &StrategyExitMetadata, kind: StrategyExitFillAlertKind) -> String {
    let specific = match kind {
        StrategyExitFillAlertKind::Profit => metadata.alert_profit.as_ref(),
        StrategyExitFillAlertKind::Loss => metadata.alert_loss.as_ref(),
        StrategyExitFillAlertKind::Trailing => metadata.alert_trailing.as_ref(),
        StrategyExitFillAlertKind::Generic => None,
    };
    specific
        .or(metadata.alert_message.as_ref())
        .cloned()
        .unwrap_or_default()
}

impl BrokerState {
    pub(super) fn record_position_snapshot(&mut self, bar_index: usize) {
        self.position.push(crate::StrategyPositionSnapshot {
            bar_index,
            size: self.position_size,
            avg_price: (self.position_size != 0.0).then_some(self.avg_price),
        });
    }

    pub(super) fn record_order_event(
        &mut self,
        id: String,
        bar_index: usize,
        time: i64,
        direction: &str,
        qty: f64,
        price: f64,
    ) {
        self.orders.push(StrategyOrderEvent {
            id,
            bar_index,
            time,
            direction: direction.to_owned(),
            qty,
            price,
        });
        self.check_risk_after_fill(bar_index, time, price);
    }

    pub(super) fn record_order_fill_alert_from_order_metadata(
        &mut self,
        metadata: &StrategyOrderMetadata,
        mut event: StrategyOrderFillAlertEvent,
    ) {
        if metadata.disable_alert {
            return;
        }
        event.message = metadata.alert_message.clone().unwrap_or_default();
        self.order_fill_alerts.push(event);
    }

    fn record_order_fill_alert_from_exit_metadata(
        &mut self,
        metadata: &StrategyExitMetadata,
        kind: StrategyExitFillAlertKind,
        mut event: StrategyOrderFillAlertEvent,
    ) {
        if metadata.disable_alert {
            return;
        }
        event.message = exit_alert_message(metadata, kind);
        self.order_fill_alerts.push(event);
    }

    pub(crate) fn evaluate_margin_call_long(
        &mut self,
        bar_index: usize,
        time: i64,
        current_price: f64,
    ) {
        let _ = self.check_risk_before_forced_close();
        if self.position_size <= 0.0 || !self.margin_long.is_active() || !current_price.is_finite()
        {
            return;
        }
        let Some(qty) = self.margin_call_quantity(current_price) else {
            return;
        };

        let entry_id = self
            .entry_id
            .clone()
            .unwrap_or_else(|| "Margin Call".to_owned());
        let exit_id = "Margin Call".to_owned();
        let allocations = self.trade_ledger.allocate_exit_fifo(None, qty);
        let entry_fill = AllocatedEntryFill::from_allocations(
            &allocations,
            self.avg_price,
            self.entry_bar_index.unwrap_or(bar_index),
            self.entry_time.unwrap_or(time),
            self.entry_commission_for_closed_quantity(qty),
        );
        let exit_commission = self.exit_commission_for_fill(qty, current_price);
        let commission = entry_fill.entry_commission + exit_commission;
        let profit = (current_price - entry_fill.entry_price) * qty - commission;
        let closed_entry_commission = entry_fill.entry_commission;

        self.order_book.exits_mut().clear_for_entry(&entry_id);
        self.record_order_event(
            exit_id.clone(),
            bar_index,
            time,
            "strategy.short",
            qty,
            current_price,
        );
        self.record_closed_trade_fill(ClosedTradeFill {
            entry_id,
            exit_id,
            entry_fill,
            exit_bar_index: bar_index,
            exit_time: time,
            exit_price: current_price,
            qty,
            profit,
            commission,
            close_metadata: StrategyOrderMetadata::default(),
        });

        self.apply_reduction_cash_and_position(
            qty * current_price - exit_commission,
            &allocations,
            qty,
            closed_entry_commission,
            bar_index,
        );
    }

    pub(crate) fn evaluate_margin_call_short(
        &mut self,
        bar_index: usize,
        time: i64,
        current_price: f64,
    ) {
        let _ = self.check_risk_before_forced_close();
        if self.position_size >= 0.0
            || !self.margin_short.is_active()
            || !current_price.is_finite()
            || current_price <= 0.0
        {
            return;
        }
        let Some(qty) = self.margin_call_quantity(current_price) else {
            return;
        };

        let entry_id = self
            .entry_id
            .clone()
            .unwrap_or_else(|| "Margin Call".to_owned());
        let exit_id = "Margin Call".to_owned();
        let allocations =
            self.trade_ledger
                .allocate_exit_fifo_for_direction(TradeDirection::Short, None, qty);
        let entry_fill = AllocatedEntryFill::from_allocations(
            &allocations,
            self.avg_price,
            self.entry_bar_index.unwrap_or(bar_index),
            self.entry_time.unwrap_or(time),
            self.entry_commission_for_closed_quantity(qty),
        );
        let exit_commission = self.exit_commission_for_fill(qty, current_price);
        let commission = entry_fill.entry_commission + exit_commission;
        let signed_qty = TradeDirection::Short.signed_quantity(qty);
        let profit = (current_price - entry_fill.entry_price) * signed_qty - commission;
        let closed_entry_commission = entry_fill.entry_commission;

        self.order_book.exits_mut().clear_for_entry(&entry_id);
        self.record_order_event(
            exit_id.clone(),
            bar_index,
            time,
            "strategy.long",
            qty,
            current_price,
        );
        self.record_closed_trade_fill(ClosedTradeFill {
            entry_id,
            exit_id,
            entry_fill,
            exit_bar_index: bar_index,
            exit_time: time,
            exit_price: current_price,
            qty: signed_qty,
            profit,
            commission,
            close_metadata: StrategyOrderMetadata::default(),
        });

        self.apply_reduction_cash_and_position(
            signed_qty * current_price - exit_commission,
            &allocations,
            qty,
            closed_entry_commission,
            bar_index,
        );
    }

    pub(super) fn fill_pending_exit(
        &mut self,
        pending_exit: PendingExit,
        bar_index: usize,
        time: i64,
        exit_price: f64,
    ) {
        let Some(direction) = self.active_close_direction() else {
            return;
        };
        let qty = pending_exit.reserved_quantity.min(self.position_size.abs());
        if !qty.is_finite() || qty <= 0.0 {
            self.diagnostics.push(RuntimeDiagnostic {
                code: "E_STRATEGY_EXIT_QTY".to_owned(),
                message: "`strategy.exit` quantity must be finite and positive".to_owned(),
            });
            return;
        }
        let raw_exit_price = exit_price;
        let exit_price =
            if pending_exit_is_limit_fill(&pending_exit.trigger, direction, raw_exit_price) {
                raw_exit_price
            } else {
                self.exit_fill_price(direction, raw_exit_price)
            };
        if !exit_price.is_finite() {
            self.diagnostics.push(RuntimeDiagnostic {
                code: "E_STRATEGY_PRICE".to_owned(),
                message: "`strategy.exit` slipped fill price must be finite".to_owned(),
            });
            return;
        }
        let alert_kind = pending_exit_alert_kind(&pending_exit.trigger, raw_exit_price);
        let alert_metadata = pending_exit.metadata.clone();
        let allocations = if let Some(target_trade_key) = pending_exit.target_trade_key {
            self.trade_ledger
                .allocate_exit_for_key(target_trade_key, qty)
        } else {
            let from_entry_filter = if pending_exit.from_entry.is_empty() {
                None
            } else {
                Some(pending_exit.from_entry.as_str())
            };
            self.allocate_close_rule_exit_for_direction(direction, from_entry_filter, qty)
        };
        if allocations.is_empty() {
            return;
        }
        let exit_commission = self.exit_commission_for_fill(qty, exit_price);
        let exit_id = pending_exit.id;
        let closed_entry_commission = {
            let mut closed_entry_commission = 0.0;
            for allocation in &allocations {
                let allocated_exit_commission = exit_commission * (allocation.quantity / qty);
                let entry_fill = AllocatedEntryFill {
                    entry_price: allocation.entry_price,
                    entry_bar_index: allocation.entry_bar_index,
                    entry_time: allocation.entry_time,
                    entry_commission: allocation.entry_commission,
                    entry_metadata: allocation.entry_metadata.clone(),
                };
                let commission = allocation.entry_commission + allocated_exit_commission;
                let signed_qty = direction.signed_quantity(allocation.quantity);
                let profit = (exit_price - allocation.entry_price) * signed_qty - commission;

                self.record_order_event(
                    exit_id.clone(),
                    bar_index,
                    time,
                    "strategy.exit",
                    allocation.quantity,
                    exit_price,
                );
                self.record_order_fill_alert_from_exit_metadata(
                    &alert_metadata,
                    alert_kind,
                    StrategyOrderFillAlertEvent {
                        id: exit_id.clone(),
                        bar_index,
                        time,
                        direction: "strategy.exit".to_owned(),
                        qty: allocation.quantity,
                        price: exit_price,
                        entry_id: Some(allocation.entry_id.clone()),
                        exit_id: Some(exit_id.clone()),
                        message: String::new(),
                    },
                );
                self.record_closed_trade_fill(ClosedTradeFill {
                    entry_id: allocation.entry_id.clone(),
                    exit_id: exit_id.clone(),
                    entry_fill,
                    exit_bar_index: bar_index,
                    exit_time: time,
                    exit_price,
                    qty: signed_qty,
                    profit,
                    commission,
                    close_metadata: StrategyOrderMetadata {
                        comment: exit_comment(&alert_metadata, alert_kind),
                        ..StrategyOrderMetadata::default()
                    },
                });
                closed_entry_commission += allocation.entry_commission;
            }
            closed_entry_commission
        };

        self.apply_reduction_cash_and_position(
            direction.signed_quantity(qty) * exit_price - exit_commission,
            &allocations,
            qty,
            closed_entry_commission,
            bar_index,
        );
    }
}
