use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    account::{ClearingError, FeeRatePpm, Money, PositionQty, fee_for, notional},
    model::{AccountId, PriceTick, Qty, Side, Trade},
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
    pub fees_paid: Money,
}

impl SpotAccount {
    pub fn snapshot(&self) -> SpotAccountSnapshot {
        SpotAccountSnapshot {
            account_id: self.account_id,
            cash_balance: self.cash_balance,
            position_qty: self.position_qty,
            fees_paid: self.fees_paid,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SpotAccountSnapshot {
    pub account_id: AccountId,
    pub cash_balance: Money,
    pub position_qty: PositionQty,
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

#[derive(Debug, Default)]
pub struct SpotAccountStore {
    accounts: BTreeMap<AccountId, SpotAccount>,
    config: SpotClearingConfig,
}

impl SpotAccountStore {
    pub fn new(config: SpotClearingConfig) -> Self {
        Self {
            accounts: BTreeMap::new(),
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
        let account = self.accounts.entry(account_id).or_insert(SpotAccount {
            account_id,
            cash_balance: 0,
            position_qty: 0,
            fees_paid: 0,
        });
        account.cash_balance = cash_balance;
        account.position_qty = position_qty;
        account.snapshot()
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

    pub fn settle_trade(&mut self, trade: &Trade) -> Result<SpotClearingEvent, ClearingError> {
        let notional = notional(trade.price_tick, trade.qty)?;
        let maker_fee = fee_for(notional, self.config.maker_fee_ppm);
        let taker_fee = fee_for(notional, self.config.taker_fee_ppm);
        let participants = SpotTradeParticipants::from_trade(trade, maker_fee, taker_fee);

        let buyer = self.apply_fill(
            participants.buyer_account_id,
            notional,
            participants.buyer_fee,
            PositionQty::from(trade.qty),
        );
        let seller = self.apply_fill(
            participants.seller_account_id,
            -notional,
            participants.seller_fee,
            -PositionQty::from(trade.qty),
        );

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
    ) -> SpotAccountSnapshot {
        let account = self.accounts.entry(account_id).or_insert(SpotAccount {
            account_id,
            cash_balance: 0,
            position_qty: 0,
            fees_paid: 0,
        });

        account.cash_balance -= signed_notional;
        account.cash_balance -= fee;
        account.position_qty += position_delta;
        account.fees_paid += fee;
        account.snapshot()
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
                    fees_paid: 1,
                },
                seller: SpotAccountSnapshot {
                    account_id: 10,
                    cash_balance: 11_000,
                    position_qty: -10,
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
                fees_paid: 5,
            })
        );
        assert_eq!(
            accounts.account_snapshot(20),
            Some(SpotAccountSnapshot {
                account_id: 20,
                cash_balance: 4_990,
                position_qty: -5,
                fees_paid: 10,
            })
        );
    }
}
