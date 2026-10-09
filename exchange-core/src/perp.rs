use crate::shared_map::SharedMap;
use std::sync::{Arc, OnceLock};
use std::{cmp::Reverse, collections::BTreeMap};

use serde::{Deserialize, Serialize};

use crate::{
    account::{ClearingError, FeeRatePpm, Money, PositionQty, fee_for, notional},
    model::{AccountId, OrderId, PositionSide, PriceTick, Qty, Side, Trade},
};

pub const DEFAULT_MAINTENANCE_MARGIN_PPM: FeeRatePpm = 50_000;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum PositionMode {
    #[default]
    OneWay,
    Hedge,
}

impl PositionMode {
    pub fn is_one_way(&self) -> bool {
        *self == Self::OneWay
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerpPositionLeg {
    #[serde(with = "crate::funding::json_money")]
    pub qty: PositionQty,
    pub avg_entry_price_tick: PriceTick,
    #[serde(with = "crate::funding::json_money")]
    pub realized_pnl: Money,
    #[serde(with = "crate::funding::json_money")]
    pub fees_paid: Money,
    #[serde(default, with = "crate::funding::json_money")]
    pub funding_pnl: Money,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct HedgePositions {
    pub long: PerpPositionLeg,
    pub short: PerpPositionLeg,
}

impl HedgePositions {
    pub fn gross_qty(&self) -> PositionQty {
        self.long.qty + self.short.qty
    }
    pub fn leg(&self, side: PositionSide) -> Option<&PerpPositionLeg> {
        match side {
            PositionSide::Long => Some(&self.long),
            PositionSide::Short => Some(&self.short),
            PositionSide::Both => None,
        }
    }
    fn leg_mut(&mut self, side: PositionSide) -> Option<&mut PerpPositionLeg> {
        match side {
            PositionSide::Long => Some(&mut self.long),
            PositionSide::Short => Some(&mut self.short),
            PositionSide::Both => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerpClearingConfig {
    #[serde(default, skip_serializing_if = "PositionMode::is_one_way")]
    pub position_mode: PositionMode,
    pub maker_fee_ppm: FeeRatePpm,
    pub taker_fee_ppm: FeeRatePpm,
    #[serde(default)]
    pub liquidation_fee_ppm: FeeRatePpm,
    pub leverage: u32,
    #[serde(default = "default_maintenance_margin_ppm")]
    pub maintenance_margin_ppm: FeeRatePpm,
    #[serde(default)]
    pub initial_insurance_fund: Money,
    #[serde(default)]
    pub auto_deleveraging_enabled: bool,
    #[serde(default)]
    pub socialized_loss_enabled: bool,
}

impl Default for PerpClearingConfig {
    fn default() -> Self {
        Self {
            position_mode: PositionMode::OneWay,
            maker_fee_ppm: 0,
            taker_fee_ppm: 0,
            liquidation_fee_ppm: 0,
            leverage: 1,
            maintenance_margin_ppm: DEFAULT_MAINTENANCE_MARGIN_PPM,
            initial_insurance_fund: 0,
            auto_deleveraging_enabled: false,
            socialized_loss_enabled: false,
        }
    }
}

fn default_maintenance_margin_ppm() -> FeeRatePpm {
    DEFAULT_MAINTENANCE_MARGIN_PPM
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PerpMarginStatus {
    Flat,
    Healthy,
    MarginCall,
    Liquidatable,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerpCrossMarginContext {
    pub other_unrealized_pnl: Money,
    pub other_required_margin: Money,
    pub other_initial_margin: Money,
    pub other_maintenance_margin: Money,
    pub other_position_open: bool,
    pub liquidation_pending: bool,
}

impl PerpMarginStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::Healthy => "healthy",
            Self::MarginCall => "margin_call",
            Self::Liquidatable => "liquidatable",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerpAccount {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hedge_positions: Option<Box<HedgePositions>>,
    pub account_id: AccountId,
    pub cash_balance: Money,
    pub position_qty: PositionQty,
    pub avg_entry_price_tick: PriceTick,
    pub realized_pnl: Money,
    pub fees_paid: Money,
    #[serde(default, with = "crate::funding::json_money")]
    pub funding_pnl: Money,
    pub reserved_margin: Money,
}

impl PerpAccount {
    fn position_in_direction(
        &self,
        direction: PositionQty,
    ) -> (PositionQty, PriceTick, PositionSide) {
        if let Some(p) = &self.hedge_positions {
            if direction > 0 {
                (p.long.qty, p.long.avg_entry_price_tick, PositionSide::Long)
            } else {
                (
                    -p.short.qty,
                    p.short.avg_entry_price_tick,
                    PositionSide::Short,
                )
            }
        } else if self.position_qty.signum() == direction.signum() {
            (
                self.position_qty,
                self.avg_entry_price_tick,
                PositionSide::Both,
            )
        } else {
            (0, 0, PositionSide::Both)
        }
    }
    pub fn has_open_position(&self) -> bool {
        self.hedge_positions
            .as_ref()
            .map_or(self.position_qty != 0, |p| p.gross_qty() != 0)
    }
    fn initial_margin(&self, config: PerpClearingConfig) -> Money {
        self.hedge_positions.as_ref().map_or_else(
            || initial_margin(self.position_qty, self.avg_entry_price_tick, config),
            |p| {
                initial_margin(p.long.qty, p.long.avg_entry_price_tick, config)
                    + initial_margin(p.short.qty, p.short.avg_entry_price_tick, config)
            },
        )
    }
    pub fn snapshot(
        &self,
        config: PerpClearingConfig,
        mark_price_tick: PriceTick,
    ) -> PerpAccountSnapshot {
        let unrealized_pnl = self.unrealized_pnl_at_mark(mark_price_tick).unwrap_or(0);
        let equity = self.cash_balance + unrealized_pnl;
        let initial_margin = self.initial_margin(config);
        let gross_qty = self
            .hedge_positions
            .as_ref()
            .map_or(self.position_qty, |p| p.gross_qty());
        let maintenance_margin = maintenance_margin(gross_qty, mark_price_tick, config);
        PerpAccountSnapshot {
            hedge_positions: self.hedge_positions.clone(),
            account_id: self.account_id,
            cash_balance: self.cash_balance,
            position_qty: self.position_qty,
            avg_entry_price_tick: self.avg_entry_price_tick,
            realized_pnl: self.realized_pnl,
            unrealized_pnl,
            equity,
            initial_margin,
            maintenance_margin,
            portfolio_initial_margin: initial_margin,
            portfolio_maintenance_margin: maintenance_margin,
            margin_status: margin_status(gross_qty, equity, initial_margin, maintenance_margin),
            reserved_margin: self.reserved_margin,
            available_cash: self.available_cash(),
            fees_paid: self.fees_paid,
            funding_pnl: self.funding_pnl,
        }
    }

    pub fn available_cash(&self) -> Money {
        self.cash_balance - self.reserved_margin
    }

    pub fn unrealized_pnl_at_mark(&self, mark_price_tick: PriceTick) -> Option<Money> {
        if mark_price_tick < 0 || self.avg_entry_price_tick < 0 {
            return None;
        }
        if let Some(p) = &self.hedge_positions {
            let long = p.long.qty.checked_mul(
                Money::from(mark_price_tick) - Money::from(p.long.avg_entry_price_tick),
            )?;
            let short = p.short.qty.checked_mul(
                Money::from(p.short.avg_entry_price_tick) - Money::from(mark_price_tick),
            )?;
            return long.checked_add(short);
        }
        self.position_qty
            .checked_mul(Money::from(mark_price_tick) - Money::from(self.avg_entry_price_tick))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerpAccountSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hedge_positions: Option<Box<HedgePositions>>,
    pub account_id: AccountId,
    pub cash_balance: Money,
    pub position_qty: PositionQty,
    pub avg_entry_price_tick: PriceTick,
    pub realized_pnl: Money,
    pub unrealized_pnl: Money,
    pub equity: Money,
    pub initial_margin: Money,
    #[serde(default)]
    pub maintenance_margin: Money,
    #[serde(default)]
    pub portfolio_initial_margin: Money,
    #[serde(default)]
    pub portfolio_maintenance_margin: Money,
    #[serde(default = "default_perp_margin_status")]
    pub margin_status: PerpMarginStatus,
    #[serde(default)]
    pub reserved_margin: Money,
    #[serde(default)]
    pub available_cash: Money,
    pub fees_paid: Money,
    #[serde(default, with = "crate::funding::json_money")]
    pub funding_pnl: Money,
}

impl PerpAccountSnapshot {
    pub fn has_open_position(&self) -> bool {
        self.hedge_positions
            .as_ref()
            .map_or(self.position_qty != 0, |p| p.gross_qty() != 0)
    }
}

fn default_perp_margin_status() -> PerpMarginStatus {
    PerpMarginStatus::Flat
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerpSocializedLossAllocation {
    pub account_id: AccountId,
    pub loss: Money,
    pub snapshot: PerpAccountSnapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerpAutoDeleveragingAllocation {
    #[serde(default, skip_serializing_if = "PositionSide::is_both")]
    pub position_side: PositionSide,
    pub account_id: AccountId,
    pub position_delta: PositionQty,
    pub price_tick: PriceTick,
    pub qty: Qty,
    pub realized_pnl: Money,
    pub loss: Money,
    pub snapshot: PerpAccountSnapshot,
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PerpClearingEvent {
    FundingSettled {
        account_id: AccountId,
        funding_time_ms: u64,
        rate_ppm: i32,
        cash_delta: Money,
        snapshot: PerpAccountSnapshot,
    },
    TradeSettled {
        trade_id: u64,
        buyer_account_id: AccountId,
        seller_account_id: AccountId,
        price_tick: PriceTick,
        qty: Qty,
        notional: Money,
        buyer_fee: Money,
        seller_fee: Money,
        buyer_realized_pnl: Money,
        seller_realized_pnl: Money,
        buyer: PerpAccountSnapshot,
        seller: PerpAccountSnapshot,
    },
    MarginStatusChanged {
        account_id: AccountId,
        previous_status: PerpMarginStatus,
        new_status: PerpMarginStatus,
        mark_price_tick: PriceTick,
        snapshot: PerpAccountSnapshot,
    },
    LiquidationSettled {
        account_id: AccountId,
        order_id: OrderId,
        liquidation_notional: Money,
        liquidation_fee: Money,
        insurance_fund_payment: Money,
        auto_deleveraging_loss: Money,
        auto_deleveraging_allocations: Vec<PerpAutoDeleveragingAllocation>,
        socialized_loss: Money,
        socialized_loss_allocations: Vec<PerpSocializedLossAllocation>,
        bad_debt: Money,
        insurance_fund_balance: Money,
        snapshot: PerpAccountSnapshot,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PerpAccountStore {
    #[serde(skip)]
    pub(crate) reservation_changes: crate::account::ReservationChanges,
    // Exact successful inputs, not a relaxed risk policy. Every account is
    // still checked; only identical account/order/mark/context inputs reuse work.
    #[serde(skip)]
    sync_checks: SharedMap<AccountId, Arc<MarginSyncCheck>>,
    #[serde(skip)]
    reservation_index: OnceLock<Arc<BTreeMap<AccountId, Vec<OrderId>>>>,
    accounts: SharedMap<AccountId, PerpAccount>,
    #[serde(default)]
    order_reservations: SharedMap<OrderId, PerpOrderReservation>,
    #[serde(default)]
    margin_statuses: SharedMap<AccountId, PerpMarginStatus>,
    #[serde(default)]
    insurance_fund_balance: Money,
    #[serde(default)]
    cross_margin_contexts: SharedMap<AccountId, PerpCrossMarginContext>,
    config: PerpClearingConfig,
    mark_price_tick: PriceTick,
}

/// Account-local inputs needed by peer markets. No portfolio projection or
/// cloned hedge legs: those are only needed by public account snapshots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PerpMarginInputs {
    pub account_id: AccountId,
    pub unrealized_pnl: Money,
    pub initial_margin: Money,
    pub maintenance_margin: Money,
    pub reserved_margin: Money,
    pub position_open: bool,
}

impl PerpMarginInputs {
    pub fn collateral_reservation(self) -> Result<Money, ClearingError> {
        self.initial_margin
            .checked_add(self.reserved_margin)
            .ok_or(ClearingError::BalanceOverflow)
    }
}

#[derive(Clone, Debug)]
struct MarginSyncCheck {
    account: PerpAccount,
    orders: Vec<(OrderId, PerpOrderReservation)>,
    context: PerpCrossMarginContext,
    config: PerpClearingConfig,
    mark: PriceTick,
    snapshot: PerpAccountSnapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct PerpOrderReservation {
    account_id: AccountId,
    /// Legacy snapshots did not persist the order side. Treating a missing side
    /// as both directions keeps restored state conservative until the order is
    /// canceled or amended.
    #[serde(default)]
    side: Option<Side>,
    price_tick: PriceTick,
    qty: Qty,
    reserved_margin: Money,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PerpPendingOrderRisk {
    pub side: Side,
    pub price_tick: PriceTick,
    pub qty: Qty,
    pub fee: Money,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PerpRiskExposure {
    pub equity: Money,
    pub local_required_margin: Money,
    pub required_margin: Money,
    pub available_equity: Money,
    pub max_abs_position_qty: PositionQty,
    buy_notional: Money,
    sell_notional: Money,
    order_fees: Money,
    pub margin_status: PerpMarginStatus,
    pub liquidation_pending: bool,
}

impl PerpRiskExposure {
    pub fn is_strict_reduction_from(self, previous: Self) -> bool {
        let does_not_increase = self.required_margin <= previous.required_margin
            && self.max_abs_position_qty <= previous.max_abs_position_qty
            && self.buy_notional <= previous.buy_notional
            && self.sell_notional <= previous.sell_notional
            && self.order_fees <= previous.order_fees;
        let reduces_something = self.required_margin < previous.required_margin
            || self.max_abs_position_qty < previous.max_abs_position_qty
            || self.buy_notional < previous.buy_notional
            || self.sell_notional < previous.sell_notional
            || self.order_fees < previous.order_fees;
        does_not_increase && reduces_something
    }

    pub fn is_strict_position_reduction_from(self, previous: Self) -> bool {
        let does_not_increase = self.max_abs_position_qty <= previous.max_abs_position_qty
            && self.buy_notional <= previous.buy_notional
            && self.sell_notional <= previous.sell_notional;
        let reduces_something = self.max_abs_position_qty < previous.max_abs_position_qty
            || self.buy_notional < previous.buy_notional
            || self.sell_notional < previous.sell_notional;
        does_not_increase && reduces_something
    }
}

#[derive(Clone, Copy, Debug)]
struct PerpRiskOrderChunk {
    side: Option<Side>,
    price_tick: PriceTick,
    qty: Qty,
    fee: Money,
}

impl PerpAccountStore {
    pub fn new(
        config: PerpClearingConfig,
        initial_mark_price_tick: PriceTick,
    ) -> Result<Self, ClearingError> {
        if config.leverage == 0 {
            return Err(ClearingError::InvalidLeverage);
        }
        if config.maintenance_margin_ppm > 1_000_000 {
            return Err(ClearingError::InvalidMarginRate);
        }
        if config.liquidation_fee_ppm > 1_000_000 {
            return Err(ClearingError::InvalidFeeRate);
        }
        if config.maker_fee_ppm > 1_000_000 || config.taker_fee_ppm > 1_000_000 {
            return Err(ClearingError::InvalidFeeRate);
        }
        if initial_mark_price_tick <= 0 {
            return Err(ClearingError::InvalidPrice);
        }

        Ok(Self {
            reservation_changes: crate::account::ReservationChanges::default(),
            accounts: SharedMap::default(),
            order_reservations: SharedMap::default(),
            reservation_index: OnceLock::new(),
            sync_checks: SharedMap::default(),
            margin_statuses: SharedMap::default(),
            insurance_fund_balance: config.initial_insurance_fund,
            cross_margin_contexts: SharedMap::default(),
            config,
            mark_price_tick: initial_mark_price_tick,
        })
    }

    pub fn create_account(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
    ) -> PerpAccountSnapshot {
        self.reservation_changes.mark(account_id);
        let account = self.accounts.entry(account_id).or_insert(PerpAccount {
            hedge_positions: (self.config.position_mode == PositionMode::Hedge)
                .then(|| Box::new(HedgePositions::default())),
            account_id,
            cash_balance: 0,
            position_qty: 0,
            avg_entry_price_tick: 0,
            realized_pnl: 0,
            fees_paid: 0,
            funding_pnl: 0,
            reserved_margin: 0,
        });
        account.cash_balance = cash_balance;
        let snapshot = self
            .account_snapshot(account_id)
            .expect("newly created perp account should exist");
        self.margin_statuses
            .insert(account_id, snapshot.margin_status);
        snapshot
    }

    pub fn mark_price_tick(&self) -> PriceTick {
        self.mark_price_tick
    }

    pub(crate) fn settle_funding(
        &mut self,
        settlement: &mut crate::FundingSettlement,
    ) -> Result<Vec<PerpClearingEvent>, ClearingError> {
        if settlement.mark_price_tick != self.mark_price_tick {
            return Err(ClearingError::InvalidPrice);
        }
        self.reservation_changes.invalidate();
        let leg_allocations =
            crate::funding::funding_leg_allocations(&self.snapshots(), settlement)?;
        let mut allocations = BTreeMap::<AccountId, Money>::new();
        for ((id, side), delta) in leg_allocations {
            if let Some(positions) = self
                .accounts
                .get_mut(&id)
                .and_then(|a| a.hedge_positions.as_mut())
            {
                let leg = positions
                    .leg_mut(side)
                    .ok_or(ClearingError::WrongMarketKind)?;
                leg.funding_pnl = leg
                    .funding_pnl
                    .checked_add(delta)
                    .ok_or(ClearingError::BalanceOverflow)?;
            }
            let total = allocations.entry(id).or_default();
            *total = total
                .checked_add(delta)
                .ok_or(ClearingError::BalanceOverflow)?;
        }
        for (id, delta) in &allocations {
            let account = self
                .accounts
                .get_mut(id)
                .ok_or(ClearingError::AccountNotFound)?;
            account.cash_balance = account
                .cash_balance
                .checked_add(*delta)
                .ok_or(ClearingError::BalanceOverflow)?;
            account.funding_pnl = account
                .funding_pnl
                .checked_add(*delta)
                .ok_or(ClearingError::BalanceOverflow)?;
        }
        self.refresh_all_reserved_margins()?;
        let mut events = self.refresh_all_margin_statuses();
        for (id, delta) in allocations {
            events.push(PerpClearingEvent::FundingSettled {
                account_id: id,
                funding_time_ms: settlement.funding_time_ms,
                rate_ppm: settlement.rate_ppm,
                cash_delta: delta,
                snapshot: self
                    .account_snapshot(id)
                    .ok_or(ClearingError::AccountNotFound)?,
            });
        }
        Ok(events)
    }

    pub fn sync_cash_balance(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
    ) -> Result<PerpAccountSnapshot, ClearingError> {
        self.sync_cross_margin_account(account_id, cash_balance, PerpCrossMarginContext::default())
    }

    pub fn sync_cross_margin_account(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
        context: PerpCrossMarginContext,
    ) -> Result<PerpAccountSnapshot, ClearingError> {
        self.sync_cross_margin_account_ref(account_id, cash_balance, context)
            .cloned()
    }

    pub(crate) fn sync_cross_margin_account_in_place(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
        context: PerpCrossMarginContext,
    ) -> Result<(), ClearingError> {
        self.sync_cross_margin_account_ref(account_id, cash_balance, context)
            .map(|_| ())
    }

    pub(crate) fn sync_cross_margin_accounts(
        &mut self,
        requests: &[(AccountId, Money, PerpCrossMarginContext)],
    ) -> Result<(), ClearingError> {
        // The actor submits each account once in ascending order. Keep scalar
        // semantics for any other caller, including duplicate account updates.
        if !requests.windows(2).all(|pair| pair[0].0 < pair[1].0) {
            for &(id, cash, context) in requests {
                self.sync_cross_margin_account_in_place(id, cash, context)?;
            }
            return Ok(());
        }
        fn next_value<'a, T>(
            cursor: &mut std::iter::Peekable<std::collections::btree_map::Iter<'a, AccountId, T>>,
            id: AccountId,
        ) -> Option<&'a T> {
            while cursor.peek().is_some_and(|(key, _)| **key < id) {
                cursor.next();
            }
            if cursor.peek().is_some_and(|(key, _)| **key == id) {
                cursor.next().map(|(_, value)| value)
            } else {
                None
            }
        }
        // Small affected-account batches are faster with indexed lookups than
        // with a scan from the start of every account table.
        if requests.len() < self.accounts.len() / 4 {
            for &(id, cash, context) in requests {
                self.sync_cross_margin_account_in_place(id, cash, context)?;
            }
            return Ok(());
        }
        let misses = {
            let mut accounts = self.accounts.iter().peekable();
            let mut checks = self.sync_checks.iter().peekable();
            let mut contexts = self.cross_margin_contexts.iter().peekable();
            let mut statuses = self.margin_statuses.iter().peekable();
            let mut misses = Vec::new();
            for (index, &(id, cash, context)) in requests.iter().enumerate() {
                let account = next_value(&mut accounts, id);
                let check = next_value(&mut checks, id);
                let current_context = next_value(&mut contexts, id);
                let current_status = next_value(&mut statuses, id);
                let unchanged = check.is_some_and(|check| {
                    check.account.cash_balance == cash
                        && account == Some(&check.account)
                        && check.context == context
                        && check.config == self.config
                        && check.mark == self.mark_price_tick
                        && current_context == Some(&context)
                        && current_status == Some(&check.snapshot.margin_status)
                        && self
                            .reservations_for(id)
                            .eq(check.orders.iter().map(|(id, order)| (id, order)))
                });
                if !unchanged {
                    misses.push(index);
                }
            }
            misses
        };
        // Synchronization only mutates the requested account and its own risk
        // context/reserved margin. Earlier misses cannot invalidate another
        // account's exact successful check. Errors preserve scalar order.
        for index in misses {
            let (id, cash, context) = requests[index];
            self.sync_cross_margin_account_in_place(id, cash, context)?;
        }
        Ok(())
    }

    fn sync_cross_margin_account_ref(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
        context: PerpCrossMarginContext,
    ) -> Result<&PerpAccountSnapshot, ClearingError> {
        if let Some(check) = self.sync_checks.get(&account_id)
            && check.account.cash_balance == cash_balance
            && self.accounts.get(&account_id) == Some(&check.account)
            && check.context == context
            && check.config == self.config
            && check.mark == self.mark_price_tick
            && self.reservations_for(account_id).eq(check
                .orders
                .iter()
                .map(|(id, reservation)| (id, reservation)))
        {
            let status = check.snapshot.margin_status;
            // A previously successful check can survive other context/status
            // writes. Restore them when needed, but avoid rewriting equal data.
            if self.cross_margin_contexts.get(&account_id) != Some(&context) {
                self.cross_margin_contexts.insert(account_id, context);
            }
            if self.margin_statuses.get(&account_id) != Some(&status) {
                self.margin_statuses.insert(account_id, status);
            }
            return Ok(&self.sync_checks[&account_id].snapshot);
        }
        // Synchronization only changes this account and its context. Preserve
        // those entries for rollback instead of copying every account and order
        // once for each member of a venue-wide cross-margin refresh.
        let previous_account = self.accounts.get(&account_id).cloned();
        let previous_context = self.cross_margin_contexts.get(&account_id).copied();
        if previous_context != Some(context) {
            self.cross_margin_contexts.insert(account_id, context);
        }
        if self
            .accounts
            .get(&account_id)
            .is_none_or(|account| account.cash_balance != cash_balance)
        {
            self.account_mut(account_id).cash_balance = cash_balance;
        }
        let result = (|| {
            self.refresh_reserved_margin_for(account_id)?;
            let account = self
                .account(account_id)
                .ok_or(ClearingError::AccountNotFound)?;
            self.checked_snapshot_with_cross_margin(account)
        })();
        match result {
            Ok(snapshot) => {
                if self.margin_statuses.get(&account_id) != Some(&snapshot.margin_status) {
                    self.margin_statuses
                        .insert(account_id, snapshot.margin_status);
                }
                let check = MarginSyncCheck {
                    account: self.accounts[&account_id].clone(),
                    orders: self
                        .reservations_for(account_id)
                        .map(|(&id, order)| (id, order.clone()))
                        .collect(),
                    context,
                    config: self.config,
                    mark: self.mark_price_tick,
                    snapshot,
                };
                self.sync_checks.insert(account_id, Arc::new(check));
                Ok(&self.sync_checks[&account_id].snapshot)
            }
            Err(error) => {
                if let Some(account) = previous_account {
                    self.accounts.insert(account_id, account);
                } else {
                    self.accounts.remove(&account_id);
                }
                if let Some(context) = previous_context {
                    self.cross_margin_contexts.insert(account_id, context);
                } else {
                    self.cross_margin_contexts.remove(&account_id);
                }
                Err(error)
            }
        }
    }

    pub fn insurance_fund_balance(&self) -> Money {
        self.insurance_fund_balance
    }

    pub fn set_mark_price_tick(
        &mut self,
        mark_price_tick: PriceTick,
    ) -> Result<Vec<PerpClearingEvent>, ClearingError> {
        if mark_price_tick <= 0 {
            return Err(ClearingError::InvalidPrice);
        }
        self.reservation_changes.invalidate();
        self.mark_price_tick = mark_price_tick;
        self.refresh_all_reserved_margins()?;
        Ok(self.refresh_all_margin_statuses())
    }

    pub fn account(&self, account_id: AccountId) -> Option<&PerpAccount> {
        self.accounts.get(&account_id)
    }

    pub fn config(&self) -> PerpClearingConfig {
        self.config
    }

    pub fn account_snapshot(&self, account_id: AccountId) -> Option<PerpAccountSnapshot> {
        self.account(account_id)
            .map(|account| self.snapshot_with_cross_margin(account))
    }

    pub fn snapshots(&self) -> Vec<PerpAccountSnapshot> {
        self.accounts
            .values()
            .map(|account| self.snapshot_with_cross_margin(account))
            .collect()
    }

    pub(crate) fn margin_inputs(
        &self,
        affected: Option<&std::collections::BTreeSet<AccountId>>,
    ) -> Vec<PerpMarginInputs> {
        let project = |account: &PerpAccount| {
            let gross_qty = account
                .hedge_positions
                .as_ref()
                .map_or(account.position_qty, |p| p.gross_qty());
            PerpMarginInputs {
                account_id: account.account_id,
                unrealized_pnl: account
                    .unrealized_pnl_at_mark(self.mark_price_tick)
                    .unwrap_or(0),
                initial_margin: account.initial_margin(self.config),
                maintenance_margin: maintenance_margin(
                    gross_qty,
                    self.mark_price_tick,
                    self.config,
                ),
                reserved_margin: account.reserved_margin,
                position_open: account.has_open_position(),
            }
        };
        if let Some(ids) = affected {
            ids.iter()
                .filter_map(|id| self.accounts.get(id))
                .map(project)
                .collect()
        } else {
            self.accounts.values().map(project).collect()
        }
    }

    pub(crate) fn reservation_balances(
        &self,
    ) -> impl Iterator<Item = Result<(AccountId, Money), ClearingError>> + '_ {
        self.accounts.values().map(|account| {
            account
                .initial_margin(self.config)
                .checked_add(account.reserved_margin)
                .map(|amount| (account.account_id, amount))
                .ok_or(ClearingError::BalanceOverflow)
        })
    }

    pub(crate) fn open_position_accounts(&self) -> impl Iterator<Item = AccountId> + '_ {
        self.accounts
            .values()
            .filter(|account| account.has_open_position())
            .map(|account| account.account_id)
    }

    pub fn cross_margin_context(&self, account_id: AccountId) -> PerpCrossMarginContext {
        self.cross_margin_contexts
            .get(&account_id)
            .copied()
            .unwrap_or_default()
    }

    fn snapshot_with_cross_margin(&self, account: &PerpAccount) -> PerpAccountSnapshot {
        let mut snapshot = account.snapshot(self.config, self.mark_price_tick);
        let context = self.cross_margin_context(account.account_id);
        // Snapshots are an infallible projection API. The mutation/risk paths
        // use `checked_snapshot_with_cross_margin` and reject overflow; this
        // fallback only keeps inspection of corrupt/extreme restored state
        // total and visibly clamps at the numeric boundary.
        let portfolio_equity = snapshot.equity.saturating_add(context.other_unrealized_pnl);
        let portfolio_initial_margin = snapshot
            .initial_margin
            .saturating_add(context.other_initial_margin);
        let portfolio_maintenance_margin = snapshot
            .maintenance_margin
            .saturating_add(context.other_maintenance_margin);
        let local_required_margin = snapshot
            .initial_margin
            .saturating_add(snapshot.reserved_margin);
        snapshot.equity = portfolio_equity;
        snapshot.portfolio_initial_margin = portfolio_initial_margin;
        snapshot.portfolio_maintenance_margin = portfolio_maintenance_margin;
        snapshot.available_cash = portfolio_equity
            .saturating_sub(local_required_margin)
            .saturating_sub(context.other_required_margin);
        snapshot.margin_status = margin_status_for_portfolio(
            snapshot.has_open_position() || context.other_position_open,
            portfolio_equity,
            portfolio_initial_margin,
            portfolio_maintenance_margin,
        );
        snapshot
    }

    fn checked_snapshot_with_cross_margin(
        &self,
        account: &PerpAccount,
    ) -> Result<PerpAccountSnapshot, ClearingError> {
        let mut snapshot = account.snapshot(self.config, self.mark_price_tick);
        let context = self.cross_margin_context(account.account_id);
        let portfolio_equity = snapshot
            .equity
            .checked_add(context.other_unrealized_pnl)
            .ok_or(ClearingError::BalanceOverflow)?;
        let portfolio_initial_margin = snapshot
            .initial_margin
            .checked_add(context.other_initial_margin)
            .ok_or(ClearingError::BalanceOverflow)?;
        let portfolio_maintenance_margin = snapshot
            .maintenance_margin
            .checked_add(context.other_maintenance_margin)
            .ok_or(ClearingError::BalanceOverflow)?;
        let local_required_margin = snapshot
            .initial_margin
            .checked_add(snapshot.reserved_margin)
            .ok_or(ClearingError::BalanceOverflow)?;
        let required_margin = local_required_margin
            .checked_add(context.other_required_margin)
            .ok_or(ClearingError::BalanceOverflow)?;
        snapshot.equity = portfolio_equity;
        snapshot.portfolio_initial_margin = portfolio_initial_margin;
        snapshot.portfolio_maintenance_margin = portfolio_maintenance_margin;
        snapshot.available_cash = portfolio_equity
            .checked_sub(required_margin)
            .ok_or(ClearingError::BalanceOverflow)?;
        snapshot.margin_status = margin_status_for_portfolio(
            snapshot.has_open_position() || context.other_position_open,
            portfolio_equity,
            portfolio_initial_margin,
            portfolio_maintenance_margin,
        );
        Ok(snapshot)
    }

    pub fn reserve_resting_order(
        &mut self,
        order_id: OrderId,
        account_id: AccountId,
        side: Side,
        price_tick: PriceTick,
        qty: Qty,
    ) -> Result<(), ClearingError> {
        if qty == 0 {
            return Ok(());
        }
        self.release_order_reservation(order_id)?;

        let reservation = self.reservation_for_order(account_id, Some(side), price_tick, qty)?;
        self.reservation_index.take();
        self.order_reservations.insert(order_id, reservation);
        self.refresh_reserved_margin_for(account_id)?;
        Ok(())
    }

    pub fn release_order_reservation(&mut self, order_id: OrderId) -> Result<(), ClearingError> {
        self.reservation_index.take();
        let Some(reservation) = self.order_reservations.remove(&order_id) else {
            return Ok(());
        };
        self.refresh_reserved_margin_for(reservation.account_id)?;
        Ok(())
    }

    pub fn amend_order_reservation(
        &mut self,
        order_id: OrderId,
        price_tick: PriceTick,
        qty: Qty,
    ) -> Result<(), ClearingError> {
        let Some(existing) = self.order_reservations.get(&order_id).cloned() else {
            return Ok(());
        };
        let next_reservation =
            self.reservation_for_order(existing.account_id, existing.side, price_tick, qty)?;

        if qty == 0 {
            self.order_reservations.remove(&order_id);
        } else {
            self.order_reservations.insert(order_id, next_reservation);
        }
        self.reservation_index.take();
        self.refresh_reserved_margin_for(existing.account_id)?;
        Ok(())
    }

    pub fn amend_order_requires_available_margin(
        &self,
        order_id: OrderId,
        price_tick: Option<PriceTick>,
        qty: Option<Qty>,
    ) -> Result<bool, ClearingError> {
        let Some((before, after)) = self.amend_order_risk_exposures(order_id, price_tick, qty)?
        else {
            return Ok(true);
        };
        Ok(after.is_strict_reduction_from(before) || after.available_equity >= 0)
    }

    pub(crate) fn risk_exposure(
        &self,
        account_id: AccountId,
    ) -> Result<PerpRiskExposure, ClearingError> {
        self.risk_exposure_with(account_id, None, None)
    }

    pub(crate) fn projected_order_risk(
        &self,
        account_id: AccountId,
        order: PerpPendingOrderRisk,
    ) -> Result<PerpRiskExposure, ClearingError> {
        self.risk_exposure_with(account_id, None, Some(order))
    }

    pub(crate) fn amend_order_risk_exposures(
        &self,
        order_id: OrderId,
        price_tick: Option<PriceTick>,
        qty: Option<Qty>,
    ) -> Result<Option<(PerpRiskExposure, PerpRiskExposure)>, ClearingError> {
        let Some(existing) = self.order_reservations.get(&order_id) else {
            return Ok(None);
        };
        let before = self.risk_exposure(existing.account_id)?;
        let new_price_tick = price_tick.unwrap_or(existing.price_tick);
        let new_qty = qty.unwrap_or(existing.qty);
        let fee = if new_qty == 0 {
            0
        } else {
            fee_for(
                notional(new_price_tick, new_qty)?,
                self.config.maker_fee_ppm,
            )?
        };
        let Some(side) = existing.side else {
            // A legacy reservation without a side is already modeled in both
            // directions. Preserve that conservative state during amendment.
            let mut staged = self.clone();
            staged.reservation_index.take();
            if new_qty == 0 {
                staged.order_reservations.remove(&order_id);
            } else {
                let reservation = staged.reservation_for_order(
                    existing.account_id,
                    None,
                    new_price_tick,
                    new_qty,
                )?;
                staged.order_reservations.insert(order_id, reservation);
            }
            let after = staged.risk_exposure(existing.account_id)?;
            return Ok(Some((before, after)));
        };
        let after = self.risk_exposure_with(
            existing.account_id,
            Some(order_id),
            (new_qty > 0).then_some(PerpPendingOrderRisk {
                side,
                price_tick: new_price_tick,
                qty: new_qty,
                fee,
            }),
        )?;
        Ok(Some((before, after)))
    }

    pub fn settle_trade(&mut self, trade: &Trade) -> Result<Vec<PerpClearingEvent>, ClearingError> {
        let mut staged = self.clone();
        let events = staged.settle_trade_inner(trade)?;
        *self = staged;
        Ok(events)
    }

    fn settle_trade_inner(
        &mut self,
        trade: &Trade,
    ) -> Result<Vec<PerpClearingEvent>, ClearingError> {
        let notional = notional(trade.price_tick, trade.qty)?;
        let maker_fee = fee_for(notional, self.config.maker_fee_ppm)?;
        let taker_fee = fee_for(notional, self.config.taker_fee_ppm)?;
        let participants = PerpTradeParticipants::from_trade(trade, maker_fee, taker_fee);
        let (buyer_side, seller_side) = match trade.taker_side {
            Side::Buy => (trade.taker_position_side, trade.maker_position_side),
            Side::Sell => (trade.maker_position_side, trade.taker_position_side),
        };

        self.release_maker_fill_reservation(trade.maker_order_id, trade.qty)?;

        let mut buyer_result = self.apply_fill(
            participants.buyer_account_id,
            buyer_side,
            PositionQty::from(trade.qty),
            trade.price_tick,
            participants.buyer_fee,
        )?;
        let mut seller_result = self.apply_fill(
            participants.seller_account_id,
            seller_side,
            -PositionQty::from(trade.qty),
            trade.price_tick,
            participants.seller_fee,
        )?;
        self.refresh_reserved_margin_for(participants.buyer_account_id)?;
        if participants.seller_account_id != participants.buyer_account_id {
            self.refresh_reserved_margin_for(participants.seller_account_id)?;
        }
        buyer_result.snapshot = self
            .account_snapshot(participants.buyer_account_id)
            .ok_or(ClearingError::AccountNotFound)?;
        seller_result.snapshot = self
            .account_snapshot(participants.seller_account_id)
            .ok_or(ClearingError::AccountNotFound)?;

        let mut events = vec![PerpClearingEvent::TradeSettled {
            trade_id: trade.trade_id,
            buyer_account_id: participants.buyer_account_id,
            seller_account_id: participants.seller_account_id,
            price_tick: trade.price_tick,
            qty: trade.qty,
            notional,
            buyer_fee: participants.buyer_fee,
            seller_fee: participants.seller_fee,
            buyer_realized_pnl: buyer_result.realized_pnl_delta,
            seller_realized_pnl: seller_result.realized_pnl_delta,
            buyer: buyer_result.snapshot,
            seller: seller_result.snapshot,
        }];

        events.extend(self.refresh_margin_status_for(participants.buyer_account_id, false));
        events.extend(self.refresh_margin_status_for(participants.seller_account_id, false));

        Ok(events)
    }

    pub fn apply_liquidation_settlement(
        &mut self,
        account_id: AccountId,
        order_id: OrderId,
        liquidation_notional: Money,
        liquidated_position_qty: PositionQty,
    ) -> Result<Vec<PerpClearingEvent>, ClearingError> {
        self.apply_liquidation_settlement_for_legs(
            account_id,
            order_id,
            liquidation_notional,
            liquidated_position_qty,
            None,
        )
    }

    pub(crate) fn apply_liquidation_settlement_for_legs(
        &mut self,
        account_id: AccountId,
        order_id: OrderId,
        liquidation_notional: Money,
        liquidated_position_qty: PositionQty,
        hedge_positions: Option<&HedgePositions>,
    ) -> Result<Vec<PerpClearingEvent>, ClearingError> {
        if liquidation_notional == 0 {
            return Ok(Vec::new());
        }

        let liquidation_fee = fee_for(liquidation_notional, self.config.liquidation_fee_ppm)?;
        let mut insurance_fund_payment = 0;
        let mut auto_deleveraging_loss = 0;
        let mut auto_deleveraging_allocations = Vec::new();
        let mut socialized_loss = 0;
        let mut socialized_loss_allocations = Vec::new();
        let mut bad_debt = 0;

        {
            let account = self.account_mut(account_id);
            account.cash_balance -= liquidation_fee;
            account.fees_paid += liquidation_fee;
        }
        self.insurance_fund_balance += liquidation_fee;

        let shortfall = self
            .accounts
            .get(&account_id)
            .filter(|account| !account.has_open_position() && account.cash_balance < 0)
            .map(|account| -account.cash_balance)
            .unwrap_or(0);

        if shortfall > 0 {
            insurance_fund_payment = shortfall.min(self.insurance_fund_balance);
            self.insurance_fund_balance -= insurance_fund_payment;
            let mut remaining_shortfall = shortfall - insurance_fund_payment;

            if self.config.auto_deleveraging_enabled && remaining_shortfall > 0 {
                let sides = hedge_positions.map_or_else(
                    || vec![liquidated_position_qty],
                    |p| vec![p.long.qty, -p.short.qty],
                );
                for side in sides {
                    let remaining = remaining_shortfall
                        - auto_deleveraging_allocations
                            .iter()
                            .map(|a: &PerpAutoDeleveragingAllocation| a.loss)
                            .sum::<Money>();
                    if remaining <= 0 {
                        break;
                    }
                    auto_deleveraging_allocations
                        .extend(self.allocate_auto_deleveraging_loss(account_id, side, remaining)?);
                }
                auto_deleveraging_loss = auto_deleveraging_allocations
                    .iter()
                    .map(|allocation| allocation.loss)
                    .sum();
                remaining_shortfall -= auto_deleveraging_loss;
            }

            if self.config.socialized_loss_enabled && remaining_shortfall > 0 {
                socialized_loss_allocations =
                    self.allocate_socialized_loss(account_id, remaining_shortfall);
                socialized_loss = socialized_loss_allocations
                    .iter()
                    .map(|allocation| allocation.loss)
                    .sum();
            }

            bad_debt = remaining_shortfall - socialized_loss;

            let account = self.account_mut(account_id);
            account.cash_balance +=
                insurance_fund_payment + auto_deleveraging_loss + socialized_loss + bad_debt;
        }

        let snapshot = self
            .account_snapshot(account_id)
            .expect("liquidation settlement account should exist");
        self.margin_statuses
            .insert(account_id, snapshot.margin_status);

        if liquidation_fee == 0
            && insurance_fund_payment == 0
            && auto_deleveraging_loss == 0
            && socialized_loss == 0
            && bad_debt == 0
        {
            return Ok(Vec::new());
        }

        let contributor_account_ids = auto_deleveraging_allocations
            .iter()
            .map(|allocation| allocation.account_id)
            .chain(
                socialized_loss_allocations
                    .iter()
                    .map(|allocation| allocation.account_id),
            )
            .collect::<Vec<_>>();
        let mut events = vec![PerpClearingEvent::LiquidationSettled {
            account_id,
            order_id,
            liquidation_notional,
            liquidation_fee,
            insurance_fund_payment,
            auto_deleveraging_loss,
            auto_deleveraging_allocations,
            socialized_loss,
            socialized_loss_allocations,
            bad_debt,
            insurance_fund_balance: self.insurance_fund_balance,
            snapshot,
        }];
        for contributor_account_id in contributor_account_ids {
            events.extend(self.refresh_margin_status_for(contributor_account_id, true));
        }

        Ok(events)
    }

    fn allocate_auto_deleveraging_loss(
        &mut self,
        liquidated_account_id: AccountId,
        liquidated_position_qty: PositionQty,
        remaining_shortfall: Money,
    ) -> Result<Vec<PerpAutoDeleveragingAllocation>, ClearingError> {
        if liquidated_position_qty == 0 || self.mark_price_tick <= 0 {
            return Ok(Vec::new());
        }

        let mark_price_tick = self.mark_price_tick;
        let liquidated_side = liquidated_position_qty.signum();
        let mut remaining = remaining_shortfall;
        let mut contributors = self
            .accounts
            .iter()
            .filter_map(|(account_id, account)| {
                let (qty, entry, _) = account.position_in_direction(-liquidated_side);
                if *account_id == liquidated_account_id || qty == 0 {
                    return None;
                }
                let unrealized_pnl =
                    qty.checked_mul(Money::from(mark_price_tick) - Money::from(entry))?;
                if unrealized_pnl <= 0 {
                    return None;
                }
                Some((*account_id, unrealized_pnl))
            })
            .collect::<Vec<_>>();
        contributors.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));

        let mut allocations = Vec::new();
        for (account_id, _) in contributors {
            if remaining == 0 {
                break;
            }

            let Some(account) = self.accounts.get(&account_id) else {
                continue;
            };
            let (contributor_position, entry, contributor_side) =
                account.position_in_direction(-liquidated_side);
            let per_contract_profit = match contributor_position.signum() {
                1 => mark_price_tick - entry,
                -1 => entry - mark_price_tick,
                _ => 0,
            };
            if per_contract_profit <= 0 {
                continue;
            }

            let Some((counterparty_id, counterparty_position, counterparty_side)) =
                self.accounts.iter().find_map(|(candidate_id, candidate)| {
                    let (qty, _, side) = candidate.position_in_direction(liquidated_side);
                    (*candidate_id != liquidated_account_id
                        && *candidate_id != account_id
                        && qty != 0)
                        .then_some((*candidate_id, qty, side))
                })
            else {
                continue;
            };

            let max_pair_qty = contributor_position.abs().min(counterparty_position.abs());
            let max_loss = Money::from(per_contract_profit)
                .checked_mul(max_pair_qty)
                .ok_or(ClearingError::BalanceOverflow)?;
            let loss = remaining.min(max_loss);
            let rounded_qty = loss
                .checked_add(Money::from(per_contract_profit) - 1)
                .ok_or(ClearingError::BalanceOverflow)?
                / Money::from(per_contract_profit);
            let qty_to_reduce = Qty::try_from(rounded_qty.min(max_pair_qty))
                .map_err(|_| ClearingError::InvalidLiquidationQuantity)?;
            if qty_to_reduce == 0 {
                continue;
            }
            let position_delta = -contributor_position.signum() * PositionQty::from(qty_to_reduce);
            let counterparty_position_delta =
                -counterparty_position.signum() * PositionQty::from(qty_to_reduce);

            let realized_pnl = {
                let account = self.account_mut(account_id);
                let realized_pnl = apply_selected_position_fill(
                    account,
                    contributor_side,
                    position_delta,
                    mark_price_tick,
                    0,
                )?;
                account.cash_balance = account
                    .cash_balance
                    .checked_add(realized_pnl)
                    .and_then(|balance| balance.checked_sub(loss))
                    .ok_or(ClearingError::BalanceOverflow)?;
                account.realized_pnl = account
                    .realized_pnl
                    .checked_add(realized_pnl)
                    .ok_or(ClearingError::BalanceOverflow)?;
                realized_pnl
            };
            self.refresh_reserved_margin_for(account_id)?;
            let snapshot = self
                .account_snapshot(account_id)
                .ok_or(ClearingError::AccountNotFound)?;
            allocations.push(PerpAutoDeleveragingAllocation {
                position_side: contributor_side,
                account_id,
                position_delta,
                price_tick: mark_price_tick,
                qty: qty_to_reduce,
                realized_pnl,
                loss,
                snapshot,
            });

            let counterparty_realized_pnl = {
                let account = self.account_mut(counterparty_id);
                let realized_pnl = apply_selected_position_fill(
                    account,
                    counterparty_side,
                    counterparty_position_delta,
                    mark_price_tick,
                    0,
                )?;
                account.cash_balance = account
                    .cash_balance
                    .checked_add(realized_pnl)
                    .ok_or(ClearingError::BalanceOverflow)?;
                account.realized_pnl = account
                    .realized_pnl
                    .checked_add(realized_pnl)
                    .ok_or(ClearingError::BalanceOverflow)?;
                realized_pnl
            };
            self.refresh_reserved_margin_for(counterparty_id)?;
            let counterparty_snapshot = self
                .account_snapshot(counterparty_id)
                .ok_or(ClearingError::AccountNotFound)?;
            allocations.push(PerpAutoDeleveragingAllocation {
                position_side: counterparty_side,
                account_id: counterparty_id,
                position_delta: counterparty_position_delta,
                price_tick: mark_price_tick,
                qty: qty_to_reduce,
                realized_pnl: counterparty_realized_pnl,
                loss: 0,
                snapshot: counterparty_snapshot,
            });
            remaining -= loss;
        }

        Ok(allocations)
    }

    fn allocate_socialized_loss(
        &mut self,
        liquidated_account_id: AccountId,
        remaining_shortfall: Money,
    ) -> Vec<PerpSocializedLossAllocation> {
        let mut remaining = remaining_shortfall;
        let mut contributors = self
            .accounts
            .iter()
            .filter_map(|(account_id, account)| {
                if *account_id == liquidated_account_id {
                    return None;
                }
                let snapshot = self.snapshot_with_cross_margin(account);
                let loss_capacity = snapshot.available_cash.min(snapshot.equity).max(0);
                if loss_capacity == 0 {
                    return None;
                }
                Some((*account_id, loss_capacity, snapshot.equity))
            })
            .collect::<Vec<_>>();
        contributors.sort_by(|left, right| right.2.cmp(&left.2).then_with(|| left.0.cmp(&right.0)));

        let mut allocations = Vec::new();
        for (account_id, loss_capacity, _) in contributors {
            if remaining == 0 {
                break;
            }
            let loss = loss_capacity.min(remaining);
            {
                let account = self.account_mut(account_id);
                account.cash_balance -= loss;
            }
            remaining -= loss;

            let snapshot = self
                .account_snapshot(account_id)
                .expect("socialized loss contributor should exist");
            allocations.push(PerpSocializedLossAllocation {
                account_id,
                loss,
                snapshot,
            });
        }

        allocations
    }

    fn apply_fill(
        &mut self,
        account_id: AccountId,
        position_side: PositionSide,
        fill_qty: PositionQty,
        price_tick: PriceTick,
        fee: Money,
    ) -> Result<PerpFillResult, ClearingError> {
        let config = self.config;
        let mark_price_tick = self.mark_price_tick;
        let account = self.account_mut(account_id);

        let realized_pnl_delta =
            apply_selected_position_fill(account, position_side, fill_qty, price_tick, fee)?;
        account.cash_balance = account
            .cash_balance
            .checked_add(realized_pnl_delta)
            .and_then(|balance| balance.checked_sub(fee))
            .ok_or(ClearingError::BalanceOverflow)?;
        account.realized_pnl = account
            .realized_pnl
            .checked_add(realized_pnl_delta)
            .ok_or(ClearingError::BalanceOverflow)?;
        account.fees_paid = account
            .fees_paid
            .checked_add(fee)
            .ok_or(ClearingError::BalanceOverflow)?;

        Ok(PerpFillResult {
            realized_pnl_delta,
            snapshot: account.snapshot(config, mark_price_tick),
        })
    }

    fn release_maker_fill_reservation(
        &mut self,
        order_id: OrderId,
        fill_qty: Qty,
    ) -> Result<(), ClearingError> {
        self.reservation_index.take();
        let Some(mut reservation) = self.order_reservations.remove(&order_id) else {
            return Ok(());
        };

        let remaining_qty = reservation
            .qty
            .checked_sub(fill_qty)
            .ok_or(ClearingError::ReservationUnderflow)?;
        let next_reservation = self.reservation_for_order(
            reservation.account_id,
            reservation.side,
            reservation.price_tick,
            remaining_qty,
        )?;

        if remaining_qty == 0 {
            self.refresh_reserved_margin_for(reservation.account_id)?;
            return Ok(());
        }

        reservation.qty = remaining_qty;
        reservation.reserved_margin = next_reservation.reserved_margin;
        let account_id = next_reservation.account_id;
        self.order_reservations.insert(order_id, reservation);
        self.refresh_reserved_margin_for(account_id)?;
        Ok(())
    }

    fn reservation_for_order(
        &self,
        account_id: AccountId,
        side: Option<Side>,
        price_tick: PriceTick,
        qty: Qty,
    ) -> Result<PerpOrderReservation, ClearingError> {
        let order_notional = if qty == 0 {
            0
        } else {
            notional(price_tick, qty)?
        };
        let reserved_margin = (order_notional / Money::from(self.config.leverage))
            .checked_add(fee_for(order_notional, self.config.maker_fee_ppm)?)
            .ok_or(ClearingError::BalanceOverflow)?;
        Ok(PerpOrderReservation {
            account_id,
            side,
            price_tick,
            qty,
            reserved_margin,
        })
    }

    fn reservations_for(
        &self,
        account_id: AccountId,
    ) -> impl Iterator<Item = (&OrderId, &PerpOrderReservation)> {
        let index = self.reservation_index.get_or_init(|| {
            let mut index: BTreeMap<AccountId, Vec<OrderId>> = BTreeMap::new();
            for (&id, reservation) in self.order_reservations.iter() {
                index.entry(reservation.account_id).or_default().push(id);
            }
            Arc::new(index)
        });
        index
            .get(&account_id)
            .into_iter()
            .flatten()
            .map(|id| (id, &self.order_reservations[id]))
    }

    fn risk_exposure_with(
        &self,
        account_id: AccountId,
        excluded_order_id: Option<OrderId>,
        candidate: Option<PerpPendingOrderRisk>,
    ) -> Result<PerpRiskExposure, ClearingError> {
        let account = self
            .accounts
            .get(&account_id)
            .ok_or(ClearingError::AccountNotFound)?;
        let mut orders = Vec::new();
        for (order_id, reservation) in self.reservations_for(account_id) {
            if Some(*order_id) == excluded_order_id {
                continue;
            }
            let order_notional = notional(reservation.price_tick, reservation.qty)?;
            orders.push(PerpRiskOrderChunk {
                side: reservation.side,
                price_tick: reservation.price_tick,
                qty: reservation.qty,
                fee: fee_for(order_notional, self.config.maker_fee_ppm)?,
            });
        }
        if let Some(candidate) = candidate
            && candidate.qty > 0
        {
            if candidate.price_tick <= 0 || candidate.fee < 0 {
                return Err(ClearingError::InvalidPrice);
            }
            orders.push(PerpRiskOrderChunk {
                side: Some(candidate.side),
                price_tick: candidate.price_tick,
                qty: candidate.qty,
                fee: candidate.fee,
            });
        }

        let (max_abs_position_qty, buy_notional, sell_notional, worst_notional) =
            if let Some(positions) = &account.hedge_positions {
                let mut long_qty = positions.long.qty;
                let mut short_qty = positions.short.qty;
                let mark = Money::from(self.mark_price_tick);
                let mut long_notional = long_qty
                    .checked_mul(mark)
                    .ok_or(ClearingError::BalanceOverflow)?;
                let mut short_notional = short_qty
                    .checked_mul(mark)
                    .ok_or(ClearingError::BalanceOverflow)?;
                for order in &orders {
                    let qty = PositionQty::from(order.qty);
                    let value = qty
                        .checked_mul(Money::from(order.price_tick.max(self.mark_price_tick)))
                        .ok_or(ClearingError::BalanceOverflow)?;
                    if order.side.is_none_or(|side| side == Side::Buy) {
                        long_qty = long_qty
                            .checked_add(qty)
                            .ok_or(ClearingError::BalanceOverflow)?;
                        long_notional = long_notional
                            .checked_add(value)
                            .ok_or(ClearingError::BalanceOverflow)?;
                    }
                    if order.side.is_none_or(|side| side == Side::Sell) {
                        short_qty = short_qty
                            .checked_add(qty)
                            .ok_or(ClearingError::BalanceOverflow)?;
                        short_notional = short_notional
                            .checked_add(value)
                            .ok_or(ClearingError::BalanceOverflow)?;
                    }
                }
                (
                    long_qty
                        .checked_add(short_qty)
                        .ok_or(ClearingError::BalanceOverflow)?,
                    long_notional,
                    short_notional,
                    long_notional
                        .checked_add(short_notional)
                        .ok_or(ClearingError::BalanceOverflow)?,
                )
            } else {
                let (buy_position_qty, buy_notional) = directional_risk_scenario(
                    account.position_qty,
                    self.mark_price_tick,
                    Side::Buy,
                    orders
                        .iter()
                        .copied()
                        .filter(|order| order.side.is_none_or(|side| side == Side::Buy)),
                )?;
                let (sell_position_qty, sell_notional) = directional_risk_scenario(
                    account.position_qty,
                    self.mark_price_tick,
                    Side::Sell,
                    orders
                        .iter()
                        .copied()
                        .filter(|order| order.side.is_none_or(|side| side == Side::Sell)),
                )?;
                let current_abs_position_qty = account
                    .position_qty
                    .checked_abs()
                    .ok_or(ClearingError::BalanceOverflow)?;
                let current_notional = Money::from(self.mark_price_tick)
                    .checked_mul(current_abs_position_qty)
                    .ok_or(ClearingError::BalanceOverflow)?;
                let max_abs_position_qty = current_abs_position_qty
                    .max(
                        buy_position_qty
                            .checked_abs()
                            .ok_or(ClearingError::BalanceOverflow)?,
                    )
                    .max(
                        sell_position_qty
                            .checked_abs()
                            .ok_or(ClearingError::BalanceOverflow)?,
                    );
                let worst_notional = current_notional.max(buy_notional).max(sell_notional);
                (
                    max_abs_position_qty,
                    buy_notional,
                    sell_notional,
                    worst_notional,
                )
            };
        let order_fees = orders.iter().try_fold(0i128, |total, order| {
            total
                .checked_add(order.fee)
                .ok_or(ClearingError::BalanceOverflow)
        })?;
        let local_required_margin = (worst_notional / Money::from(self.config.leverage))
            .checked_add(order_fees)
            .ok_or(ClearingError::BalanceOverflow)?;
        let cross_margin_context = self.cross_margin_context(account_id);
        let required_margin = local_required_margin
            .checked_add(cross_margin_context.other_required_margin)
            .ok_or(ClearingError::BalanceOverflow)?;
        let unrealized_pnl = account
            .unrealized_pnl_at_mark(self.mark_price_tick)
            .ok_or(ClearingError::BalanceOverflow)?;
        let equity = account
            .cash_balance
            .checked_add(unrealized_pnl)
            .and_then(|equity| equity.checked_add(cross_margin_context.other_unrealized_pnl))
            .ok_or(ClearingError::BalanceOverflow)?;
        let available_equity = equity
            .checked_sub(required_margin)
            .ok_or(ClearingError::BalanceOverflow)?;
        let margin_status = self
            .account_snapshot(account_id)
            .ok_or(ClearingError::AccountNotFound)?
            .margin_status;

        Ok(PerpRiskExposure {
            equity,
            local_required_margin,
            required_margin,
            available_equity,
            max_abs_position_qty,
            buy_notional,
            sell_notional,
            order_fees,
            margin_status,
            liquidation_pending: cross_margin_context.liquidation_pending,
        })
    }

    fn refresh_reserved_margin_for(&mut self, account_id: AccountId) -> Result<(), ClearingError> {
        let exposure = self.risk_exposure(account_id)?;
        let account = self
            .accounts
            .get(&account_id)
            .ok_or(ClearingError::AccountNotFound)?;
        let snapshot_initial_margin = account.initial_margin(self.config);
        // ExchangeActor reserves `initial_margin + reserved_margin`. Basing the
        // top-up on the same snapshot initial margin keeps that aggregate at
        // least as large as the mark-aware worst-case requirement.
        let reserved_margin = exposure
            .local_required_margin
            .checked_sub(snapshot_initial_margin)
            .ok_or(ClearingError::BalanceOverflow)?
            .max(0);
        if account.reserved_margin != reserved_margin {
            self.reservation_changes.mark(account_id);
            self.accounts
                .get_mut(&account_id)
                .ok_or(ClearingError::AccountNotFound)?
                .reserved_margin = reserved_margin;
        }
        Ok(())
    }

    fn refresh_all_reserved_margins(&mut self) -> Result<(), ClearingError> {
        let account_ids = self.accounts.keys().copied().collect::<Vec<_>>();
        for account_id in account_ids {
            self.refresh_reserved_margin_for(account_id)?;
        }
        Ok(())
    }

    fn account_mut(&mut self, account_id: AccountId) -> &mut PerpAccount {
        self.reservation_changes.mark(account_id);
        self.accounts.entry(account_id).or_insert(PerpAccount {
            hedge_positions: (self.config.position_mode == PositionMode::Hedge)
                .then(|| Box::new(HedgePositions::default())),
            account_id,
            cash_balance: 0,
            position_qty: 0,
            avg_entry_price_tick: 0,
            realized_pnl: 0,
            fees_paid: 0,
            funding_pnl: 0,
            reserved_margin: 0,
        })
    }

    fn refresh_margin_status_for(
        &mut self,
        account_id: AccountId,
        emit_all_changes: bool,
    ) -> Option<PerpClearingEvent> {
        let snapshot = self.account_snapshot(account_id)?;
        let previous_status = self.margin_statuses.get(&account_id).copied();
        if previous_status != Some(snapshot.margin_status) {
            self.margin_statuses
                .insert(account_id, snapshot.margin_status);
        }
        let previous_status = previous_status.unwrap_or(snapshot.margin_status);
        if previous_status == snapshot.margin_status {
            return None;
        }
        if !emit_all_changes
            && !matches!(
                snapshot.margin_status,
                PerpMarginStatus::MarginCall | PerpMarginStatus::Liquidatable
            )
        {
            return None;
        }

        Some(PerpClearingEvent::MarginStatusChanged {
            account_id,
            previous_status,
            new_status: snapshot.margin_status,
            mark_price_tick: self.mark_price_tick,
            snapshot,
        })
    }

    fn refresh_all_margin_statuses(&mut self) -> Vec<PerpClearingEvent> {
        let account_ids = self.accounts.keys().copied().collect::<Vec<_>>();
        let mut events = Vec::new();

        for account_id in account_ids {
            events.extend(self.refresh_margin_status_for(account_id, true));
        }

        events
    }
}

fn directional_risk_scenario(
    position_qty: PositionQty,
    mark_price_tick: PriceTick,
    side: Side,
    orders: impl IntoIterator<Item = PerpRiskOrderChunk>,
) -> Result<(PositionQty, Money), ClearingError> {
    let mut orders = orders.into_iter().collect::<Vec<_>>();
    let total_order_qty = orders.iter().try_fold(0i128, |total, order| {
        total
            .checked_add(PositionQty::from(order.qty))
            .ok_or(ClearingError::BalanceOverflow)
    })?;
    let position_delta = match side {
        Side::Buy => total_order_qty,
        Side::Sell => total_order_qty
            .checked_neg()
            .ok_or(ClearingError::BalanceOverflow)?,
    };
    let final_position_qty = position_qty
        .checked_add(position_delta)
        .ok_or(ClearingError::BalanceOverflow)?;
    let current_abs_qty = position_qty
        .checked_abs()
        .ok_or(ClearingError::BalanceOverflow)?;
    let current_notional = Money::from(mark_price_tick)
        .checked_mul(current_abs_qty)
        .ok_or(ClearingError::BalanceOverflow)?;

    if total_order_qty == 0 {
        return Ok((position_qty, current_notional));
    }

    let order_direction = match side {
        Side::Buy => 1,
        Side::Sell => -1,
    };
    if position_qty == 0 || position_qty.signum() == order_direction {
        let scenario_notional = orders.iter().try_fold(current_notional, |total, order| {
            let order_notional = notional(order.price_tick, order.qty)?;
            total
                .checked_add(order_notional)
                .ok_or(ClearingError::BalanceOverflow)
        })?;
        return Ok((final_position_qty, scenario_notional));
    }

    if total_order_qty <= current_abs_qty {
        let remaining_qty = current_abs_qty - total_order_qty;
        let scenario_notional = Money::from(mark_price_tick)
            .checked_mul(remaining_qty)
            .ok_or(ClearingError::BalanceOverflow)?;
        return Ok((final_position_qty, scenario_notional));
    }

    // The order in which same-side resting orders fill is not known. Allocate
    // the closing quantity to the cheapest orders and the newly opened
    // exposure to the most expensive orders to obtain the conservative path.
    orders.sort_by_key(|order| Reverse(order.price_tick));
    let mut opening_qty = total_order_qty - current_abs_qty;
    let mut scenario_notional = 0i128;
    for order in orders {
        if opening_qty == 0 {
            break;
        }
        let take_qty = opening_qty.min(PositionQty::from(order.qty));
        let take_qty = Qty::try_from(take_qty).map_err(|_| ClearingError::BalanceOverflow)?;
        scenario_notional = scenario_notional
            .checked_add(notional(order.price_tick, take_qty)?)
            .ok_or(ClearingError::BalanceOverflow)?;
        opening_qty -= PositionQty::from(take_qty);
    }

    Ok((final_position_qty, scenario_notional))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PerpFillResult {
    realized_pnl_delta: Money,
    snapshot: PerpAccountSnapshot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PerpTradeParticipants {
    buyer_account_id: AccountId,
    seller_account_id: AccountId,
    buyer_fee: Money,
    seller_fee: Money,
}

impl PerpTradeParticipants {
    fn from_trade(trade: &Trade, maker_fee: Money, taker_fee: Money) -> Self {
        match trade.taker_side {
            Side::Buy => Self {
                buyer_account_id: trade.taker_account_id,
                seller_account_id: trade.maker_account_id,
                buyer_fee: taker_fee,
                seller_fee: maker_fee,
            },
            Side::Sell => Self {
                buyer_account_id: trade.maker_account_id,
                seller_account_id: trade.taker_account_id,
                buyer_fee: maker_fee,
                seller_fee: taker_fee,
            },
        }
    }
}

fn apply_position_fill(
    account: &mut PerpAccount,
    fill_qty: PositionQty,
    price_tick: PriceTick,
) -> Money {
    if fill_qty == 0 {
        return 0;
    }

    if account.position_qty == 0 || account.position_qty.signum() == fill_qty.signum() {
        open_or_add_position(account, fill_qty, price_tick);
        return 0;
    }

    let closing_qty = account.position_qty.abs().min(fill_qty.abs());
    let position_sign = account.position_qty.signum();
    let realized_pnl =
        closing_qty * Money::from(price_tick - account.avg_entry_price_tick) * position_sign;

    account.position_qty += closing_qty * fill_qty.signum();

    let remaining_open_qty = fill_qty.abs() - closing_qty;
    if account.position_qty == 0 {
        account.avg_entry_price_tick = 0;
    }
    if remaining_open_qty > 0 {
        account.position_qty = remaining_open_qty * fill_qty.signum();
        account.avg_entry_price_tick = price_tick;
    }

    realized_pnl
}

fn apply_selected_position_fill(
    account: &mut PerpAccount,
    position_side: PositionSide,
    fill_qty: PositionQty,
    price_tick: PriceTick,
    fee: Money,
) -> Result<Money, ClearingError> {
    let Some(positions) = account.hedge_positions.as_mut() else {
        if position_side != PositionSide::Both {
            return Err(ClearingError::WrongMarketKind);
        }
        return Ok(apply_position_fill(account, fill_qty, price_tick));
    };
    let leg = positions
        .leg_mut(position_side)
        .ok_or(ClearingError::WrongMarketKind)?;
    let direction = if position_side == PositionSide::Long {
        1
    } else {
        -1
    };
    let delta = fill_qty
        .checked_mul(direction)
        .ok_or(ClearingError::BalanceOverflow)?;
    let next_qty = leg
        .qty
        .checked_add(delta)
        .ok_or(ClearingError::BalanceOverflow)?;
    if next_qty < 0 {
        return Err(ClearingError::InsufficientAvailableBalance);
    }
    let pnl = if delta < 0 {
        (-delta)
            .checked_mul(
                (Money::from(price_tick) - Money::from(leg.avg_entry_price_tick)) * direction,
            )
            .ok_or(ClearingError::BalanceOverflow)?
    } else {
        0
    };
    if delta > 0 {
        let weighted = leg
            .qty
            .checked_mul(Money::from(leg.avg_entry_price_tick))
            .and_then(|old| {
                delta
                    .checked_mul(Money::from(price_tick))
                    .and_then(|new| old.checked_add(new))
            })
            .ok_or(ClearingError::BalanceOverflow)?;
        leg.avg_entry_price_tick =
            PriceTick::try_from(weighted / next_qty).map_err(|_| ClearingError::BalanceOverflow)?;
    } else if next_qty == 0 {
        leg.avg_entry_price_tick = 0;
    }
    leg.qty = next_qty;
    leg.realized_pnl = leg
        .realized_pnl
        .checked_add(pnl)
        .ok_or(ClearingError::BalanceOverflow)?;
    leg.fees_paid = leg
        .fees_paid
        .checked_add(fee)
        .ok_or(ClearingError::BalanceOverflow)?;
    // Check gross size as well as net size so hedges cannot hide overflow.
    positions
        .long
        .qty
        .checked_add(positions.short.qty)
        .ok_or(ClearingError::BalanceOverflow)?;
    account.position_qty = positions
        .long
        .qty
        .checked_sub(positions.short.qty)
        .ok_or(ClearingError::BalanceOverflow)?;
    // A hedge has two entry prices; the legacy scalar must not masquerade as either.
    account.avg_entry_price_tick = 0;
    Ok(pnl)
}

fn open_or_add_position(account: &mut PerpAccount, fill_qty: PositionQty, price_tick: PriceTick) {
    let old_abs_qty = account.position_qty.abs();
    let fill_abs_qty = fill_qty.abs();
    let new_abs_qty = old_abs_qty + fill_abs_qty;

    account.avg_entry_price_tick = if old_abs_qty == 0 {
        price_tick
    } else {
        let weighted_notional = Money::from(account.avg_entry_price_tick) * old_abs_qty
            + Money::from(price_tick) * fill_abs_qty;
        PriceTick::try_from(weighted_notional / new_abs_qty)
            .expect("weighted average entry should fit in PriceTick")
    };
    account.position_qty += fill_qty;
}

fn initial_margin(
    position_qty: PositionQty,
    avg_entry_price_tick: PriceTick,
    config: PerpClearingConfig,
) -> Money {
    if position_qty == 0 || avg_entry_price_tick <= 0 {
        return 0;
    }

    Money::from(avg_entry_price_tick) * position_qty.abs() / Money::from(config.leverage)
}

fn maintenance_margin(
    position_qty: PositionQty,
    mark_price_tick: PriceTick,
    config: PerpClearingConfig,
) -> Money {
    if position_qty == 0 || mark_price_tick <= 0 {
        return 0;
    }

    Money::from(mark_price_tick) * position_qty.abs() * Money::from(config.maintenance_margin_ppm)
        / 1_000_000
}

fn margin_status(
    position_qty: PositionQty,
    equity: Money,
    initial_margin: Money,
    maintenance_margin: Money,
) -> PerpMarginStatus {
    margin_status_for_portfolio(
        position_qty != 0,
        equity,
        initial_margin,
        maintenance_margin,
    )
}

fn margin_status_for_portfolio(
    has_open_position: bool,
    equity: Money,
    initial_margin: Money,
    maintenance_margin: Money,
) -> PerpMarginStatus {
    if !has_open_position {
        return PerpMarginStatus::Flat;
    }
    if equity <= maintenance_margin {
        return PerpMarginStatus::Liquidatable;
    }
    if equity <= initial_margin {
        return PerpMarginStatus::MarginCall;
    }
    PerpMarginStatus::Healthy
}

#[cfg(test)]
mod tests {
    #[test]
    fn cached_margin_sync_matches_cold_checks_after_every_risk_input_change() {
        let mut store = PerpAccountStore::new(PerpClearingConfig::default(), 100).unwrap();
        store.create_account(1, 10_000);
        store.create_account(2, 10_000);
        fn check(store: &mut PerpAccountStore, cash: Money, context: PerpCrossMarginContext) {
            let mut cold: PerpAccountStore =
                serde_json::from_value(serde_json::to_value(&*store).unwrap()).unwrap();
            assert!(cold.sync_checks.is_empty());
            let mut in_place = store.clone();
            let expected = cold.sync_cross_margin_account(1, cash, context);
            assert_eq!(store.sync_cross_margin_account(1, cash, context), expected);
            assert_eq!(
                in_place.sync_cross_margin_account_in_place(1, cash, context),
                expected.map(|_| ())
            );
            assert_eq!(
                serde_json::to_value(&in_place).unwrap(),
                serde_json::to_value(&cold).unwrap()
            );
            assert_eq!(
                serde_json::to_value(&*store).unwrap(),
                serde_json::to_value(cold).unwrap()
            );
        }
        let context = PerpCrossMarginContext::default();
        check(&mut store, 10_000, context);
        let first = store.sync_checks[&1].clone();
        check(&mut store, 10_000, context);
        assert!(Arc::ptr_eq(&first, &store.sync_checks[&1]));
        store
            .reserve_resting_order(10, 1, Side::Buy, 100, 8)
            .unwrap();
        check(&mut store, 10_000, context);
        store.amend_order_reservation(10, 99, 5).unwrap();
        check(&mut store, 10_000, context);
        store.set_mark_price_tick(110).unwrap();
        check(&mut store, 10_000, context);
        store.release_maker_fill_reservation(10, 5).unwrap();
        check(&mut store, 10_000, context);
        store
            .settle_trade(&trade(20, 2, 1, 110, 3, Side::Buy))
            .unwrap();
        check(&mut store, 9_900, context);
        check(
            &mut store,
            9_900,
            PerpCrossMarginContext {
                other_required_margin: 50,
                liquidation_pending: true,
                ..context
            },
        );
        check(
            &mut store,
            i128::MAX,
            PerpCrossMarginContext {
                other_unrealized_pnl: 1,
                ..context
            },
        );
        check(&mut store, 9_900, context);
    }

    #[test]
    fn ordered_batch_sync_matches_scalar_with_misses_errors_and_duplicates() {
        let context = PerpCrossMarginContext::default();
        let requests = (1..=40).map(|id| (id, 10_000, context)).collect::<Vec<_>>();
        let mut store = PerpAccountStore::new(PerpClearingConfig::default(), 100).unwrap();
        for id in 1..=40 {
            store.create_account(id, 10_000);
        }
        fn compare(
            store: &mut PerpAccountStore,
            requests: &[(AccountId, Money, PerpCrossMarginContext)],
        ) {
            let mut scalar = store.clone();
            let expected = requests.iter().try_for_each(|&(id, cash, context)| {
                scalar
                    .sync_cross_margin_account(id, cash, context)
                    .map(|_| ())
            });
            assert_eq!(store.sync_cross_margin_accounts(requests), expected);
            assert_eq!(
                serde_json::to_value(&*store).unwrap(),
                serde_json::to_value(scalar).unwrap()
            );
        }
        compare(&mut store, &requests); // All cold misses.
        compare(&mut store, &requests); // All exact successful checks.
        store
            .reserve_resting_order(1, 3, Side::Buy, 100, 5)
            .unwrap();
        store.cross_margin_contexts.remove(&2);
        store
            .margin_statuses
            .insert(1, PerpMarginStatus::Liquidatable);
        compare(&mut store, &requests); // Mixed misses; restore absent/stale entries.
        let mut changed = requests.clone();
        changed[1].1 = 20_000;
        changed[4].1 = i128::MAX;
        changed[4].2.other_unrealized_pnl = 1;
        compare(&mut store, &changed); // Earlier writes retained, later writes skipped.
        compare(&mut store, &requests);
        compare(&mut store, &[(30, 20_000, context)]); // Sparse indexed fallback.
        compare(
            &mut store,
            &[
                (30, 30_000, context),
                (1, 20_000, context),
                (30, 40_000, context),
            ],
        );
        store.set_mark_price_tick(110).unwrap();
        compare(&mut store, &requests);
        let mut restored: PerpAccountStore =
            serde_json::from_value(serde_json::to_value(&store).unwrap()).unwrap();
        compare(&mut restored, &requests);
    }

    #[test]
    fn compact_margin_inputs_match_full_projection_and_selected_accounts() {
        for mode in [PositionMode::OneWay, PositionMode::Hedge] {
            let mut store = PerpAccountStore::new(
                PerpClearingConfig {
                    position_mode: mode,
                    leverage: 10,
                    ..PerpClearingConfig::default()
                },
                100,
            )
            .unwrap();
            for id in 1..=3 {
                store.create_account(id, 10_000);
            }
            let account = store.accounts.get_mut(&2).unwrap();
            if let Some(legs) = &mut account.hedge_positions {
                legs.long.qty = 4;
                legs.long.avg_entry_price_tick = 95;
                legs.short.qty = 4;
                legs.short.avg_entry_price_tick = 110;
            } else {
                account.position_qty = -4;
                account.avg_entry_price_tick = 110;
            }
            store.reserve_resting_order(1, 2, Side::Buy, 99, 3).unwrap();
            store.cross_margin_contexts.insert(
                2,
                PerpCrossMarginContext {
                    other_unrealized_pnl: -500,
                    other_required_margin: 500,
                    other_initial_margin: 400,
                    other_maintenance_margin: 100,
                    other_position_open: true,
                    liquidation_pending: true,
                },
            );
            for mark in [80, 100, 125] {
                store.set_mark_price_tick(mark).unwrap();
                let restored: PerpAccountStore =
                    serde_json::from_value(serde_json::to_value(&store).unwrap()).unwrap();
                for source in [&store, &restored] {
                    let all = source.margin_inputs(None);
                    assert_eq!(all.len(), 3);
                    for input in &all {
                        let snapshot = source.account_snapshot(input.account_id).unwrap();
                        assert_eq!(
                            (
                                input.unrealized_pnl,
                                input.initial_margin,
                                input.maintenance_margin,
                                input.reserved_margin,
                                input.position_open
                            ),
                            (
                                snapshot.unrealized_pnl,
                                snapshot.initial_margin,
                                snapshot.maintenance_margin,
                                snapshot.reserved_margin,
                                snapshot.has_open_position()
                            )
                        );
                        assert_eq!(
                            input.collateral_reservation().unwrap(),
                            snapshot.initial_margin + snapshot.reserved_margin
                        );
                    }
                    assert!(all[1].position_open); // Includes zero-net hedge exposure.
                    let ids = std::collections::BTreeSet::from([2, 99]);
                    assert_eq!(source.margin_inputs(Some(&ids)), vec![all[1]]);
                    assert!(
                        source
                            .margin_inputs(Some(&std::collections::BTreeSet::new()))
                            .is_empty()
                    );
                }
            }
        }
    }

    #[test]
    fn reservation_lookup_tracks_mutations_and_old_snapshot_restore() {
        let mut accounts = PerpAccountStore::new(PerpClearingConfig::default(), 100).unwrap();
        for id in 1..=20 {
            accounts.create_account(id, 100_000);
            accounts
                .reserve_resting_order(id, id, Side::Buy, 100, 10)
                .unwrap();
        }
        // Compare the lazy index with a checkpoint rebuild after every mutation,
        // including partial/full maker fills and legacy side-less reservations.
        fn verify(accounts: &PerpAccountStore) {
            let json = serde_json::to_value(accounts).unwrap();
            assert!(json.get("reservation_index").is_none());
            let restored: PerpAccountStore = serde_json::from_value(json).unwrap();
            for id in 1..=20 {
                assert_eq!(accounts.risk_exposure(id), restored.risk_exposure(id));
                let expected: Vec<_> = accounts
                    .order_reservations
                    .iter()
                    .filter(|(_, r)| r.account_id == id)
                    .map(|(&id, _)| id)
                    .collect();
                assert_eq!(
                    accounts
                        .reservation_index
                        .get()
                        .unwrap()
                        .get(&id)
                        .cloned()
                        .unwrap_or_default(),
                    expected
                );
            }
        }
        verify(&accounts);
        let frozen = accounts.clone();
        accounts.amend_order_reservation(1, 99, 7).unwrap();
        verify(&accounts);
        accounts.release_maker_fill_reservation(1, 3).unwrap();
        verify(&accounts);
        accounts.release_maker_fill_reservation(1, 4).unwrap();
        verify(&accounts);
        accounts.release_order_reservation(2).unwrap();
        verify(&accounts);
        accounts.amend_order_reservation(3, 100, 0).unwrap();
        verify(&accounts);
        verify(&frozen);
        let mut json = serde_json::to_value(&accounts).unwrap();
        json["order_reservations"]["4"]
            .as_object_mut()
            .unwrap()
            .remove("side");
        let legacy: PerpAccountStore = serde_json::from_value(json).unwrap();
        legacy.risk_exposure(4).unwrap();
        let (before, after) = legacy
            .amend_order_risk_exposures(4, None, Some(0))
            .unwrap()
            .unwrap();
        assert_ne!(before, after);
        verify(&legacy);
    }

    use super::*;

    #[test]
    fn unchanged_margin_refresh_keeps_shared_tables_and_detaches_real_changes() {
        let mut accounts = PerpAccountStore::new(PerpClearingConfig::default(), 100).unwrap();
        accounts.create_account(10, 10_000);
        accounts
            .sync_cross_margin_account(10, 10_000, PerpCrossMarginContext::default())
            .unwrap();
        let frozen = accounts.clone();
        let before = serde_json::to_value(&frozen).unwrap();
        accounts.refresh_reserved_margin_for(10).unwrap();
        assert!(accounts.refresh_margin_status_for(10, true).is_none());
        assert!(accounts.accounts.shares_storage(&frozen.accounts));
        assert!(
            accounts
                .margin_statuses
                .shares_storage(&frozen.margin_statuses)
        );
        accounts
            .sync_cross_margin_account(10, 9_999, PerpCrossMarginContext::default())
            .unwrap();
        assert!(!accounts.accounts.shares_storage(&frozen.accounts));
        assert!(
            accounts
                .cross_margin_contexts
                .shares_storage(&frozen.cross_margin_contexts)
        );
        assert!(
            accounts
                .margin_statuses
                .shares_storage(&frozen.margin_statuses)
        );
        assert!(!accounts.sync_checks.shares_storage(&frozen.sync_checks));
        assert_eq!(serde_json::to_value(&frozen).unwrap(), before);
        let restored: PerpAccountStore = serde_json::from_value(before.clone()).unwrap();
        assert_eq!(serde_json::to_value(&restored).unwrap(), before);
    }

    fn trade(
        trade_id: u64,
        maker_account_id: AccountId,
        taker_account_id: AccountId,
        price_tick: PriceTick,
        qty: Qty,
        taker_side: Side,
    ) -> Trade {
        Trade {
            maker_position_side: Default::default(),
            taker_position_side: Default::default(),
            trade_id,
            maker_order_id: trade_id * 10,
            maker_account_id,
            taker_order_id: trade_id * 10 + 1,
            taker_account_id,
            price_tick,
            qty,
            taker_side,
        }
    }

    #[test]
    fn buy_opens_long_and_sell_opens_short_without_notional_cash_transfer() {
        let mut accounts = PerpAccountStore::new(
            PerpClearingConfig {
                maker_fee_ppm: 0,
                taker_fee_ppm: 1_000,
                leverage: 10,
                ..PerpClearingConfig::default()
            },
            100,
        )
        .expect("valid config");

        accounts.create_account(10, 10_000);
        accounts.create_account(20, 10_000);
        accounts
            .settle_trade(&trade(1, 10, 20, 100, 10, Side::Buy))
            .expect("trade should settle");

        assert_eq!(
            accounts.account_snapshot(20),
            Some(PerpAccountSnapshot {
                hedge_positions: None,
                account_id: 20,
                cash_balance: 9_999,
                position_qty: 10,
                avg_entry_price_tick: 100,
                realized_pnl: 0,
                unrealized_pnl: 0,
                equity: 9_999,
                initial_margin: 100,
                maintenance_margin: 50,
                portfolio_initial_margin: 100,
                portfolio_maintenance_margin: 50,
                margin_status: PerpMarginStatus::Healthy,
                reserved_margin: 0,
                available_cash: 9_899,
                fees_paid: 1,
                funding_pnl: 0,
            })
        );
        assert_eq!(
            accounts.account_snapshot(10),
            Some(PerpAccountSnapshot {
                hedge_positions: None,
                account_id: 10,
                cash_balance: 10_000,
                position_qty: -10,
                avg_entry_price_tick: 100,
                realized_pnl: 0,
                unrealized_pnl: 0,
                equity: 10_000,
                initial_margin: 100,
                maintenance_margin: 50,
                portfolio_initial_margin: 100,
                portfolio_maintenance_margin: 50,
                margin_status: PerpMarginStatus::Healthy,
                reserved_margin: 0,
                available_cash: 9_900,
                fees_paid: 0,
                funding_pnl: 0,
            })
        );
    }

    #[test]
    fn mark_price_changes_unrealized_pnl_for_long_and_short() {
        let mut accounts = PerpAccountStore::new(PerpClearingConfig::default(), 100).unwrap();

        accounts
            .settle_trade(&trade(1, 10, 20, 100, 10, Side::Buy))
            .unwrap();
        accounts.set_mark_price_tick(110).unwrap();

        assert_eq!(accounts.account_snapshot(20).unwrap().unrealized_pnl, 100);
        assert_eq!(accounts.account_snapshot(10).unwrap().unrealized_pnl, -100);
    }

    #[test]
    fn opposite_fill_realizes_pnl_and_reduces_position() {
        let mut accounts = PerpAccountStore::new(PerpClearingConfig::default(), 120).unwrap();

        accounts
            .settle_trade(&trade(1, 10, 20, 100, 10, Side::Buy))
            .unwrap();
        accounts
            .settle_trade(&trade(2, 20, 30, 120, 4, Side::Buy))
            .unwrap();

        assert_eq!(
            accounts.account_snapshot(20),
            Some(PerpAccountSnapshot {
                hedge_positions: None,
                account_id: 20,
                cash_balance: 80,
                position_qty: 6,
                avg_entry_price_tick: 100,
                realized_pnl: 80,
                unrealized_pnl: 120,
                equity: 200,
                initial_margin: 600,
                maintenance_margin: 36,
                portfolio_initial_margin: 600,
                portfolio_maintenance_margin: 36,
                margin_status: PerpMarginStatus::MarginCall,
                reserved_margin: 120,
                available_cash: -520,
                fees_paid: 0,
                funding_pnl: 0,
            })
        );
    }

    #[test]
    fn reversal_realizes_old_position_and_opens_new_side() {
        let mut accounts = PerpAccountStore::new(PerpClearingConfig::default(), 90).unwrap();

        accounts
            .settle_trade(&trade(1, 10, 20, 100, 5, Side::Buy))
            .unwrap();
        accounts
            .settle_trade(&trade(2, 20, 30, 90, 8, Side::Buy))
            .unwrap();

        assert_eq!(
            accounts.account_snapshot(20),
            Some(PerpAccountSnapshot {
                hedge_positions: None,
                account_id: 20,
                cash_balance: -50,
                position_qty: -3,
                avg_entry_price_tick: 90,
                realized_pnl: -50,
                unrealized_pnl: 0,
                equity: -50,
                initial_margin: 270,
                maintenance_margin: 13,
                portfolio_initial_margin: 270,
                portfolio_maintenance_margin: 13,
                margin_status: PerpMarginStatus::Liquidatable,
                reserved_margin: 0,
                available_cash: -320,
                fees_paid: 0,
                funding_pnl: 0,
            })
        );
    }

    #[test]
    fn margin_status_tracks_initial_and_maintenance_thresholds() {
        let mut accounts = PerpAccountStore::new(
            PerpClearingConfig {
                leverage: 10,
                maintenance_margin_ppm: 50_000,
                ..PerpClearingConfig::default()
            },
            100,
        )
        .unwrap();

        accounts.create_account(20, 200);
        accounts
            .settle_trade(&trade(1, 10, 20, 100, 10, Side::Buy))
            .unwrap();
        assert_eq!(
            accounts.account_snapshot(20).unwrap().margin_status,
            PerpMarginStatus::Healthy
        );

        accounts.set_mark_price_tick(90).unwrap();
        let margin_call = accounts.account_snapshot(20).unwrap();
        assert_eq!(margin_call.equity, 100);
        assert_eq!(margin_call.initial_margin, 100);
        assert_eq!(margin_call.maintenance_margin, 45);
        assert_eq!(margin_call.margin_status, PerpMarginStatus::MarginCall);

        accounts.set_mark_price_tick(80).unwrap();
        let liquidatable = accounts.account_snapshot(20).unwrap();
        assert_eq!(liquidatable.equity, 0);
        assert_eq!(liquidatable.maintenance_margin, 40);
        assert_eq!(liquidatable.margin_status, PerpMarginStatus::Liquidatable);
    }

    #[test]
    fn cross_margin_sync_rejects_overflow_without_mutating_account() {
        let mut accounts = PerpAccountStore::new(PerpClearingConfig::default(), 100).unwrap();
        accounts.create_account(20, 1);
        accounts.create_account(30, 1000);
        let before = serde_json::to_value(&accounts).unwrap();

        assert_eq!(
            accounts.sync_cross_margin_account(
                20,
                Money::MAX,
                PerpCrossMarginContext {
                    other_unrealized_pnl: 1,
                    ..PerpCrossMarginContext::default()
                },
            ),
            Err(ClearingError::BalanceOverflow)
        );
        assert_eq!(accounts.account_snapshot(20).unwrap().cash_balance, 1);
        assert_eq!(
            accounts.cross_margin_context(20),
            PerpCrossMarginContext::default()
        );
        assert_eq!(serde_json::to_value(&accounts).unwrap(), before);
        // A failed synchronization must also remove entries created for an
        // account that did not exist before the transaction.
        assert_eq!(
            accounts.sync_cross_margin_account(
                99,
                Money::MAX,
                PerpCrossMarginContext {
                    other_unrealized_pnl: 1,
                    ..PerpCrossMarginContext::default()
                }
            ),
            Err(ClearingError::BalanceOverflow)
        );
        assert_eq!(serde_json::to_value(&accounts).unwrap(), before);
    }

    #[test]
    fn rejects_invalid_maintenance_margin_rate() {
        assert!(matches!(
            PerpAccountStore::new(
                PerpClearingConfig {
                    maintenance_margin_ppm: 1_000_001,
                    ..PerpClearingConfig::default()
                },
                100,
            ),
            Err(ClearingError::InvalidMarginRate)
        ));
    }
}
