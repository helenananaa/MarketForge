//! Optional execution of an unchanged finite business parent at visible touch.
//! Entitlement is the existing TWAP schedule; only authoritative child fills
//! consume demand. No new capital, target, deadline or impact coefficient.
use super::*;
use crate::{ActorExecutionResult, Command, Event, GatewayExecution, MarketExecution};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingChild {
    born_ms: u64,
    qty: u64,
    price_tick: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExecutionLedger {
    demand_lots: u64,
    filled_lots: u64,
    remaining_lots: u64,
    expired_lots: u64,
    deadline_ms: u64,
    children: u64,
    submitted_lots: u64,
    pending: Option<PendingChild>,
    last_feedback_seq: Option<u64>,
}

impl ExecutionLedger {
    pub(super) fn valid(&self, c: &Config, s: &State) -> bool {
        self.demand_lots == c.target_qty
            && u128::from(self.filled_lots)
                + u128::from(self.remaining_lots)
                + u128::from(self.expired_lots)
                == u128::from(self.demand_lots)
            && s.completed_qty == self.filled_lots
            && s.execution_start_ms
                .is_some_and(|start| self.deadline_ms == start.saturating_add(c.horizon_ms))
            && (self.expired_lots == 0 || (s.deadline_reached && self.remaining_lots == 0))
            && self.pending.as_ref().is_none_or(|p| {
                p.qty > 0
                    && p.qty <= c.max_qty
                    && p.price_tick > 0
                    && u128::from(p.qty)
                        <= u128::from(self.remaining_lots) + u128::from(self.expired_lots)
                    && s.execution_start_ms.is_some_and(|start| start <= p.born_ms)
                    && p.born_ms < self.deadline_ms
            })
    }
}

impl MarketBot {
    pub(super) fn expire_execution_parent(&mut self, now: u64) {
        if !self.config.liquidity_aware {
            return;
        }
        if let Some(ledger) = self.state.execution_ledger.as_mut() {
            if now >= ledger.deadline_ms {
                self.state.deadline_reached = true;
                ledger.expired_lots += ledger.remaining_lots;
                ledger.remaining_lots = 0;
            }
        }
    }
    pub(super) fn liquidity_execution(
        &mut self,
        view: &ParticipantObservationView<'_>,
        budget: &mut Budget,
        mid: i64,
        lot: u64,
    ) -> Vec<OrderAction> {
        let now = view.market_time_ms;
        if now < self.config.start_after_ms {
            return vec![];
        }
        let start = *self.state.execution_start_ms.get_or_insert(now);
        let ledger = self
            .state
            .execution_ledger
            .get_or_insert_with(|| ExecutionLedger {
                demand_lots: self.config.target_qty,
                filled_lots: 0,
                remaining_lots: self.config.target_qty,
                expired_lots: 0,
                deadline_ms: start.saturating_add(self.config.horizon_ms),
                children: 0,
                submitted_lots: 0,
                pending: None,
                last_feedback_seq: None,
            });
        self.state.completed_qty = ledger.filled_lots;
        self.state.deadline_reached = now >= ledger.deadline_ms;
        if self.state.deadline_reached {
            ledger.expired_lots += ledger.remaining_lots;
            ledger.remaining_lots = 0;
            return vec![];
        }
        // One in-flight child survives snapshots. A host without feedback must
        // wait rather than issue the same uncertain demand a second time.
        if ledger.remaining_lots == 0 || ledger.pending.is_some() {
            return vec![];
        }
        let elapsed = now.saturating_sub(start);
        let due = (u128::from(self.config.target_qty)
            * u128::from(
                elapsed
                    .saturating_add(self.config.decision_interval_ms)
                    .min(self.config.horizon_ms),
            ))
        .div_ceil(u128::from(self.config.horizon_ms)) as u64;
        let owed = due
            .saturating_sub(ledger.filled_lots)
            .min(ledger.remaining_lots)
            .min(self.config.max_qty);
        let side = self.config.side;
        let Some(top) = (if side == Side::Buy {
            view.book.asks.first()
        } else {
            view.book.bids.first()
        }) else {
            return vec![];
        };
        let original_limit = if side == Side::Buy {
            mid.saturating_add(self.config.max_slippage_ticks)
        } else {
            mid.saturating_sub(self.config.max_slippage_ticks).max(1)
        };
        if (side == Side::Buy && top.price_tick > original_limit)
            || (side == Side::Sell && top.price_tick < original_limit)
        {
            return vec![];
        }
        let crossing: Vec<_> = view
            .own_orders
            .iter()
            .filter(|o| {
                o.side != side
                    && if side == Side::Buy {
                        o.price_tick <= top.price_tick
                    } else {
                        o.price_tick >= top.price_tick
                    }
            })
            .take(64)
            .map(|o| OrderAction::Cancel {
                order_id: o.order_id,
            })
            .collect();
        if !crossing.is_empty() {
            return crossing;
        }
        let own = view
            .own_orders
            .iter()
            .filter(|o| o.side != side && o.price_tick == top.price_tick)
            .map(|o| o.remaining_qty)
            .sum::<u64>();
        // Retain the old conservative affordability check at its full limit,
        // although this child can only execute at the observed best price.
        let qty = budget.allocate(side, original_limit, owed.min(top.qty.saturating_sub(own)));
        let lot = lot.max(1);
        let qty = qty / lot * lot;
        if qty == 0 {
            return vec![];
        }
        ledger.pending = Some(PendingChild {
            born_ms: now,
            qty,
            price_tick: top.price_tick,
        });
        ledger.children += 1;
        ledger.submitted_lots = ledger.submitted_lots.saturating_add(qty);
        vec![OrderAction::PlaceProtected {
            side,
            position_side: Default::default(),
            qty,
            price_tick: top.price_tick,
            order_type: crate::model::ProtectedOrderType::ImmediateOrCancel,
            reduce_only: budget.reduce_only,
            valid_until_market_time_ms: Some(ledger.deadline_ms),
            expires_at_market_time_ms: None,
        }]
    }

    pub(super) fn parent_execution_feedback(&mut self, action: &OrderAction, e: &GatewayExecution) {
        if !self.config.liquidity_aware
            || e.participant_id != self.plugin.participant.participant_id
            || e.room_id != self.plugin.participant.room_id
            || e.account_id != self.plugin.participant.account_id
            || e.instrument_id
                != self
                    .plugin
                    .participant
                    .instrument_id
                    .as_deref()
                    .unwrap_or("")
            || *action != e.action
        {
            return;
        }
        let Some(ledger) = self.state.execution_ledger.as_mut() else {
            return;
        };
        let Some(pending) = ledger.pending.as_ref() else {
            return;
        };
        if ledger
            .last_feedback_seq
            .is_some_and(|seq| e.execution.command_seq <= seq)
        {
            return;
        }
        let OrderAction::PlaceProtected {
            side,
            qty,
            price_tick,
            order_type: crate::model::ProtectedOrderType::ImmediateOrCancel,
            valid_until_market_time_ms: Some(deadline),
            ..
        } = action
        else {
            return;
        };
        if *side != self.config.side
            || *qty != pending.qty
            || *price_tick != pending.price_tick
            || *deadline != ledger.deadline_ms
            || e.execution.market_time_ms < pending.born_ms
        {
            return;
        }
        let Command::NewOrder(order) = &e.command else {
            return;
        };
        if order.account_id != e.account_id || order.side != *side || order.qty != *qty {
            return;
        }
        ledger.last_feedback_seq = Some(e.execution.command_seq);
        ledger.pending = None;
        if !self.state.deadline_reached && ledger.remaining_lots > 0 {
            // Acknowledgement releases only this child's in-flight reservation.
            // Revisit residual debt on the next *new* market observation,
            // rather than sleeping a full planning interval after a partial or
            // rejected child. The clock guard still forbids same-time loops.
            self.state.next_decision_ms =
                self.state.next_decision_ms.min(e.execution.market_time_ms);
        }
        let ActorExecutionResult::Accepted(result) = &e.execution.result else {
            return;
        };
        let events = match result {
            MarketExecution::Spot(r) => &r.events,
            MarketExecution::Perp(r) => &r.events,
        };
        let actual = events
            .iter()
            .filter_map(|e| match &e.event {
                Event::TradePrinted(t)
                    if t.taker_order_id == order.order_id
                        && t.taker_account_id == order.account_id
                        && t.taker_side == order.side =>
                {
                    Some(t.qty)
                }
                _ => None,
            })
            .sum::<u64>()
            .min(*qty);
        let filled = actual.min(ledger.demand_lots - ledger.filled_lots);
        // An accepted delayed acknowledgement can correct a previously expired
        // residual, but never create demand or reopen the parent's deadline.
        if self.state.deadline_reached {
            ledger.expired_lots -= filled.min(ledger.expired_lots);
        } else {
            ledger.remaining_lots -= filled.min(ledger.remaining_lots);
        }
        ledger.filled_lots += filled;
        self.state.completed_qty = ledger.filled_lots;
    }
}
