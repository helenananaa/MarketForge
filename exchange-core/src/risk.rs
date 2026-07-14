use serde::{Deserialize, Serialize};

use crate::{
    account::{Money, PositionQty, fee_for, notional},
    engine::FillQuote,
    model::{Command, NewOrder, PriceTick, RiskRejectReason, Side},
    perp::PerpAccountStore,
    spot::SpotAccountStore,
};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SpotRiskConfig {
    pub price_tick_size: Option<PriceTick>,
    pub lot_size: Option<u64>,
    pub max_order_qty: Option<u64>,
    pub max_order_notional: Option<Money>,
    pub allow_short: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerpRiskConfig {
    pub price_tick_size: Option<PriceTick>,
    pub lot_size: Option<u64>,
    pub max_order_qty: Option<u64>,
    pub max_order_notional: Option<Money>,
    pub max_abs_position_qty: Option<PositionQty>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RiskContext {
    pub best_bid: Option<PriceTick>,
    pub best_ask: Option<PriceTick>,
    pub fill_quote: FillQuote,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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
        if let Command::AmendOrder(amend) = command {
            return check_amend_tick_and_lot(
                amend.price_tick,
                amend.qty,
                self.config.price_tick_size,
                self.config.lot_size,
            );
        }

        let Command::NewOrder(order) = command else {
            return Ok(());
        };

        self.check_order_limits(order, context)?;
        let account = accounts
            .account(order.account_id)
            .ok_or(RiskRejectReason::AccountNotFound)?;

        if order.reduce_only {
            return Err(RiskRejectReason::ReduceOnlyUnsupported);
        }

        match order.side {
            Side::Buy => {
                let required_cash = required_spot_buy_cash(
                    order,
                    context,
                    accounts.config().maker_fee_ppm,
                    accounts.config().taker_fee_ppm,
                )?;
                if account.available_cash() < required_cash {
                    return Err(RiskRejectReason::InsufficientCash);
                }
            }
            Side::Sell if !self.config.allow_short => {
                if account.available_position() < PositionQty::from(order.qty) {
                    return Err(RiskRejectReason::InsufficientPosition);
                }
            }
            Side::Sell => {
                let unfilled_qty = order.qty.saturating_sub(context.fill_quote.qty);
                if order.kind.rests_remainder()
                    && account.available_position() < PositionQty::from(unfilled_qty)
                {
                    return Err(RiskRejectReason::InsufficientPosition);
                }
            }
        }

        Ok(())
    }

    fn check_order_limits(
        &self,
        order: &NewOrder,
        context: RiskContext,
    ) -> Result<(), RiskRejectReason> {
        check_tick_and_lot(order, self.config.price_tick_size, self.config.lot_size)?;

        if self
            .config
            .max_order_qty
            .is_some_and(|max_qty| order.qty > max_qty)
        {
            return Err(RiskRejectReason::MaxOrderQtyExceeded);
        }

        if let Some(max_notional) = self.config.max_order_notional {
            let order_notional = risk_notional(order, context)?;
            if order_notional > max_notional {
                return Err(RiskRejectReason::MaxOrderNotionalExceeded);
            }
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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
        if let Command::AmendOrder(amend) = command {
            check_amend_tick_and_lot(
                amend.price_tick,
                amend.qty,
                self.config.price_tick_size,
                self.config.lot_size,
            )?;

            if amend.price_tick.is_some_and(|price_tick| price_tick <= 0) || amend.qty == Some(0) {
                return Ok(());
            }
            if !accounts
                .amend_order_requires_available_margin(amend.order_id, amend.price_tick, amend.qty)
                .map_err(|_| RiskRejectReason::InsufficientMargin)?
            {
                return Err(RiskRejectReason::InsufficientMargin);
            }
            return Ok(());
        }

        let Command::NewOrder(order) = command else {
            return Ok(());
        };

        self.check_order_limits(order, context)?;
        let account = accounts
            .account(order.account_id)
            .ok_or(RiskRejectReason::AccountNotFound)?;

        if order.reduce_only {
            self.check_reduce_only_order(order, account.position_qty)?;
            return Ok(());
        }

        let fill_delta = order_position_delta(order);
        let estimated_position = account
            .position_qty
            .checked_add(fill_delta)
            .ok_or(RiskRejectReason::MaxPositionExceeded)?;
        let estimated_abs_position = estimated_position
            .checked_abs()
            .ok_or(RiskRejectReason::MaxPositionExceeded)?;

        if self
            .config
            .max_abs_position_qty
            .is_some_and(|max_qty| estimated_abs_position > max_qty)
        {
            return Err(RiskRejectReason::MaxPositionExceeded);
        }

        let existing_notional = Money::from(account.avg_entry_price_tick)
            .checked_mul(
                account
                    .position_qty
                    .checked_abs()
                    .ok_or(RiskRejectReason::MaxPositionExceeded)?,
            )
            .ok_or(RiskRejectReason::MaxOrderNotionalExceeded)?
            .checked_add(risk_notional(order, context)?)
            .ok_or(RiskRejectReason::MaxOrderNotionalExceeded)?;
        let estimated_margin = existing_notional / Money::from(accounts.config().leverage);
        let estimated_fee = taker_fee_estimate(
            risk_notional(order, context)?,
            accounts.config().taker_fee_ppm,
        )?;
        let required_cash = estimated_margin
            .checked_add(estimated_fee)
            .ok_or(RiskRejectReason::MaxOrderNotionalExceeded)?;
        if account.available_cash() < required_cash {
            return Err(RiskRejectReason::InsufficientMargin);
        }

        Ok(())
    }

    fn check_order_limits(
        &self,
        order: &NewOrder,
        context: RiskContext,
    ) -> Result<(), RiskRejectReason> {
        check_tick_and_lot(order, self.config.price_tick_size, self.config.lot_size)?;

        if self
            .config
            .max_order_qty
            .is_some_and(|max_qty| order.qty > max_qty)
        {
            return Err(RiskRejectReason::MaxOrderQtyExceeded);
        }

        if let Some(max_notional) = self.config.max_order_notional {
            let order_notional = risk_notional(order, context)?;
            if order_notional > max_notional {
                return Err(RiskRejectReason::MaxOrderNotionalExceeded);
            }
        }

        Ok(())
    }

    fn check_reduce_only_order(
        &self,
        order: &NewOrder,
        position_qty: PositionQty,
    ) -> Result<(), RiskRejectReason> {
        if order.kind.rests_remainder() {
            return Err(RiskRejectReason::ReduceOnlyUnsupported);
        }

        let fill_delta = order_position_delta(order);
        if position_qty == 0 || position_qty.signum() == fill_delta.signum() {
            return Err(RiskRejectReason::ReduceOnlyWouldIncreasePosition);
        }
        if fill_delta.abs() > position_qty.abs() {
            return Err(RiskRejectReason::ReduceOnlyExceedsPosition);
        }

        Ok(())
    }
}

fn check_tick_and_lot(
    order: &NewOrder,
    price_tick_size: Option<PriceTick>,
    lot_size: Option<u64>,
) -> Result<(), RiskRejectReason> {
    if let Some(lot_size) = lot_size
        && (lot_size == 0 || !order.qty.is_multiple_of(lot_size))
    {
        return Err(RiskRejectReason::InvalidLotSize);
    }

    if let (Some(price_tick), Some(price_tick_size)) =
        (order.kind.limit_price_tick(), price_tick_size)
        && (price_tick_size <= 0 || price_tick % price_tick_size != 0)
    {
        return Err(RiskRejectReason::InvalidPriceTick);
    }

    Ok(())
}

fn check_amend_tick_and_lot(
    price_tick: Option<PriceTick>,
    qty: Option<u64>,
    price_tick_size: Option<PriceTick>,
    lot_size: Option<u64>,
) -> Result<(), RiskRejectReason> {
    if let Some(lot_size) = lot_size
        && let Some(qty) = qty
        && (lot_size == 0 || !qty.is_multiple_of(lot_size))
    {
        return Err(RiskRejectReason::InvalidLotSize);
    }

    if let (Some(price_tick), Some(price_tick_size)) = (price_tick, price_tick_size)
        && (price_tick_size <= 0 || price_tick % price_tick_size != 0)
    {
        return Err(RiskRejectReason::InvalidPriceTick);
    }

    Ok(())
}

fn risk_notional(order: &NewOrder, context: RiskContext) -> Result<Money, RiskRejectReason> {
    if let Some(price_tick) = order.kind.limit_price_tick() {
        return notional(price_tick, order.qty)
            .map_err(|_| RiskRejectReason::MaxOrderNotionalExceeded);
    }
    if context.fill_quote.qty == 0 {
        return Err(RiskRejectReason::UnsupportedMarketOrder);
    }
    Ok(context.fill_quote.notional)
}

fn required_spot_buy_cash(
    order: &NewOrder,
    context: RiskContext,
    maker_fee_ppm: u32,
    taker_fee_ppm: u32,
) -> Result<Money, RiskRejectReason> {
    if order.kind.rests_remainder() {
        let price_tick = order
            .kind
            .limit_price_tick()
            .ok_or(RiskRejectReason::UnsupportedMarketOrder)?;
        let fill_qty = context.fill_quote.qty.min(order.qty);
        let resting_qty = order.qty - fill_qty;
        let resting_notional = notional(price_tick, resting_qty)
            .map_err(|_| RiskRejectReason::MaxOrderNotionalExceeded)?;
        let taker_fee = taker_fee_estimate(context.fill_quote.notional, taker_fee_ppm)?;
        let maker_fee = taker_fee_estimate(resting_notional, maker_fee_ppm)?;
        return context
            .fill_quote
            .notional
            .checked_add(resting_notional)
            .and_then(|required| required.checked_add(taker_fee))
            .and_then(|required| required.checked_add(maker_fee))
            .ok_or(RiskRejectReason::MaxOrderNotionalExceeded);
    }

    let fill_notional = risk_notional(order, context)?;
    let taker_fee = taker_fee_estimate(fill_notional, taker_fee_ppm)?;
    fill_notional
        .checked_add(taker_fee)
        .ok_or(RiskRejectReason::MaxOrderNotionalExceeded)
}

fn taker_fee_estimate(
    order_notional: Money,
    taker_fee_ppm: u32,
) -> Result<Money, RiskRejectReason> {
    fee_for(order_notional, taker_fee_ppm).map_err(|_| RiskRejectReason::MaxOrderNotionalExceeded)
}

fn order_position_delta(order: &NewOrder) -> PositionQty {
    match order.side {
        Side::Buy => PositionQty::from(order.qty),
        Side::Sell => -PositionQty::from(order.qty),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{NewOrder, OrderKind, Trade},
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
            reduce_only: false,
        })
    }

    fn reduce_only(account_id: u64, side: Side, kind: OrderKind, qty: u64) -> Command {
        Command::NewOrder(NewOrder {
            order_id: account_id + qty + 10_000,
            account_id,
            side,
            kind,
            qty,
            reduce_only: true,
        })
    }

    fn seed_long_position(accounts: &mut PerpAccountStore, account_id: u64, qty: u64) {
        accounts.create_account(account_id, 1_000);
        accounts.create_account(99, 1_000);
        accounts
            .settle_trade(&Trade {
                trade_id: 1,
                maker_order_id: 10,
                maker_account_id: 99,
                taker_order_id: 11,
                taker_account_id: account_id,
                price_tick: 100,
                qty,
                taker_side: Side::Buy,
            })
            .expect("seed trade should settle");
        accounts.create_account(account_id, 1_000);
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
                    fill_quote: FillQuote::default(),
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
                    fill_quote: FillQuote::default(),
                },
            ),
            Err(RiskRejectReason::InsufficientPosition)
        );
    }

    #[test]
    fn spot_rejects_reduce_only_orders() {
        let mut accounts = SpotAccountStore::new(SpotClearingConfig::default());
        accounts.create_account(1, 1_000);
        let risk = SpotRiskEngine::new(SpotRiskConfig::default());

        assert_eq!(
            risk.check(
                &reduce_only(1, Side::Sell, OrderKind::Market, 1),
                &accounts,
                RiskContext {
                    best_bid: Some(100),
                    best_ask: None,
                    fill_quote: FillQuote::default(),
                },
            ),
            Err(RiskRejectReason::ReduceOnlyUnsupported)
        );
    }

    #[test]
    fn perp_order_requires_margin() {
        let mut accounts = PerpAccountStore::new(
            PerpClearingConfig {
                maker_fee_ppm: 0,
                taker_fee_ppm: 0,
                leverage: 10,
                ..PerpClearingConfig::default()
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
                    fill_quote: FillQuote::default(),
                },
            ),
            Err(RiskRejectReason::InsufficientMargin)
        );
    }

    #[test]
    fn perp_reduce_only_allows_opposite_non_resting_order_within_position() {
        let mut accounts = PerpAccountStore::new(PerpClearingConfig::default(), 100).unwrap();
        seed_long_position(&mut accounts, 1, 5);
        let risk = PerpRiskEngine::new(PerpRiskConfig::default());

        assert_eq!(
            risk.check(
                &reduce_only(1, Side::Sell, OrderKind::Market, 3),
                &accounts,
                RiskContext {
                    best_bid: Some(100),
                    best_ask: None,
                    fill_quote: FillQuote::default(),
                },
            ),
            Ok(())
        );
    }

    #[test]
    fn perp_reduce_only_rejects_same_direction_and_over_reducing_orders() {
        let mut accounts = PerpAccountStore::new(PerpClearingConfig::default(), 100).unwrap();
        seed_long_position(&mut accounts, 1, 5);
        let risk = PerpRiskEngine::new(PerpRiskConfig::default());

        assert_eq!(
            risk.check(
                &reduce_only(1, Side::Buy, OrderKind::Market, 1),
                &accounts,
                RiskContext {
                    best_bid: None,
                    best_ask: Some(100),
                    fill_quote: FillQuote::default(),
                },
            ),
            Err(RiskRejectReason::ReduceOnlyWouldIncreasePosition)
        );
        assert_eq!(
            risk.check(
                &reduce_only(1, Side::Sell, OrderKind::Market, 6),
                &accounts,
                RiskContext {
                    best_bid: Some(100),
                    best_ask: None,
                    fill_quote: FillQuote::default(),
                },
            ),
            Err(RiskRejectReason::ReduceOnlyExceedsPosition)
        );
    }

    #[test]
    fn perp_reduce_only_rejects_resting_order_kinds() {
        let mut accounts = PerpAccountStore::new(PerpClearingConfig::default(), 100).unwrap();
        seed_long_position(&mut accounts, 1, 5);
        let risk = PerpRiskEngine::new(PerpRiskConfig::default());

        assert_eq!(
            risk.check(
                &reduce_only(1, Side::Sell, OrderKind::Limit { price_tick: 100 }, 3),
                &accounts,
                RiskContext {
                    best_bid: Some(99),
                    best_ask: Some(101),
                    fill_quote: FillQuote::default(),
                },
            ),
            Err(RiskRejectReason::ReduceOnlyUnsupported)
        );
    }
}
