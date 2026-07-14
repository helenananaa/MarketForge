use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    account::{ClearingError, FeeRatePpm, Money, PositionQty, fee_for, notional},
    model::{AccountId, OrderId, PriceTick, Qty, Side, Trade},
};

pub const DEFAULT_MAINTENANCE_MARGIN_PPM: FeeRatePpm = 50_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerpClearingConfig {
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
    pub account_id: AccountId,
    pub cash_balance: Money,
    pub position_qty: PositionQty,
    pub avg_entry_price_tick: PriceTick,
    pub realized_pnl: Money,
    pub fees_paid: Money,
    pub reserved_margin: Money,
}

impl PerpAccount {
    pub fn snapshot(
        &self,
        config: PerpClearingConfig,
        mark_price_tick: PriceTick,
    ) -> PerpAccountSnapshot {
        let unrealized_pnl = self.unrealized_pnl_at_mark(mark_price_tick).unwrap_or(0);
        let equity = self.cash_balance + unrealized_pnl;
        let initial_margin = initial_margin(self.position_qty, self.avg_entry_price_tick, config);
        let maintenance_margin = maintenance_margin(self.position_qty, mark_price_tick, config);
        PerpAccountSnapshot {
            account_id: self.account_id,
            cash_balance: self.cash_balance,
            position_qty: self.position_qty,
            avg_entry_price_tick: self.avg_entry_price_tick,
            realized_pnl: self.realized_pnl,
            unrealized_pnl,
            equity,
            initial_margin,
            maintenance_margin,
            margin_status: margin_status(
                self.position_qty,
                equity,
                initial_margin,
                maintenance_margin,
            ),
            reserved_margin: self.reserved_margin,
            available_cash: self.available_cash(),
            fees_paid: self.fees_paid,
        }
    }

    pub fn available_cash(&self) -> Money {
        self.cash_balance - self.reserved_margin
    }

    pub fn unrealized_pnl_at_mark(&self, mark_price_tick: PriceTick) -> Option<Money> {
        if mark_price_tick < 0 || self.avg_entry_price_tick < 0 {
            return None;
        }
        Some(self.position_qty * Money::from(mark_price_tick - self.avg_entry_price_tick))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerpAccountSnapshot {
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
    #[serde(default = "default_perp_margin_status")]
    pub margin_status: PerpMarginStatus,
    #[serde(default)]
    pub reserved_margin: Money,
    #[serde(default)]
    pub available_cash: Money,
    pub fees_paid: Money,
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
    accounts: BTreeMap<AccountId, PerpAccount>,
    #[serde(default)]
    order_reservations: BTreeMap<OrderId, PerpOrderReservation>,
    #[serde(default)]
    margin_statuses: BTreeMap<AccountId, PerpMarginStatus>,
    #[serde(default)]
    insurance_fund_balance: Money,
    config: PerpClearingConfig,
    mark_price_tick: PriceTick,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct PerpOrderReservation {
    account_id: AccountId,
    price_tick: PriceTick,
    qty: Qty,
    reserved_margin: Money,
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
            accounts: BTreeMap::new(),
            order_reservations: BTreeMap::new(),
            margin_statuses: BTreeMap::new(),
            insurance_fund_balance: config.initial_insurance_fund,
            config,
            mark_price_tick: initial_mark_price_tick,
        })
    }

    pub fn create_account(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
    ) -> PerpAccountSnapshot {
        let account = self.accounts.entry(account_id).or_insert(PerpAccount {
            account_id,
            cash_balance: 0,
            position_qty: 0,
            avg_entry_price_tick: 0,
            realized_pnl: 0,
            fees_paid: 0,
            reserved_margin: 0,
        });
        account.cash_balance = cash_balance;
        account.reserved_margin = 0;
        let snapshot = account.snapshot(self.config, self.mark_price_tick);
        self.margin_statuses
            .insert(account_id, snapshot.margin_status);
        snapshot
    }

    pub fn mark_price_tick(&self) -> PriceTick {
        self.mark_price_tick
    }

    pub fn sync_cash_balance(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
    ) -> Result<PerpAccountSnapshot, ClearingError> {
        let config = self.config;
        let mark_price_tick = self.mark_price_tick;
        let account = self.account_mut(account_id);
        if cash_balance < account.reserved_margin {
            return Err(ClearingError::InsufficientAvailableBalance);
        }
        account.cash_balance = cash_balance;
        Ok(account.snapshot(config, mark_price_tick))
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
        self.mark_price_tick = mark_price_tick;
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
            .map(|account| account.snapshot(self.config, self.mark_price_tick))
    }

    pub fn snapshots(&self) -> Vec<PerpAccountSnapshot> {
        self.accounts
            .values()
            .map(|account| account.snapshot(self.config, self.mark_price_tick))
            .collect()
    }

    pub fn reserve_resting_order(
        &mut self,
        order_id: OrderId,
        account_id: AccountId,
        price_tick: PriceTick,
        qty: Qty,
    ) -> Result<(), ClearingError> {
        if qty == 0 {
            return Ok(());
        }
        self.release_order_reservation(order_id)?;

        let reservation = self.reservation_for_order(account_id, price_tick, qty)?;
        let account = self.account_mut(account_id);
        if account.available_cash() < reservation.reserved_margin {
            return Err(ClearingError::InsufficientAvailableBalance);
        }
        account.reserved_margin = account
            .reserved_margin
            .checked_add(reservation.reserved_margin)
            .ok_or(ClearingError::BalanceOverflow)?;
        self.order_reservations.insert(order_id, reservation);
        Ok(())
    }

    pub fn release_order_reservation(&mut self, order_id: OrderId) -> Result<(), ClearingError> {
        let Some(reservation) = self.order_reservations.remove(&order_id) else {
            return Ok(());
        };
        let account = self.account_mut(reservation.account_id);
        account.reserved_margin = account
            .reserved_margin
            .checked_sub(reservation.reserved_margin)
            .ok_or(ClearingError::ReservationUnderflow)?;
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
        let next_reservation = self.reservation_for_order(existing.account_id, price_tick, qty)?;
        let account = self.account_mut(existing.account_id);

        if next_reservation.reserved_margin >= existing.reserved_margin {
            let additional_margin = next_reservation.reserved_margin - existing.reserved_margin;
            if account.available_cash() < additional_margin {
                return Err(ClearingError::InsufficientAvailableBalance);
            }
            account.reserved_margin = account
                .reserved_margin
                .checked_add(additional_margin)
                .ok_or(ClearingError::BalanceOverflow)?;
        } else {
            let release_margin = existing.reserved_margin - next_reservation.reserved_margin;
            account.reserved_margin = account
                .reserved_margin
                .checked_sub(release_margin)
                .ok_or(ClearingError::ReservationUnderflow)?;
        }

        if qty == 0 {
            self.order_reservations.remove(&order_id);
        } else {
            self.order_reservations.insert(order_id, next_reservation);
        }
        Ok(())
    }

    pub fn amend_order_requires_available_margin(
        &self,
        order_id: OrderId,
        price_tick: Option<PriceTick>,
        qty: Option<Qty>,
    ) -> Result<bool, ClearingError> {
        let Some(existing) = self.order_reservations.get(&order_id) else {
            return Ok(true);
        };
        let new_price_tick = price_tick.unwrap_or(existing.price_tick);
        let new_qty = qty.unwrap_or(existing.qty);
        let next_reservation =
            self.reservation_for_order(existing.account_id, new_price_tick, new_qty)?;

        if next_reservation.reserved_margin <= existing.reserved_margin {
            return Ok(true);
        }

        let additional_margin = next_reservation.reserved_margin - existing.reserved_margin;
        let Some(account) = self.accounts.get(&existing.account_id) else {
            return Ok(false);
        };
        Ok(account.available_cash() >= additional_margin)
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

        self.release_maker_fill_reservation(trade.maker_order_id, trade.qty)?;

        let buyer_result = self.apply_fill(
            participants.buyer_account_id,
            PositionQty::from(trade.qty),
            trade.price_tick,
            participants.buyer_fee,
        )?;
        let seller_result = self.apply_fill(
            participants.seller_account_id,
            -PositionQty::from(trade.qty),
            trade.price_tick,
            participants.seller_fee,
        )?;

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
        if liquidation_notional == 0 {
            return Ok(Vec::new());
        }

        let liquidation_fee = fee_for(liquidation_notional, self.config.liquidation_fee_ppm)?;
        let config = self.config;
        let mark_price_tick = self.mark_price_tick;
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
            .filter(|account| account.position_qty == 0 && account.cash_balance < 0)
            .map(|account| -account.cash_balance)
            .unwrap_or(0);

        if shortfall > 0 {
            insurance_fund_payment = shortfall.min(self.insurance_fund_balance);
            self.insurance_fund_balance -= insurance_fund_payment;
            let mut remaining_shortfall = shortfall - insurance_fund_payment;

            if self.config.auto_deleveraging_enabled && remaining_shortfall > 0 {
                auto_deleveraging_allocations = self.allocate_auto_deleveraging_loss(
                    account_id,
                    liquidated_position_qty,
                    remaining_shortfall,
                )?;
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
            .accounts
            .get(&account_id)
            .expect("liquidation settlement account should exist")
            .snapshot(config, mark_price_tick);
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

        let config = self.config;
        let mark_price_tick = self.mark_price_tick;
        let liquidated_side = liquidated_position_qty.signum();
        let mut remaining = remaining_shortfall;
        let mut contributors = self
            .accounts
            .iter()
            .filter_map(|(account_id, account)| {
                if *account_id == liquidated_account_id
                    || account.position_qty == 0
                    || account.position_qty.signum() == liquidated_side
                {
                    return None;
                }
                let unrealized_pnl = account.unrealized_pnl_at_mark(mark_price_tick)?;
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
            let per_contract_profit = match account.position_qty.signum() {
                1 => mark_price_tick - account.avg_entry_price_tick,
                -1 => account.avg_entry_price_tick - mark_price_tick,
                _ => 0,
            };
            if per_contract_profit <= 0 {
                continue;
            }

            let contributor_position = account.position_qty;
            let Some((counterparty_id, counterparty_position)) =
                self.accounts.iter().find_map(|(candidate_id, candidate)| {
                    (*candidate_id != liquidated_account_id
                        && *candidate_id != account_id
                        && candidate.position_qty.signum() == liquidated_side
                        && candidate.position_qty != 0)
                        .then_some((*candidate_id, candidate.position_qty))
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
                let realized_pnl = apply_position_fill(account, position_delta, mark_price_tick);
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
            let snapshot = self
                .accounts
                .get(&account_id)
                .ok_or(ClearingError::AccountNotFound)?
                .snapshot(config, mark_price_tick);
            allocations.push(PerpAutoDeleveragingAllocation {
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
                let realized_pnl =
                    apply_position_fill(account, counterparty_position_delta, mark_price_tick);
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
            let counterparty_snapshot = self
                .accounts
                .get(&counterparty_id)
                .ok_or(ClearingError::AccountNotFound)?
                .snapshot(config, mark_price_tick);
            allocations.push(PerpAutoDeleveragingAllocation {
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
        let config = self.config;
        let mark_price_tick = self.mark_price_tick;
        let mut remaining = remaining_shortfall;
        let mut contributors = self
            .accounts
            .iter()
            .filter_map(|(account_id, account)| {
                if *account_id == liquidated_account_id {
                    return None;
                }
                let snapshot = account.snapshot(config, mark_price_tick);
                let loss_capacity = account.available_cash().min(snapshot.equity).max(0);
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
                .accounts
                .get(&account_id)
                .expect("socialized loss contributor should exist")
                .snapshot(config, mark_price_tick);
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
        fill_qty: PositionQty,
        price_tick: PriceTick,
        fee: Money,
    ) -> Result<PerpFillResult, ClearingError> {
        let config = self.config;
        let mark_price_tick = self.mark_price_tick;
        let account = self.account_mut(account_id);

        let realized_pnl_delta = apply_position_fill(account, fill_qty, price_tick);
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
        let Some(mut reservation) = self.order_reservations.remove(&order_id) else {
            return Ok(());
        };

        let remaining_qty = reservation
            .qty
            .checked_sub(fill_qty)
            .ok_or(ClearingError::ReservationUnderflow)?;
        let next_reservation = self.reservation_for_order(
            reservation.account_id,
            reservation.price_tick,
            remaining_qty,
        )?;
        let release_margin = reservation
            .reserved_margin
            .checked_sub(next_reservation.reserved_margin)
            .ok_or(ClearingError::ReservationUnderflow)?;

        {
            let account = self.account_mut(reservation.account_id);
            account.reserved_margin = account
                .reserved_margin
                .checked_sub(release_margin)
                .ok_or(ClearingError::ReservationUnderflow)?;
        }

        if remaining_qty == 0 {
            return Ok(());
        }

        reservation.qty = remaining_qty;
        reservation.reserved_margin = next_reservation.reserved_margin;
        self.order_reservations.insert(order_id, reservation);
        Ok(())
    }

    fn reservation_for_order(
        &self,
        account_id: AccountId,
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
            price_tick,
            qty,
            reserved_margin,
        })
    }

    fn account_mut(&mut self, account_id: AccountId) -> &mut PerpAccount {
        self.accounts.entry(account_id).or_insert(PerpAccount {
            account_id,
            cash_balance: 0,
            position_qty: 0,
            avg_entry_price_tick: 0,
            realized_pnl: 0,
            fees_paid: 0,
            reserved_margin: 0,
        })
    }

    fn refresh_margin_status_for(
        &mut self,
        account_id: AccountId,
        emit_all_changes: bool,
    ) -> Option<PerpClearingEvent> {
        let snapshot = self.account_snapshot(account_id)?;
        let previous_status = self
            .margin_statuses
            .insert(account_id, snapshot.margin_status);
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
    if position_qty == 0 {
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
    use super::*;

    fn trade(
        trade_id: u64,
        maker_account_id: AccountId,
        taker_account_id: AccountId,
        price_tick: PriceTick,
        qty: Qty,
        taker_side: Side,
    ) -> Trade {
        Trade {
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
                account_id: 20,
                cash_balance: 9_999,
                position_qty: 10,
                avg_entry_price_tick: 100,
                realized_pnl: 0,
                unrealized_pnl: 0,
                equity: 9_999,
                initial_margin: 100,
                maintenance_margin: 50,
                margin_status: PerpMarginStatus::Healthy,
                reserved_margin: 0,
                available_cash: 9_999,
                fees_paid: 1,
            })
        );
        assert_eq!(
            accounts.account_snapshot(10),
            Some(PerpAccountSnapshot {
                account_id: 10,
                cash_balance: 10_000,
                position_qty: -10,
                avg_entry_price_tick: 100,
                realized_pnl: 0,
                unrealized_pnl: 0,
                equity: 10_000,
                initial_margin: 100,
                maintenance_margin: 50,
                margin_status: PerpMarginStatus::Healthy,
                reserved_margin: 0,
                available_cash: 10_000,
                fees_paid: 0,
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
                account_id: 20,
                cash_balance: 80,
                position_qty: 6,
                avg_entry_price_tick: 100,
                realized_pnl: 80,
                unrealized_pnl: 120,
                equity: 200,
                initial_margin: 600,
                maintenance_margin: 36,
                margin_status: PerpMarginStatus::MarginCall,
                reserved_margin: 0,
                available_cash: 80,
                fees_paid: 0,
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
                account_id: 20,
                cash_balance: -50,
                position_qty: -3,
                avg_entry_price_tick: 90,
                realized_pnl: -50,
                unrealized_pnl: 0,
                equity: -50,
                initial_margin: 270,
                maintenance_margin: 13,
                margin_status: PerpMarginStatus::Liquidatable,
                reserved_margin: 0,
                available_cash: -50,
                fees_paid: 0,
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
