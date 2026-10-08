//! Restore bounded bot inputs from durable receipts, without executing orders.
use crate::{
    ClearingEventSummary, EventSummary, RoomExecutionSummary,
    journal::{ExecutionPage, JournalError, JournalStore},
    journal_worker::JournalCoordinator,
};
use exchange_core::{
    BotRegistry, RoomManager, SchedulerState, Trade, observation::BotTradeReceipt,
};
use std::collections::BTreeMap;

const MAX_HISTORY_COMMANDS: usize = 100_000;
const MAX_HISTORY_TRADES: usize = 100_000;

struct History {
    cursor: Option<u64>,
    receipts: Vec<BotTradeReceipt>,
    complete: bool,
    commands: usize,
}
impl History {
    fn new() -> Self {
        Self {
            cursor: None,
            receipts: Vec::new(),
            complete: true,
            commands: 0,
        }
    }
    fn page(&mut self, page: ExecutionPage) -> bool {
        for execution in &page.executions {
            let expected = self.cursor.map_or(0, |value| value.saturating_add(1));
            if execution.command_seq != expected {
                self.complete = false;
            }
            self.cursor = Some(execution.command_seq);
            self.commands += 1;
            let (mut trades, valid) = from_summary(execution);
            self.receipts.append(&mut trades);
            self.complete &= valid;
        }
        if self.commands > MAX_HISTORY_COMMANDS || self.receipts.len() > MAX_HISTORY_TRADES {
            self.complete = false;
            self.receipts.truncate(MAX_HISTORY_TRADES);
            return false;
        }
        if page.has_more && page.executions.is_empty() {
            self.complete = false;
            return false;
        }
        page.has_more
    }
}

fn needs_history(scheduler: &SchedulerState, registry: &BotRegistry) -> bool {
    scheduler.agents.iter().any(|agent| {
        registry
            .market_data_request(&agent.template)
            .ok()
            .flatten()
            .is_some()
    })
}

pub(crate) fn restore(
    rooms: &mut RoomManager,
    schedulers: &BTreeMap<String, SchedulerState>,
    registry: &BotRegistry,
    journal: &mut dyn JournalStore,
) -> Result<(), JournalError> {
    for (room, scheduler) in schedulers {
        if !needs_history(scheduler, registry) {
            continue;
        }
        let mut history = History::new();
        loop {
            let page = match journal.query_executions(room, history.cursor, true, 500) {
                Ok(page) => page,
                Err(JournalError::UnsupportedOperation("query_executions")) => break,
                Err(error) => return Err(error),
            };
            if !history.page(page) {
                break;
            }
        }
        rooms.restore_bot_history(room, history.receipts, history.complete);
    }
    Ok(())
}

/// Lease takeover uses the same projection as process startup.
pub(crate) async fn restore_async(
    rooms: &mut RoomManager,
    room: &str,
    scheduler: Option<&SchedulerState>,
    registry: &BotRegistry,
    journal: &JournalCoordinator,
) -> Result<(), JournalError> {
    if !scheduler.is_some_and(|scheduler| needs_history(scheduler, registry)) {
        return Ok(());
    }
    let mut history = History::new();
    loop {
        let page = match journal
            .query_executions(room, history.cursor, true, 500)
            .await
        {
            Ok(page) => page,
            Err(JournalError::UnsupportedOperation("query_executions")) => break,
            Err(error) => return Err(error),
        };
        if !history.page(page) {
            break;
        }
    }
    rooms.restore_bot_history(room, history.receipts, history.complete);
    Ok(())
}

fn from_summary(execution: &RoomExecutionSummary) -> (Vec<BotTradeReceipt>, bool) {
    let mut receipts = Vec::new();
    let mut complete = true;
    for event in &execution.events {
        if let EventSummary::TradePrinted {
            trade_id,
            maker_order_id,
            maker_account_id,
            taker_order_id,
            taker_account_id,
            price_tick,
            qty,
            taker_side,
            ..
        } = event
        {
            let (Some(time), Some(instrument)) =
                (execution.market_time_ms, &execution.instrument_id)
            else {
                complete = false;
                continue;
            };
            let fees = execution
                .clearing_events
                .iter()
                .find_map(|event| match event {
                    ClearingEventSummary::SpotTradeSettled {
                        trade_id: id,
                        buyer_fee,
                        seller_fee,
                        ..
                    }
                    | ClearingEventSummary::PerpTradeSettled {
                        trade_id: id,
                        buyer_fee,
                        seller_fee,
                        ..
                    } if id == trade_id => Some((*buyer_fee, *seller_fee)),
                    _ => None,
                });
            complete &= fees.is_some();
            receipts.push(BotTradeReceipt {
                instrument_id: instrument.clone(),
                market_time_ms: time,
                trade: Trade {
                    trade_id: *trade_id,
                    maker_order_id: *maker_order_id,
                    maker_account_id: *maker_account_id,
                    taker_order_id: *taker_order_id,
                    taker_account_id: *taker_account_id,
                    price_tick: *price_tick,
                    qty: *qty,
                    taker_side: *taker_side,
                },
                buyer_fee: fees.map(|fee| fee.0),
                seller_fee: fees.map(|fee| fee.1),
            });
        }
    }
    (receipts, complete)
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange_core::{Command, NewOrder, OrderKind, ScenarioConfig, Side};

    fn summary() -> RoomExecutionSummary {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../scripts/fixtures/f6_batch_spec.json"))
                .unwrap();
        let scenario: ScenarioConfig = serde_json::from_value(fixture["scenario"].clone()).unwrap();
        let mut rooms = RoomManager::new();
        rooms.create_room(scenario).unwrap();
        let execution = rooms
            .apply(
                "f6-batch",
                Command::NewOrder(NewOrder {
                    order_id: 3,
                    account_id: 20,
                    side: Side::Buy,
                    kind: OrderKind::Limit { price_tick: 101 },
                    qty: 1,
                    reduce_only: false,
                }),
            )
            .unwrap();
        RoomExecutionSummary::from_execution(execution)
    }

    #[test]
    fn durable_receipts_preserve_actual_trade_time_side_and_fees() {
        let mut summary = summary();
        let (receipts, complete) = from_summary(&summary);
        assert!(complete);
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].trade.taker_account_id, 20);
        assert_eq!(receipts[0].trade.taker_side, Side::Buy);
        assert_eq!(receipts[0].trade.price_tick, 101);
        assert_eq!(receipts[0].buyer_fee, Some(0));
        assert_eq!(receipts[0].market_time_ms, 0);
        summary.market_time_ms = None;
        assert!(!from_summary(&summary).1);
        summary.market_time_ms = Some(0);
        summary.clearing_events.clear();
        assert!(!from_summary(&summary).1);
    }

    #[test]
    fn archived_command_gap_and_restore_limit_never_claim_complete_history() {
        let mut summary = summary();
        summary.command_seq = 0;
        let mut history = History::new();
        assert!(!history.page(ExecutionPage {
            executions: vec![summary.clone()],
            ..ExecutionPage::default()
        }));
        assert!(history.complete);
        summary.command_seq = 2;
        history.page(ExecutionPage {
            executions: vec![summary],
            ..ExecutionPage::default()
        });
        assert!(!history.complete);
        let mut bounded = History::new();
        bounded.commands = MAX_HISTORY_COMMANDS + 1;
        assert!(!bounded.page(ExecutionPage::default()));
        assert!(!bounded.complete);
    }
}
