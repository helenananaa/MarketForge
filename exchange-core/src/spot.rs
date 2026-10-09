use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    account::{ClearingError, FeeRatePpm, Money, PositionQty, fee_for, notional},
    model::{AccountId, OrderId, PriceTick, Qty, Side, Trade},
};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SpotClearingConfig {
    pub maker_fee_ppm: FeeRatePpm,
    pub taker_fee_ppm: FeeRatePpm,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SpotAccount {
    pub account_id: AccountId,
    pub cash_balance: Money,
    pub position_qty: PositionQty,
    pub reserved_cash: Money,
    pub reserved_position: PositionQty,
    pub fees_paid: Money,
}

impl SpotAccount {
    pub fn snapshot(&self) -> SpotAccountSnapshot {
        SpotAccountSnapshot {
            account_id: self.account_id,
            cash_balance: self.cash_balance,
            position_qty: self.position_qty,
            reserved_cash: self.reserved_cash,
            reserved_position: self.reserved_position,
            available_cash: self.available_cash(),
            available_position: self.available_position(),
            fees_paid: self.fees_paid,
        }
    }

    pub fn available_cash(&self) -> Money {
        self.cash_balance - self.reserved_cash
    }

    pub fn available_position(&self) -> PositionQty {
        self.position_qty - self.reserved_position
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SpotAccountSnapshot {
    pub account_id: AccountId,
    pub cash_balance: Money,
    pub position_qty: PositionQty,
    #[serde(default)]
    pub reserved_cash: Money,
    #[serde(default)]
    pub reserved_position: PositionQty,
    #[serde(default)]
    pub available_cash: Money,
    #[serde(default)]
    pub available_position: PositionQty,
    pub fees_paid: Money,
}

impl SpotAccountSnapshot {
    pub fn equity_at_mark(&self, mark_price_tick: PriceTick) -> Option<Money> {
        if mark_price_tick < 0 {
            return None;
        }
        Some(self.cash_balance + self.position_qty * Money::from(mark_price_tick))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SpotClearingEvent {
    TradeSettled {
        trade_id: u64,
        buyer_account_id: AccountId,
        seller_account_id: AccountId,
        price_tick: PriceTick,
        qty: Qty,
        notional: Money,
        buyer_fee: Money,
        seller_fee: Money,
        buyer: SpotAccountSnapshot,
        seller: SpotAccountSnapshot,
    },
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SpotAccountStore {
    #[serde(skip)]
    pub(crate) reservation_changes: crate::account::ReservationChanges,
    accounts: BTreeMap<AccountId, SpotAccount>,
    #[serde(default)]
    order_reservations: BTreeMap<OrderId, SpotOrderReservation>,
    config: SpotClearingConfig,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct SpotOrderReservation {
    account_id: AccountId,
    side: Side,
    price_tick: PriceTick,
    qty: Qty,
    reserved_cash: Money,
    reserved_position: PositionQty,
}

impl SpotAccountStore {
    pub fn new(config: SpotClearingConfig) -> Self {
        Self {
            reservation_changes: crate::account::ReservationChanges::default(),
            accounts: BTreeMap::new(),
            order_reservations: BTreeMap::new(),
            config,
        }
    }

    pub fn create_account(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
    ) -> SpotAccountSnapshot {
        self.create_account_with_position(account_id, cash_balance, 0)
    }

    pub fn create_account_with_position(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
        position_qty: PositionQty,
    ) -> SpotAccountSnapshot {
        self.reservation_changes.mark(account_id);
        let account = self.accounts.entry(account_id).or_insert(SpotAccount {
            account_id,
            cash_balance: 0,
            position_qty: 0,
            reserved_cash: 0,
            reserved_position: 0,
            fees_paid: 0,
        });
        account.cash_balance = cash_balance;
        account.position_qty = position_qty;
        account.reserved_cash = 0;
        account.reserved_position = 0;
        account.snapshot()
    }

    pub fn sync_account_balances(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
        position_qty: PositionQty,
    ) -> Result<SpotAccountSnapshot, ClearingError> {
        let account = self.account_mut(account_id);
        if cash_balance < account.reserved_cash || position_qty < account.reserved_position {
            return Err(ClearingError::InsufficientAvailableBalance);
        }
        account.cash_balance = cash_balance;
        account.position_qty = position_qty;
        Ok(account.snapshot())
    }

    pub fn account(&self, account_id: AccountId) -> Option<&SpotAccount> {
        self.accounts.get(&account_id)
    }

    pub fn config(&self) -> SpotClearingConfig {
        self.config
    }

    pub fn account_snapshot(&self, account_id: AccountId) -> Option<SpotAccountSnapshot> {
        self.account(account_id).map(SpotAccount::snapshot)
    }

    pub fn snapshots(&self) -> Vec<SpotAccountSnapshot> {
        self.accounts.values().map(SpotAccount::snapshot).collect()
    }

    pub(crate) fn reservation_balances(
        &self,
    ) -> impl Iterator<Item = (AccountId, Money, PositionQty)> + '_ {
        self.accounts.values().map(|account| {
            (
                account.account_id,
                account.reserved_cash,
                account.reserved_position,
            )
        })
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

        let reservation = self.reservation_for_order(account_id, side, price_tick, qty)?;
        let account = self.account_mut(account_id);
        if account.available_cash() < reservation.reserved_cash
            || account.available_position() < reservation.reserved_position
        {
            return Err(ClearingError::InsufficientAvailableBalance);
        }
        account.reserved_cash = account
            .reserved_cash
            .checked_add(reservation.reserved_cash)
            .ok_or(ClearingError::BalanceOverflow)?;
        account.reserved_position = account
            .reserved_position
            .checked_add(reservation.reserved_position)
            .ok_or(ClearingError::BalanceOverflow)?;
        self.order_reservations.insert(order_id, reservation);
        Ok(())
    }

    pub fn release_order_reservation(&mut self, order_id: OrderId) -> Result<(), ClearingError> {
        let Some(reservation) = self.order_reservations.remove(&order_id) else {
            return Ok(());
        };
        let account = self.account_mut(reservation.account_id);
        account.reserved_cash = account
            .reserved_cash
            .checked_sub(reservation.reserved_cash)
            .ok_or(ClearingError::ReservationUnderflow)?;
        account.reserved_position = account
            .reserved_position
            .checked_sub(reservation.reserved_position)
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
        self.release_order_reservation(order_id)?;
        self.reserve_resting_order(
            order_id,
            existing.account_id,
            existing.side,
            price_tick,
            qty,
        )
    }

    pub fn settle_trade(&mut self, trade: &Trade) -> Result<SpotClearingEvent, ClearingError> {
        let mut staged = self.clone();
        let event = staged.settle_trade_inner(trade)?;
        *self = staged;
        Ok(event)
    }

    fn settle_trade_inner(&mut self, trade: &Trade) -> Result<SpotClearingEvent, ClearingError> {
        let notional = notional(trade.price_tick, trade.qty)?;
        let maker_fee = fee_for(notional, self.config.maker_fee_ppm)?;
        let taker_fee = fee_for(notional, self.config.taker_fee_ppm)?;
        let participants = SpotTradeParticipants::from_trade(trade, maker_fee, taker_fee);

        self.release_maker_fill_reservation(trade.maker_order_id, trade.qty)?;

        let buyer = self.apply_fill(
            participants.buyer_account_id,
            notional,
            participants.buyer_fee,
            PositionQty::from(trade.qty),
        )?;
        let seller = self.apply_fill(
            participants.seller_account_id,
            -notional,
            participants.seller_fee,
            -PositionQty::from(trade.qty),
        )?;

        Ok(SpotClearingEvent::TradeSettled {
            trade_id: trade.trade_id,
            buyer_account_id: participants.buyer_account_id,
            seller_account_id: participants.seller_account_id,
            price_tick: trade.price_tick,
            qty: trade.qty,
            notional,
            buyer_fee: participants.buyer_fee,
            seller_fee: participants.seller_fee,
            buyer,
            seller,
        })
    }

    fn apply_fill(
        &mut self,
        account_id: AccountId,
        signed_notional: Money,
        fee: Money,
        position_delta: PositionQty,
    ) -> Result<SpotAccountSnapshot, ClearingError> {
        let account = self.account_mut(account_id);

        account.cash_balance = account
            .cash_balance
            .checked_sub(signed_notional)
            .and_then(|balance| balance.checked_sub(fee))
            .ok_or(ClearingError::BalanceOverflow)?;
        account.position_qty = account
            .position_qty
            .checked_add(position_delta)
            .ok_or(ClearingError::BalanceOverflow)?;
        account.fees_paid = account
            .fees_paid
            .checked_add(fee)
            .ok_or(ClearingError::BalanceOverflow)?;
        Ok(account.snapshot())
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
            reservation.side,
            reservation.price_tick,
            remaining_qty,
        )?;

        let release_cash = reservation
            .reserved_cash
            .checked_sub(next_reservation.reserved_cash)
            .ok_or(ClearingError::ReservationUnderflow)?;
        let release_position = reservation
            .reserved_position
            .checked_sub(next_reservation.reserved_position)
            .ok_or(ClearingError::ReservationUnderflow)?;

        {
            let account = self.account_mut(reservation.account_id);
            account.reserved_cash = account
                .reserved_cash
                .checked_sub(release_cash)
                .ok_or(ClearingError::ReservationUnderflow)?;
            account.reserved_position = account
                .reserved_position
                .checked_sub(release_position)
                .ok_or(ClearingError::ReservationUnderflow)?;
        }

        if remaining_qty == 0 {
            return Ok(());
        }

        reservation.qty = remaining_qty;
        reservation.reserved_cash = next_reservation.reserved_cash;
        reservation.reserved_position = next_reservation.reserved_position;
        self.order_reservations.insert(order_id, reservation);

        Ok(())
    }

    fn reservation_for_order(
        &self,
        account_id: AccountId,
        side: Side,
        price_tick: PriceTick,
        qty: Qty,
    ) -> Result<SpotOrderReservation, ClearingError> {
        let reserved_cash = if side == Side::Buy && qty > 0 {
            let order_notional = notional(price_tick, qty)?;
            order_notional
                .checked_add(fee_for(order_notional, self.config.maker_fee_ppm)?)
                .ok_or(ClearingError::BalanceOverflow)?
        } else {
            0
        };
        let reserved_position = if side == Side::Sell {
            PositionQty::from(qty)
        } else {
            0
        };
        Ok(SpotOrderReservation {
            account_id,
            side,
            price_tick,
            qty,
            reserved_cash,
            reserved_position,
        })
    }

    fn account_mut(&mut self, account_id: AccountId) -> &mut SpotAccount {
        self.reservation_changes.mark(account_id);
        self.accounts.entry(account_id).or_insert(SpotAccount {
            account_id,
            cash_balance: 0,
            position_qty: 0,
            reserved_cash: 0,
            reserved_position: 0,
            fees_paid: 0,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SpotTradeParticipants {
    buyer_account_id: AccountId,
    seller_account_id: AccountId,
    buyer_fee: Money,
    seller_fee: Money,
}

impl SpotTradeParticipants {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settles_buy_taker_trade_to_cash_position_and_fees() {
        let mut accounts = SpotAccountStore::new(SpotClearingConfig {
            maker_fee_ppm: 500,
            taker_fee_ppm: 1_000,
        });
        accounts.create_account(10, 10_000);
        accounts.create_account(20, 10_000);

        let event = accounts
            .settle_trade(&Trade {
                maker_position_side: Default::default(),
                taker_position_side: Default::default(),
                trade_id: 1,
                maker_order_id: 100,
                maker_account_id: 10,
                taker_order_id: 200,
                taker_account_id: 20,
                price_tick: 100,
                qty: 10,
                taker_side: Side::Buy,
            })
            .expect("trade should settle");

        assert_eq!(
            event,
            SpotClearingEvent::TradeSettled {
                trade_id: 1,
                buyer_account_id: 20,
                seller_account_id: 10,
                price_tick: 100,
                qty: 10,
                notional: 1_000,
                buyer_fee: 1,
                seller_fee: 0,
                buyer: SpotAccountSnapshot {
                    account_id: 20,
                    cash_balance: 8_999,
                    position_qty: 10,
                    reserved_cash: 0,
                    reserved_position: 0,
                    available_cash: 8_999,
                    available_position: 10,
                    fees_paid: 1,
                },
                seller: SpotAccountSnapshot {
                    account_id: 10,
                    cash_balance: 11_000,
                    position_qty: -10,
                    reserved_cash: 0,
                    reserved_position: 0,
                    available_cash: 11_000,
                    available_position: -10,
                    fees_paid: 0,
                },
            }
        );
    }

    #[test]
    fn settles_sell_taker_trade_with_maker_as_buyer() {
        let mut accounts = SpotAccountStore::new(SpotClearingConfig {
            maker_fee_ppm: 1_000,
            taker_fee_ppm: 2_000,
        });

        accounts
            .settle_trade(&Trade {
                maker_position_side: Default::default(),
                taker_position_side: Default::default(),
                trade_id: 1,
                maker_order_id: 100,
                maker_account_id: 10,
                taker_order_id: 200,
                taker_account_id: 20,
                price_tick: 1_000,
                qty: 5,
                taker_side: Side::Sell,
            })
            .expect("trade should settle");

        assert_eq!(
            accounts.account_snapshot(10),
            Some(SpotAccountSnapshot {
                account_id: 10,
                cash_balance: -5_005,
                position_qty: 5,
                reserved_cash: 0,
                reserved_position: 0,
                available_cash: -5_005,
                available_position: 5,
                fees_paid: 5,
            })
        );
        assert_eq!(
            accounts.account_snapshot(20),
            Some(SpotAccountSnapshot {
                account_id: 20,
                cash_balance: 4_990,
                position_qty: -5,
                reserved_cash: 0,
                reserved_position: 0,
                available_cash: 4_990,
                available_position: -5,
                fees_paid: 10,
            })
        );
    }
}
