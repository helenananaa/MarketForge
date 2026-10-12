use super::{
    BrokerState, StrategyOrderFillAlertEvent, StrategyOrderMetadata,
    closed_trades::{AllocatedEntryFill, ClosedTradeFill},
    ledger::{TradeAllocation, TradeDirection},
    pending_closes::{PendingClose, PendingCloseKind, PendingCloseQuantity},
    types::StrategyCommandOrigin,
};
use crate::RuntimeDiagnostic;

impl BrokerState {
    pub(crate) fn place_pending_close(
        &mut self,
        id: String,
        quantity: PendingCloseQuantity,
        created_bar_index: usize,
        metadata: StrategyOrderMetadata,
    ) {
        self.place_pending_close_with_immediately(id, quantity, created_bar_index, metadata, false);
    }

    pub(crate) fn place_pending_close_with_immediately(
        &mut self,
        id: String,
        quantity: PendingCloseQuantity,
        created_bar_index: usize,
        metadata: StrategyOrderMetadata,
        immediately: bool,
    ) {
        self.order_book.with_close_allocator(|closes, allocate| {
            closes.place(
                PendingClose {
                    key: super::types::InternalOrderKey(0),
                    origin: StrategyCommandOrigin::Close,
                    kind: PendingCloseKind::Close { id },
                    quantity,
                    created_bar_index,
                    immediately,
                    metadata,
                },
                allocate,
            );
        });
    }

    pub(crate) fn fill_pending_market_closes(
        &mut self,
        bar_index: usize,
        time: i64,
        fill_price: f64,
    ) {
        let pending_closes = self.order_book.closes_mut().take_eligible(bar_index);
        self.apply_pending_market_closes(pending_closes, bar_index, time, fill_price);
    }

    pub(crate) fn fill_same_bar_market_closes(
        &mut self,
        bar_index: usize,
        time: i64,
        fill_price: f64,
    ) {
        let pending_closes = self.order_book.closes_mut().take_same_bar(bar_index);
        self.apply_pending_market_closes(pending_closes, bar_index, time, fill_price);
    }

    pub(crate) fn fill_immediate_market_closes(
        &mut self,
        bar_index: usize,
        time: i64,
        fill_price: f64,
    ) {
        let pending_closes = self.order_book.closes_mut().take_immediate();
        self.apply_pending_market_closes(pending_closes, bar_index, time, fill_price);
    }

    fn apply_pending_market_closes(
        &mut self,
        pending_closes: Vec<PendingClose>,
        bar_index: usize,
        time: i64,
        fill_price: f64,
    ) {
        for pending in pending_closes {
            self.with_next_close_metadata(pending.metadata, |broker| match pending.kind {
                PendingCloseKind::Close { id } => match pending.quantity {
                    PendingCloseQuantity::Full => {
                        broker.close_long(id, bar_index, time, fill_price);
                    }
                    PendingCloseQuantity::Qty(qty) => {
                        broker.close_long_qty(id, bar_index, time, fill_price, qty);
                    }
                    PendingCloseQuantity::QtyPercent(qty_percent) => {
                        broker.close_long_qty_percent(id, bar_index, time, fill_price, qty_percent);
                    }
                },
                PendingCloseKind::CloseAll => {
                    broker.close_all_position(bar_index, time, fill_price);
                }
            });
        }
    }

    pub(crate) fn place_pending_close_all(
        &mut self,
        quantity: PendingCloseQuantity,
        created_bar_index: usize,
        metadata: StrategyOrderMetadata,
    ) {
        self.place_pending_close_all_with_immediately(quantity, created_bar_index, metadata, false);
    }

    pub(crate) fn place_pending_close_all_with_immediately(
        &mut self,
        quantity: PendingCloseQuantity,
        created_bar_index: usize,
        metadata: StrategyOrderMetadata,
        immediately: bool,
    ) {
        self.order_book.with_close_allocator(|closes, allocate| {
            closes.place(
                PendingClose {
                    key: super::types::InternalOrderKey(0),
                    origin: StrategyCommandOrigin::CloseAll,
                    kind: PendingCloseKind::CloseAll,
                    quantity,
                    created_bar_index,
                    immediately,
                    metadata,
                },
                allocate,
            );
        });
    }

    pub(super) fn active_close_direction(&self) -> Option<TradeDirection> {
        if self.position_size > 0.0 {
            Some(TradeDirection::Long)
        } else if self.position_size < 0.0 {
            Some(TradeDirection::Short)
        } else {
            None
        }
    }

    pub(super) fn exit_fill_price(&self, direction: TradeDirection, price: f64) -> f64 {
        match direction {
            TradeDirection::Long => self.long_exit_fill_price(price),
            TradeDirection::Short => self.short_exit_fill_price(price),
        }
    }

    pub(crate) fn close_all_position(&mut self, bar_index: usize, time: i64, price: f64) {
        self.close_all_position_with_slippage(bar_index, time, price, true);
    }

    pub(super) fn close_all_position_with_slippage(
        &mut self,
        bar_index: usize,
        time: i64,
        price: f64,
        apply_slippage: bool,
    ) {
        self.close_all_position_with_slippage_preserving_exit(
            bar_index,
            time,
            price,
            apply_slippage,
            None,
        );
    }

    pub(super) fn close_all_position_with_slippage_preserving_exit(
        &mut self,
        bar_index: usize,
        time: i64,
        price: f64,
        apply_slippage: bool,
        filling_entry_id: Option<&str>,
    ) {
        let Some(direction) = self.active_close_direction() else {
            return;
        };
        if !price.is_finite() {
            self.diagnostics.push(RuntimeDiagnostic {
                code: "E_STRATEGY_PRICE".to_owned(),
                message: "`strategy.close_all` fill price must be finite".to_owned(),
            });
            return;
        }

        let price = if apply_slippage {
            self.exit_fill_price(direction, price)
        } else {
            price
        };
        if !price.is_finite() {
            self.diagnostics.push(RuntimeDiagnostic {
                code: "E_STRATEGY_PRICE".to_owned(),
                message: "`strategy.close_all` slipped fill price must be finite".to_owned(),
            });
            return;
        }

        let qty = self.position_size.abs();
        let allocations = self.trade_ledger.allocate_all_for_direction(direction);
        if allocations.is_empty() {
            return;
        }

        let exit_commission = self.exit_commission_for_fill(qty, price);
        let metadata = self.take_next_close_metadata();
        for allocation in &allocations {
            let allocated_exit_commission = exit_commission * (allocation.quantity / qty);
            let commission = allocation.entry_commission + allocated_exit_commission;
            let signed_qty = direction.signed_quantity(allocation.quantity);
            let profit = (price - allocation.entry_price) * signed_qty - commission;
            self.record_closed_trade_fill(ClosedTradeFill {
                entry_id: allocation.entry_id.clone(),
                exit_id: allocation.entry_id.clone(),
                entry_fill: AllocatedEntryFill {
                    entry_price: allocation.entry_price,
                    entry_bar_index: allocation.entry_bar_index,
                    entry_time: allocation.entry_time,
                    entry_commission: allocation.entry_commission,
                    entry_metadata: allocation.entry_metadata.clone(),
                },
                exit_bar_index: bar_index,
                exit_time: time,
                exit_price: price,
                qty: signed_qty,
                profit,
                commission,
                close_metadata: metadata.clone(),
            });
            self.record_order_fill_alert_from_order_metadata(
                &metadata,
                StrategyOrderFillAlertEvent {
                    id: allocation.entry_id.clone(),
                    bar_index,
                    time,
                    direction: "strategy.close_all".to_owned(),
                    qty: allocation.quantity,
                    price,
                    entry_id: Some(allocation.entry_id.clone()),
                    exit_id: Some(allocation.entry_id.clone()),
                    message: String::new(),
                },
            );
        }

        self.order_book
            .retain_exits_for_pending_entries(filling_entry_id);
        self.apply_reduction_cash_and_position(
            direction.signed_quantity(qty) * price - exit_commission,
            &allocations,
            qty,
            0.0,
            bar_index,
        );
    }

    pub(crate) fn close_long(&mut self, id: String, bar_index: usize, time: i64, price: f64) {
        self.close_long_quantity(id, bar_index, time, price, None);
    }

    pub(crate) fn close_long_qty(
        &mut self,
        id: String,
        bar_index: usize,
        time: i64,
        price: f64,
        qty: f64,
    ) {
        self.close_long_quantity(id, bar_index, time, price, Some(qty));
    }

    pub(crate) fn close_long_qty_percent(
        &mut self,
        id: String,
        bar_index: usize,
        time: i64,
        price: f64,
        qty_percent: f64,
    ) {
        let Some(direction) = self.active_close_direction() else {
            return;
        };
        let matching_position_size = self
            .trade_ledger
            .open_quantity_for_entry_direction(direction, &id);
        if matching_position_size <= 0.0 {
            return;
        }
        if !qty_percent.is_finite() || qty_percent <= 0.0 {
            self.diagnostics.push(RuntimeDiagnostic {
                code: "E_STRATEGY_CLOSE_QTY_PERCENT".to_owned(),
                message: "`strategy.close` percent quantity must be finite and positive".to_owned(),
            });
            return;
        }

        let qty = matching_position_size * qty_percent / 100.0;
        if !qty.is_finite() || qty <= 0.0 {
            self.diagnostics.push(RuntimeDiagnostic {
                code: "E_STRATEGY_CLOSE_QTY_PERCENT".to_owned(),
                message: "`strategy.close` percent quantity must be finite and positive".to_owned(),
            });
            return;
        }
        self.close_long_quantity(id, bar_index, time, price, Some(qty));
    }

    #[allow(dead_code)]
    pub(crate) fn reduce_long_with_short_order(
        &mut self,
        id: String,
        bar_index: usize,
        time: i64,
        price: f64,
        requested_qty: f64,
        metadata: StrategyOrderMetadata,
    ) {
        if self.position_size <= 0.0 {
            return;
        }
        if !requested_qty.is_finite() || requested_qty <= 0.0 {
            self.diagnostics.push(RuntimeDiagnostic {
                code: "E_STRATEGY_QTY".to_owned(),
                message: "`strategy.order` quantity must be finite and positive".to_owned(),
            });
            return;
        }
        if !price.is_finite() {
            self.diagnostics.push(RuntimeDiagnostic {
                code: "E_STRATEGY_PRICE".to_owned(),
                message: "`strategy.order` fill price must be finite".to_owned(),
            });
            return;
        }

        let price = self.long_exit_fill_price(price);
        if !price.is_finite() {
            self.diagnostics.push(RuntimeDiagnostic {
                code: "E_STRATEGY_PRICE".to_owned(),
                message: "`strategy.order` slipped fill price must be finite".to_owned(),
            });
            return;
        }

        let qty = requested_qty.min(self.position_size);
        let allocations = self.trade_ledger.allocate_exit_fifo(None, qty);
        if allocations.is_empty() {
            return;
        }

        let exit_commission = self.exit_commission_for_fill(qty, price);
        self.record_order_event(id.clone(), bar_index, time, "strategy.short", qty, price);
        self.record_order_fill_alert_from_order_metadata(
            &metadata,
            StrategyOrderFillAlertEvent {
                id: id.clone(),
                bar_index,
                time,
                direction: "strategy.short".to_owned(),
                qty,
                price,
                entry_id: None,
                exit_id: Some(id.clone()),
                message: String::new(),
            },
        );

        let mut closed_entry_commission = 0.0;
        for allocation in &allocations {
            let allocated_exit_commission = exit_commission * (allocation.quantity / qty);
            let commission = allocation.entry_commission + allocated_exit_commission;
            let profit = (price - allocation.entry_price) * allocation.quantity - commission;
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
                exit_price: price,
                qty: allocation.quantity,
                profit,
                commission,
                close_metadata: metadata.clone(),
            });
        }

        if qty >= self.position_size {
            self.order_book.exits_mut().clear_all();
        }
        self.apply_reduction_cash_and_position(
            qty * price - exit_commission,
            &allocations,
            qty,
            closed_entry_commission,
            bar_index,
        );
    }

    fn close_long_quantity(
        &mut self,
        id: String,
        bar_index: usize,
        time: i64,
        price: f64,
        requested_qty: Option<f64>,
    ) {
        let Some(direction) = self.active_close_direction() else {
            return;
        };
        let matching_position_size = self
            .trade_ledger
            .open_quantity_for_entry_direction(direction, &id);
        if matching_position_size <= 0.0 {
            return;
        }
        if let Some(requested_qty) = requested_qty
            && (!requested_qty.is_finite() || requested_qty <= 0.0)
        {
            self.diagnostics.push(RuntimeDiagnostic {
                code: "E_STRATEGY_CLOSE_QTY".to_owned(),
                message: "`strategy.close` quantity must be finite and positive".to_owned(),
            });
            return;
        }
        if !price.is_finite() {
            self.diagnostics.push(RuntimeDiagnostic {
                code: "E_STRATEGY_PRICE".to_owned(),
                message: "`strategy.close` fill price must be finite".to_owned(),
            });
            return;
        }

        let price = self.exit_fill_price(direction, price);
        if !price.is_finite() {
            self.diagnostics.push(RuntimeDiagnostic {
                code: "E_STRATEGY_PRICE".to_owned(),
                message: "`strategy.close` slipped fill price must be finite".to_owned(),
            });
            return;
        }

        let qty = requested_qty.map_or(matching_position_size, |qty| {
            qty.min(matching_position_size)
        });
        let allocations = self.allocate_close_rule_exit_for_direction(direction, Some(&id), qty);
        if allocations.is_empty() {
            return;
        }
        let exit_commission = self.exit_commission_for_fill(qty, price);
        let metadata = self.take_next_close_metadata();
        let mut closed_entry_commission = 0.0;
        self.record_order_event(
            format!("Close entry(s) order {id}"),
            bar_index,
            time,
            "strategy.close",
            qty,
            price,
        );
        for allocation in &allocations {
            let allocated_exit_commission = exit_commission * (allocation.quantity / qty);
            let commission = allocation.entry_commission + allocated_exit_commission;
            let signed_qty = direction.signed_quantity(allocation.quantity);
            let profit = (price - allocation.entry_price) * signed_qty - commission;
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
                exit_price: price,
                qty: signed_qty,
                profit,
                commission,
                close_metadata: metadata.clone(),
            });
            self.record_order_fill_alert_from_order_metadata(
                &metadata,
                StrategyOrderFillAlertEvent {
                    id: id.clone(),
                    bar_index,
                    time,
                    direction: "strategy.close".to_owned(),
                    qty: allocation.quantity,
                    price,
                    entry_id: Some(allocation.entry_id.clone()),
                    exit_id: Some(id.clone()),
                    message: String::new(),
                },
            );
        }

        if qty >= self.position_size.abs() {
            self.cancel_exit_for_entry(&id);
        } else {
            self.drop_exits_for_closed_trade_keys(&allocations);
        }
        self.apply_reduction_cash_and_position(
            direction.signed_quantity(qty) * price - exit_commission,
            &allocations,
            qty,
            closed_entry_commission,
            bar_index,
        );
    }

    pub(super) fn allocate_close_rule_exit_for_direction(
        &self,
        direction: TradeDirection,
        from_entry: Option<&str>,
        requested_quantity: f64,
    ) -> Vec<TradeAllocation> {
        match (self.close_entries_rule, from_entry) {
            (pine_ir::StrategyCloseEntriesRule::Any, Some(entry_id)) => self
                .trade_ledger
                .allocate_exit_any_for_entry_direction(direction, entry_id, requested_quantity),
            // `from_entry` sizes/reserves the exit order. With the default
            // FIFO rule its fill closes the oldest open trade regardless of
            // the entry ID named by that order.
            _ => self.trade_ledger.allocate_exit_fifo_for_direction(
                direction,
                None,
                requested_quantity,
            ),
        }
    }

    pub(super) fn allocate_generic_order_close(
        &self,
        direction: TradeDirection,
        order_id: &str,
        requested_quantity: f64,
    ) -> Vec<TradeAllocation> {
        if self.close_entries_rule == pine_ir::StrategyCloseEntriesRule::Any {
            let matching = self.trade_ledger.allocate_exit_any_for_entry_direction(
                direction,
                order_id,
                requested_quantity,
            );
            if !matching.is_empty() {
                return matching;
            }
        }
        self.trade_ledger
            .allocate_exit_fifo_for_direction(direction, None, requested_quantity)
    }
}
