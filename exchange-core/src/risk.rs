use serde::{Deserialize, Serialize};

use crate::{
    account::{Money, PositionQty, fee_for, notional},
    model::{Command, NewOrder, OrderKind, PriceTick, RiskRejectReason, Side},
    perp::PerpAccountStore,
    spot::SpotAccountStore,
};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SpotRiskConfig {
    pub max_order_qty: Option<u64>,
    pub max_order_notional: Option<Money>,
    pub allow_short: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerpRiskConfig {
    pub max_order_qty: Option<u64>,
    pub max_order_notional: Option<Money>,
    pub max_abs_position_qty: Option<PositionQty>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RiskContext {
    pub best_bid: Option<PriceTick>,
    pub best_ask: Option<PriceTick>,
}

#[derive(Debug)]
pub struct SpotRiskEngine {
    config: SpotRiskConfig,
}

impl SpotRiskEngine {
    pub fn new(config: SpotRiskConfig) -> Self {
        Self { config }
    }

    pub fn check(
        &self,
        command: &Command,
        accounts: &SpotAccountStore,
        context: RiskContext,
    ) -> Result<(), RiskRejectReason> {
        let Command::NewOrder(order) = command else {
            return Ok(());
        };

        self.check_order_limits(order, context)?;
        let account = accounts
            .account(order.account_id)
            .ok_or(RiskRejectReason::AccountNotFound)?;

        match order.side {
            Side::Buy => {
                let price_tick = risk_price_tick(order, context)?;
                let required_cash = notional(price_tick, order.qty)
                    .map_err(|_| RiskRejectReason::MaxOrderNotionalExceeded)?
                    + taker_fee_estimate(price_tick, order.qty, accounts.config().taker_fee_ppm)?;
                if account.cash_balance < required_cash {
                    return Err(RiskRejectReason::InsufficientCash);
                }
            }
            Side::Sell if !self.config.allow_short => {
                if account.position_qty < PositionQty::from(order.qty) {
                    return Err(RiskRejectReason::InsufficientPosition);
                }
            }
            Side::Sell => {}
        }

        Ok(())
    }

    fn check_order_limits(
        &self,
        order: &NewOrder,
        context: RiskContext,
    ) -> Result<(), RiskRejectReason> {
        if self
            .config
            .max_order_qty
            .is_some_and(|max_qty| order.qty > max_qty)
        {
            return Err(RiskRejectReason::MaxOrderQtyExceeded);
        }

        if let Some(max_notional) = self.config.max_order_notional {
            let price_tick = risk_price_tick(order, context)?;
            let order_notional = notional(price_tick, order.qty)
                .map_err(|_| RiskRejectReason::MaxOrderNotionalExceeded)?;
            if order_notional > max_notional {
                return Err(RiskRejectReason::MaxOrderNotionalExceeded);
            }
        }

        Ok(())
    }
}

#[derive(Debug)]
pub struct PerpRiskEngine {
    config: PerpRiskConfig,
}

impl PerpRiskEngine {
    pub fn new(config: PerpRiskConfig) -> Self {
        Self { config }
    }

    pub fn check(
        &self,
        command: &Command,
        accounts: &PerpAccountStore,
        context: RiskContext,
    ) -> Result<(), RiskRejectReason> {
        let Command::NewOrder(order) = command else {
            return Ok(());
        };

        self.check_order_limits(order, context)?;
        let account = accounts
            .account(order.account_id)
            .ok_or(RiskRejectReason::AccountNotFound)?;

        let price_tick = risk_price_tick(order, context)?;
        let fill_delta = match order.side {
            Side::Buy => PositionQty::from(order.qty),
            Side::Sell => -PositionQty::from(order.qty),
        };
        let estimated_position = account.position_qty + fill_delta;

        if self
            .config
            .max_abs_position_qty
            .is_some_and(|max_qty| estimated_position.abs() > max_qty)
        {
            return Err(RiskRejectReason::MaxPositionExceeded);
        }

        let estimated_margin = Money::from(price_tick) * estimated_position.abs()
            / Money::from(accounts.config().leverage);
        let estimated_fee =
            taker_fee_estimate(price_tick, order.qty, accounts.config().taker_fee_ppm)?;
        if account.cash_balance < estimated_margin + estimated_fee {
            return Err(RiskRejectReason::InsufficientMargin);
        }

        Ok(())
    }

    fn check_order_limits(
        &self,
        order: &NewOrder,
        context: RiskContext,
    ) -> Result<(), RiskRejectReason> {
        if self
            .config
            .max_order_qty
            .is_some_and(|max_qty| order.qty > max_qty)
        {
            return Err(RiskRejectReason::MaxOrderQtyExceeded);
        }

        if let Some(max_notional) = self.config.max_order_notional {
            let price_tick = risk_price_tick(order, context)?;
            let order_notional = notional(price_tick, order.qty)
                .map_err(|_| RiskRejectReason::MaxOrderNotionalExceeded)?;
            if order_notional > max_notional {
                return Err(RiskRejectReason::MaxOrderNotionalExceeded);
            }
        }

        Ok(())
    }
}

fn risk_price_tick(order: &NewOrder, context: RiskContext) -> Result<PriceTick, RiskRejectReason> {
    match order.kind {
        OrderKind::Limit { price_tick } => Ok(price_tick),
        OrderKind::Market => match order.side {
            Side::Buy => context
                .best_ask
                .ok_or(RiskRejectReason::UnsupportedMarketOrder),
            Side::Sell => context
                .best_bid
                .ok_or(RiskRejectReason::UnsupportedMarketOrder),
        },
    }
}

fn taker_fee_estimate(
    price_tick: PriceTick,
    qty: u64,
    taker_fee_ppm: u32,
) -> Result<Money, RiskRejectReason> {
    let notional =
        notional(price_tick, qty).map_err(|_| RiskRejectReason::MaxOrderNotionalExceeded)?;
    Ok(fee_for(notional, taker_fee_ppm))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{NewOrder, OrderKind},
        perp::PerpClearingConfig,
        spot::SpotClearingConfig,
    };

    fn limit(account_id: u64, side: Side, price_tick: i64, qty: u64) -> Command {
        Command::NewOrder(NewOrder {
            order_id: account_id + qty,
            account_id,
            side,
            kind: OrderKind::Limit { price_tick },
            qty,
        })
    }

    #[test]
    fn spot_buy_requires_cash() {
        let mut accounts = SpotAccountStore::new(SpotClearingConfig::default());
        accounts.create_account(1, 99);
        let risk = SpotRiskEngine::new(SpotRiskConfig::default());

        assert_eq!(
            risk.check(
                &limit(1, Side::Buy, 100, 1),
                &accounts,
                RiskContext {
                    best_bid: None,
                    best_ask: None,
                },
            ),
            Err(RiskRejectReason::InsufficientCash)
        );
    }

    #[test]
    fn spot_sell_requires_position_when_short_disabled() {
        let mut accounts = SpotAccountStore::new(SpotClearingConfig::default());
        accounts.create_account(1, 1_000);
        let risk = SpotRiskEngine::new(SpotRiskConfig {
            allow_short: false,
            ..SpotRiskConfig::default()
        });

        assert_eq!(
            risk.check(
                &limit(1, Side::Sell, 100, 1),
                &accounts,
                RiskContext {
                    best_bid: None,
                    best_ask: None,
                },
            ),
            Err(RiskRejectReason::InsufficientPosition)
        );
    }

    #[test]
    fn perp_order_requires_margin() {
        let mut accounts = PerpAccountStore::new(
            PerpClearingConfig {
                maker_fee_ppm: 0,
                taker_fee_ppm: 0,
                leverage: 10,
            },
            100,
        )
        .unwrap();
        accounts.create_account(1, 9);
        let risk = PerpRiskEngine::new(PerpRiskConfig::default());

        assert_eq!(
            risk.check(
                &limit(1, Side::Buy, 100, 1),
                &accounts,
                RiskContext {
                    best_bid: None,
                    best_ask: None,
                },
            ),
            Err(RiskRejectReason::InsufficientMargin)
        );
    }
}
