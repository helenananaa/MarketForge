use crate::history::History;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    OrderBook,
    account::{ClearingError, Money, PositionQty, notional},
    log::{CommandRecord, EventLog, EventRecord},
    model::{
        AccountId, BookSnapshot, Command, Event, NewOrder, OrderId, OrderKind, RiskRejectReason,
        Side,
    },
    perp::{
        PerpAccountSnapshot, PerpAccountStore, PerpClearingConfig, PerpClearingEvent,
        PerpCrossMarginContext, PerpMarginStatus,
    },
    risk::{PerpRiskConfig, PerpRiskEngine, RiskContext, SpotRiskConfig, SpotRiskEngine},
    spot::{SpotAccountSnapshot, SpotAccountStore, SpotClearingConfig, SpotClearingEvent},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SpotTradingEngine {
    book: OrderBook,
    accounts: SpotAccountStore,
    risk: SpotRiskEngine,
    log: EventLog,
    clearing_events: History<SpotClearingEvent>,
}

impl SpotTradingEngine {
    pub(crate) fn reservation_balance(&self, id: AccountId) -> Option<(Money, PositionQty)> {
        self.accounts.reservation_balance(id)
    }
    pub(crate) fn expiring_order_ids(&self, market_time_ms: u64) -> Vec<OrderId> {
        self.book.expiring_order_ids(market_time_ms)
    }
    pub fn new(config: SpotClearingConfig) -> Self {
        Self::new_with_risk(config, SpotRiskConfig::default())
    }

    pub fn new_with_risk(config: SpotClearingConfig, risk_config: SpotRiskConfig) -> Self {
        Self {
            book: OrderBook::new(),
            accounts: SpotAccountStore::new(config),
            risk: SpotRiskEngine::new(risk_config),
            log: EventLog::new(),
            clearing_events: History::default(),
        }
    }

    pub fn create_account(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
    ) -> SpotAccountSnapshot {
        self.accounts.create_account(account_id, cash_balance)
    }

    pub fn create_account_with_position(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
        position_qty: crate::account::PositionQty,
    ) -> SpotAccountSnapshot {
        self.accounts
            .create_account_with_position(account_id, cash_balance, position_qty)
    }

    pub fn sync_account_balances(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
        position_qty: crate::account::PositionQty,
    ) -> Result<SpotAccountSnapshot, ClearingError> {
        self.accounts
            .sync_account_balances(account_id, cash_balance, position_qty)
    }

    #[cfg(test)]
    pub(crate) fn sync_account_balances_reference(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
        position_qty: PositionQty,
    ) -> Result<SpotAccountSnapshot, ClearingError> {
        self.accounts
            .sync_account_balances_reference(account_id, cash_balance, position_qty)
    }

    pub fn order_owner(&self, order_id: OrderId) -> Option<AccountId> {
        self.book.order_owner(order_id)
    }

    pub fn resting_orders_for_account(&self, account_id: AccountId) -> Vec<crate::model::Order> {
        self.book.resting_orders_for_account(account_id)
    }

    pub fn resting_order_ids_for_account_on_side(
        &self,
        account_id: AccountId,
        side: Side,
    ) -> Vec<OrderId> {
        self.book.order_ids_for_account_on_side(account_id, side)
    }

    pub fn apply(&mut self, command: Command) -> Result<SpotTradingExecution, ClearingError> {
        let mut staged = self.clone();
        let execution = staged.apply_inner(command)?;
        *self = staged;
        Ok(execution)
    }

    fn apply_inner(&mut self, command: Command) -> Result<SpotTradingExecution, ClearingError> {
        if matches!(
            command,
            Command::SetConditionalOrder { .. }
                | Command::SetPositionProtection { .. }
                | Command::NewOrderWithProtection { .. }
        ) {
            return Err(ClearingError::WrongMarketKind); // Requires the exchange actor transaction.
        }
        if matches!(command, Command::SetMarkPrice(_)) {
            return Err(ClearingError::WrongMarketKind);
        }

        let risk_context = risk_context(&self.book, &command)?;
        if let Err(reason) = self.risk.check(&command, &self.accounts, risk_context) {
            let order_id = command.order_id();
            let recorded = self
                .log
                .record(command, vec![Event::RiskRejected { order_id, reason }]);
            return Ok(SpotTradingExecution {
                command: recorded.command,
                events: recorded.events,
                clearing_events: Vec::new(),
            });
        }

        let events = self.book.apply(command.clone());
        let recorded = self.log.record(command, events);
        let clearing_events =
            self.settle_recorded_events(&recorded.command.command, &recorded.events)?;

        Ok(SpotTradingExecution {
            command: recorded.command,
            events: recorded.events,
            clearing_events,
        })
    }

    pub fn snapshot(&self) -> BookSnapshot {
        self.book.snapshot()
    }

    pub fn account_snapshot(&self, account_id: AccountId) -> Option<SpotAccountSnapshot> {
        self.accounts.account_snapshot(account_id)
    }

    pub fn account_snapshots(&self) -> Vec<SpotAccountSnapshot> {
        self.accounts.snapshots()
    }

    pub(crate) fn reservation_changes(&self) -> &crate::account::ReservationChanges {
        &self.accounts.reservation_changes
    }

    pub(crate) fn reservation_changes_mut(&mut self) -> &mut crate::account::ReservationChanges {
        &mut self.accounts.reservation_changes
    }

    pub(crate) fn reservation_balances(
        &self,
    ) -> impl Iterator<Item = (AccountId, Money, crate::account::PositionQty)> + '_ {
        self.accounts.reservation_balances()
    }

    pub fn command_log(&self) -> &[CommandRecord] {
        self.log.commands()
    }

    pub fn event_log(&self) -> &[EventRecord] {
        self.log.events()
    }

    pub fn clearing_log(&self) -> &[SpotClearingEvent] {
        &self.clearing_events
    }

    fn settle_recorded_events(
        &mut self,
        command: &Command,
        events: &[EventRecord],
    ) -> Result<Vec<SpotClearingEvent>, ClearingError> {
        let mut clearing_events = Vec::new();

        for record in events {
            match &record.event {
                Event::TradePrinted(trade) => {
                    let clearing_event = self.accounts.settle_trade(trade)?;
                    self.clearing_events.push(clearing_event.clone());
                    clearing_events.push(clearing_event);
                }
                Event::OrderRested {
                    order_id,
                    price_tick,
                    remaining_qty,
                } => {
                    if let Command::NewOrder(order) = command
                        && order.order_id == *order_id
                    {
                        self.accounts.reserve_resting_order(
                            *order_id,
                            order.account_id,
                            order.side,
                            *price_tick,
                            *remaining_qty,
                        )?;
                    }
                }
                Event::OrderCanceled { order_id, .. } | Event::OrderExpired { order_id, .. } => {
                    self.accounts.release_order_reservation(*order_id)?;
                }
                Event::OrderAmended {
                    order_id,
                    new_price_tick,
                    new_qty,
                    ..
                } => {
                    self.accounts
                        .amend_order_reservation(*order_id, *new_price_tick, *new_qty)?;
                }
                Event::OrderAccepted { .. }
                | Event::OrderRejected { .. }
                | Event::RiskRejected { .. }
                | Event::OrderPartiallyFilled { .. }
                | Event::OrderFilled { .. }
                | Event::CancelRejected { .. }
                | Event::AmendRejected { .. } => {}
            }
        }

        Ok(clearing_events)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpotTradingExecution {
    pub command: CommandRecord,
    pub events: Vec<EventRecord>,
    pub clearing_events: Vec<SpotClearingEvent>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PerpTradingEngine {
    book: OrderBook,
    accounts: PerpAccountStore,
    risk: PerpRiskEngine,
    log: EventLog,
    clearing_events: History<PerpClearingEvent>,
    #[serde(default)]
    pending_liquidations: BTreeMap<AccountId, PendingLiquidation>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PendingLiquidation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_hedge_positions: Option<Box<crate::HedgePositions>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_hedge_positions: Option<Box<crate::HedgePositions>>,
    pub account_id: AccountId,
    pub initial_position_qty: PositionQty,
    pub remaining_position_qty: PositionQty,
    pub accumulated_notional: Money,
    pub attempt_count: u64,
    pub initial_margin_status: PerpMarginStatus,
}

impl PerpTradingEngine {
    pub(crate) fn liquidatable_account_ids(&self) -> Vec<AccountId> {
        self.accounts.liquidatable_account_ids()
    }
    pub(crate) fn reservation_balance(
        &self,
        id: AccountId,
    ) -> Result<Option<Money>, ClearingError> {
        self.accounts.reservation_balance(id)
    }
    pub(crate) fn expiring_order_ids(&self, market_time_ms: u64) -> Vec<OrderId> {
        self.book.expiring_order_ids(market_time_ms)
    }
    pub fn new(
        config: PerpClearingConfig,
        initial_mark_price_tick: crate::model::PriceTick,
    ) -> Result<Self, ClearingError> {
        Self::new_with_risk(config, initial_mark_price_tick, PerpRiskConfig::default())
    }

    pub fn new_with_risk(
        config: PerpClearingConfig,
        initial_mark_price_tick: crate::model::PriceTick,
        risk_config: PerpRiskConfig,
    ) -> Result<Self, ClearingError> {
        Ok(Self {
            book: OrderBook::new(),
            accounts: PerpAccountStore::new(config, initial_mark_price_tick)?,
            risk: PerpRiskEngine::new(risk_config),
            log: EventLog::new(),
            clearing_events: History::default(),
            pending_liquidations: BTreeMap::new(),
        })
    }

    pub fn create_account(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
    ) -> PerpAccountSnapshot {
        self.accounts.create_account(account_id, cash_balance)
    }

    pub fn sync_cash_balance(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
    ) -> Result<PerpAccountSnapshot, ClearingError> {
        self.accounts.sync_cash_balance(account_id, cash_balance)
    }

    pub fn sync_cross_margin_account(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
        context: PerpCrossMarginContext,
    ) -> Result<PerpAccountSnapshot, ClearingError> {
        self.accounts
            .sync_cross_margin_account(account_id, cash_balance, context)
    }

    pub(crate) fn margin_inputs(
        &self,
        affected: Option<&std::collections::BTreeSet<AccountId>>,
    ) -> Vec<crate::perp::PerpMarginInputs> {
        self.accounts.margin_inputs(affected)
    }

    pub(crate) fn sync_cross_margin_accounts(
        &mut self,
        requests: &[(AccountId, Money, PerpCrossMarginContext)],
    ) -> Result<(), ClearingError> {
        self.accounts.sync_cross_margin_accounts(requests)
    }

    pub fn order_owner(&self, order_id: OrderId) -> Option<AccountId> {
        self.book.order_owner(order_id)
    }

    pub fn apply(&mut self, command: Command) -> Result<PerpTradingExecution, ClearingError> {
        let mut staged = self.clone();
        let execution = staged.apply_inner(command)?;
        *self = staged;
        Ok(execution)
    }

    fn apply_inner(&mut self, command: Command) -> Result<PerpTradingExecution, ClearingError> {
        if matches!(
            command,
            Command::SetConditionalOrder { .. }
                | Command::SetPositionProtection { .. }
                | Command::NewOrderWithProtection { .. }
        ) {
            return Err(ClearingError::WrongMarketKind); // Requires the exchange actor transaction.
        }
        if let Command::SettleFunding(mut settlement) = command {
            let clearing_events = self.accounts.settle_funding(&mut settlement)?;
            self.clearing_events.extend(clearing_events.iter().cloned());
            let recorded = self
                .log
                .record(Command::SettleFunding(settlement), Vec::new());
            return Ok(PerpTradingExecution {
                command: recorded.command,
                events: recorded.events,
                clearing_events,
            });
        }
        if let Command::SetMarkPrice(mark_price) = command {
            let clearing_events = self.accounts.set_mark_price_tick(mark_price.price_tick)?;
            self.clearing_events.extend(clearing_events.iter().cloned());
            let recorded = self
                .log
                .record(Command::SetMarkPrice(mark_price), Vec::new());
            return Ok(PerpTradingExecution {
                command: recorded.command,
                events: recorded.events,
                clearing_events,
            });
        }

        if let Command::NewOrder(order) = &command
            && self.pending_liquidations.contains_key(&order.account_id)
        {
            let order_id = order.order_id;
            let recorded = self.log.record(
                command,
                vec![Event::RiskRejected {
                    order_id,
                    reason: RiskRejectReason::InsufficientMargin,
                }],
            );
            return Ok(PerpTradingExecution {
                command: recorded.command,
                events: recorded.events,
                clearing_events: Vec::new(),
            });
        }

        let risk_context = risk_context(&self.book, &command)?;
        if let Err(reason) = self.risk.check(&command, &self.accounts, risk_context) {
            let order_id = command.order_id();
            let recorded = self
                .log
                .record(command, vec![Event::RiskRejected { order_id, reason }]);
            return Ok(PerpTradingExecution {
                command: recorded.command,
                events: recorded.events,
                clearing_events: Vec::new(),
            });
        }

        let mut guard = ReduceOnlyMatching {
            accounts: &self.accounts,
            positions: BTreeMap::new(),
        };
        let events = self.book.apply_guarded(command.clone(), &mut guard);
        let recorded = self.log.record(command, events);
        let clearing_events =
            self.settle_recorded_events(&recorded.command.command, &recorded.events)?;

        Ok(PerpTradingExecution {
            command: recorded.command,
            events: recorded.events,
            clearing_events,
        })
    }

    pub fn set_mark_price_tick(
        &mut self,
        mark_price_tick: crate::model::PriceTick,
    ) -> Result<Vec<PerpClearingEvent>, ClearingError> {
        let clearing_events = self.accounts.set_mark_price_tick(mark_price_tick)?;
        self.clearing_events.extend(clearing_events.iter().cloned());
        Ok(clearing_events)
    }

    pub fn liquidate_account(
        &mut self,
        account_id: AccountId,
        order_id: OrderId,
    ) -> Result<PerpTradingExecution, ClearingError> {
        self.ensure_pending_liquidation(account_id)?;
        let mut staged = self.clone();
        let execution = staged.advance_liquidation_inner(account_id, order_id)?;
        *self = staged;
        Ok(execution)
    }

    pub fn pending_liquidation(&self, account_id: AccountId) -> Option<&PendingLiquidation> {
        self.pending_liquidations.get(&account_id)
    }

    pub fn pending_liquidation_accounts(&self) -> Vec<AccountId> {
        self.pending_liquidations.keys().copied().collect()
    }

    pub fn resting_order_ids_for_account(&self, account_id: AccountId) -> Vec<OrderId> {
        self.book.order_ids_for_account(account_id)
    }

    pub fn resting_orders_for_account(&self, account_id: AccountId) -> Vec<crate::model::Order> {
        self.book.resting_orders_for_account(account_id)
    }

    fn ensure_pending_liquidation(&mut self, account_id: AccountId) -> Result<(), ClearingError> {
        if self.pending_liquidations.contains_key(&account_id) {
            return Ok(());
        }

        let snapshot = self
            .account_snapshot(account_id)
            .ok_or(ClearingError::AccountNotFound)?;
        if snapshot.margin_status != PerpMarginStatus::Liquidatable {
            return Err(ClearingError::AccountNotLiquidatable);
        }
        if !snapshot.has_open_position() {
            return Err(ClearingError::InvalidLiquidationQuantity);
        }

        self.pending_liquidations.insert(
            account_id,
            PendingLiquidation {
                initial_hedge_positions: snapshot.hedge_positions.clone(),
                remaining_hedge_positions: snapshot.hedge_positions.clone(),
                account_id,
                initial_position_qty: snapshot.position_qty,
                remaining_position_qty: snapshot.position_qty,
                accumulated_notional: 0,
                attempt_count: 0,
                initial_margin_status: snapshot.margin_status,
            },
        );
        Ok(())
    }

    fn advance_liquidation_inner(
        &mut self,
        account_id: AccountId,
        order_id: OrderId,
    ) -> Result<PerpTradingExecution, ClearingError> {
        let pending = self
            .pending_liquidations
            .get(&account_id)
            .cloned()
            .ok_or(ClearingError::AccountNotLiquidatable)?;
        let before = self
            .account_snapshot(account_id)
            .ok_or(ClearingError::AccountNotFound)?;
        if !before.has_open_position() {
            return self.finalize_externally_flattened_liquidation(order_id, pending);
        }
        if let Some(positions) = &before.hedge_positions {
            let remaining = pending
                .remaining_hedge_positions
                .as_ref()
                .ok_or(ClearingError::InvalidLiquidationQuantity)?;
            if positions.long.qty > remaining.long.qty || positions.short.qty > remaining.short.qty
            {
                return Err(ClearingError::InvalidLiquidationQuantity);
            }
        } else if before.position_qty.signum() != pending.initial_position_qty.signum()
            || before.position_qty.abs() > pending.remaining_position_qty.abs()
        {
            return Err(ClearingError::InvalidLiquidationQuantity);
        }

        let (position_side, position_qty) = if let Some(positions) = &before.hedge_positions {
            let long_has_depth =
                positions.long.qty > 0 && self.book.fill_quote(Side::Sell, None, 1)?.qty > 0;
            if positions.long.qty > 0 && (long_has_depth || positions.short.qty == 0) {
                (crate::PositionSide::Long, positions.long.qty)
            } else {
                (crate::PositionSide::Short, -positions.short.qty)
            }
        } else {
            (crate::PositionSide::Both, before.position_qty)
        };
        let side = if position_qty > 0 {
            Side::Sell
        } else if position_qty < 0 {
            Side::Buy
        } else {
            return Err(ClearingError::InvalidLiquidationQuantity);
        };
        let qty = u64::try_from(position_qty.unsigned_abs())
            .map_err(|_| ClearingError::InvalidLiquidationQuantity)?;

        let command = Command::NewOrder(NewOrder {
            position_side,
            order_id,
            account_id,
            side,
            kind: OrderKind::ImmediateOrCancel { price_tick: None },
            qty,
            reduce_only: true,
        });
        let mut events = self.book.cancel_orders_for_account(account_id);
        let liquidation_events = self.book.apply(command.clone());
        events.extend(liquidation_events);

        let recorded = self.log.record(command, events);
        let clearing_events =
            self.settle_recorded_events(&recorded.command.command, &recorded.events)?;
        let mut execution = PerpTradingExecution {
            command: recorded.command,
            events: recorded.events,
            clearing_events,
        };
        let attempt_notional =
            liquidation_notional_from_events(&execution.events, account_id, order_id)?;
        let after_trade = self
            .account_snapshot(account_id)
            .ok_or(ClearingError::AccountNotFound)?;
        if let (Some(before), Some(after)) = (&before.hedge_positions, &after_trade.hedge_positions)
        {
            if after.long.qty > before.long.qty || after.short.qty > before.short.qty {
                return Err(ClearingError::InvalidLiquidationQuantity);
            }
        } else if after_trade.position_qty.signum() != 0
            && after_trade.position_qty.signum() != pending.initial_position_qty.signum()
        {
            return Err(ClearingError::InvalidLiquidationQuantity);
        }
        if before.hedge_positions.is_none()
            && after_trade.position_qty.abs() > before.position_qty.abs()
        {
            return Err(ClearingError::InvalidLiquidationQuantity);
        }
        let accumulated_notional = pending
            .accumulated_notional
            .checked_add(attempt_notional)
            .ok_or(ClearingError::NotionalOverflow)?;
        let attempt_count = pending
            .attempt_count
            .checked_add(1)
            .ok_or(ClearingError::InvalidLiquidationQuantity)?;
        if let Some(state) = self.pending_liquidations.get_mut(&account_id) {
            state.remaining_position_qty = after_trade.position_qty;
            state.remaining_hedge_positions = after_trade.hedge_positions.clone();
            state.accumulated_notional = accumulated_notional;
            state.attempt_count = attempt_count;
        }

        if after_trade.has_open_position() {
            self.append_liquidation_status_change_if_missing(
                &mut execution,
                account_id,
                before.margin_status,
                &after_trade,
            );
            return Ok(execution);
        }

        // Apply liquidation-level settlement once, after the position is flat.
        // Every IOC leg remains auditable through its command/trade records,
        // while cumulative fee rounding and the insurance/ADL/loss waterfall
        // retain the same semantics as a one-shot liquidation.
        self.pending_liquidations.remove(&account_id);
        let liquidation_events = self.accounts.apply_liquidation_settlement_for_legs(
            account_id,
            order_id,
            accumulated_notional,
            pending.initial_position_qty,
            pending.initial_hedge_positions.as_deref(),
        )?;
        self.clearing_events
            .extend(liquidation_events.iter().cloned());
        execution.clearing_events.extend(liquidation_events);
        let after_settlement = self
            .account_snapshot(account_id)
            .ok_or(ClearingError::AccountNotFound)?;
        self.append_liquidation_status_change_if_missing(
            &mut execution,
            account_id,
            before.margin_status,
            &after_settlement,
        );

        Ok(execution)
    }

    fn append_liquidation_status_change_if_missing(
        &mut self,
        execution: &mut PerpTradingExecution,
        account_id: AccountId,
        previous_status: PerpMarginStatus,
        snapshot: &PerpAccountSnapshot,
    ) {
        let already_recorded = execution.clearing_events.iter().any(|event| {
            matches!(
                event,
                PerpClearingEvent::MarginStatusChanged {
                    account_id: changed_account_id,
                    new_status,
                    ..
                } if *changed_account_id == account_id
                    && *new_status == snapshot.margin_status
            )
        });
        if previous_status == snapshot.margin_status || already_recorded {
            return;
        }
        let event = PerpClearingEvent::MarginStatusChanged {
            account_id,
            previous_status,
            new_status: snapshot.margin_status,
            mark_price_tick: self.accounts.mark_price_tick(),
            snapshot: snapshot.clone(),
        };
        self.clearing_events.push(event.clone());
        execution.clearing_events.push(event);
    }

    fn finalize_externally_flattened_liquidation(
        &mut self,
        order_id: OrderId,
        pending: PendingLiquidation,
    ) -> Result<PerpTradingExecution, ClearingError> {
        let (position_side, remaining_position) =
            if let Some(p) = &pending.remaining_hedge_positions {
                if p.long.qty > 0 {
                    (crate::PositionSide::Long, p.long.qty)
                } else {
                    (crate::PositionSide::Short, -p.short.qty)
                }
            } else {
                (crate::PositionSide::Both, pending.remaining_position_qty)
            };
        let side = if remaining_position > 0 {
            Side::Sell
        } else {
            Side::Buy
        };
        let qty = u64::try_from(remaining_position.unsigned_abs())
            .map_err(|_| ClearingError::InvalidLiquidationQuantity)?;
        let command = Command::NewOrder(NewOrder {
            position_side,
            order_id,
            account_id: pending.account_id,
            side,
            kind: OrderKind::ImmediateOrCancel { price_tick: None },
            qty,
            reduce_only: true,
        });
        let recorded = self.log.record(
            command,
            vec![
                Event::OrderAccepted { order_id },
                Event::OrderExpired {
                    order_id,
                    unfilled_qty: qty,
                },
            ],
        );
        self.pending_liquidations.remove(&pending.account_id);

        let liquidation_events = if pending.accumulated_notional == 0 {
            Vec::new()
        } else {
            self.accounts.apply_liquidation_settlement_for_legs(
                pending.account_id,
                order_id,
                pending.accumulated_notional,
                pending.initial_position_qty,
                pending.initial_hedge_positions.as_deref(),
            )?
        };
        self.clearing_events
            .extend(liquidation_events.iter().cloned());
        Ok(PerpTradingExecution {
            command: recorded.command,
            events: recorded.events,
            clearing_events: liquidation_events,
        })
    }

    pub fn snapshot(&self) -> BookSnapshot {
        self.book.snapshot()
    }

    pub fn mark_price_tick(&self) -> i64 {
        self.accounts.mark_price_tick()
    }

    pub(crate) fn position_exit_qty(
        &self,
        side: Side,
        qty: u64,
        lot: u64,
        cap: Option<Money>,
    ) -> u64 {
        let Some(cap) = cap else {
            return qty;
        };
        let mut low = 0;
        let mut high = qty / lot;
        while low < high {
            let mid = low + (high - low) / 2 + 1;
            let allowed = self
                .book
                .fill_quote(side, None, mid * lot)
                .is_ok_and(|q| q.notional <= cap);
            if allowed {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        low * lot
    }

    pub fn account_snapshot(&self, account_id: AccountId) -> Option<PerpAccountSnapshot> {
        self.accounts.account_snapshot(account_id)
    }

    pub fn account_snapshots(&self) -> Vec<PerpAccountSnapshot> {
        self.accounts.snapshots()
    }

    pub(crate) fn reservation_changes(&self) -> &crate::account::ReservationChanges {
        &self.accounts.reservation_changes
    }

    pub(crate) fn reservation_changes_mut(&mut self) -> &mut crate::account::ReservationChanges {
        &mut self.accounts.reservation_changes
    }

    pub(crate) fn reservation_balances(
        &self,
    ) -> impl Iterator<Item = Result<(AccountId, Money), ClearingError>> + '_ {
        self.accounts.reservation_balances()
    }

    pub(crate) fn open_position_accounts(&self) -> impl Iterator<Item = AccountId> + '_ {
        self.accounts.open_position_accounts()
    }

    pub(crate) fn has_open_position(&self, account_id: AccountId) -> bool {
        self.accounts.has_open_position(account_id)
    }

    pub fn command_log(&self) -> &[CommandRecord] {
        self.log.commands()
    }

    pub fn event_log(&self) -> &[EventRecord] {
        self.log.events()
    }

    pub fn clearing_log(&self) -> &[PerpClearingEvent] {
        &self.clearing_events
    }

    fn settle_recorded_events(
        &mut self,
        command: &Command,
        events: &[EventRecord],
    ) -> Result<Vec<PerpClearingEvent>, ClearingError> {
        let mut clearing_events = Vec::new();

        for record in events {
            match &record.event {
                Event::TradePrinted(trade) => {
                    let trade_clearing_events = self.accounts.settle_trade(trade)?;
                    self.clearing_events
                        .extend(trade_clearing_events.iter().cloned());
                    clearing_events.extend(trade_clearing_events);
                }
                Event::OrderRested {
                    order_id,
                    price_tick,
                    remaining_qty,
                } => {
                    if let Command::NewOrder(order) = command
                        && order.order_id == *order_id
                        && !self.book.is_reduce_order(*order_id)
                    {
                        self.accounts.reserve_resting_order(
                            *order_id,
                            order.account_id,
                            order.side,
                            *price_tick,
                            *remaining_qty,
                        )?;
                    }
                }
                Event::OrderCanceled { order_id, .. } | Event::OrderExpired { order_id, .. } => {
                    self.accounts.release_order_reservation(*order_id)?;
                }
                Event::OrderAmended {
                    order_id,
                    new_price_tick,
                    new_qty,
                    ..
                } => {
                    if !self.book.is_reduce_order(*order_id) {
                        self.accounts.amend_order_reservation(
                            *order_id,
                            *new_price_tick,
                            *new_qty,
                        )?;
                    }
                }
                Event::OrderAccepted { .. }
                | Event::OrderRejected { .. }
                | Event::RiskRejected { .. }
                | Event::OrderPartiallyFilled { .. }
                | Event::OrderFilled { .. }
                | Event::CancelRejected { .. }
                | Event::AmendRejected { .. } => {}
            }
        }

        Ok(clearing_events)
    }
}

#[derive(Clone)]
struct ReduceOnlyMatching<'a> {
    accounts: &'a PerpAccountStore,
    positions: BTreeMap<AccountId, PerpAccountSnapshot>,
}
impl ReduceOnlyMatching<'_> {
    fn position(&mut self, id: AccountId) -> Option<&mut PerpAccountSnapshot> {
        if !self.positions.contains_key(&id) {
            self.positions
                .insert(id, self.accounts.account_snapshot(id)?);
        }
        self.positions.get_mut(&id)
    }
}
impl crate::engine::MatchingGuard for ReduceOnlyMatching<'_> {
    fn cap(
        &mut self,
        id: AccountId,
        side: Side,
        leg: crate::PositionSide,
        reduce: bool,
        qty: u64,
    ) -> u64 {
        if !reduce {
            return qty;
        }
        let Some(a) = self.position(id) else {
            return 0;
        };
        let Some(position) = crate::position_protection::position_qty(a, leg) else {
            return 0;
        };
        if position == 0 || (position > 0) != (side == Side::Sell) {
            return 0;
        }
        u128::from(qty).min(position.unsigned_abs()) as u64
    }
    fn settle(&mut self, t: &crate::Trade) {
        for (id, side, leg) in [
            (
                t.maker_account_id,
                t.taker_side.opposite(),
                t.maker_position_side,
            ),
            (t.taker_account_id, t.taker_side, t.taker_position_side),
        ] {
            if let Some(a) = self.position(id) {
                let q = i128::from(t.qty);
                if let Some(p) = a.hedge_positions.as_mut() {
                    match (leg, side) {
                        (crate::PositionSide::Long, Side::Buy) => p.long.qty += q,
                        (crate::PositionSide::Long, Side::Sell) => p.long.qty -= q,
                        (crate::PositionSide::Short, Side::Sell) => p.short.qty += q,
                        (crate::PositionSide::Short, Side::Buy) => p.short.qty -= q,
                        _ => {}
                    }
                } else {
                    a.position_qty += if side == Side::Buy { q } else { -q };
                }
            }
        }
    }
}

fn liquidation_notional_from_events(
    events: &[EventRecord],
    account_id: AccountId,
    order_id: OrderId,
) -> Result<Money, ClearingError> {
    let mut liquidation_notional: Money = 0;

    for event in events {
        let Event::TradePrinted(trade) = &event.event else {
            continue;
        };
        if trade.taker_account_id == account_id && trade.taker_order_id == order_id {
            liquidation_notional = liquidation_notional
                .checked_add(notional(trade.price_tick, trade.qty)?)
                .ok_or(ClearingError::NotionalOverflow)?;
        }
    }

    Ok(liquidation_notional)
}

fn risk_context(book: &OrderBook, command: &Command) -> Result<RiskContext, ClearingError> {
    let fill_quote = match command {
        Command::NewOrder(order) => {
            book.fill_quote(order.side, order.kind.limit_price_tick(), order.qty)?
        }
        Command::CancelOrder(_)
        | Command::ExpireOrder { .. }
        | Command::AmendOrder(_)
        | Command::SetMarkPrice(_)
        | Command::SettleFunding(_)
        | Command::SetConditionalOrder { .. }
        | Command::SetPositionProtection { .. }
        | Command::NewOrderWithProtection { .. } => Default::default(),
    };
    Ok(RiskContext {
        best_bid: book.best_bid(),
        best_ask: book.best_ask(),
        fill_quote,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PerpTradingExecution {
    pub command: CommandRecord,
    pub events: Vec<EventRecord>,
    pub clearing_events: Vec<PerpClearingEvent>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AmendOrder, CancelOrder, NewOrder, OrderKind, Side, Trade};

    fn limit(order_id: u64, account_id: u64, side: Side, price_tick: i64, qty: u64) -> Command {
        Command::NewOrder(NewOrder {
            position_side: crate::model::PositionSide::Both,
            order_id,
            account_id,
            side,
            kind: OrderKind::Limit { price_tick },
            qty,
            reduce_only: false,
        })
    }

    fn market(order_id: u64, account_id: u64, side: Side, qty: u64) -> Command {
        Command::NewOrder(NewOrder {
            position_side: crate::model::PositionSide::Both,
            order_id,
            account_id,
            side,
            kind: OrderKind::Market,
            qty,
            reduce_only: false,
        })
    }

    fn unpriced_ioc(order_id: u64, account_id: u64, side: Side, qty: u64) -> Command {
        Command::NewOrder(NewOrder {
            position_side: crate::model::PositionSide::Both,
            order_id,
            account_id,
            side,
            kind: OrderKind::ImmediateOrCancel { price_tick: None },
            qty,
            reduce_only: false,
        })
    }

    fn unpriced_fok(order_id: u64, account_id: u64, side: Side, qty: u64) -> Command {
        Command::NewOrder(NewOrder {
            position_side: crate::model::PositionSide::Both,
            order_id,
            account_id,
            side,
            kind: OrderKind::FillOrKill { price_tick: None },
            qty,
            reduce_only: false,
        })
    }

    fn cancel(order_id: u64) -> Command {
        Command::CancelOrder(CancelOrder { order_id })
    }

    fn amend(order_id: u64, price_tick: Option<i64>, qty: Option<u64>) -> Command {
        Command::AmendOrder(AmendOrder {
            order_id,
            price_tick,
            qty,
        })
    }

    #[test]
    fn spot_trading_engine_reserves_cash_and_rejects_overlapping_buy_orders() {
        let mut engine = SpotTradingEngine::new(SpotClearingConfig::default());
        engine.create_account(20, 1_000);

        engine.apply(limit(1, 20, Side::Buy, 100, 10)).unwrap();
        assert_eq!(
            engine.account_snapshot(20),
            Some(SpotAccountSnapshot {
                account_id: 20,
                cash_balance: 1_000,
                position_qty: 0,
                reserved_cash: 1_000,
                reserved_position: 0,
                available_cash: 0,
                available_position: 0,
                fees_paid: 0,
            })
        );

        let rejected = engine.apply(limit(2, 20, Side::Buy, 100, 1)).unwrap();
        assert!(matches!(
            rejected.events[0].event,
            Event::RiskRejected {
                reason: crate::model::RiskRejectReason::InsufficientCash,
                ..
            }
        ));
    }

    #[test]
    fn spot_trading_engine_releases_reserved_position_on_cancel() {
        let mut engine = SpotTradingEngine::new(SpotClearingConfig::default());
        engine.create_account_with_position(10, 0, 5);

        engine.apply(limit(1, 10, Side::Sell, 100, 5)).unwrap();
        assert_eq!(engine.account_snapshot(10).unwrap().reserved_position, 5);

        engine.apply(cancel(1)).unwrap();
        assert_eq!(
            engine.account_snapshot(10),
            Some(SpotAccountSnapshot {
                account_id: 10,
                cash_balance: 0,
                position_qty: 5,
                reserved_cash: 0,
                reserved_position: 0,
                available_cash: 0,
                available_position: 5,
                fees_paid: 0,
            })
        );
    }

    #[test]
    fn spot_trading_engine_amend_reduces_buy_reservation() {
        let mut engine = SpotTradingEngine::new(SpotClearingConfig::default());
        engine.create_account(20, 1_000);

        engine.apply(limit(1, 20, Side::Buy, 100, 10)).unwrap();
        engine.apply(amend(1, Some(80), Some(5))).unwrap();

        assert_eq!(
            engine.account_snapshot(20),
            Some(SpotAccountSnapshot {
                account_id: 20,
                cash_balance: 1_000,
                position_qty: 0,
                reserved_cash: 400,
                reserved_position: 0,
                available_cash: 600,
                available_position: 0,
                fees_paid: 0,
            })
        );
    }

    #[test]
    fn spot_trading_engine_settles_cash_and_position_after_match() {
        let mut engine = SpotTradingEngine::new(SpotClearingConfig {
            maker_fee_ppm: 0,
            taker_fee_ppm: 1_000,
        });
        engine.create_account_with_position(10, 10_000, 10);
        engine.create_account(20, 10_000);

        let resting = engine
            .apply(limit(1, 10, Side::Sell, 100, 10))
            .expect("resting order should apply");
        assert!(resting.clearing_events.is_empty());

        let execution = engine
            .apply(limit(2, 20, Side::Buy, 100, 4))
            .expect("crossing order should apply");

        assert_eq!(execution.clearing_events.len(), 1);
        assert_eq!(
            engine.account_snapshot(20),
            Some(SpotAccountSnapshot {
                account_id: 20,
                cash_balance: 9_600,
                position_qty: 4,
                reserved_cash: 0,
                reserved_position: 0,
                available_cash: 9_600,
                available_position: 4,
                fees_paid: 0,
            })
        );
        assert_eq!(
            engine.account_snapshot(10),
            Some(SpotAccountSnapshot {
                account_id: 10,
                cash_balance: 10_400,
                position_qty: 6,
                reserved_cash: 0,
                reserved_position: 6,
                available_cash: 10_400,
                available_position: 0,
                fees_paid: 0,
            })
        );
    }

    #[test]
    fn spot_trading_engine_applies_taker_fee_when_notional_is_large_enough() {
        let mut engine = SpotTradingEngine::new(SpotClearingConfig {
            maker_fee_ppm: 500,
            taker_fee_ppm: 1_000,
        });
        engine.create_account_with_position(10, 0, 10);
        engine.create_account(20, 20_000);

        engine.apply(limit(1, 10, Side::Sell, 1_000, 10)).unwrap();
        engine.apply(market(2, 20, Side::Buy, 10)).unwrap();

        assert_eq!(
            engine.account_snapshot(20),
            Some(SpotAccountSnapshot {
                account_id: 20,
                cash_balance: 9_990,
                position_qty: 10,
                reserved_cash: 0,
                reserved_position: 0,
                available_cash: 9_990,
                available_position: 10,
                fees_paid: 10,
            })
        );
        assert_eq!(
            engine.account_snapshot(10),
            Some(SpotAccountSnapshot {
                account_id: 10,
                cash_balance: 9_995,
                position_qty: 0,
                reserved_cash: 0,
                reserved_position: 0,
                available_cash: 9_995,
                available_position: 0,
                fees_paid: 5,
            })
        );
    }

    #[test]
    fn spot_trading_engine_clears_multiple_trades_from_one_market_order() {
        let mut engine = SpotTradingEngine::new(SpotClearingConfig::default());
        engine.create_account_with_position(10, 0, 3);
        engine.create_account_with_position(11, 0, 4);
        engine.create_account(20, 1_000);

        engine.apply(limit(1, 10, Side::Sell, 100, 3)).unwrap();
        engine.apply(limit(2, 11, Side::Sell, 101, 4)).unwrap();
        let execution = engine.apply(market(3, 20, Side::Buy, 10)).unwrap();

        assert_eq!(execution.clearing_events.len(), 2);
        assert_eq!(engine.clearing_log().len(), 2);
        assert_eq!(
            engine.account_snapshot(20),
            Some(SpotAccountSnapshot {
                account_id: 20,
                cash_balance: 296,
                position_qty: 7,
                reserved_cash: 0,
                reserved_position: 0,
                available_cash: 296,
                available_position: 7,
                fees_paid: 0,
            })
        );
        assert_eq!(
            engine.account_snapshot(10),
            Some(SpotAccountSnapshot {
                account_id: 10,
                cash_balance: 300,
                position_qty: 0,
                reserved_cash: 0,
                reserved_position: 0,
                available_cash: 300,
                available_position: 0,
                fees_paid: 0,
            })
        );
        assert_eq!(
            engine.account_snapshot(11),
            Some(SpotAccountSnapshot {
                account_id: 11,
                cash_balance: 404,
                position_qty: 0,
                reserved_cash: 0,
                reserved_position: 0,
                available_cash: 404,
                available_position: 0,
                fees_paid: 0,
            })
        );
    }

    #[test]
    fn unpriced_spot_buys_use_full_depth_cost_and_never_overdraw_cash() {
        for (index, command) in [
            market(3, 20, Side::Buy, 2),
            unpriced_ioc(4, 20, Side::Buy, 2),
            unpriced_fok(5, 20, Side::Buy, 2),
        ]
        .into_iter()
        .enumerate()
        {
            let mut engine = SpotTradingEngine::new(SpotClearingConfig::default());
            engine.create_account_with_position(10, 0, 1);
            engine.create_account_with_position(11, 0, 1);
            engine.create_account(20, 2);
            engine.apply(limit(1, 10, Side::Sell, 1, 1)).unwrap();
            engine.apply(limit(2, 11, Side::Sell, 100, 1)).unwrap();

            let execution = engine.apply(command).unwrap();

            assert!(matches!(
                execution.events[0].event,
                Event::RiskRejected {
                    reason: crate::model::RiskRejectReason::InsufficientCash,
                    ..
                }
            ));
            assert_eq!(engine.account_snapshot(20).unwrap().cash_balance, 2);
            assert_eq!(
                engine
                    .snapshot()
                    .asks
                    .iter()
                    .map(|level| level.qty)
                    .sum::<u64>(),
                2
            );
            assert_eq!(engine.clearing_log().len(), 0, "case {index}");
        }
    }

    #[test]
    fn unpriced_spot_buy_with_depth_funding_still_fills_normally() {
        let mut engine = SpotTradingEngine::new(SpotClearingConfig::default());
        engine.create_account_with_position(10, 0, 1);
        engine.create_account_with_position(11, 0, 1);
        engine.create_account(20, 101);
        engine.apply(limit(1, 10, Side::Sell, 1, 1)).unwrap();
        engine.apply(limit(2, 11, Side::Sell, 100, 1)).unwrap();

        let execution = engine.apply(market(3, 20, Side::Buy, 2)).unwrap();

        assert_eq!(execution.clearing_events.len(), 2);
        assert_eq!(engine.account_snapshot(20).unwrap().cash_balance, 0);
        assert_eq!(engine.account_snapshot(20).unwrap().position_qty, 2);
    }

    #[test]
    fn clearing_failure_rolls_back_book_accounts_and_logs() {
        let mut engine = SpotTradingEngine::new(SpotClearingConfig::default());
        engine.create_account_with_position(10, Money::MAX, 1);
        engine.create_account(20, 1);
        engine.apply(limit(1, 10, Side::Sell, 1, 1)).unwrap();
        let commands_before = engine.command_log().len();
        let events_before = engine.event_log().len();

        assert_eq!(
            engine.apply(market(2, 20, Side::Buy, 1)),
            Err(ClearingError::BalanceOverflow)
        );
        assert_eq!(engine.snapshot().asks[0].qty, 1);
        assert_eq!(
            engine.account_snapshot(10).unwrap().cash_balance,
            Money::MAX
        );
        assert_eq!(engine.account_snapshot(20).unwrap().cash_balance, 1);
        assert_eq!(engine.command_log().len(), commands_before);
        assert_eq!(engine.event_log().len(), events_before);
        assert!(engine.clearing_log().is_empty());
    }

    #[test]
    fn spot_trading_engine_risk_rejects_order_before_matching() {
        let mut engine = SpotTradingEngine::new(SpotClearingConfig::default());
        engine.create_account(20, 99);

        let execution = engine
            .apply(limit(1, 20, Side::Buy, 100, 1))
            .expect("risk rejection is a recorded execution");

        assert!(execution.clearing_events.is_empty());
        assert!(matches!(
            execution.events[0].event,
            Event::RiskRejected {
                order_id: 1,
                reason: crate::model::RiskRejectReason::InsufficientCash
            }
        ));
        assert!(engine.snapshot().bids.is_empty());
    }

    #[test]
    fn short_enabled_resting_sell_is_rejected_before_it_can_become_a_ghost_order() {
        let mut engine = SpotTradingEngine::new_with_risk(
            SpotClearingConfig::default(),
            SpotRiskConfig {
                allow_short: true,
                ..SpotRiskConfig::default()
            },
        );
        engine.create_account(20, 0);

        let execution = engine.apply(limit(1, 20, Side::Sell, 100, 1)).unwrap();

        assert!(matches!(
            execution.events[0].event,
            Event::RiskRejected {
                reason: crate::model::RiskRejectReason::InsufficientPosition,
                ..
            }
        ));
        assert!(engine.snapshot().asks.is_empty());
        assert!(engine.clearing_log().is_empty());
    }

    #[test]
    fn perp_trading_engine_uses_same_order_book_but_contract_clearing() {
        let mut engine = PerpTradingEngine::new(
            PerpClearingConfig {
                maker_fee_ppm: 0,
                taker_fee_ppm: 1_000,
                leverage: 10,
                ..PerpClearingConfig::default()
            },
            100,
        )
        .expect("valid perp engine");
        engine.create_account(10, 10_000);
        engine.create_account(20, 10_000);

        engine.apply(limit(1, 10, Side::Sell, 100, 10)).unwrap();
        let execution = engine.apply(market(2, 20, Side::Buy, 4)).unwrap();

        assert_eq!(execution.clearing_events.len(), 1);
        assert_eq!(
            engine.account_snapshot(20),
            Some(PerpAccountSnapshot {
                hedge_positions: None,
                account_id: 20,
                cash_balance: 10_000,
                position_qty: 4,
                avg_entry_price_tick: 100,
                realized_pnl: 0,
                unrealized_pnl: 0,
                equity: 10_000,
                initial_margin: 40,
                maintenance_margin: 20,
                portfolio_initial_margin: 40,
                portfolio_maintenance_margin: 20,
                margin_status: crate::perp::PerpMarginStatus::Healthy,
                reserved_margin: 0,
                available_cash: 9_960,
                fees_paid: 0,
                funding_pnl: 0,
            })
        );
        assert_eq!(
            engine.account_snapshot(10),
            Some(PerpAccountSnapshot {
                hedge_positions: None,
                account_id: 10,
                cash_balance: 10_000,
                position_qty: -4,
                avg_entry_price_tick: 100,
                realized_pnl: 0,
                unrealized_pnl: 0,
                equity: 10_000,
                initial_margin: 40,
                maintenance_margin: 20,
                portfolio_initial_margin: 40,
                portfolio_maintenance_margin: 20,
                margin_status: crate::perp::PerpMarginStatus::Healthy,
                reserved_margin: 60,
                available_cash: 9_900,
                fees_paid: 0,
                funding_pnl: 0,
            })
        );
    }

    #[test]
    fn perp_trading_engine_reserves_margin_and_rejects_overlapping_orders() {
        let mut engine = PerpTradingEngine::new(
            PerpClearingConfig {
                maker_fee_ppm: 0,
                taker_fee_ppm: 0,
                leverage: 10,
                ..PerpClearingConfig::default()
            },
            100,
        )
        .expect("valid perp engine");
        engine.create_account(20, 100);

        engine.apply(limit(1, 20, Side::Buy, 100, 10)).unwrap();
        assert_eq!(
            engine.account_snapshot(20),
            Some(PerpAccountSnapshot {
                hedge_positions: None,
                account_id: 20,
                cash_balance: 100,
                position_qty: 0,
                avg_entry_price_tick: 0,
                realized_pnl: 0,
                unrealized_pnl: 0,
                equity: 100,
                initial_margin: 0,
                maintenance_margin: 0,
                portfolio_initial_margin: 0,
                portfolio_maintenance_margin: 0,
                margin_status: crate::perp::PerpMarginStatus::Flat,
                reserved_margin: 100,
                available_cash: 0,
                fees_paid: 0,
                funding_pnl: 0,
            })
        );

        let rejected = engine.apply(limit(2, 20, Side::Buy, 100, 1)).unwrap();
        assert!(matches!(
            rejected.events[0].event,
            Event::RiskRejected {
                order_id: 2,
                reason: crate::model::RiskRejectReason::InsufficientMargin,
            }
        ));
        assert_eq!(engine.snapshot().bids[0].qty, 10);
    }

    #[test]
    fn perp_trading_engine_releases_reserved_margin_on_cancel() {
        let mut engine = PerpTradingEngine::new(
            PerpClearingConfig {
                maker_fee_ppm: 0,
                taker_fee_ppm: 0,
                leverage: 10,
                ..PerpClearingConfig::default()
            },
            100,
        )
        .expect("valid perp engine");
        engine.create_account(20, 100);

        engine.apply(limit(1, 20, Side::Buy, 100, 10)).unwrap();
        assert_eq!(engine.account_snapshot(20).unwrap().reserved_margin, 100);

        engine.apply(cancel(1)).unwrap();
        assert_eq!(
            engine.account_snapshot(20),
            Some(PerpAccountSnapshot {
                hedge_positions: None,
                account_id: 20,
                cash_balance: 100,
                position_qty: 0,
                avg_entry_price_tick: 0,
                realized_pnl: 0,
                unrealized_pnl: 0,
                equity: 100,
                initial_margin: 0,
                maintenance_margin: 0,
                portfolio_initial_margin: 0,
                portfolio_maintenance_margin: 0,
                margin_status: crate::perp::PerpMarginStatus::Flat,
                reserved_margin: 0,
                available_cash: 100,
                fees_paid: 0,
                funding_pnl: 0,
            })
        );
    }

    #[test]
    fn perp_trading_engine_amend_reprices_reserved_margin() {
        let mut engine = PerpTradingEngine::new(
            PerpClearingConfig {
                maker_fee_ppm: 0,
                taker_fee_ppm: 0,
                leverage: 10,
                ..PerpClearingConfig::default()
            },
            100,
        )
        .expect("valid perp engine");
        engine.create_account(20, 100);

        engine.apply(limit(1, 20, Side::Buy, 100, 10)).unwrap();
        engine.apply(amend(1, Some(80), Some(5))).unwrap();

        assert_eq!(
            engine.account_snapshot(20),
            Some(PerpAccountSnapshot {
                hedge_positions: None,
                account_id: 20,
                cash_balance: 100,
                position_qty: 0,
                avg_entry_price_tick: 0,
                realized_pnl: 0,
                unrealized_pnl: 0,
                equity: 100,
                initial_margin: 0,
                maintenance_margin: 0,
                portfolio_initial_margin: 0,
                portfolio_maintenance_margin: 0,
                margin_status: crate::perp::PerpMarginStatus::Flat,
                reserved_margin: 40,
                available_cash: 60,
                fees_paid: 0,
                funding_pnl: 0,
            })
        );
    }

    #[test]
    fn perp_trading_engine_rejects_amend_that_needs_unavailable_margin() {
        let mut engine = PerpTradingEngine::new(
            PerpClearingConfig {
                maker_fee_ppm: 0,
                taker_fee_ppm: 0,
                leverage: 10,
                ..PerpClearingConfig::default()
            },
            100,
        )
        .expect("valid perp engine");
        engine.create_account(20, 100);

        engine.apply(limit(1, 20, Side::Sell, 100, 10)).unwrap();
        let rejected = engine.apply(amend(1, Some(120), None)).unwrap();

        assert!(matches!(
            rejected.events[0].event,
            Event::RiskRejected {
                order_id: 1,
                reason: crate::model::RiskRejectReason::InsufficientMargin,
            }
        ));
        assert_eq!(engine.account_snapshot(20).unwrap().reserved_margin, 100);
        assert_eq!(engine.snapshot().asks[0].price_tick, 100);
    }

    #[test]
    fn perp_trading_engine_mark_price_updates_unrealized_pnl() {
        let mut engine =
            PerpTradingEngine::new(PerpClearingConfig::default(), 100).expect("valid engine");
        engine.create_account(10, 10_000);
        engine.create_account(20, 10_000);

        engine.apply(limit(1, 10, Side::Sell, 100, 10)).unwrap();
        engine.apply(market(2, 20, Side::Buy, 10)).unwrap();
        engine.set_mark_price_tick(110).unwrap();

        assert_eq!(engine.account_snapshot(20).unwrap().unrealized_pnl, 100);
        assert_eq!(engine.account_snapshot(10).unwrap().unrealized_pnl, -100);
    }

    #[test]
    fn perp_trading_engine_emits_margin_status_changes_on_mark_price_updates() {
        let mut engine = PerpTradingEngine::new(
            PerpClearingConfig {
                leverage: 10,
                maintenance_margin_ppm: 50_000,
                ..PerpClearingConfig::default()
            },
            100,
        )
        .expect("valid engine");
        engine.create_account(10, 10_000);
        engine.create_account(20, 200);

        engine.apply(limit(1, 10, Side::Sell, 100, 10)).unwrap();
        engine.apply(market(2, 20, Side::Buy, 10)).unwrap();

        let margin_call_events = engine.set_mark_price_tick(90).unwrap();
        assert_eq!(margin_call_events.len(), 1);
        assert!(matches!(
            margin_call_events[0],
            PerpClearingEvent::MarginStatusChanged {
                account_id: 20,
                previous_status: crate::perp::PerpMarginStatus::Healthy,
                new_status: crate::perp::PerpMarginStatus::MarginCall,
                mark_price_tick: 90,
                ..
            }
        ));

        assert!(engine.set_mark_price_tick(90).unwrap().is_empty());

        let liquidation_events = engine.set_mark_price_tick(80).unwrap();
        assert_eq!(liquidation_events.len(), 1);
        assert!(matches!(
            liquidation_events[0],
            PerpClearingEvent::MarginStatusChanged {
                account_id: 20,
                previous_status: crate::perp::PerpMarginStatus::MarginCall,
                new_status: crate::perp::PerpMarginStatus::Liquidatable,
                mark_price_tick: 80,
                ..
            }
        ));
        assert_eq!(
            engine.account_snapshot(20).unwrap().margin_status,
            crate::perp::PerpMarginStatus::Liquidatable
        );
        assert_eq!(engine.clearing_log().len(), 3);
    }

    #[test]
    fn perp_trading_engine_rejects_liquidation_for_healthy_account() {
        let mut engine =
            PerpTradingEngine::new(PerpClearingConfig::default(), 100).expect("valid engine");
        engine.create_account(20, 10_000);

        assert_eq!(
            engine.liquidate_account(20, 99),
            Err(ClearingError::AccountNotLiquidatable)
        );
    }

    #[test]
    fn pending_liquidation_survives_no_depth_and_progresses_across_partial_fills() {
        let mut engine = PerpTradingEngine::new(
            PerpClearingConfig {
                leverage: 10,
                maintenance_margin_ppm: 50_000,
                liquidation_fee_ppm: 10_000,
                ..PerpClearingConfig::default()
            },
            100,
        )
        .unwrap();
        engine.create_account(10, 10_000);
        engine.create_account(20, 240);
        engine.create_account(30, 10_000);
        engine.apply(limit(1, 10, Side::Sell, 100, 10)).unwrap();
        engine.apply(market(2, 20, Side::Buy, 10)).unwrap();
        engine.set_mark_price_tick(80).unwrap();
        let command_count = engine.command_log().len();
        let event_count = engine.event_log().len();

        let empty_attempt = engine.liquidate_account(20, 3).unwrap();
        assert!(matches!(
            empty_attempt.command.command,
            Command::NewOrder(NewOrder {
                kind: OrderKind::ImmediateOrCancel { price_tick: None },
                qty: 10,
                reduce_only: true,
                ..
            })
        ));
        assert!(empty_attempt.events.iter().any(|event| matches!(
            event.event,
            Event::OrderExpired {
                order_id: 3,
                unfilled_qty: 10
            }
        )));
        assert_eq!(engine.account_snapshot(20).unwrap().position_qty, 10);
        assert_eq!(engine.command_log().len(), command_count + 1);
        assert_eq!(engine.event_log().len(), event_count + 2);
        assert_eq!(
            engine.pending_liquidation(20),
            Some(&PendingLiquidation {
                initial_hedge_positions: None,
                remaining_hedge_positions: None,
                account_id: 20,
                initial_position_qty: 10,
                remaining_position_qty: 10,
                accumulated_notional: 0,
                attempt_count: 1,
                initial_margin_status: PerpMarginStatus::Liquidatable,
            })
        );

        engine.apply(limit(4, 30, Side::Buy, 80, 7)).unwrap();
        let partial = engine.liquidate_account(20, 5).unwrap();
        assert!(
            partial
                .events
                .iter()
                .any(|event| matches!(event.event, Event::TradePrinted(_)))
        );
        assert!(
            partial
                .clearing_events
                .iter()
                .any(|event| matches!(event, PerpClearingEvent::TradeSettled { qty: 7, .. }))
        );
        assert!(
            !partial
                .clearing_events
                .iter()
                .any(|event| matches!(event, PerpClearingEvent::LiquidationSettled { .. }))
        );
        assert!(partial.clearing_events.iter().any(|event| matches!(
            event,
            PerpClearingEvent::MarginStatusChanged {
                account_id: 20,
                previous_status: PerpMarginStatus::Liquidatable,
                new_status: PerpMarginStatus::Healthy,
                ..
            }
        )));
        let partially_liquidated = engine.account_snapshot(20).unwrap();
        assert_eq!(partially_liquidated.position_qty, 3);
        assert_eq!(
            partially_liquidated.margin_status,
            PerpMarginStatus::Healthy
        );
        let blocked_reentry = engine.apply(market(50, 20, Side::Buy, 1)).unwrap();
        assert!(blocked_reentry.events.iter().any(|event| matches!(
            event.event,
            Event::RiskRejected {
                order_id: 50,
                reason: RiskRejectReason::InsufficientMargin,
            }
        )));
        assert_eq!(engine.account_snapshot(20).unwrap().position_qty, 3);
        assert_eq!(
            engine.pending_liquidation(20),
            Some(&PendingLiquidation {
                initial_hedge_positions: None,
                remaining_hedge_positions: None,
                account_id: 20,
                initial_position_qty: 10,
                remaining_position_qty: 3,
                accumulated_notional: 560,
                attempt_count: 2,
                initial_margin_status: PerpMarginStatus::Liquidatable,
            })
        );

        engine.apply(limit(6, 30, Side::Buy, 80, 3)).unwrap();
        let execution = engine.liquidate_account(20, 7).unwrap();
        assert!(
            execution
                .events
                .iter()
                .any(|event| matches!(event.event, Event::TradePrinted(_)))
        );
        assert!(execution.clearing_events.iter().any(|event| matches!(
            event,
            PerpClearingEvent::LiquidationSettled {
                account_id: 20,
                order_id: 7,
                liquidation_notional: 800,
                liquidation_fee: 8,
                ..
            }
        )));
        let status_changes = execution
            .clearing_events
            .iter()
            .filter_map(|event| match event {
                PerpClearingEvent::MarginStatusChanged {
                    account_id: 20,
                    previous_status,
                    new_status,
                    ..
                } => Some((*previous_status, *new_status)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            status_changes,
            vec![(PerpMarginStatus::Healthy, PerpMarginStatus::Flat)]
        );
        assert_eq!(engine.account_snapshot(20).unwrap().position_qty, 0);
        assert!(engine.pending_liquidation(20).is_none());
        assert!(engine.snapshot().bids.is_empty());
    }

    #[test]
    fn externally_flattened_pending_liquidation_finalizes_once() {
        let mut engine = PerpTradingEngine::new(
            PerpClearingConfig {
                leverage: 10,
                maintenance_margin_ppm: 50_000,
                liquidation_fee_ppm: 10_000,
                ..PerpClearingConfig::default()
            },
            100,
        )
        .unwrap();
        engine.create_account(10, 10_000);
        engine.create_account(20, 240);
        engine.create_account(30, 10_000);
        engine.apply(limit(1, 10, Side::Sell, 100, 10)).unwrap();
        engine.apply(market(2, 20, Side::Buy, 10)).unwrap();
        engine.set_mark_price_tick(80).unwrap();

        engine.apply(limit(3, 30, Side::Buy, 80, 7)).unwrap();
        engine.liquidate_account(20, 4).unwrap();
        assert_eq!(engine.account_snapshot(20).unwrap().position_qty, 3);
        assert_eq!(
            engine.pending_liquidation(20).unwrap().accumulated_notional,
            560
        );

        engine
            .accounts
            .settle_trade(&Trade {
                maker_position_side: Default::default(),
                taker_position_side: Default::default(),
                trade_id: 999,
                maker_order_id: 999,
                maker_account_id: 20,
                taker_order_id: 1_000,
                taker_account_id: 30,
                price_tick: 80,
                qty: 3,
                taker_side: Side::Buy,
            })
            .unwrap();
        assert_eq!(engine.account_snapshot(20).unwrap().position_qty, 0);

        let finalized = engine.liquidate_account(20, 5).unwrap();
        assert!(matches!(
            finalized.events[0].event,
            Event::OrderAccepted { order_id: 5 }
        ));
        assert!(matches!(
            finalized.events[1].event,
            Event::OrderExpired {
                order_id: 5,
                unfilled_qty: 3,
            }
        ));
        assert!(finalized.clearing_events.iter().any(|event| matches!(
            event,
            PerpClearingEvent::LiquidationSettled {
                account_id: 20,
                order_id: 5,
                liquidation_notional: 560,
                liquidation_fee: 5,
                ..
            }
        )));
        assert!(engine.pending_liquidation(20).is_none());
        let settlement_count = engine
            .clearing_log()
            .iter()
            .filter(|event| matches!(event, PerpClearingEvent::LiquidationSettled { .. }))
            .count();

        assert_eq!(
            engine.liquidate_account(20, 6),
            Err(ClearingError::AccountNotLiquidatable)
        );
        assert_eq!(
            engine
                .clearing_log()
                .iter()
                .filter(|event| matches!(event, PerpClearingEvent::LiquidationSettled { .. }))
                .count(),
            settlement_count
        );
    }

    #[test]
    fn perp_trading_engine_liquidates_account_with_reduce_only_ioc_order() {
        let mut engine = PerpTradingEngine::new(
            PerpClearingConfig {
                leverage: 10,
                maintenance_margin_ppm: 50_000,
                ..PerpClearingConfig::default()
            },
            100,
        )
        .expect("valid engine");
        engine.create_account(10, 10_000);
        engine.create_account(20, 200);
        engine.create_account(30, 10_000);

        engine.apply(limit(1, 10, Side::Sell, 100, 10)).unwrap();
        engine.apply(market(2, 20, Side::Buy, 10)).unwrap();
        engine.apply(limit(3, 30, Side::Buy, 80, 10)).unwrap();
        engine.set_mark_price_tick(80).unwrap();

        let execution = engine.liquidate_account(20, 4).unwrap();
        assert!(matches!(
            execution.command.command,
            Command::NewOrder(NewOrder {
                position_side: crate::model::PositionSide::Both,
                order_id: 4,
                account_id: 20,
                side: Side::Sell,
                kind: OrderKind::ImmediateOrCancel { price_tick: None },
                qty: 10,
                reduce_only: true,
            })
        ));
        assert_eq!(execution.clearing_events.len(), 2);
        assert!(matches!(
            execution.clearing_events[1],
            PerpClearingEvent::MarginStatusChanged {
                account_id: 20,
                previous_status: PerpMarginStatus::Liquidatable,
                new_status: PerpMarginStatus::Flat,
                mark_price_tick: 80,
                ..
            }
        ));

        let liquidated = engine.account_snapshot(20).unwrap();
        assert_eq!(liquidated.position_qty, 0);
        assert_eq!(liquidated.cash_balance, 0);
        assert_eq!(liquidated.realized_pnl, -200);
        assert_eq!(liquidated.margin_status, PerpMarginStatus::Flat);
        assert!(engine.snapshot().bids.is_empty());
    }

    #[test]
    fn perp_liquidation_records_fee_insurance_payment_and_bad_debt() {
        let mut engine = PerpTradingEngine::new(
            PerpClearingConfig {
                leverage: 10,
                maintenance_margin_ppm: 50_000,
                liquidation_fee_ppm: 10_000,
                ..PerpClearingConfig::default()
            },
            100,
        )
        .expect("valid engine");
        engine.create_account(10, 10_000);
        engine.create_account(20, 190);
        engine.create_account(30, 10_000);

        engine.apply(limit(1, 10, Side::Sell, 100, 10)).unwrap();
        engine.apply(market(2, 20, Side::Buy, 10)).unwrap();
        engine.apply(limit(3, 30, Side::Buy, 80, 10)).unwrap();
        engine.set_mark_price_tick(80).unwrap();

        let execution = engine.liquidate_account(20, 4).unwrap();

        assert!(matches!(
            execution.clearing_events[1],
            PerpClearingEvent::LiquidationSettled {
                account_id: 20,
                order_id: 4,
                liquidation_notional: 800,
                liquidation_fee: 8,
                insurance_fund_payment: 8,
                bad_debt: 10,
                insurance_fund_balance: 0,
                ..
            }
        ));
        let liquidated = engine.account_snapshot(20).unwrap();
        assert_eq!(liquidated.position_qty, 0);
        assert_eq!(liquidated.cash_balance, 0);
        assert_eq!(liquidated.realized_pnl, -200);
        assert_eq!(liquidated.fees_paid, 8);
        assert_eq!(liquidated.margin_status, PerpMarginStatus::Flat);
    }

    #[test]
    fn perp_liquidation_socializes_shortfall_before_bad_debt() {
        let mut engine = PerpTradingEngine::new(
            PerpClearingConfig {
                leverage: 10,
                maintenance_margin_ppm: 50_000,
                liquidation_fee_ppm: 10_000,
                socialized_loss_enabled: true,
                ..PerpClearingConfig::default()
            },
            100,
        )
        .expect("valid engine");
        engine.create_account(10, 10_000);
        engine.create_account(20, 190);
        engine.create_account(30, 10_000);

        engine.apply(limit(1, 10, Side::Sell, 100, 10)).unwrap();
        engine.apply(market(2, 20, Side::Buy, 10)).unwrap();
        engine.apply(limit(3, 30, Side::Buy, 80, 10)).unwrap();
        engine.set_mark_price_tick(80).unwrap();

        let execution = engine.liquidate_account(20, 4).unwrap();

        assert!(matches!(
            &execution.clearing_events[1],
            PerpClearingEvent::LiquidationSettled {
                account_id: 20,
                order_id: 4,
                liquidation_notional: 800,
                liquidation_fee: 8,
                insurance_fund_payment: 8,
                socialized_loss: 10,
                bad_debt: 0,
                socialized_loss_allocations,
                ..
            } if socialized_loss_allocations.len() == 1
                && socialized_loss_allocations[0].account_id == 10
                && socialized_loss_allocations[0].loss == 10
        ));
        assert_eq!(engine.account_snapshot(20).unwrap().cash_balance, 0);
        assert_eq!(engine.account_snapshot(10).unwrap().cash_balance, 9_990);
    }

    #[test]
    fn perp_liquidation_auto_deleverages_profitable_opposite_position_before_socializing() {
        let mut engine = PerpTradingEngine::new(
            PerpClearingConfig {
                leverage: 10,
                maintenance_margin_ppm: 50_000,
                liquidation_fee_ppm: 10_000,
                auto_deleveraging_enabled: true,
                socialized_loss_enabled: true,
                ..PerpClearingConfig::default()
            },
            100,
        )
        .expect("valid engine");
        engine.create_account(10, 10_000);
        engine.create_account(20, 190);
        engine.create_account(30, 10_000);

        engine.apply(limit(1, 10, Side::Sell, 100, 10)).unwrap();
        engine.apply(market(2, 20, Side::Buy, 10)).unwrap();
        engine.apply(limit(3, 30, Side::Buy, 80, 10)).unwrap();
        engine.set_mark_price_tick(80).unwrap();

        let execution = engine.liquidate_account(20, 4).unwrap();

        assert!(matches!(
            &execution.clearing_events[1],
            PerpClearingEvent::LiquidationSettled {
                account_id: 20,
                order_id: 4,
                liquidation_notional: 800,
                liquidation_fee: 8,
                insurance_fund_payment: 8,
                auto_deleveraging_loss: 10,
                socialized_loss: 0,
                bad_debt: 0,
                auto_deleveraging_allocations,
                socialized_loss_allocations,
                ..
            } if auto_deleveraging_allocations.len() == 2
                && auto_deleveraging_allocations[0].account_id == 10
                && auto_deleveraging_allocations[0].position_delta == 1
                && auto_deleveraging_allocations[0].qty == 1
                && auto_deleveraging_allocations[0].realized_pnl == 20
                && auto_deleveraging_allocations[0].loss == 10
                && auto_deleveraging_allocations[1].account_id == 30
                && auto_deleveraging_allocations[1].position_delta == -1
                && auto_deleveraging_allocations[1].qty == 1
                && auto_deleveraging_allocations[1].realized_pnl == 0
                && auto_deleveraging_allocations[1].loss == 0
                && socialized_loss_allocations.is_empty()
        ));
        let liquidated = engine.account_snapshot(20).unwrap();
        assert_eq!(liquidated.cash_balance, 0);
        assert_eq!(liquidated.position_qty, 0);

        let deleveraged = engine.account_snapshot(10).unwrap();
        assert_eq!(deleveraged.cash_balance, 10_010);
        assert_eq!(deleveraged.position_qty, -9);
        assert_eq!(deleveraged.realized_pnl, 20);
        assert_eq!(deleveraged.unrealized_pnl, 180);
        assert_eq!(deleveraged.equity, 10_190);
        let counterparty = engine.account_snapshot(30).unwrap();
        assert_eq!(counterparty.position_qty, 9);
        assert_eq!(
            engine
                .account_snapshots()
                .iter()
                .map(|account| account.position_qty)
                .sum::<crate::account::PositionQty>(),
            0
        );
    }
}
