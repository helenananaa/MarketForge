use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    account::{ClearingError, FeeRatePpm, Money, PositionQty, fee_for, notional},
    model::{AccountId, PriceTick, Qty, Side, Trade},
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerpClearingConfig {
    pub maker_fee_ppm: FeeRatePpm,
    pub taker_fee_ppm: FeeRatePpm,
    pub leverage: u32,
}

impl Default for PerpClearingConfig {
    fn default() -> Self {
        Self {
            maker_fee_ppm: 0,
            taker_fee_ppm: 0,
            leverage: 1,
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
}

impl PerpAccount {
    pub fn snapshot(
        &self,
        config: PerpClearingConfig,
        mark_price_tick: PriceTick,
    ) -> PerpAccountSnapshot {
        let unrealized_pnl = self.unrealized_pnl_at_mark(mark_price_tick).unwrap_or(0);
        PerpAccountSnapshot {
            account_id: self.account_id,
            cash_balance: self.cash_balance,
            position_qty: self.position_qty,
            avg_entry_price_tick: self.avg_entry_price_tick,
            realized_pnl: self.realized_pnl,
            unrealized_pnl,
            equity: self.cash_balance + unrealized_pnl,
            initial_margin: initial_margin(self.position_qty, self.avg_entry_price_tick, config),
            fees_paid: self.fees_paid,
        }
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
    pub fees_paid: Money,
}

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
}

#[derive(Debug)]
pub struct PerpAccountStore {
    accounts: BTreeMap<AccountId, PerpAccount>,
    config: PerpClearingConfig,
    mark_price_tick: PriceTick,
}

impl PerpAccountStore {
    pub fn new(
        config: PerpClearingConfig,
        initial_mark_price_tick: PriceTick,
    ) -> Result<Self, ClearingError> {
        if config.leverage == 0 {
            return Err(ClearingError::InvalidLeverage);
        }
        if initial_mark_price_tick <= 0 {
            return Err(ClearingError::InvalidPrice);
        }

        Ok(Self {
            accounts: BTreeMap::new(),
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
        });
        account.cash_balance = cash_balance;
        account.snapshot(self.config, self.mark_price_tick)
    }

    pub fn mark_price_tick(&self) -> PriceTick {
        self.mark_price_tick
    }

    pub fn set_mark_price_tick(&mut self, mark_price_tick: PriceTick) -> Result<(), ClearingError> {
        if mark_price_tick <= 0 {
            return Err(ClearingError::InvalidPrice);
        }
        self.mark_price_tick = mark_price_tick;
        Ok(())
    }

    pub fn account(&self, account_id: AccountId) -> Option<&PerpAccount> {
        self.accounts.get(&account_id)
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

    pub fn settle_trade(&mut self, trade: &Trade) -> Result<PerpClearingEvent, ClearingError> {
        let notional = notional(trade.price_tick, trade.qty)?;
        let maker_fee = fee_for(notional, self.config.maker_fee_ppm);
        let taker_fee = fee_for(notional, self.config.taker_fee_ppm);
        let participants = PerpTradeParticipants::from_trade(trade, maker_fee, taker_fee);

        let buyer_result = self.apply_fill(
            participants.buyer_account_id,
            PositionQty::from(trade.qty),
            trade.price_tick,
            participants.buyer_fee,
        );
        let seller_result = self.apply_fill(
            participants.seller_account_id,
            -PositionQty::from(trade.qty),
            trade.price_tick,
            participants.seller_fee,
        );

        Ok(PerpClearingEvent::TradeSettled {
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
        })
    }

    fn apply_fill(
        &mut self,
        account_id: AccountId,
        fill_qty: PositionQty,
        price_tick: PriceTick,
        fee: Money,
    ) -> PerpFillResult {
        let account = self.accounts.entry(account_id).or_insert(PerpAccount {
            account_id,
            cash_balance: 0,
            position_qty: 0,
            avg_entry_price_tick: 0,
            realized_pnl: 0,
            fees_paid: 0,
        });

        let realized_pnl_delta = apply_position_fill(account, fill_qty, price_tick);
        account.cash_balance += realized_pnl_delta;
        account.cash_balance -= fee;
        account.realized_pnl += realized_pnl_delta;
        account.fees_paid += fee;

        PerpFillResult {
            realized_pnl_delta,
            snapshot: account.snapshot(self.config, self.mark_price_tick),
        }
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
                fees_paid: 0,
            })
        );
    }
}
