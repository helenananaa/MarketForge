use super::closed_trades::{AllocatedEntryFill, ClosedTradeFill};
use super::fill_transition::{
    FillCalcError, FillRequest, FillTriggerReason, PositionSnapshot, calculate_netting_transition,
    calculate_same_side_addition,
};
use super::ledger::{OpenTrade, TradeDirection};
use super::types::{EntryFill, EntryPyramidingMode, InternalOrderKey, StrategyOrderMetadata};
use super::{BrokerState, StrategyOrderFillAlertEvent};
use crate::RuntimeDiagnostic;

impl BrokerState {
    pub(super) fn apply_validated_same_side_open(
        &mut self,
        fill: EntryFill,
        fill_price: f64,
        pyramiding_mode: EntryPyramidingMode,
        direction: TradeDirection,
        order_direction: &str,
        closing_qty: f64,
    ) -> bool {
        let entry_commission = self.entry_commission_for_fill(fill.qty, fill_price);
        let snapshot = PositionSnapshot {
            signed_size: self.position_size,
            avg_price: self.avg_price,
        };
        let request = FillRequest {
            order_key: InternalOrderKey(0),
            bar_index: fill.bar_index,
            time: fill.time,
            raw_price: fill_price,
            trigger_reason: FillTriggerReason::Market,
        };
        let signed_quantity = direction.signed_quantity(fill.qty);
        let equity_on_entry = self.cash;
        match calculate_same_side_addition(
            &snapshot,
            request,
            signed_quantity,
            fill_price,
            entry_commission,
        ) {
            Ok(transition) => {
                self.cash += transition.cash_delta;
                self.record_open_fill(
                    fill,
                    fill_price,
                    entry_commission,
                    pyramiding_mode,
                    direction,
                    order_direction,
                    equity_on_entry,
                    closing_qty,
                );
                true
            }
            Err(FillCalcError::NotSameSide) => {
                self.cash -= signed_quantity * fill_price + entry_commission;
                self.record_open_fill(
                    fill,
                    fill_price,
                    entry_commission,
                    pyramiding_mode,
                    direction,
                    order_direction,
                    equity_on_entry,
                    closing_qty,
                );
                true
            }
            Err(_) => false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record_open_fill(
        &mut self,
        fill: EntryFill,
        fill_price: f64,
        entry_commission: f64,
        pyramiding_mode: EntryPyramidingMode,
        direction: TradeDirection,
        order_direction: &str,
        equity_on_entry: f64,
        closing_qty: f64,
    ) {
        let min_equity_before_entry = self.min_equity_before_open_trade;
        let max_equity_before_entry = self.max_equity_before_open_trade;
        let alert_id = fill.id.clone();
        let alert_metadata = fill.metadata.clone();
        let open_trade = OpenTrade {
            key: 0,
            id: fill.id.clone(),
            direction,
            quantity: fill.qty,
            entry_price: fill_price,
            entry_bar_index: fill.bar_index,
            entry_time: fill.time,
            entry_commission,
            max_high: Some(fill_price),
            min_low: Some(fill_price),
            equity_on_entry: Some(equity_on_entry),
            min_equity_before_entry: Some(min_equity_before_entry),
            max_equity_before_entry: Some(max_equity_before_entry),
            entry_metadata: fill.metadata,
        };
        let bypass = matches!(
            pyramiding_mode,
            EntryPyramidingMode::BypassLimit | EntryPyramidingMode::SameTickPriceException
        );
        match (direction, bypass) {
            (TradeDirection::Long, true) => {
                self.record_open_long_trade_exceeding_pyramiding(open_trade);
            }
            (TradeDirection::Long, false) => self.record_open_long_trade(open_trade),
            (TradeDirection::Short, true) => {
                self.record_open_short_trade_exceeding_pyramiding(open_trade);
            }
            (TradeDirection::Short, false) => self.record_open_short_trade(open_trade),
        }
        // Reversing entries transact both the closed exposure and new entry.
        // The ledger and entry fee still use only the new trade's quantity.
        let transaction_qty = closing_qty + fill.qty;
        self.record_order_event(
            fill.id,
            fill.bar_index,
            fill.time,
            order_direction,
            transaction_qty,
            fill_price,
        );
        self.record_order_fill_alert_from_order_metadata(
            &alert_metadata,
            StrategyOrderFillAlertEvent {
                id: alert_id.clone(),
                bar_index: fill.bar_index,
                time: fill.time,
                direction: order_direction.to_owned(),
                qty: transaction_qty,
                price: fill_price,
                entry_id: Some(alert_id),
                exit_id: None,
                message: String::new(),
            },
        );
        self.record_position_snapshot(fill.bar_index);
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "broker fill accounting needs explicit order and tick fields"
    )]
    pub(crate) fn apply_generic_market_order_netting(
        &mut self,
        id: String,
        signed_quantity: f64,
        bar_index: usize,
        time: i64,
        raw_price: f64,
        metadata: StrategyOrderMetadata,
        apply_slippage: bool,
    ) -> bool {
        self.apply_order_netting_with_exit_policy(
            id,
            signed_quantity,
            bar_index,
            time,
            raw_price,
            metadata,
            apply_slippage,
            false,
        )
    }

    pub(super) fn apply_entry_closing_transaction(&mut self, fill: EntryFill) -> bool {
        self.apply_order_netting_with_exit_policy(
            fill.id,
            -self.position_size,
            fill.bar_index,
            fill.time,
            fill.price,
            fill.metadata,
            fill.apply_slippage,
            true,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "fill accounting and pending-entry exit policy"
    )]
    fn apply_order_netting_with_exit_policy(
        &mut self,
        id: String,
        signed_quantity: f64,
        bar_index: usize,
        time: i64,
        raw_price: f64,
        metadata: StrategyOrderMetadata,
        apply_slippage: bool,
        preserve_pending_entry_exits: bool,
    ) -> bool {
        if !signed_quantity.is_finite() || signed_quantity == 0.0 {
            self.diagnostics.push(RuntimeDiagnostic {
                code: "E_STRATEGY_QTY".to_owned(),
                message: "`strategy.order` quantity must be finite and positive".to_owned(),
            });
            return false;
        }
        if !raw_price.is_finite() {
            self.diagnostics.push(RuntimeDiagnostic {
                code: "E_STRATEGY_PRICE".to_owned(),
                message: "`strategy.order` fill price must be finite".to_owned(),
            });
            return false;
        }

        let order_direction = if signed_quantity > 0.0 {
            TradeDirection::Long
        } else {
            TradeDirection::Short
        };
        let fill_price = match (order_direction, apply_slippage) {
            (TradeDirection::Long, true) => self.long_entry_fill_price(raw_price),
            (TradeDirection::Short, true) => self.short_entry_fill_price(raw_price),
            (_, false) => raw_price,
        };
        if !fill_price.is_finite() {
            self.diagnostics.push(RuntimeDiagnostic {
                code: "E_STRATEGY_PRICE".to_owned(),
                message: "`strategy.order` slipped fill price must be finite".to_owned(),
            });
            return false;
        }

        let filled = signed_quantity.abs();
        let snapshot = PositionSnapshot {
            signed_size: self.position_size,
            avg_price: self.avg_price,
        };
        let split = match super::fill_transition::split_fill_quantities(
            snapshot.signed_size,
            signed_quantity,
        ) {
            Ok(split) => split,
            Err(_) => return false,
        };
        let allocations = match split.close_direction {
            Some(close_direction) if split.close_quantity > 0.0 => {
                self.allocate_generic_order_close(close_direction, &id, split.close_quantity)
            }
            _ => Vec::new(),
        };
        if split.close_quantity > 0.0 && allocations.is_empty() {
            return false;
        }
        if let Some(open_direction) = split.open_direction {
            let affordable = match open_direction {
                TradeDirection::Long => self.can_afford_long_entry(split.open_quantity, fill_price),
                TradeDirection::Short => {
                    self.can_afford_short_entry(split.open_quantity, fill_price)
                }
            };
            if !affordable {
                self.diagnostics.push(RuntimeDiagnostic {
                    code: "E_STRATEGY_MARGIN".to_owned(),
                    message: "`strategy.order` requires more margin than available equity"
                        .to_owned(),
                });
                return false;
            }
        }

        let request = FillRequest {
            order_key: InternalOrderKey(0),
            bar_index,
            time,
            raw_price: fill_price,
            trigger_reason: FillTriggerReason::Market,
        };
        let Ok(transition) = calculate_netting_transition(
            &snapshot,
            request,
            signed_quantity,
            fill_price,
            self.entry_commission_for_fill(filled, fill_price),
            self.exit_commission_for_fill(filled, fill_price),
            allocations.clone(),
        ) else {
            return false;
        };

        let open_commission = transition
            .opened_trade
            .as_ref()
            .map(|opened| opened.commission)
            .unwrap_or(0.0);
        let close_commission = transition.commission - open_commission;
        let open_cash = match transition.opened_trade.as_ref() {
            Some(opened) => {
                -(opened.direction.signed_quantity(opened.quantity) * opened.price
                    + opened.commission)
            }
            None => 0.0,
        };
        let close_cash = transition.cash_delta - open_cash;

        if transition.close_quantity > 0.0 {
            if transition.close_quantity >= snapshot.signed_size.abs() {
                if preserve_pending_entry_exits {
                    self.order_book.retain_exits_for_pending_entries(Some(&id));
                } else {
                    self.order_book.exits_mut().clear_all();
                }
            }
            let mut closed_entry_commission = 0.0;
            for allocation in &transition.closed_allocations {
                let allocated_exit = if transition.close_quantity > 0.0 {
                    close_commission * (allocation.quantity / transition.close_quantity)
                } else {
                    0.0
                };
                let commission = allocation.entry_commission + allocated_exit;
                let signed_qty = allocation.direction.signed_quantity(allocation.quantity);
                let profit = (fill_price - allocation.entry_price) * signed_qty - commission;
                closed_entry_commission += allocation.entry_commission;
                self.record_closed_trade_fill(ClosedTradeFill {
                    entry_id: allocation.entry_id.clone(),
                    exit_id: id.clone(),
                    entry_fill: AllocatedEntryFill {
                        entry_price: allocation.entry_price,
                        entry_bar_index: allocation.entry_bar_index,
                        entry_time: allocation.entry_time,
                        entry_commission: allocation.entry_commission,
                        entry_metadata: allocation.entry_metadata.clone(),
                    },
                    exit_bar_index: bar_index,
                    exit_time: time,
                    exit_price: fill_price,
                    qty: signed_qty,
                    profit,
                    commission,
                    close_metadata: metadata.clone(),
                });
            }
            self.apply_reduction_cash_and_position(
                close_cash,
                &transition.closed_allocations,
                transition.close_quantity,
                closed_entry_commission,
                bar_index,
            );
        }

        if let Some(opened) = transition.opened_trade {
            let equity_on_entry = self.cash;
            self.cash += open_cash;
            self.append_generic_open_trade(
                EntryFill {
                    id: id.clone(),
                    bar_index,
                    time,
                    price: fill_price,
                    apply_slippage,
                    qty: opened.quantity,
                    metadata: metadata.clone(),
                },
                fill_price,
                opened.commission,
                opened.direction,
                equity_on_entry,
            );
        }

        let public_direction = match order_direction {
            TradeDirection::Long => "strategy.long",
            TradeDirection::Short => "strategy.short",
        };
        self.record_order_event(
            id.clone(),
            bar_index,
            time,
            public_direction,
            filled,
            fill_price,
        );
        self.record_order_fill_alert_from_order_metadata(
            &metadata,
            StrategyOrderFillAlertEvent {
                id: id.clone(),
                bar_index,
                time,
                direction: public_direction.to_owned(),
                qty: filled,
                price: fill_price,
                entry_id: transition.opened_trade.as_ref().map(|_| id.clone()),
                exit_id: (transition.close_quantity > 0.0).then(|| id.clone()),
                message: String::new(),
            },
        );
        true
    }

    fn append_generic_open_trade(
        &mut self,
        fill: EntryFill,
        fill_price: f64,
        entry_commission: f64,
        direction: TradeDirection,
        equity_on_entry: f64,
    ) {
        let min_equity_before_entry = self.min_equity_before_open_trade;
        let max_equity_before_entry = self.max_equity_before_open_trade;
        let open_trade = OpenTrade {
            key: 0,
            id: fill.id,
            direction,
            quantity: fill.qty,
            entry_price: fill_price,
            entry_bar_index: fill.bar_index,
            entry_time: fill.time,
            entry_commission,
            max_high: Some(fill_price),
            min_low: Some(fill_price),
            equity_on_entry: Some(equity_on_entry),
            min_equity_before_entry: Some(min_equity_before_entry),
            max_equity_before_entry: Some(max_equity_before_entry),
            entry_metadata: fill.metadata,
        };
        match direction {
            TradeDirection::Long => {
                self.record_open_long_trade_exceeding_pyramiding(open_trade);
            }
            TradeDirection::Short => {
                self.record_open_short_trade_exceeding_pyramiding(open_trade);
            }
        }
        self.record_position_snapshot(fill.bar_index);
    }

    pub(super) fn apply_reduction_cash_and_position(
        &mut self,
        cash_delta: f64,
        allocations: &[super::ledger::TradeAllocation],
        qty: f64,
        closed_entry_commission: f64,
        bar_index: usize,
    ) {
        self.cash += cash_delta;
        if qty >= self.position_size.abs() {
            self.min_equity_before_open_trade = self.min_equity_before_open_trade.min(self.cash);
            self.max_equity_before_open_trade = self.max_equity_before_open_trade.max(self.cash);
            self.clear_open_long_legacy_state();
            self.apply_trade_allocations_and_sync_position(allocations);
            self.record_position_snapshot(bar_index);
            return;
        }
        self.open_entry_commission -= closed_entry_commission;
        self.apply_trade_allocations_and_sync_position(allocations);
        self.record_position_snapshot(bar_index);
    }
}
