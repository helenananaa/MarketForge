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
    accounts: Vec<exchange_core::observation::BotAccountEvent>,
    cursor: Option<u64>,
    receipts: Vec<BotTradeReceipt>,
    complete: bool,
    commands: usize,
}
impl History {
    fn new() -> Self {
        Self {
            accounts: Vec::new(),
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
            let (mut accounts, valid) = accounts_from_summary(execution);
            self.accounts.append(&mut accounts);
            self.complete &= valid;
        }
        if self.commands > MAX_HISTORY_COMMANDS
            || self.receipts.len() > MAX_HISTORY_TRADES
            || self.accounts.len() > 2 * MAX_HISTORY_TRADES
        {
            self.complete = false;
            self.receipts.truncate(MAX_HISTORY_TRADES);
            self.accounts.truncate(2 * MAX_HISTORY_TRADES);
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
        rooms.restore_bot_accounts(room, history.accounts);
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
    rooms.restore_bot_accounts(room, history.accounts);
    Ok(())
}

fn accounts_from_summary(
    execution: &RoomExecutionSummary,
) -> (Vec<exchange_core::observation::BotAccountEvent>, bool) {
    use exchange_core::{PerpAccountSnapshot, PerpMarginStatus, observation::BotAccountFrame};
    let perp = |a: &crate::PerpAccountStateSummary| -> Option<BotAccountFrame> {
        let margin_status = match a.margin_status.as_str() {
            "flat" => PerpMarginStatus::Flat,
            "healthy" => PerpMarginStatus::Healthy,
            "margin_call" => PerpMarginStatus::MarginCall,
            "liquidatable" => PerpMarginStatus::Liquidatable,
            _ => return None,
        };
        Some(BotAccountFrame::Perp(Box::new(PerpAccountSnapshot {
            hedge_positions: a.hedge_positions.clone(),
            account_id: a.account_id,
            cash_balance: a.cash_balance,
            position_qty: a.position_qty,
            avg_entry_price_tick: a.avg_entry_price_tick,
            realized_pnl: a.realized_pnl,
            unrealized_pnl: a.unrealized_pnl,
            equity: a.equity,
            initial_margin: a.initial_margin,
            maintenance_margin: a.maintenance_margin,
            portfolio_initial_margin: a.portfolio_initial_margin.unwrap_or(a.initial_margin),
            portfolio_maintenance_margin: a
                .portfolio_maintenance_margin
                .unwrap_or(a.maintenance_margin),
            margin_status,
            reserved_margin: a.reserved_margin,
            available_cash: a.available_cash,
            fees_paid: a.fees_paid,
            funding_pnl: a.funding_pnl,
        })))
    };
    let mut snapshots = Vec::new();
    for event in &execution.clearing_events {
        match event {
            ClearingEventSummary::SpotTradeSettled { buyer, seller, .. } => {
                for a in [buyer, seller] {
                    snapshots.push(Some(BotAccountFrame::Spot {
                        account_id: a.account_id,
                        cash_balance: a.cash_balance,
                        position_qty: a.position_qty,
                        fees_paid: a.fees_paid,
                    }));
                }
            }
            ClearingEventSummary::PerpTradeSettled { buyer, seller, .. } => {
                snapshots.push(perp(buyer));
                snapshots.push(perp(seller));
            }
            ClearingEventSummary::PerpFundingSettled { account, .. }
            | ClearingEventSummary::PerpMarginStatusChanged { account, .. } => {
                snapshots.push(perp(account))
            }
            ClearingEventSummary::PerpLiquidationSettled {
                account,
                auto_deleveraging_allocations,
                socialized_loss_allocations,
                ..
            } => {
                snapshots.push(perp(account));
                snapshots.extend(
                    auto_deleveraging_allocations
                        .iter()
                        .map(|a| perp(&a.account)),
                );
                snapshots.extend(socialized_loss_allocations.iter().map(|a| perp(&a.account)));
            }
        }
    }
    if snapshots.is_empty() {
        return (vec![], true);
    }
    let (Some(time), Some(instrument)) = (execution.market_time_ms, &execution.instrument_id)
    else {
        return (vec![], false);
    };
    let complete = snapshots.iter().all(Option::is_some);
    (
        snapshots
            .into_iter()
            .flatten()
            .map(|account| exchange_core::observation::BotAccountEvent {
                instrument_id: instrument.clone(),
                command_seq: execution.command_seq,
                market_time_ms: time,
                account,
            })
            .collect(),
        complete,
    )
}

fn from_summary(execution: &RoomExecutionSummary) -> (Vec<BotTradeReceipt>, bool) {
    let mut receipts = Vec::new();
    let mut complete = true;
    for event in &execution.events {
        if let EventSummary::TradePrinted {
            trade_id,
            maker_position_side,
            taker_position_side,
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
            // Older hedge-mode journals did not preserve the trade's leg.
            // Such a receipt cannot be silently relabeled as a one-way fill.
            for clearing in &execution.clearing_events {
                if let ClearingEventSummary::PerpTradeSettled {
                    trade_id: id,
                    buyer,
                    seller,
                    ..
                } = clearing
                    && id == trade_id
                {
                    let (buy_leg, sell_leg) = if *taker_side == exchange_core::Side::Buy {
                        (*taker_position_side, *maker_position_side)
                    } else {
                        (*maker_position_side, *taker_position_side)
                    };
                    complete &= !(buyer.hedge_positions.is_some()
                        && buy_leg == exchange_core::PositionSide::Both
                        || seller.hedge_positions.is_some()
                            && sell_leg == exchange_core::PositionSide::Both);
                }
            }
            receipts.push(BotTradeReceipt {
                instrument_id: instrument.clone(),
                market_time_ms: time,
                trade: Trade {
                    maker_position_side: *maker_position_side,
                    taker_position_side: *taker_position_side,
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
                    position_side: Default::default(),
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
    fn hedge_trade_legs_and_account_frames_restore_without_executing_orders() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../scripts/fixtures/microstructure_market.json"
        ))
        .unwrap();
        let mut scenario: ScenarioConfig =
            serde_json::from_value(fixture["scenario"].clone()).unwrap();
        scenario.seed_orders.clear();
        scenario.routed_seed_orders.clear();
        let exchange_core::MarketConfig::Perp(config) = &mut scenario.extra_markets[0] else {
            panic!()
        };
        config.clearing.position_mode = exchange_core::PositionMode::Hedge;
        config.price_link = None;
        config.funding = None;
        let room = scenario.room_id.clone();
        let mut rooms = RoomManager::new();
        rooms.create_room(scenario.clone()).unwrap();
        for (id, account, side, leg) in [
            (1, 10, Side::Sell, exchange_core::PositionSide::Short),
            (2, 20, Side::Buy, exchange_core::PositionSide::Long),
        ] {
            let execution = rooms
                .apply_to_instrument(
                    &room,
                    "V-BTC-PERP",
                    Command::NewOrder(NewOrder {
                        position_side: leg,
                        order_id: id,
                        account_id: account,
                        side,
                        kind: OrderKind::Limit { price_tick: 100 },
                        qty: 1,
                        reduce_only: false,
                    }),
                )
                .unwrap();
            if id == 2 {
                let mut summary = RoomExecutionSummary::from_execution(execution);
                let (receipts, complete) = from_summary(&summary);
                assert!(complete);
                assert_eq!(
                    receipts[0].trade.maker_position_side,
                    exchange_core::PositionSide::Short
                );
                assert_eq!(
                    receipts[0].trade.taker_position_side,
                    exchange_core::PositionSide::Long
                );
                let (accounts, complete) = accounts_from_summary(&summary);
                assert!(complete);
                let mut recovered = RoomManager::new();
                recovered.create_room(scenario.clone()).unwrap();
                recovered.restore_bot_history(&room, receipts, true);
                recovered.restore_bot_accounts(&room, accounts);
                let own = recovered
                    .bot_account_events(&room, "V-BTC-PERP", 20)
                    .unwrap();
                assert!(!own.is_empty());
                assert!(own.iter().all(|e| matches!(&e.account, exchange_core::observation::BotAccountFrame::Perp(a) if a.account_id == 20)));
                let exchange_core::observation::BotAccountFrame::Perp(last) =
                    &own.last().unwrap().account
                else {
                    panic!()
                };
                assert_eq!(last.hedge_positions.as_ref().unwrap().long.qty, 1);
                assert!(
                    recovered
                        .book_snapshot_for(&room, "V-BTC-PERP")
                        .unwrap()
                        .bids
                        .is_empty()
                );
                let mut owner = recovered.take_room(&room).unwrap();
                assert_eq!(
                    owner.bot_account_events(&room, "V-BTC-PERP", 20).unwrap(),
                    own
                );
                recovered.join_disjoint(std::mem::take(&mut owner)).unwrap();
                assert_eq!(
                    recovered
                        .bot_account_events(&room, "V-BTC-PERP", 20)
                        .unwrap(),
                    own
                );
                let current_events = summary.events.clone();
                for event in &mut summary.events {
                    if let EventSummary::TradePrinted {
                        maker_position_side,
                        taker_position_side,
                        ..
                    } = event
                    {
                        *maker_position_side = Default::default();
                        *taker_position_side = Default::default();
                    }
                }
                for (legacy, current) in summary.events.iter().zip(&current_events) {
                    assert!(crate::event_summary_matches(legacy, current));
                }
                assert!(!from_summary(&summary).1);
            }
        }
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
