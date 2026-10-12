#![allow(dead_code)]

use super::BrokerState;
use super::pending_entries::{PendingEntry, PendingEntryDirection, PendingEntryKind};
use super::pending_exits::{PendingExit, PendingExitTrigger, PendingTrailingState};
use super::types::{InternalOrderKey, StrategyCommandOrigin};
use crate::runtime::strategy_path::{HistoricalPathKind, MagnifierHostGap, PathLeg};
use crate::strategy::broker::ledger::TradeDirection;
use std::cmp::Ordering;

pub(super) const MARGIN_ORDER_KEY: InternalOrderKey = InternalOrderKey(u64::MAX);
pub(super) const RISK_ORDER_KEY: InternalOrderKey = InternalOrderKey(u64::MAX - 1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum BrokerCandidatePhase {
    MarketOpen,
    PathLeg,
    BarClose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BrokerCandidateEvent {
    EntryOrOrderFill,
    ExitFill,
    StopLimitActivation,
    TrailingActivation,
    TrailingRatchet,
    MarginCall,
    RiskFlatten,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct BrokerCandidate {
    pub event_kind: BrokerCandidateEvent,
    pub phase: BrokerCandidatePhase,
    pub path_leg: u8,
    pub crossing_price: f64,
    pub fill_price_or_mark: f64,
    pub creation_sequence: u64,
    pub stable_order_key: InternalOrderKey,
    pub observed_generation: u64,
    pub origin: StrategyCommandOrigin,
    pub public_id: String,
}

impl BrokerCandidate {
    fn user_before_synthetic_rank(&self) -> u8 {
        match self.event_kind {
            BrokerCandidateEvent::MarginCall => 1,
            BrokerCandidateEvent::RiskFlatten => 2,
            BrokerCandidateEvent::EntryOrOrderFill
            | BrokerCandidateEvent::ExitFill
            | BrokerCandidateEvent::StopLimitActivation
            | BrokerCandidateEvent::TrailingActivation
            | BrokerCandidateEvent::TrailingRatchet => 0,
        }
    }
}

pub(super) fn cmp_candidates(
    left: &BrokerCandidate,
    right: &BrokerCandidate,
    leg: Option<PathLeg>,
) -> Ordering {
    left.phase
        .cmp(&right.phase)
        .then(left.path_leg.cmp(&right.path_leg))
        .then_with(|| {
            if let Some(leg) = leg {
                leg.cmp_crossing_prices(left.crossing_price, right.crossing_price)
            } else {
                left.crossing_price.total_cmp(&right.crossing_price)
            }
        })
        .then(
            left.user_before_synthetic_rank()
                .cmp(&right.user_before_synthetic_rank()),
        )
        .then(left.creation_sequence.cmp(&right.creation_sequence))
        .then(left.stable_order_key.0.cmp(&right.stable_order_key.0))
}

impl BrokerState {
    pub(super) fn event_generation(&self) -> u64 {
        self.event_generation
    }

    pub(super) fn bump_event_generation(&mut self) {
        self.event_generation = self.event_generation.wrapping_add(1);
    }

    pub(super) fn collect_market_open_candidates(&self, bar_index: usize) -> Vec<BrokerCandidate> {
        let generation = self.event_generation;
        let mut candidates = Vec::new();
        for pending in self.order_book.entries().iter() {
            if pending.kind != PendingEntryKind::Market || pending.created_bar_index >= bar_index {
                continue;
            }
            candidates.push(entry_fill_candidate(
                pending,
                BrokerCandidatePhase::MarketOpen,
                0,
                0.0,
                0.0,
                generation,
            ));
        }
        for pending in self.order_book.closes().iter() {
            if pending.created_bar_index >= bar_index {
                continue;
            }
            candidates.push(BrokerCandidate {
                event_kind: BrokerCandidateEvent::ExitFill,
                phase: BrokerCandidatePhase::MarketOpen,
                path_leg: 0,
                crossing_price: 0.0,
                fill_price_or_mark: 0.0,
                creation_sequence: pending.key.0,
                stable_order_key: pending.key,
                observed_generation: generation,
                origin: pending.origin,
                public_id: pending.public_id().unwrap_or("close_all").to_owned(),
            });
        }
        candidates.sort_by(|left, right| cmp_candidates(left, right, None));
        candidates
    }

    pub(super) fn collect_path_leg_candidates(
        &self,
        bar_index: usize,
        leg: PathLeg,
    ) -> Vec<BrokerCandidate> {
        self.collect_path_leg_candidates_for(bar_index, leg, HistoricalPathKind::OpenHighLowClose)
    }

    pub(super) fn collect_path_leg_candidates_for(
        &self,
        bar_index: usize,
        leg: PathLeg,
        path_kind: HistoricalPathKind,
    ) -> Vec<BrokerCandidate> {
        let generation = self.event_generation;
        let high = leg.from.price.max(leg.to.price);
        let low = leg.from.price.min(leg.to.price);
        let verify = self.limit_verification_price_offset;
        let mut candidates = Vec::new();
        for pending in self.order_book.entries().iter() {
            if !self
                .order_book
                .entries()
                .price_created_eligible(pending.created_bar_index, bar_index)
            {
                continue;
            }
            candidates.extend(entry_leg_candidates(
                pending, bar_index, path_kind, leg, verify, generation,
            ));
        }
        let direction = if self.position_size < 0.0 {
            TradeDirection::Short
        } else {
            TradeDirection::Long
        };
        for pending in self.order_book.exits().iter() {
            if !self
                .order_book
                .exits()
                .price_created_eligible(pending.last_update_bar_index, bar_index)
            {
                continue;
            }
            if !self.pending_exit_has_position(pending) {
                continue;
            }
            // Decimal chart extremes can arrive one binary ULP either side
            // of a tick. Fixed exit triggers compare on that tick, but their
            // crossing marks must remain inside the original host path.
            let mut exit_leg = leg;
            if !matches!(pending.trigger, PendingExitTrigger::Trailing(_))
                && let Some(tick) = self.price_tick
            {
                exit_leg.from.price = canonical_grid_endpoint(leg.from.price, tick);
                exit_leg.to.price = canonical_grid_endpoint(leg.to.price, tick);
            }
            let mut exits = exit_leg_candidates(
                pending,
                direction,
                exit_leg,
                exit_leg.from.price.max(exit_leg.to.price),
                exit_leg.from.price.min(exit_leg.to.price),
                verify,
                generation,
            );
            for candidate in &mut exits {
                candidate.crossing_price = candidate.crossing_price.clamp(low, high);
            }
            candidates.extend(exits);
        }
        if let Some(candidate) = self.margin_candidate_at(leg.index, low, high, generation) {
            candidates.push(candidate);
        }
        candidates.sort_by(|left, right| cmp_candidates(left, right, Some(leg)));
        candidates
    }

    pub(super) fn collect_gap_candidates(
        &self,
        bar_index: usize,
        gap: MagnifierHostGap,
        generation: u64,
    ) -> Vec<BrokerCandidate> {
        self.collect_observed_price_candidates(bar_index, gap.next_open, generation, false)
    }

    pub(super) fn collect_observed_price_candidates(
        &self,
        bar_index: usize,
        fill: f64,
        generation: u64,
        realtime: bool,
    ) -> Vec<BrokerCandidate> {
        let mut candidates = Vec::new();
        let verify = self.limit_verification_price_offset;
        for pending in self.order_book.entries().iter() {
            if !self
                .order_book
                .entries()
                .price_created_eligible(pending.created_bar_index, bar_index)
            {
                continue;
            }
            match (pending.direction, &pending.kind) {
                (direction, PendingEntryKind::Limit { price })
                    if entry_limit_marketable(direction, fill, *price, verify) =>
                {
                    candidates.push(entry_fill_candidate(
                        pending,
                        BrokerCandidatePhase::PathLeg,
                        0,
                        fill,
                        fill,
                        generation,
                    ));
                }
                (direction, PendingEntryKind::Stop { price })
                    if entry_stop_marketable(direction, fill, *price) =>
                {
                    candidates.push(entry_fill_candidate(
                        pending,
                        BrokerCandidatePhase::PathLeg,
                        0,
                        fill,
                        fill,
                        generation,
                    ));
                }
                (
                    direction,
                    PendingEntryKind::StopLimit {
                        stop_price,
                        limit_price,
                        activated_bar_index,
                    },
                ) => {
                    if activated_bar_index.is_none()
                        && entry_stop_marketable(direction, fill, *stop_price)
                    {
                        if (realtime
                            || same_bar_stop_limit_fill_allowed(
                                HistoricalPathKind::OpenHighLowClose,
                                direction,
                            ))
                            && entry_limit_marketable(direction, fill, *limit_price, verify)
                        {
                            candidates.push(entry_fill_candidate(
                                pending,
                                BrokerCandidatePhase::PathLeg,
                                0,
                                fill,
                                fill,
                                generation,
                            ));
                            continue;
                        }
                        candidates.push(BrokerCandidate {
                            event_kind: BrokerCandidateEvent::StopLimitActivation,
                            phase: BrokerCandidatePhase::PathLeg,
                            path_leg: 0,
                            crossing_price: fill,
                            fill_price_or_mark: fill,
                            creation_sequence: pending.key.0,
                            stable_order_key: pending.key,
                            observed_generation: generation,
                            origin: pending.origin,
                            public_id: pending.id.clone(),
                        });
                    } else if activated_bar_index.is_some_and(|activated| {
                        realtime
                            || stop_limit_fill_bar_eligible(
                                activated,
                                bar_index,
                                HistoricalPathKind::OpenHighLowClose,
                                direction,
                            )
                    }) && entry_limit_marketable(direction, fill, *limit_price, verify)
                    {
                        candidates.push(entry_fill_candidate(
                            pending,
                            BrokerCandidatePhase::PathLeg,
                            0,
                            fill,
                            fill,
                            generation,
                        ));
                    }
                }
                _ => {}
            }
        }
        let direction = if self.position_size < 0.0 {
            TradeDirection::Short
        } else {
            TradeDirection::Long
        };
        for pending in self.order_book.exits().iter() {
            if !self
                .order_book
                .exits()
                .price_created_eligible(pending.last_update_bar_index, bar_index)
            {
                continue;
            }
            if !self.pending_exit_has_position(pending) {
                continue;
            }
            candidates.extend(exit_gap_candidates(
                pending, direction, fill, verify, generation,
            ));
        }
        candidates.sort_by(|left, right| cmp_candidates(left, right, None));
        // Multiple profit targets can be marketable at one gap open. Their
        // fills all use that open, while the deeper target has priority.
        let priority = |candidate: &BrokerCandidate| {
            (candidate.event_kind == BrokerCandidateEvent::ExitFill)
                .then(|| {
                    self.order_book
                        .exits()
                        .iter()
                        .find(|pending| pending.key == candidate.stable_order_key)
                        .and_then(|pending| match &pending.trigger {
                            PendingExitTrigger::Limit(price) => Some(*price),
                            PendingExitTrigger::Bracket { downside, upside }
                                if !exit_stop_marketable(direction, fill, *downside)
                                    && exit_limit_marketable(direction, fill, *upside, verify) =>
                            {
                                Some(*upside)
                            }
                            _ => None,
                        })
                })
                .flatten()
                .map(|price| match direction {
                    TradeDirection::Short => price,
                    TradeDirection::Long => -price,
                })
        };
        let profit_slots: Vec<_> = candidates
            .iter()
            .enumerate()
            .filter_map(|(index, candidate)| priority(candidate).map(|price| (index, price)))
            .collect();
        let mut profit_exits: Vec<_> = profit_slots
            .iter()
            .map(|(index, price)| (candidates[*index].clone(), *price))
            .collect();
        profit_exits.sort_by(|(left, left_price), (right, right_price)| {
            left_price
                .total_cmp(right_price)
                .then(left.creation_sequence.cmp(&right.creation_sequence))
        });
        for ((index, _), (candidate, _)) in profit_slots.into_iter().zip(profit_exits) {
            candidates[index] = candidate;
        }
        candidates
    }

    pub(super) fn collect_same_bar_close_exit_candidates(
        &self,
        bar_index: usize,
        close: f64,
        generation: u64,
    ) -> Vec<BrokerCandidate> {
        let Some(direction) = self.active_close_direction() else {
            return Vec::new();
        };
        let mut candidates = Vec::new();
        for pending in self.order_book.exits().iter() {
            if pending.last_update_bar_index > bar_index || !self.pending_exit_has_position(pending)
            {
                continue;
            }
            candidates.extend(exit_gap_candidates(
                pending,
                direction,
                close,
                self.limit_verification_price_offset,
                generation,
            ));
        }
        candidates.retain(|candidate| candidate.event_kind == BrokerCandidateEvent::ExitFill);
        candidates.sort_by(|left, right| cmp_candidates(left, right, None));
        candidates
    }

    fn margin_candidate_at(
        &self,
        path_leg: u8,
        low: f64,
        high: f64,
        generation: u64,
    ) -> Option<BrokerCandidate> {
        let mark = if self.position_size > 0.0 {
            low
        } else if self.position_size < 0.0 {
            high
        } else {
            return None;
        };
        let qty = self.margin_call_quantity(mark)?;
        if qty <= 0.0 {
            return None;
        }
        Some(BrokerCandidate {
            event_kind: BrokerCandidateEvent::MarginCall,
            phase: BrokerCandidatePhase::PathLeg,
            path_leg,
            crossing_price: mark,
            fill_price_or_mark: mark,
            creation_sequence: u64::MAX,
            stable_order_key: MARGIN_ORDER_KEY,
            observed_generation: generation,
            origin: StrategyCommandOrigin::MarginCall,
            public_id: "Margin Call".to_owned(),
        })
    }
}

fn canonical_grid_endpoint(price: f64, tick: f64) -> f64 {
    if !price.is_finite() || !tick.is_finite() || tick <= 0.0 {
        return price;
    }
    let canonical = super::exit_price_orders::canonical_tick_price((price / tick).round(), tick);
    if (price - canonical).abs() <= 8.0 * f64::EPSILON * price.abs().max(tick) {
        canonical
    } else {
        price
    }
}

fn entry_limit_marketable(
    direction: PendingEntryDirection,
    open: f64,
    price: f64,
    verify: f64,
) -> bool {
    match direction {
        PendingEntryDirection::Long => open <= price - verify,
        PendingEntryDirection::Short => open >= price + verify,
    }
}

fn entry_stop_marketable(direction: PendingEntryDirection, open: f64, price: f64) -> bool {
    match direction {
        PendingEntryDirection::Long => open >= price,
        PendingEntryDirection::Short => open <= price,
    }
}

fn entry_fill_candidate(
    pending: &PendingEntry,
    phase: BrokerCandidatePhase,
    path_leg: u8,
    crossing_price: f64,
    fill_price: f64,
    generation: u64,
) -> BrokerCandidate {
    BrokerCandidate {
        event_kind: BrokerCandidateEvent::EntryOrOrderFill,
        phase,
        path_leg,
        crossing_price,
        fill_price_or_mark: fill_price,
        creation_sequence: pending.key.0,
        stable_order_key: pending.key,
        observed_generation: generation,
        origin: pending.origin,
        public_id: pending.id.clone(),
    }
}

fn price_on_leg(leg: PathLeg, price: f64) -> bool {
    leg.contains_price(price)
}

/// Crossing used to visit a trigger on this leg.
///
/// If the exact price is inside the segment, use it. If the bar already
/// traded through the trigger (gap or a doji beyond the level), clamp onto
/// the segment so the fill still occurs at the requested price.
fn crossing_on_leg(leg: PathLeg, price: f64) -> Option<f64> {
    if !price.is_finite() {
        return None;
    }
    if price_on_leg(leg, price) {
        return Some(price);
    }
    let low = leg.from.price.min(leg.to.price);
    let high = leg.from.price.max(leg.to.price);
    Some(price.clamp(low, high))
}

fn same_bar_stop_limit_fill_allowed(
    path_kind: HistoricalPathKind,
    direction: PendingEntryDirection,
) -> bool {
    path_kind == HistoricalPathKind::OpenHighLowClose && direction == PendingEntryDirection::Long
}

fn stop_limit_fill_bar_eligible(
    activated_bar_index: usize,
    bar_index: usize,
    path_kind: HistoricalPathKind,
    direction: PendingEntryDirection,
) -> bool {
    activated_bar_index < bar_index
        || (activated_bar_index == bar_index
            && same_bar_stop_limit_fill_allowed(path_kind, direction))
}

fn entry_leg_candidates(
    pending: &PendingEntry,
    bar_index: usize,
    path_kind: HistoricalPathKind,
    leg: PathLeg,
    verify: f64,
    generation: u64,
) -> Vec<BrokerCandidate> {
    let high = leg.from.price.max(leg.to.price);
    let low = leg.from.price.min(leg.to.price);
    let mut out = Vec::new();
    match (pending.direction, &pending.kind) {
        (PendingEntryDirection::Long, PendingEntryKind::Limit { price })
            if low <= *price - verify && price_on_leg(leg, *price) =>
        {
            out.push(entry_fill_candidate(
                pending,
                BrokerCandidatePhase::PathLeg,
                leg.index,
                *price,
                *price,
                generation,
            ));
        }
        (PendingEntryDirection::Short, PendingEntryKind::Limit { price })
            if high >= *price + verify && price_on_leg(leg, *price) =>
        {
            out.push(entry_fill_candidate(
                pending,
                BrokerCandidatePhase::PathLeg,
                leg.index,
                *price,
                *price,
                generation,
            ));
        }
        (PendingEntryDirection::Long, PendingEntryKind::Stop { price })
            if high >= *price && price_on_leg(leg, *price) =>
        {
            out.push(entry_fill_candidate(
                pending,
                BrokerCandidatePhase::PathLeg,
                leg.index,
                *price,
                *price,
                generation,
            ));
        }
        (PendingEntryDirection::Short, PendingEntryKind::Stop { price })
            if low <= *price && price_on_leg(leg, *price) =>
        {
            out.push(entry_fill_candidate(
                pending,
                BrokerCandidatePhase::PathLeg,
                leg.index,
                *price,
                *price,
                generation,
            ));
        }
        (
            PendingEntryDirection::Long,
            PendingEntryKind::StopLimit {
                stop_price,
                limit_price,
                activated_bar_index,
            },
        ) => {
            if activated_bar_index.is_none()
                && high >= *stop_price
                && price_on_leg(leg, *stop_price)
            {
                out.push(BrokerCandidate {
                    event_kind: BrokerCandidateEvent::StopLimitActivation,
                    phase: BrokerCandidatePhase::PathLeg,
                    path_leg: leg.index,
                    crossing_price: *stop_price,
                    fill_price_or_mark: *stop_price,
                    creation_sequence: pending.key.0,
                    stable_order_key: pending.key,
                    observed_generation: generation,
                    origin: pending.origin,
                    public_id: pending.id.clone(),
                });
            }
            if let Some(activated) = *activated_bar_index
                && stop_limit_fill_bar_eligible(activated, bar_index, path_kind, pending.direction)
                && low <= *limit_price - verify
                && price_on_leg(leg, *limit_price)
            {
                out.push(entry_fill_candidate(
                    pending,
                    BrokerCandidatePhase::PathLeg,
                    leg.index,
                    *limit_price,
                    *limit_price,
                    generation,
                ));
            }
        }
        (
            PendingEntryDirection::Short,
            PendingEntryKind::StopLimit {
                stop_price,
                limit_price,
                activated_bar_index,
            },
        ) => {
            if activated_bar_index.is_none() && low <= *stop_price && price_on_leg(leg, *stop_price)
            {
                out.push(BrokerCandidate {
                    event_kind: BrokerCandidateEvent::StopLimitActivation,
                    phase: BrokerCandidatePhase::PathLeg,
                    path_leg: leg.index,
                    crossing_price: *stop_price,
                    fill_price_or_mark: *stop_price,
                    creation_sequence: pending.key.0,
                    stable_order_key: pending.key,
                    observed_generation: generation,
                    origin: pending.origin,
                    public_id: pending.id.clone(),
                });
            }
            if activated_bar_index.is_some_and(|activated| {
                stop_limit_fill_bar_eligible(activated, bar_index, path_kind, pending.direction)
            }) && high >= *limit_price + verify
                && price_on_leg(leg, *limit_price)
            {
                out.push(entry_fill_candidate(
                    pending,
                    BrokerCandidatePhase::PathLeg,
                    leg.index,
                    *limit_price,
                    *limit_price,
                    generation,
                ));
            }
        }
        _ => {}
    }
    out
}

fn exit_fill_candidate(
    pending: &PendingExit,
    path_leg: u8,
    crossing_price: f64,
    fill_price: f64,
    generation: u64,
) -> BrokerCandidate {
    BrokerCandidate {
        event_kind: BrokerCandidateEvent::ExitFill,
        phase: BrokerCandidatePhase::PathLeg,
        path_leg,
        crossing_price,
        fill_price_or_mark: fill_price,
        creation_sequence: pending.key.0,
        stable_order_key: pending.key,
        observed_generation: generation,
        origin: StrategyCommandOrigin::Exit,
        public_id: pending.id.clone(),
    }
}

fn trailing_state_candidate(
    pending: &PendingExit,
    kind: BrokerCandidateEvent,
    path_leg: u8,
    mark: f64,
    generation: u64,
) -> BrokerCandidate {
    BrokerCandidate {
        event_kind: kind,
        phase: BrokerCandidatePhase::PathLeg,
        path_leg,
        crossing_price: mark,
        fill_price_or_mark: mark,
        creation_sequence: pending.key.0,
        stable_order_key: pending.key,
        observed_generation: generation,
        origin: StrategyCommandOrigin::Exit,
        public_id: pending.id.clone(),
    }
}

fn exit_leg_candidates(
    pending: &PendingExit,
    direction: TradeDirection,
    leg: PathLeg,
    high: f64,
    low: f64,
    verify: f64,
    generation: u64,
) -> Vec<BrokerCandidate> {
    let mut out = Vec::new();
    match &pending.trigger {
        PendingExitTrigger::Trailing(trailing) => match trailing.state {
            PendingTrailingState::Inactive => {
                let activation = trailing.spec.activation.price();
                let mark = match direction {
                    TradeDirection::Long if high >= activation => {
                        Some(leg.from.price.max(activation).min(high))
                    }
                    TradeDirection::Short if low <= activation => {
                        Some(leg.from.price.min(activation).max(low))
                    }
                    _ => None,
                };
                if let Some(mark) = mark
                    && price_on_leg(leg, mark)
                {
                    out.push(trailing_state_candidate(
                        pending,
                        BrokerCandidateEvent::TrailingActivation,
                        leg.index,
                        mark,
                        generation,
                    ));
                }
            }
            PendingTrailingState::Active { stop_price } => {
                let hit = match direction {
                    TradeDirection::Long => low <= stop_price,
                    TradeDirection::Short => high >= stop_price,
                };
                if hit && let Some(crossing) = crossing_on_leg(leg, stop_price) {
                    out.push(exit_fill_candidate(
                        pending, leg.index, crossing, stop_price, generation,
                    ));
                }
                let next_stop = match direction {
                    TradeDirection::Long => high - trailing.spec.offset_price_distance,
                    TradeDirection::Short => low + trailing.spec.offset_price_distance,
                };
                let improves = match direction {
                    TradeDirection::Long => next_stop > stop_price,
                    TradeDirection::Short => next_stop < stop_price,
                };
                let extreme = match direction {
                    TradeDirection::Long => high,
                    TradeDirection::Short => low,
                };
                if improves && price_on_leg(leg, extreme) {
                    out.push(trailing_state_candidate(
                        pending,
                        BrokerCandidateEvent::TrailingRatchet,
                        leg.index,
                        extreme,
                        generation,
                    ));
                }
            }
        },
        PendingExitTrigger::Bracket { downside, upside } => {
            let stop_touched = match direction {
                TradeDirection::Long => low <= *downside,
                TradeDirection::Short => high >= *downside,
            };
            if stop_touched && let Some(crossing) = crossing_on_leg(leg, *downside) {
                out.push(exit_fill_candidate(
                    pending, leg.index, crossing, *downside, generation,
                ));
            }
            let limit_touched = match direction {
                TradeDirection::Long => high >= *upside + verify,
                TradeDirection::Short => low <= *upside - verify,
            };
            if limit_touched && let Some(crossing) = crossing_on_leg(leg, *upside) {
                out.push(exit_fill_candidate(
                    pending, leg.index, crossing, *upside, generation,
                ));
            }
        }
        trigger => {
            if let Some(touch) = trigger.touched_candidate_for(direction, high, low, verify)
                && let Some(crossing) = crossing_on_leg(leg, touch.exit_price)
            {
                out.push(exit_fill_candidate(
                    pending,
                    leg.index,
                    crossing,
                    touch.exit_price,
                    generation,
                ));
            }
        }
    }
    out
}

fn exit_gap_candidates(
    pending: &PendingExit,
    direction: TradeDirection,
    open: f64,
    verify: f64,
    generation: u64,
) -> Vec<BrokerCandidate> {
    let fill = || exit_fill_candidate(pending, 0, open, open, generation);
    match &pending.trigger {
        PendingExitTrigger::Stop(price) => exit_stop_marketable(direction, open, *price)
            .then(fill)
            .into_iter()
            .collect(),
        PendingExitTrigger::Limit(price) => exit_limit_marketable(direction, open, *price, verify)
            .then(fill)
            .into_iter()
            .collect(),
        PendingExitTrigger::Bracket { downside, upside } => {
            if exit_stop_marketable(direction, open, *downside)
                || exit_limit_marketable(direction, open, *upside, verify)
            {
                vec![fill()]
            } else {
                Vec::new()
            }
        }
        PendingExitTrigger::Trailing(trailing) => match trailing.state {
            PendingTrailingState::Inactive => {
                let activation = trailing.spec.activation.price();
                let activates = match direction {
                    TradeDirection::Long => open >= activation,
                    TradeDirection::Short => open <= activation,
                };
                activates
                    .then(|| {
                        trailing_state_candidate(
                            pending,
                            BrokerCandidateEvent::TrailingActivation,
                            0,
                            open,
                            generation,
                        )
                    })
                    .into_iter()
                    .collect()
            }
            PendingTrailingState::Active { stop_price } => {
                if exit_stop_marketable(direction, open, stop_price) {
                    return vec![fill()];
                }
                let next_stop = match direction {
                    TradeDirection::Long => open - trailing.spec.offset_price_distance,
                    TradeDirection::Short => open + trailing.spec.offset_price_distance,
                };
                let improves = match direction {
                    TradeDirection::Long => next_stop > stop_price,
                    TradeDirection::Short => next_stop < stop_price,
                };
                improves
                    .then(|| {
                        trailing_state_candidate(
                            pending,
                            BrokerCandidateEvent::TrailingRatchet,
                            0,
                            open,
                            generation,
                        )
                    })
                    .into_iter()
                    .collect()
            }
        },
    }
}

fn exit_stop_marketable(direction: TradeDirection, open: f64, price: f64) -> bool {
    match direction {
        TradeDirection::Long => open <= price,
        TradeDirection::Short => open >= price,
    }
}

fn exit_limit_marketable(direction: TradeDirection, open: f64, price: f64, verify: f64) -> bool {
    match direction {
        TradeDirection::Long => open >= price + verify,
        TradeDirection::Short => open <= price - verify,
    }
}
