//! Receipts are buffered with candidate work and emitted only after durable
//! installation and immutable publication. A computed intent is not an order.
use super::*;

#[derive(Default)]
pub(crate) struct CommitTrace(Vec<serde_json::Value>);

impl CommitTrace {
    pub(super) fn action(
        &mut self,
        decision_id: Option<u64>,
        action_index: usize,
        execution: &exchange_core::ActorExecution,
    ) {
        if let Some(decision_id) = decision_id {
            let accepted = match &execution.result {
                exchange_core::ActorExecutionResult::Rejected(_) => false,
                exchange_core::ActorExecutionResult::Accepted(market) => {
                    let events = match market {
                        exchange_core::MarketExecution::Spot(result) => &result.events,
                        exchange_core::MarketExecution::Perp(result) => &result.events,
                    };
                    // The actor accepted processing the command; the engine
                    // can still reject the actual order, cancel or amendment.
                    // Match the authoritative feedback supplied to the bot.
                    !events.iter().any(|record| {
                        matches!(
                            record.event,
                            exchange_core::Event::OrderRejected { .. }
                                | exchange_core::Event::RiskRejected { .. }
                                | exchange_core::Event::CancelRejected { .. }
                                | exchange_core::Event::AmendRejected { .. }
                        )
                    })
                }
            };
            self.0.push(serde_json::json!({
                "stage": "action_durable", "decision_id": decision_id,
                "action_index": action_index, "command_seq": execution.command_seq,
                "outcome": if accepted { "accepted" } else { "rejected" },
            }));
        }
    }

    pub(super) fn state(&mut self, decision_id: Option<u64>) {
        if let Some(decision_id) = decision_id {
            self.0.push(serde_json::json!({
                "stage": "decision_state_committed", "decision_id": decision_id,
            }));
        }
    }

    pub(super) fn discard(&mut self, id: Option<u64>, reason: &str) {
        if let Some(decision_id) = id {
            self.0.push(serde_json::json!({
                "stage": "decision_discarded", "decision_id": decision_id, "reason": reason,
            }));
        }
    }

    pub(super) fn extend(&mut self, other: Self) {
        self.0.extend(other.0);
    }

    pub(crate) fn publish(self, wall_us: Option<u64>) {
        let Some(wall_us) = wall_us else { return };
        for event in self.0 {
            crate::bot_runtime_diagnostics::trace_event_at(event, wall_us);
        }
    }
}

impl Work {
    pub(crate) fn trace_ids(&self) -> Vec<u64> {
        match self {
            Self::Clock(_) => Vec::new(),
            Self::Bot { decision, .. } => decision.trace_id.into_iter().collect(),
            Self::Bots { decisions, .. } => decisions.iter().filter_map(|d| d.trace_id).collect(),
        }
    }

    pub(crate) fn discard_trace(&self, reason: &str) {
        for id in self.trace_ids() {
            discard(Some(id), reason);
        }
    }
}

pub(super) fn discard(id: Option<u64>, reason: &str) {
    if let Some(decision_id) = id {
        crate::bot_runtime_diagnostics::trace_event(serde_json::json!({
            "stage": "decision_discarded", "decision_id": decision_id, "reason": reason,
        }));
    }
}

pub(super) fn failed(id: Option<u64>, reason: &str) {
    if let Some(decision_id) = id {
        crate::bot_runtime_diagnostics::trace_event(serde_json::json!({
            "stage": "decision_failed", "decision_id": decision_id, "reason": reason,
        }));
    }
}

pub(super) fn decision(
    trace_id: Option<u64>,
    prior: &SharedAgent,
    next: &exchange_core::PersistedAgentKindState,
    observation: &DecisionObservation,
    action_count: usize,
) {
    let Some(decision_id) = trace_id else { return };
    let (public, data) = match observation {
        DecisionObservation::Owned(view) => (view, &view.bot_market_data),
        DecisionObservation::Shared(view) => (view.public.as_ref(), &view.bot_market_data),
    };
    let advanced_bar = match (prior.kind_state.as_ref(), next) {
        (
            exchange_core::PersistedAgentKindState::Plugin { data: before, .. },
            exchange_core::PersistedAgentKindState::Plugin { data: after, .. },
        ) => {
            after.get("last_bar_time").is_some()
                && before.get("last_bar_time") != after.get("last_bar_time")
        }
        _ => false,
    };
    let bar_close_ms = advanced_bar
        .then(|| {
            data.as_ref()
                .and_then(|d| d.candles.last())
                .map(|c| c.close_time_ms)
        })
        .flatten();
    let progress = match next {
        exchange_core::PersistedAgentKindState::Plugin {
            plugin_id, data, ..
        } if matches!(plugin_id.as_str(), "pine.strategy" | "python.sma")
            && data
                .get("evaluated_bars")
                .and_then(serde_json::Value::as_u64)
                .is_some() =>
        {
            Some(data)
        }
        _ => None,
    };
    crate::bot_runtime_diagnostics::trace_event(serde_json::json!({
        "stage": "decision", "decision_id": decision_id,
        "room_id": public.room_id, "participant_id": prior.template.participant_id(),
        "instrument_id": public.instrument_id, "observation_market_ms": public.market_time_ms,
        "bar_close_ms": bar_close_ms,
        "evaluated_bars": progress.and_then(|state| state.get("evaluated_bars")),
        "last_closed_bar_ms": progress.and_then(|state| state.get("last_bar_time"))
            .and_then(serde_json::Value::as_u64).and_then(|open| data.as_ref().and_then(|d| open.checked_add(d.interval_ms))),
        "bar_interval_ms": progress.and_then(|_| data.as_ref().map(|d| d.interval_ms)),
        "action_count": action_count,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange_core::{ActorExecution, ActorExecutionResult, Event, MarketExecution};

    fn execution(events: Vec<Event>, perp: bool) -> ActorExecution {
        let command = exchange_core::CommandRecord {
            seq: 17,
            command: exchange_core::Command::NewOrder(exchange_core::NewOrder {
                order_id: 42,
                account_id: 7,
                side: exchange_core::Side::Buy,
                position_side: exchange_core::PositionSide::Both,
                kind: exchange_core::OrderKind::Limit { price_tick: 100 },
                qty: 1,
                reduce_only: false,
            }),
        };
        let events = events
            .into_iter()
            .enumerate()
            .map(|(index, event)| exchange_core::EventRecord {
                seq: index as u64,
                command_seq: 17,
                event,
            })
            .collect();
        let market = if perp {
            MarketExecution::Perp(exchange_core::trading::PerpTradingExecution {
                command,
                events,
                clearing_events: vec![],
            })
        } else {
            MarketExecution::Spot(exchange_core::trading::SpotTradingExecution {
                command,
                events,
                clearing_events: vec![],
            })
        };
        ActorExecution {
            rejected_command: None,
            room_id: "receipts".into(),
            instrument_id: "spot".into(),
            command_seq: 17,
            market_time_ms: 1000,
            status: exchange_core::MarketStatus::Running,
            result: ActorExecutionResult::Accepted(market),
            price_updates: vec![],
            funding_settlement: None,
            book_delta: None,
            is_liquidation: false,
            wallet_updates: vec![],
            margin_updates: vec![],
        }
    }

    #[test]
    fn durable_receipt_rejects_nested_engine_rejections_on_spot_and_perp() {
        let rejections = [
            Event::OrderRejected {
                order_id: 42,
                reason: exchange_core::model::RejectReason::DuplicateOrderId,
            },
            Event::RiskRejected {
                order_id: 42,
                reason: exchange_core::model::RiskRejectReason::InsufficientCash,
            },
            Event::CancelRejected {
                order_id: 42,
                reason: exchange_core::model::CancelRejectReason::UnknownOrder,
            },
            Event::AmendRejected {
                order_id: 42,
                reason: exchange_core::model::AmendRejectReason::PriceWouldIncreaseAggression,
            },
        ];
        for perp in [false, true] {
            for rejected in &rejections {
                let mut trace = CommitTrace::default();
                trace.action(
                    Some(9),
                    2,
                    &execution(
                        vec![Event::OrderAccepted { order_id: 42 }, rejected.clone()],
                        perp,
                    ),
                );
                assert_eq!(trace.0.len(), 1);
                assert_eq!(
                    trace.0[0]["outcome"], "rejected",
                    "{rejected:?}; perp={perp}"
                );
                assert_eq!(trace.0[0]["command_seq"], 17);
                assert_eq!(trace.0[0]["action_index"], 2);
            }
        }
        let mut rejected = execution(vec![], false);
        rejected.result =
            ActorExecutionResult::Rejected(exchange_core::ActorRejectReason::MarketPaused);
        let mut trace = CommitTrace::default();
        trace.action(Some(9), 0, &rejected);
        assert_eq!(trace.0[0]["outcome"], "rejected");
    }

    #[test]
    fn durable_receipt_accepts_admitted_resting_and_unfilled_ioc_actions() {
        for perp in [false, true] {
            for terminal in [
                Event::OrderRested {
                    order_id: 42,
                    price_tick: 100,
                    remaining_qty: 1,
                },
                Event::OrderExpired {
                    order_id: 42,
                    unfilled_qty: 1,
                },
            ] {
                let mut trace = CommitTrace::default();
                trace.action(
                    Some(9),
                    0,
                    &execution(vec![Event::OrderAccepted { order_id: 42 }, terminal], perp),
                );
                assert_eq!(trace.0[0]["outcome"], "accepted");
            }
        }
    }
}
