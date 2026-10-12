use super::{
    BrokerState,
    candidates::{BrokerCandidate, BrokerCandidateEvent},
    pending_entries::{PendingEntry, PendingEntryDirection, PendingEntryKind},
    pending_exits::{PendingExitTrigger, PendingTrailingState},
    types::{EntryFill, EntryPyramidingMode, InternalOrderKey, OcaMember, OcaPeerEffects},
};
use crate::runtime::strategy_path::{HistoricalPathKind, MagnifierHostGap, PathLeg};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct EntryPathTick {
    pub bar_index: usize,
    pub time: i64,
    pub leg: PathLeg,
    pub path_kind: HistoricalPathKind,
    pub mark: f64,
    pub long_blocked_at_path_start: bool,
    pub short_blocked_at_path_start: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum PathEventOutcome {
    Filled { mark: f64, fill_price: f64 },
    Activated { mark: f64 },
    Ignored { mark: f64 },
}

impl PathEventOutcome {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn is_fill(self) -> bool {
        matches!(self, Self::Filled { .. })
    }

    pub(crate) fn mark(self) -> f64 {
        match self {
            Self::Filled { mark, .. } | Self::Activated { mark } | Self::Ignored { mark } => mark,
        }
    }
}

impl BrokerState {
    pub(crate) fn fill_same_bar_close_exits(&mut self, bar_index: usize, time: i64, close: f64) {
        while let Some(candidate) = self
            .collect_same_bar_close_exit_candidates(bar_index, close, self.event_generation)
            .into_iter()
            .next()
        {
            let tick = EntryPathTick {
                bar_index,
                time,
                leg: PathLeg::point(close),
                path_kind: HistoricalPathKind::OpenHighLowClose,
                mark: close,
                long_blocked_at_path_start: false,
                short_blocked_at_path_start: false,
            };
            if !self.apply_exit_path_candidate(&candidate, tick).is_fill() {
                break;
            }
        }
    }

    pub(super) fn take_next_realtime_event(
        &mut self,
        tick: EntryPathTick,
    ) -> Option<PathEventOutcome> {
        let winner = self
            .collect_observed_price_candidates(
                tick.bar_index,
                tick.mark,
                self.event_generation,
                true,
            )
            .into_iter()
            .find(|candidate| candidate.observed_generation == self.event_generation)?;
        Some(self.apply_entry_path_candidate(&winner, tick))
    }

    pub(crate) fn take_next_gap_event(
        &mut self,
        tick: EntryPathTick,
        gap: MagnifierHostGap,
    ) -> Option<PathEventOutcome> {
        let winner = self
            .collect_gap_candidates(tick.bar_index, gap, self.event_generation)
            .into_iter()
            .find(|candidate| candidate.observed_generation == self.event_generation)?;
        Some(self.apply_entry_path_candidate(&winner, tick))
    }

    pub(crate) fn take_next_entry_path_event(
        &mut self,
        tick: EntryPathTick,
    ) -> Option<PathEventOutcome> {
        let winner = self
            .collect_path_leg_candidates_for(tick.bar_index, tick.leg, tick.path_kind)
            .into_iter()
            .find(|candidate| {
                matches!(
                    candidate.event_kind,
                    BrokerCandidateEvent::EntryOrOrderFill
                        | BrokerCandidateEvent::StopLimitActivation
                        | BrokerCandidateEvent::ExitFill
                        | BrokerCandidateEvent::TrailingActivation
                        | BrokerCandidateEvent::TrailingRatchet
                        | BrokerCandidateEvent::MarginCall
                ) && candidate.observed_generation == self.event_generation
                    && tick
                        .leg
                        .contains_unconsumed(tick.mark, candidate.crossing_price)
            })?;
        Some(self.apply_entry_path_candidate(&winner, tick))
    }

    fn apply_entry_path_candidate(
        &mut self,
        candidate: &BrokerCandidate,
        tick: EntryPathTick,
    ) -> PathEventOutcome {
        match candidate.event_kind {
            BrokerCandidateEvent::StopLimitActivation => {
                self.activate_stop_limit_by_key(candidate.stable_order_key, tick.bar_index);
                self.bump_event_generation();
                PathEventOutcome::Activated {
                    mark: candidate.crossing_price,
                }
            }
            BrokerCandidateEvent::TrailingActivation | BrokerCandidateEvent::TrailingRatchet => {
                self.apply_trailing_path_update(
                    candidate.stable_order_key,
                    candidate.crossing_price,
                );
                self.bump_event_generation();
                PathEventOutcome::Activated {
                    mark: candidate.crossing_price,
                }
            }
            BrokerCandidateEvent::ExitFill => self.apply_exit_path_candidate(candidate, tick),
            BrokerCandidateEvent::MarginCall => self.apply_margin_path_candidate(candidate, tick),
            BrokerCandidateEvent::EntryOrOrderFill => {
                let Some(pending) = self
                    .order_book
                    .entries_mut()
                    .remove_by_key(candidate.stable_order_key)
                else {
                    return PathEventOutcome::Ignored {
                        mark: candidate.crossing_price,
                    };
                };
                let blocked = pending.enforce_pyramiding
                    && match pending.direction {
                        PendingEntryDirection::Long => {
                            tick.long_blocked_at_path_start && self.position_size >= 0.0
                        }
                        PendingEntryDirection::Short => {
                            tick.short_blocked_at_path_start && self.position_size <= 0.0
                        }
                    };
                if blocked {
                    self.bump_event_generation();
                    return PathEventOutcome::Ignored {
                        mark: candidate.crossing_price,
                    };
                }
                let before = self.public_fill_event_count();
                let _ = self.fill_pending_generic_or_entry(
                    pending,
                    tick.bar_index,
                    tick.time,
                    candidate.fill_price_or_mark,
                );
                self.bump_event_generation();
                if self.public_fill_event_count() > before {
                    PathEventOutcome::Filled {
                        mark: candidate.crossing_price,
                        fill_price: candidate.fill_price_or_mark,
                    }
                } else {
                    PathEventOutcome::Ignored {
                        mark: candidate.crossing_price,
                    }
                }
            }
            _ => PathEventOutcome::Ignored {
                mark: candidate.crossing_price,
            },
        }
    }

    fn apply_trailing_path_update(&mut self, key: InternalOrderKey, mark: f64) {
        let offset = {
            let Some(pending) = self
                .order_book
                .exits()
                .iter()
                .find(|pending| pending.key == key)
            else {
                return;
            };
            let PendingExitTrigger::Trailing(trailing) = &pending.trigger else {
                return;
            };
            trailing.spec.offset_price_distance
        };
        let stop_price = if self.position_size < 0.0 {
            mark + offset
        } else {
            mark - offset
        };
        let Some(pending) = self.order_book.exits_mut().find_mut_by_key(key) else {
            return;
        };
        let PendingExitTrigger::Trailing(trailing) = &mut pending.trigger else {
            return;
        };
        trailing.state = PendingTrailingState::Active { stop_price };
    }

    fn apply_exit_path_candidate(
        &mut self,
        candidate: &BrokerCandidate,
        tick: EntryPathTick,
    ) -> PathEventOutcome {
        let eligible = self
            .order_book
            .exits()
            .iter()
            .find(|pending| pending.key == candidate.stable_order_key)
            .is_some_and(|pending| self.pending_exit_has_position(pending));
        if !eligible {
            return PathEventOutcome::Ignored {
                mark: candidate.crossing_price,
            };
        }
        let Some(pending) = self
            .order_book
            .exits_mut()
            .remove_by_key(candidate.stable_order_key)
        else {
            return PathEventOutcome::Ignored {
                mark: candidate.crossing_price,
            };
        };
        let from_entry = pending.from_entry.clone();
        let exit_id = pending.id.clone();
        let target_trade_key = pending.target_trade_key;
        let filled_qty = pending.reserved_quantity.min(self.position_size.abs());
        let before = self.public_fill_event_count();
        self.fill_pending_exit(
            pending,
            tick.bar_index,
            tick.time,
            candidate.fill_price_or_mark,
        );
        self.order_book.apply_oca_after_exit_fill(
            &exit_id,
            &from_entry,
            target_trade_key,
            filled_qty,
        );
        if self.position_size == 0.0 {
            self.order_book.exits_mut().clear_all();
        } else if self.close_entries_rule == pine_ir::StrategyCloseEntriesRule::Any
            && !self.has_open_position_for_entry(&from_entry)
        {
            self.order_book.exits_mut().clear_for_entry(&from_entry);
        }
        self.debug_assert_ledger_aggregates();
        self.bump_event_generation();
        if self.public_fill_event_count() > before {
            PathEventOutcome::Filled {
                mark: candidate.crossing_price,
                fill_price: candidate.fill_price_or_mark,
            }
        } else {
            PathEventOutcome::Ignored {
                mark: candidate.crossing_price,
            }
        }
    }

    fn apply_margin_path_candidate(
        &mut self,
        candidate: &BrokerCandidate,
        tick: EntryPathTick,
    ) -> PathEventOutcome {
        let before = self.public_fill_event_count();
        let mark = candidate.fill_price_or_mark;
        if self.position_size > 0.0 {
            self.evaluate_margin_call_long(tick.bar_index, tick.time, mark);
        } else if self.position_size < 0.0 {
            self.evaluate_margin_call_short(tick.bar_index, tick.time, mark);
        }
        self.debug_assert_ledger_aggregates();
        self.bump_event_generation();
        if self.public_fill_event_count() > before {
            self.order_book.entries_mut().clear_all();
            PathEventOutcome::Filled {
                mark: candidate.crossing_price,
                fill_price: mark,
            }
        } else {
            PathEventOutcome::Ignored {
                mark: candidate.crossing_price,
            }
        }
    }

    fn activate_stop_limit_by_key(&mut self, key: InternalOrderKey, bar_index: usize) {
        let Some(pending) = self.order_book.entries_mut().find_mut_by_key(key) else {
            return;
        };
        let PendingEntryKind::StopLimit {
            activated_bar_index,
            ..
        } = &mut pending.kind
        else {
            return;
        };
        if activated_bar_index.is_none() {
            *activated_bar_index = Some(bar_index);
        }
    }

    pub(crate) fn fill_pending_market_entries(
        &mut self,
        bar_index: usize,
        time: i64,
        fill_price: f64,
    ) {
        loop {
            let pending_entry = self
                .order_book
                .entries_mut()
                .take_first_eligible_market_long(bar_index);
            let Some(pending_entry) = pending_entry else {
                break;
            };
            self.fill_one_pending_market_entry(pending_entry, bar_index, time, fill_price, false);
        }
    }

    pub(crate) fn fill_same_bar_market_entries(
        &mut self,
        bar_index: usize,
        time: i64,
        fill_price: f64,
    ) {
        loop {
            let pending_entry = self
                .order_book
                .entries_mut()
                .take_first_same_bar_market(bar_index);
            let Some(pending_entry) = pending_entry else {
                break;
            };
            self.fill_one_pending_market_entry(pending_entry, bar_index, time, fill_price, true);
        }
    }

    pub(crate) fn fill_same_bar_limit_entries(&mut self, bar_index: usize, time: i64, close: f64) {
        let entries = self.order_book.entries_mut().take_same_bar_eligible_limits(
            bar_index,
            close,
            self.limit_verification_price_offset,
        );
        self.fill_taken_generic_or_entries(entries, bar_index, time, |_| Some(close));
    }

    fn fill_one_pending_market_entry(
        &mut self,
        mut pending_entry: super::pending_entries::PendingEntry,
        bar_index: usize,
        time: i64,
        fill_price: f64,
        same_bar: bool,
    ) {
        if same_bar && let Some(percent) = pending_entry.same_bar_percent_of_equity {
            let execution_price = match pending_entry.direction {
                PendingEntryDirection::Long => self.long_entry_fill_price(fill_price),
                PendingEntryDirection::Short => self.short_entry_fill_price(fill_price),
            };
            let equity = self.equity_value(fill_price);
            if !equity.is_finite()
                || !execution_price.is_finite()
                || equity <= 0.0
                || execution_price <= 0.0
            {
                return;
            }
            pending_entry.quantity = super::super::percent_of_equity_order_qty(
                equity,
                percent,
                execution_price,
                self.commission,
                self.configured_quantity_scale,
            );
        }
        if !pending_entry.enforce_pyramiding {
            let signed_quantity = match pending_entry.direction {
                PendingEntryDirection::Long => pending_entry.quantity,
                PendingEntryDirection::Short => -pending_entry.quantity,
            };
            let filled_key = pending_entry.key;
            let filled_qty = pending_entry.quantity;
            if self.apply_generic_market_order_netting(
                pending_entry.id,
                signed_quantity,
                bar_index,
                time,
                fill_price,
                pending_entry.metadata,
                true,
            ) {
                self.order_book.apply_oca_after_fill(filled_key, filled_qty);
            }
            return;
        }
        if pending_entry.direction == PendingEntryDirection::Short {
            let filled_key = pending_entry.key;
            let oca_group = self
                .order_book
                .oca_group(&OcaMember::Order(filled_key))
                .cloned();
            let filled_qty = pending_entry.quantity;
            let transaction_qty = if filled_qty == 0.0 {
                self.position_size.abs()
            } else {
                filled_qty
            };
            let entry_id = pending_entry.id;
            let filled = self.entry_short_internal(
                EntryFill {
                    id: entry_id.clone(),
                    bar_index,
                    time,
                    price: fill_price,
                    apply_slippage: true,
                    qty: filled_qty,
                    metadata: pending_entry.metadata,
                },
                EntryPyramidingMode::EnforceLimit,
            );
            if filled {
                if let Some(group) = oca_group {
                    self.order_book
                        .assign_oca(OcaMember::Order(filled_key), group);
                }
                self.order_book
                    .apply_oca_after_fill(filled_key, transaction_qty);
                self.resolve_deferred_relative_exits_for_entry(&entry_id, bar_index);
                self.expand_persistent_all_entry_exit_for_new_entry(bar_index);
            } else {
                self.order_book.exits_mut().clear_for_entry(&entry_id);
            }
            return;
        }

        let filled_key = pending_entry.key;
        let filled_qty = pending_entry.quantity;
        let transaction_qty = if filled_qty == 0.0 {
            self.position_size.abs()
        } else {
            filled_qty
        };
        let oca_group = self
            .order_book
            .oca_group(&OcaMember::Order(filled_key))
            .cloned();
        let entry_id = pending_entry.id;
        let filled = self.entry_long_internal(
            EntryFill {
                id: entry_id.clone(),
                bar_index,
                time,
                price: fill_price,
                apply_slippage: true,
                qty: filled_qty,
                metadata: pending_entry.metadata,
            },
            EntryPyramidingMode::EnforceLimit,
        );
        if filled {
            if let Some(group) = oca_group {
                self.order_book
                    .assign_oca(OcaMember::Order(filled_key), group);
            }
            self.order_book
                .apply_oca_after_fill(filled_key, transaction_qty);
            self.resolve_deferred_relative_exits_for_entry(&entry_id, bar_index);
            self.expand_persistent_all_entry_exit_for_new_entry(bar_index);
        } else {
            self.order_book.exits_mut().clear_for_entry(&entry_id);
        }
    }

    pub(crate) fn fill_pending_limit_long_entries(
        &mut self,
        bar_index: usize,
        time: i64,
        low: f64,
    ) {
        if self.same_side_long_entry_blocked()
            && !self
                .order_book
                .entries()
                .has_limit_long_bypassing_pyramiding()
        {
            if self
                .order_book
                .entries()
                .has_price_based_long_bypassing_pyramiding()
            {
                return;
            }
            self.order_book
                .entries_mut()
                .clear_direction(PendingEntryDirection::Long);
            return;
        }
        let pending_entries = self.order_book.entries_mut().take_all_eligible_limit_long(
            bar_index,
            low,
            self.limit_verification_price_offset,
        );
        if pending_entries.is_empty() {
            return;
        }

        self.fill_taken_generic_or_entries(pending_entries, bar_index, time, |pending_entry| {
            match pending_entry.kind {
                PendingEntryKind::Limit { price } => Some(price),
                _ => None,
            }
        });
    }

    pub(crate) fn fill_pending_stop_long_entries(
        &mut self,
        bar_index: usize,
        time: i64,
        high: f64,
    ) {
        if self.same_side_long_entry_blocked()
            && !self
                .order_book
                .entries()
                .has_stop_long_bypassing_pyramiding()
        {
            if self
                .order_book
                .entries()
                .has_price_based_long_bypassing_pyramiding()
            {
                return;
            }
            self.order_book
                .entries_mut()
                .clear_direction(PendingEntryDirection::Long);
            return;
        }
        let pending_entries = self
            .order_book
            .entries_mut()
            .take_all_eligible_stop_long(bar_index, high);
        if pending_entries.is_empty() {
            return;
        }

        self.fill_taken_generic_or_entries(pending_entries, bar_index, time, |pending_entry| {
            match pending_entry.kind {
                PendingEntryKind::Stop { price } => Some(price),
                _ => None,
            }
        });
    }

    pub(crate) fn fill_pending_stop_limit_long_entries(
        &mut self,
        bar_index: usize,
        time: i64,
        high: f64,
        low: f64,
    ) {
        if self.same_side_long_entry_blocked()
            && !self
                .order_book
                .entries()
                .has_stop_limit_long_bypassing_pyramiding()
        {
            if self
                .order_book
                .entries()
                .has_price_based_long_bypassing_pyramiding()
            {
                return;
            }
            self.order_book
                .entries_mut()
                .clear_direction(PendingEntryDirection::Long);
            return;
        }
        self.order_book
            .entries_mut()
            .activate_stop_limit_long_entries(bar_index, high);
        let pending_entries = self
            .order_book
            .entries_mut()
            .take_all_eligible_stop_limit_long(
                bar_index,
                low,
                self.limit_verification_price_offset,
            );
        if pending_entries.is_empty() {
            return;
        }

        self.fill_taken_generic_or_entries(pending_entries, bar_index, time, |pending_entry| {
            match pending_entry.kind {
                PendingEntryKind::StopLimit { limit_price, .. } => Some(limit_price),
                _ => None,
            }
        });
    }

    pub(crate) fn fill_pending_limit_short_entries(
        &mut self,
        bar_index: usize,
        time: i64,
        high: f64,
    ) {
        if self.same_side_short_entry_blocked()
            && !self
                .order_book
                .entries()
                .has_limit_short_bypassing_pyramiding()
        {
            if self
                .order_book
                .entries()
                .has_price_based_short_bypassing_pyramiding()
            {
                return;
            }
            self.order_book
                .entries_mut()
                .clear_direction(PendingEntryDirection::Short);
            return;
        }
        let pending_entries = self.order_book.entries_mut().take_all_eligible_limit_short(
            bar_index,
            high,
            self.limit_verification_price_offset,
        );
        if pending_entries.is_empty() {
            return;
        }

        self.fill_taken_generic_or_entries(pending_entries, bar_index, time, |pending_entry| {
            match pending_entry.kind {
                PendingEntryKind::Limit { price } => Some(price),
                _ => None,
            }
        });
    }

    fn fill_taken_generic_or_entries(
        &mut self,
        pending_entries: Vec<PendingEntry>,
        bar_index: usize,
        time: i64,
        mut fill_price: impl FnMut(&PendingEntry) -> Option<f64>,
    ) {
        let mut cancelled = HashSet::new();
        let mut reduce_by: HashMap<InternalOrderKey, f64> = HashMap::new();
        let mut remaining_qty: HashMap<InternalOrderKey, f64> = HashMap::new();
        for mut pending_entry in pending_entries {
            if cancelled.contains(&pending_entry.key) {
                continue;
            }
            if let Some(&qty) = remaining_qty.get(&pending_entry.key) {
                pending_entry.quantity = qty;
            } else if let Some(&sub) = reduce_by.get(&pending_entry.key) {
                pending_entry.quantity = (pending_entry.quantity - sub).max(0.0);
            }
            if pending_entry.quantity <= 0.0
                && (!pending_entry.enforce_pyramiding
                    || remaining_qty.contains_key(&pending_entry.key)
                    || reduce_by.contains_key(&pending_entry.key))
            {
                self.order_book.clear_oca_order(pending_entry.key);
                continue;
            }
            let Some(price) = fill_price(&pending_entry) else {
                continue;
            };
            let filled_qty = if pending_entry.enforce_pyramiding && pending_entry.quantity == 0.0 {
                self.position_size.abs()
            } else {
                pending_entry.quantity
            };
            let effects = self.fill_pending_generic_or_entry(pending_entry, bar_index, time, price);
            cancelled.extend(effects.cancelled);
            for (key, remaining) in effects.reduced {
                remaining_qty.insert(key, remaining);
                if remaining <= 0.0 {
                    cancelled.insert(key);
                }
            }
            for key in effects.reduce_taken {
                *reduce_by.entry(key).or_insert(0.0) += filled_qty;
            }
        }
    }

    fn fill_pending_generic_or_entry(
        &mut self,
        pending_entry: PendingEntry,
        bar_index: usize,
        time: i64,
        price: f64,
    ) -> OcaPeerEffects {
        let apply_slippage = matches!(
            pending_entry.kind,
            PendingEntryKind::Market | PendingEntryKind::Stop { .. }
        );
        if !pending_entry.enforce_pyramiding {
            let signed_quantity = match pending_entry.direction {
                PendingEntryDirection::Long => pending_entry.quantity,
                PendingEntryDirection::Short => -pending_entry.quantity,
            };
            let filled_key = pending_entry.key;
            let filled_qty = pending_entry.quantity;
            if self.apply_generic_market_order_netting(
                pending_entry.id,
                signed_quantity,
                bar_index,
                time,
                price,
                pending_entry.metadata,
                apply_slippage,
            ) {
                return self.order_book.apply_oca_after_fill(filled_key, filled_qty);
            }
            return OcaPeerEffects::default();
        }

        let filled_key = pending_entry.key;
        let filled_qty = pending_entry.quantity;
        let transaction_qty = if filled_qty == 0.0 {
            self.position_size.abs()
        } else {
            filled_qty
        };
        let entry_id = pending_entry.id;
        let opposite = match pending_entry.direction {
            PendingEntryDirection::Long => self.position_size < 0.0,
            PendingEntryDirection::Short => self.position_size > 0.0,
        };
        let oca_group = self
            .order_book
            .oca_group(&OcaMember::Order(filled_key))
            .cloned();
        let pyramiding_mode = if opposite {
            EntryPyramidingMode::EnforceLimit
        } else {
            EntryPyramidingMode::SameTickPriceException
        };
        let filled = match pending_entry.direction {
            PendingEntryDirection::Long => self.entry_long_internal(
                EntryFill {
                    id: entry_id.clone(),
                    bar_index,
                    time,
                    price,
                    apply_slippage,
                    qty: filled_qty,
                    metadata: pending_entry.metadata,
                },
                pyramiding_mode,
            ),
            PendingEntryDirection::Short => self.entry_short_internal(
                EntryFill {
                    id: entry_id.clone(),
                    bar_index,
                    time,
                    price,
                    apply_slippage,
                    qty: filled_qty,
                    metadata: pending_entry.metadata,
                },
                pyramiding_mode,
            ),
        };
        if filled {
            if let Some(group) = oca_group {
                self.order_book
                    .assign_oca(OcaMember::Order(filled_key), group);
            }
            let effects = self
                .order_book
                .apply_oca_after_fill(filled_key, transaction_qty);
            self.resolve_deferred_relative_exits_for_entry(&entry_id, bar_index);
            self.expand_persistent_all_entry_exit_for_new_entry(bar_index);
            effects
        } else {
            self.order_book.exits_mut().clear_for_entry(&entry_id);
            OcaPeerEffects::default()
        }
    }

    pub(crate) fn fill_pending_stop_short_entries(
        &mut self,
        bar_index: usize,
        time: i64,
        low: f64,
    ) {
        if self.same_side_short_entry_blocked()
            && !self
                .order_book
                .entries()
                .has_stop_short_bypassing_pyramiding()
        {
            if self
                .order_book
                .entries()
                .has_price_based_short_bypassing_pyramiding()
            {
                return;
            }
            self.order_book
                .entries_mut()
                .clear_direction(PendingEntryDirection::Short);
            return;
        }
        let pending_entries = self
            .order_book
            .entries_mut()
            .take_all_eligible_stop_short(bar_index, low);
        if pending_entries.is_empty() {
            return;
        }

        self.fill_taken_generic_or_entries(pending_entries, bar_index, time, |pending_entry| {
            match pending_entry.kind {
                PendingEntryKind::Stop { price } => Some(price),
                _ => None,
            }
        });
    }

    pub(crate) fn fill_pending_stop_limit_short_entries(
        &mut self,
        bar_index: usize,
        time: i64,
        high: f64,
        low: f64,
    ) {
        if self.same_side_short_entry_blocked()
            && !self
                .order_book
                .entries()
                .has_stop_limit_short_bypassing_pyramiding()
        {
            if self
                .order_book
                .entries()
                .has_price_based_short_bypassing_pyramiding()
            {
                return;
            }
            self.order_book
                .entries_mut()
                .clear_direction(PendingEntryDirection::Short);
            return;
        }
        self.order_book
            .entries_mut()
            .activate_stop_limit_short_entries(bar_index, low);
        let pending_entries = self
            .order_book
            .entries_mut()
            .take_all_eligible_stop_limit_short(
                bar_index,
                high,
                self.limit_verification_price_offset,
            );
        if pending_entries.is_empty() {
            return;
        }

        self.fill_taken_generic_or_entries(pending_entries, bar_index, time, |pending_entry| {
            match pending_entry.kind {
                PendingEntryKind::StopLimit { limit_price, .. } => Some(limit_price),
                _ => None,
            }
        });
    }
}
