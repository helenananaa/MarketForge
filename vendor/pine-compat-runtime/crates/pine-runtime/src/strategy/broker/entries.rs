use super::{
    BrokerState, StrategyOrderMetadata,
    ledger::{OpenTrade, TradeAllocation, TradeDirection},
    types::{EntryFill, EntryPyramidingMode},
};
use crate::RuntimeDiagnostic;

impl BrokerState {
    #[allow(dead_code)]
    pub(crate) fn entry_long(
        &mut self,
        id: String,
        bar_index: usize,
        time: i64,
        price: f64,
        qty: f64,
    ) -> bool {
        self.entry_long_with_metadata(
            id,
            bar_index,
            time,
            price,
            qty,
            StrategyOrderMetadata::default(),
        )
    }

    pub(crate) fn entry_long_with_metadata(
        &mut self,
        id: String,
        bar_index: usize,
        time: i64,
        price: f64,
        qty: f64,
        metadata: StrategyOrderMetadata,
    ) -> bool {
        self.entry_long_internal(
            EntryFill {
                id,
                bar_index,
                time,
                price,
                apply_slippage: true,
                qty,
                metadata,
            },
            EntryPyramidingMode::EnforceLimit,
        )
    }

    pub(super) fn entry_long_internal(
        &mut self,
        mut fill: EntryFill,
        pyramiding_mode: EntryPyramidingMode,
    ) -> bool {
        match self.entry_direction_admission(super::pending_entries::PendingEntryDirection::Long) {
            super::risk::EntryDirectionAdmission::Reject => return false,
            super::risk::EntryDirectionAdmission::CloseOnly => {
                if self.position_size < 0.0 {
                    self.close_all_position_with_slippage(
                        fill.bar_index,
                        fill.time,
                        fill.price,
                        fill.apply_slippage,
                    );
                    return true;
                }
                return false;
            }
            super::risk::EntryDirectionAdmission::Allow => {}
        }
        if fill.qty == 0.0 {
            return pyramiding_mode == EntryPyramidingMode::EnforceLimit
                && self.position_size < 0.0
                && self.apply_entry_closing_transaction(fill);
        }
        if !fill.qty.is_finite() || fill.qty < 0.0 {
            self.diagnostics.push(RuntimeDiagnostic {
                code: "E_STRATEGY_QTY".to_owned(),
                message: "`strategy.entry` quantity must be positive".to_owned(),
            });
            return false;
        }
        if !fill.price.is_finite() {
            self.diagnostics.push(RuntimeDiagnostic {
                code: "E_STRATEGY_PRICE".to_owned(),
                message: "`strategy.entry` fill price must be finite".to_owned(),
            });
            return false;
        }
        let mut closing_qty = 0.0;
        if pyramiding_mode == EntryPyramidingMode::EnforceLimit && self.position_size < 0.0 {
            if matches!(self.commission, Some(pine_ir::StrategyCommission::CashPerOrder(value)) if value > 0.0)
            {
                return self.entry_reversal_with_order_commission(fill, TradeDirection::Long);
            }
            closing_qty = self.position_size.abs();
            self.close_all_position_with_slippage_preserving_exit(
                fill.bar_index,
                fill.time,
                fill.price,
                fill.apply_slippage,
                Some(&fill.id),
            );
        }
        if pyramiding_mode == EntryPyramidingMode::EnforceLimit && !self.can_open_long_entry() {
            return false;
        }
        match self.clamp_strategy_entry_qty(
            super::pending_entries::PendingEntryDirection::Long,
            fill.qty,
        ) {
            None => return false,
            Some(qty) => fill.qty = qty,
        }

        let fill_price = if fill.apply_slippage {
            self.long_entry_fill_price(fill.price)
        } else {
            fill.price
        };
        if !fill_price.is_finite() {
            self.diagnostics.push(RuntimeDiagnostic {
                code: "E_STRATEGY_PRICE".to_owned(),
                message: "`strategy.entry` slipped fill price must be finite".to_owned(),
            });
            return false;
        }
        if !self.can_afford_long_entry(fill.qty, fill_price) {
            self.diagnostics.push(RuntimeDiagnostic {
                code: "E_STRATEGY_MARGIN".to_owned(),
                message: "`strategy.entry` requires more margin than available equity".to_owned(),
            });
            return false;
        }

        self.apply_validated_same_side_open(
            fill,
            fill_price,
            pyramiding_mode,
            TradeDirection::Long,
            "strategy.long",
            closing_qty,
        )
    }

    #[allow(dead_code)]
    pub(crate) fn entry_short(
        &mut self,
        id: String,
        bar_index: usize,
        time: i64,
        price: f64,
        qty: f64,
    ) -> bool {
        self.entry_short_with_metadata(
            id,
            bar_index,
            time,
            price,
            qty,
            StrategyOrderMetadata::default(),
        )
    }

    pub(crate) fn entry_short_with_metadata(
        &mut self,
        id: String,
        bar_index: usize,
        time: i64,
        price: f64,
        qty: f64,
        metadata: StrategyOrderMetadata,
    ) -> bool {
        self.entry_short_internal(
            EntryFill {
                id,
                bar_index,
                time,
                price,
                apply_slippage: true,
                qty,
                metadata,
            },
            EntryPyramidingMode::EnforceLimit,
        )
    }

    pub(super) fn entry_short_internal(
        &mut self,
        mut fill: EntryFill,
        pyramiding_mode: EntryPyramidingMode,
    ) -> bool {
        match self.entry_direction_admission(super::pending_entries::PendingEntryDirection::Short) {
            super::risk::EntryDirectionAdmission::Reject => return false,
            super::risk::EntryDirectionAdmission::CloseOnly => {
                if self.position_size > 0.0 {
                    self.close_all_position_with_slippage(
                        fill.bar_index,
                        fill.time,
                        fill.price,
                        fill.apply_slippage,
                    );
                    return true;
                }
                return false;
            }
            super::risk::EntryDirectionAdmission::Allow => {}
        }
        if fill.qty == 0.0 {
            return pyramiding_mode == EntryPyramidingMode::EnforceLimit
                && self.position_size > 0.0
                && self.apply_entry_closing_transaction(fill);
        }
        if !fill.qty.is_finite() || fill.qty < 0.0 {
            self.diagnostics.push(RuntimeDiagnostic {
                code: "E_STRATEGY_QTY".to_owned(),
                message: "`strategy.entry` quantity must be positive".to_owned(),
            });
            return false;
        }
        if !fill.price.is_finite() {
            self.diagnostics.push(RuntimeDiagnostic {
                code: "E_STRATEGY_PRICE".to_owned(),
                message: "`strategy.entry` fill price must be finite".to_owned(),
            });
            return false;
        }
        let mut closing_qty = 0.0;
        if pyramiding_mode == EntryPyramidingMode::EnforceLimit && self.position_size > 0.0 {
            if matches!(self.commission, Some(pine_ir::StrategyCommission::CashPerOrder(value)) if value > 0.0)
            {
                return self.entry_reversal_with_order_commission(fill, TradeDirection::Short);
            }
            closing_qty = self.position_size.abs();
            self.close_all_position_with_slippage_preserving_exit(
                fill.bar_index,
                fill.time,
                fill.price,
                fill.apply_slippage,
                Some(&fill.id),
            );
        }
        if pyramiding_mode == EntryPyramidingMode::EnforceLimit && !self.can_open_short_entry() {
            return false;
        }
        if pyramiding_mode != EntryPyramidingMode::EnforceLimit && self.position_size > 0.0 {
            return false;
        }
        match self.clamp_strategy_entry_qty(
            super::pending_entries::PendingEntryDirection::Short,
            fill.qty,
        ) {
            None => return false,
            Some(qty) => fill.qty = qty,
        }

        let fill_price = if fill.apply_slippage {
            self.short_entry_fill_price(fill.price)
        } else {
            fill.price
        };
        if !fill_price.is_finite() {
            self.diagnostics.push(RuntimeDiagnostic {
                code: "E_STRATEGY_PRICE".to_owned(),
                message: "`strategy.entry` slipped fill price must be finite".to_owned(),
            });
            return false;
        }
        if !self.can_afford_short_entry(fill.qty, fill_price) {
            self.diagnostics.push(RuntimeDiagnostic {
                code: "E_STRATEGY_MARGIN".to_owned(),
                message: "`strategy.entry` requires more margin than available equity".to_owned(),
            });
            return false;
        }

        self.apply_validated_same_side_open(
            fill,
            fill_price,
            pyramiding_mode,
            TradeDirection::Short,
            "strategy.short",
            closing_qty,
        )
    }

    // A reversal is one transaction. Reuse the atomic netting transition so a
    // cash-per-order fee is shared between closing and opening exposure, rather
    // than charging a separate full fee to each leg. Entry admission has already
    // run above; target sizing retains the entry-specific risk limit.
    fn entry_reversal_with_order_commission(
        &mut self,
        fill: EntryFill,
        direction: TradeDirection,
    ) -> bool {
        let pending_direction = match direction {
            TradeDirection::Long => super::pending_entries::PendingEntryDirection::Long,
            TradeDirection::Short => super::pending_entries::PendingEntryDirection::Short,
        };
        let Some(target) = self.clamp_strategy_entry_qty(pending_direction, fill.qty) else {
            return false;
        };
        if self.pyramiding_limit == 0 {
            return false;
        }
        let transaction = direction.signed_quantity(target + self.position_size.abs());
        self.apply_generic_market_order_netting(
            fill.id,
            transaction,
            fill.bar_index,
            fill.time,
            fill.price,
            fill.metadata,
            fill.apply_slippage,
        )
    }

    pub(super) fn record_open_long_trade(&mut self, open_trade: OpenTrade) {
        self.record_open_long_legacy_state(&open_trade);
        if self.pyramiding_limit <= 1 {
            self.trade_ledger.open_long(open_trade);
        } else {
            self.trade_ledger.append_long(open_trade);
        }
        self.sync_aggregate_position_from_ledger();
    }

    pub(super) fn record_open_long_trade_exceeding_pyramiding(&mut self, open_trade: OpenTrade) {
        self.record_open_long_legacy_state(&open_trade);
        self.trade_ledger.append_long(open_trade);
        self.sync_aggregate_position_from_ledger();
    }

    pub(super) fn record_open_short_trade(&mut self, open_trade: OpenTrade) {
        self.record_open_long_legacy_state(&open_trade);
        if self.pyramiding_limit <= 1 {
            self.trade_ledger.open_short(open_trade);
        } else {
            self.trade_ledger.append_short(open_trade);
        }
        self.sync_aggregate_position_from_ledger();
    }

    #[allow(dead_code)]
    pub(super) fn record_open_short_trade_exceeding_pyramiding(&mut self, open_trade: OpenTrade) {
        self.record_open_long_legacy_state(&open_trade);
        self.trade_ledger.append_short(open_trade);
        self.sync_aggregate_position_from_ledger();
    }

    pub(super) fn sync_aggregate_position_from_ledger(&mut self) {
        let net_position = self.trade_ledger.net_position();
        self.position_size = net_position.signed_size;
        self.avg_price = net_position.avg_price;
        if net_position.signed_size > 0.0 {
            self.max_contracts_held_long =
                self.max_contracts_held_long.max(net_position.signed_size);
        } else if net_position.signed_size < 0.0 {
            self.max_contracts_held_short =
                self.max_contracts_held_short.max(-net_position.signed_size);
        }
        self.debug_assert_ledger_aggregates();
    }

    fn aggregates_match_computed_ledger(&self) -> bool {
        let computed = self.trade_ledger.computed_net_position();
        let cached = self.trade_ledger.net_position();
        computed == cached
            && self.position_size == computed.signed_size
            && self.avg_price == computed.avg_price
    }

    pub(super) fn debug_assert_ledger_aggregates(&self) {
        debug_assert!(
            self.aggregates_match_computed_ledger(),
            "ledger/aggregate divergence: computed={:?} cached={:?} size={} avg={}",
            self.trade_ledger.computed_net_position(),
            self.trade_ledger.net_position(),
            self.position_size,
            self.avg_price
        );
    }

    #[cfg(test)]
    pub(super) fn assert_ledger_aggregates(&self) {
        assert!(
            self.aggregates_match_computed_ledger(),
            "ledger/aggregate divergence: computed={:?} cached={:?} size={} avg={}",
            self.trade_ledger.computed_net_position(),
            self.trade_ledger.net_position(),
            self.position_size,
            self.avg_price
        );
    }

    pub(super) fn apply_trade_allocations_and_sync_position(
        &mut self,
        allocations: &[TradeAllocation],
    ) {
        self.trade_ledger.apply_allocations(allocations);
        self.sync_aggregate_position_from_ledger();
    }
}
