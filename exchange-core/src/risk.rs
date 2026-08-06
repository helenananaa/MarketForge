use serde::{Deserialize, Serialize};

use crate::{
    account::{ClearingError, Money, PositionQty, fee_for, notional},
    engine::FillQuote,
    model::{Command, NewOrder, PriceTick, RiskRejectReason, Side},
    perp::{PerpAccountStore, PerpMarginStatus, PerpPendingOrderRisk},
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
            let Some((before, after)) = accounts
                .amend_order_risk_exposures(amend.order_id, amend.price_tick, amend.qty)
                .map_err(perp_risk_reject_reason)?
            else {
                return Ok(());
            };
            if self
                .config
                .max_abs_position_qty
                .is_some_and(|max_qty| after.max_abs_position_qty > max_qty)
            {
                return Err(RiskRejectReason::MaxPositionExceeded);
            }
            if before.liquidation_pending && !after.is_strict_reduction_from(before) {
                return Err(RiskRejectReason::InsufficientMargin);
            }
            if matches!(
                before.margin_status,
                PerpMarginStatus::MarginCall | PerpMarginStatus::Liquidatable
            ) && !after.is_strict_reduction_from(before)
            {
                return Err(RiskRejectReason::InsufficientMargin);
            }
            if !after.is_strict_reduction_from(before) && after.available_equity < 0 {
                return Err(RiskRejectReason::InsufficientMargin);
            }
            return Ok(());
        }

        let Command::NewOrder(order) = command else {
            return Ok(());
        };

        self.check_order_limits(order, context)?;
        let current = accounts
            .risk_exposure(order.account_id)
            .map_err(perp_risk_reject_reason)?;
        let account = accounts
            .account(order.account_id)
            .ok_or(RiskRejectReason::AccountNotFound)?;

        if current.liquidation_pending {
            return Err(RiskRejectReason::InsufficientMargin);
        }

        if order.reduce_only {
            self.check_reduce_only_order(order, account.position_qty)?;
        }

        if strictly_reduces_position(order, account.position_qty) {
            let position_reduction = accounts
                .projected_order_risk(
                    order.account_id,
                    pending_position_reduction_risk(order, accounts),
                )
                .map_err(perp_risk_reject_reason)?;
            if position_reduction.is_strict_position_reduction_from(current) {
                return Ok(());
            }
        }

        if matches!(
            current.margin_status,
            PerpMarginStatus::MarginCall | PerpMarginStatus::Liquidatable
        ) {
            return Err(RiskRejectReason::InsufficientMargin);
        }

        let pending_order = pending_perp_order_risk(order, context, accounts)?;
        let projected = accounts
            .projected_order_risk(order.account_id, pending_order)
            .map_err(perp_risk_reject_reason)?;

        if self
            .config
            .max_abs_position_qty
            .is_some_and(|max_qty| projected.max_abs_position_qty > max_qty)
        {
            return Err(RiskRejectReason::MaxPositionExceeded);
        }

        if projected.available_equity < 0 {
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

fn pending_perp_order_risk(
    order: &NewOrder,
    context: RiskContext,
    accounts: &PerpAccountStore,
) -> Result<PerpPendingOrderRisk, RiskRejectReason> {
    let at_risk_qty = if order.kind.rests_remainder() {
        order.qty
    } else {
        context.fill_quote.qty.min(order.qty)
    };
    if at_risk_qty == 0 && order.kind.limit_price_tick().is_none() {
        return Err(RiskRejectReason::UnsupportedMarketOrder);
    }

    let mut risk_price_tick = order.kind.limit_price_tick().unwrap_or(0);
    if context.fill_quote.qty > 0 {
        let quote_qty = Money::from(context.fill_quote.qty);
        let average_fill_price = context
            .fill_quote
            .notional
            .checked_add(quote_qty - 1)
            .ok_or(RiskRejectReason::MaxOrderNotionalExceeded)?
            / quote_qty;
        let average_fill_price = PriceTick::try_from(average_fill_price)
            .map_err(|_| RiskRejectReason::MaxOrderNotionalExceeded)?;
        risk_price_tick = risk_price_tick.max(average_fill_price);
    }
    if risk_price_tick <= 0 {
        return Err(RiskRejectReason::UnsupportedMarketOrder);
    }

    let order_notional = if at_risk_qty == 0 {
        0
    } else {
        notional(risk_price_tick, at_risk_qty)
            .map_err(|_| RiskRejectReason::MaxOrderNotionalExceeded)?
    };
    let fee_rate = accounts
        .config()
        .maker_fee_ppm
        .max(accounts.config().taker_fee_ppm);
    let fee = fee_for(order_notional, fee_rate)
        .map_err(|_| RiskRejectReason::MaxOrderNotionalExceeded)?;

    Ok(PerpPendingOrderRisk {
        side: order.side,
        price_tick: risk_price_tick,
        qty: at_risk_qty,
        fee,
    })
}

fn pending_position_reduction_risk(
    order: &NewOrder,
    accounts: &PerpAccountStore,
) -> PerpPendingOrderRisk {
    PerpPendingOrderRisk {
        side: order.side,
        price_tick: order
            .kind
            .limit_price_tick()
            .unwrap_or(accounts.mark_price_tick())
            .max(accounts.mark_price_tick()),
        qty: order.qty,
        // Fees can consume equity, but do not make an otherwise guaranteed
        // position reduction increase directional exposure.
        fee: 0,
    }
}

fn strictly_reduces_position(order: &NewOrder, position_qty: PositionQty) -> bool {
    if order.qty == 0 || order.kind.rests_remainder() || position_qty == 0 {
        return false;
    }
    let is_opposite_side = matches!(
        (position_qty.signum(), order.side),
        (1, Side::Sell) | (-1, Side::Buy)
    );
    is_opposite_side && u128::from(order.qty) <= position_qty.unsigned_abs()
}

fn perp_risk_reject_reason(error: ClearingError) -> RiskRejectReason {
    match error {
        ClearingError::AccountNotFound => RiskRejectReason::AccountNotFound,
        ClearingError::NotionalOverflow
        | ClearingError::BalanceOverflow
        | ClearingError::InvalidPrice => RiskRejectReason::MaxOrderNotionalExceeded,
        ClearingError::InvalidLiquidationQuantity => RiskRejectReason::MaxPositionExceeded,
        ClearingError::InvalidLeverage
        | ClearingError::InvalidMarginRate
        | ClearingError::InvalidFeeRate
        | ClearingError::AccountNotLiquidatable
        | ClearingError::LiquidationUnfilled
        | ClearingError::WrongMarketKind
        | ClearingError::InsufficientAvailableBalance
        | ClearingError::ReservationUnderflow => RiskRejectReason::InsufficientMargin,
    }
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

    #[test]
    fn perp_cross_direction_orders_use_worst_side_instead_of_summing_margin() {
        let mut accounts = PerpAccountStore::new(
            PerpClearingConfig {
                leverage: 10,
                ..PerpClearingConfig::default()
            },
            100,
        )
        .unwrap();
        accounts.create_account(1, 100);
        let risk = PerpRiskEngine::new(PerpRiskConfig::default());
        let context = RiskContext {
            best_bid: None,
            best_ask: None,
            fill_quote: FillQuote::default(),
        };

        assert_eq!(
            risk.check(&limit(1, Side::Buy, 100, 10), &accounts, context),
            Ok(())
        );
        accounts
            .reserve_resting_order(1, 1, Side::Buy, 100, 10)
            .unwrap();
        assert_eq!(
            risk.check(&limit(1, Side::Sell, 100, 10), &accounts, context),
            Ok(())
        );
        accounts
            .reserve_resting_order(2, 1, Side::Sell, 100, 10)
            .unwrap();

        assert_eq!(accounts.account_snapshot(1).unwrap().reserved_margin, 100);
        assert_eq!(
            risk.check(&limit(1, Side::Buy, 100, 1), &accounts, context),
            Err(RiskRejectReason::InsufficientMargin)
        );
    }

    #[test]
    fn perp_max_position_limit_includes_existing_orders_on_each_direction() {
        let mut accounts = PerpAccountStore::new(
            PerpClearingConfig {
                leverage: 10,
                ..PerpClearingConfig::default()
            },
            100,
        )
        .unwrap();
        accounts.create_account(1, 1_000);
        accounts
            .reserve_resting_order(1, 1, Side::Buy, 100, 10)
            .unwrap();
        accounts
            .reserve_resting_order(2, 1, Side::Sell, 100, 10)
            .unwrap();
        let risk = PerpRiskEngine::new(PerpRiskConfig {
            max_abs_position_qty: Some(10),
            ..PerpRiskConfig::default()
        });

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
            Err(RiskRejectReason::MaxPositionExceeded)
        );
    }

    #[test]
    fn margin_call_and_liquidatable_accounts_only_accept_strict_position_reductions() {
        let mut accounts = PerpAccountStore::new(
            PerpClearingConfig {
                leverage: 10,
                maintenance_margin_ppm: 50_000,
                ..PerpClearingConfig::default()
            },
            100,
        )
        .unwrap();
        accounts.create_account(1, 200);
        accounts.create_account(99, 10_000);
        accounts
            .settle_trade(&Trade {
                trade_id: 1,
                maker_order_id: 10,
                maker_account_id: 99,
                taker_order_id: 11,
                taker_account_id: 1,
                price_tick: 100,
                qty: 10,
                taker_side: Side::Buy,
            })
            .unwrap();
        accounts.set_mark_price_tick(90).unwrap();
        assert_eq!(
            accounts.account_snapshot(1).unwrap().margin_status,
            PerpMarginStatus::MarginCall
        );
        let risk = PerpRiskEngine::new(PerpRiskConfig::default());
        let context = RiskContext {
            best_bid: None,
            best_ask: None,
            fill_quote: FillQuote::default(),
        };

        assert_eq!(
            risk.check(&limit(1, Side::Sell, 90, 5), &accounts, context),
            Err(RiskRejectReason::InsufficientMargin)
        );
        assert_eq!(
            risk.check(
                &Command::NewOrder(NewOrder {
                    order_id: 2,
                    account_id: 1,
                    side: Side::Sell,
                    kind: OrderKind::Market,
                    qty: 5,
                    reduce_only: false,
                }),
                &accounts,
                context,
            ),
            Ok(())
        );
        assert_eq!(
            risk.check(
                &Command::NewOrder(NewOrder {
                    order_id: 3,
                    account_id: 1,
                    side: Side::Sell,
                    kind: OrderKind::Market,
                    qty: 11,
                    reduce_only: false,
                }),
                &accounts,
                context,
            ),
            Err(RiskRejectReason::InsufficientMargin)
        );

        accounts.set_mark_price_tick(80).unwrap();
        assert_eq!(
            accounts.account_snapshot(1).unwrap().margin_status,
            PerpMarginStatus::Liquidatable
        );
        assert_eq!(
            risk.check(
                &reduce_only(1, Side::Sell, OrderKind::Market, 5),
                &accounts,
                context,
            ),
            Ok(())
        );
    }

    #[test]
    fn underwater_account_can_amend_an_order_only_when_portfolio_risk_falls() {
        let mut accounts = PerpAccountStore::new(
            PerpClearingConfig {
                leverage: 10,
                maintenance_margin_ppm: 50_000,
                ..PerpClearingConfig::default()
            },
            100,
        )
        .unwrap();
        accounts.create_account(1, 200);
        accounts.create_account(99, 10_000);
        accounts
            .settle_trade(&Trade {
                trade_id: 1,
                maker_order_id: 10,
                maker_account_id: 99,
                taker_order_id: 11,
                taker_account_id: 1,
                price_tick: 100,
                qty: 10,
                taker_side: Side::Buy,
            })
            .unwrap();
        accounts
            .reserve_resting_order(20, 1, Side::Buy, 100, 5)
            .unwrap();
        accounts.set_mark_price_tick(90).unwrap();
        let risk = PerpRiskEngine::new(PerpRiskConfig::default());
        let context = RiskContext {
            best_bid: None,
            best_ask: None,
            fill_quote: FillQuote::default(),
        };

        assert_eq!(
            risk.check(
                &Command::AmendOrder(crate::model::AmendOrder {
                    order_id: 20,
                    price_tick: None,
                    qty: Some(2),
                }),
                &accounts,
                context,
            ),
            Ok(())
        );
        assert_eq!(
            risk.check(
                &Command::AmendOrder(crate::model::AmendOrder {
                    order_id: 20,
                    price_tick: None,
                    qty: Some(6),
                }),
                &accounts,
                context,
            ),
            Err(RiskRejectReason::InsufficientMargin)
        );
    }

    #[test]
    fn distressed_reduce_only_order_cannot_increase_worst_case_open_order_exposure() {
        let mut accounts = PerpAccountStore::new(
            PerpClearingConfig {
                leverage: 10,
                maintenance_margin_ppm: 50_000,
                ..PerpClearingConfig::default()
            },
            100,
        )
        .unwrap();
        accounts.create_account(1, 200);
        accounts.create_account(99, 10_000);
        accounts
            .settle_trade(&Trade {
                trade_id: 1,
                maker_order_id: 10,
                maker_account_id: 99,
                taker_order_id: 11,
                taker_account_id: 1,
                price_tick: 100,
                qty: 10,
                taker_side: Side::Buy,
            })
            .unwrap();
        accounts
            .reserve_resting_order(20, 1, Side::Sell, 100, 20)
            .unwrap();
        accounts.set_mark_price_tick(90).unwrap();
        let risk = PerpRiskEngine::new(PerpRiskConfig::default());

        assert_eq!(
            risk.check(
                &reduce_only(1, Side::Sell, OrderKind::Market, 5),
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
    fn perp_portfolio_projection_rejects_checked_arithmetic_overflow() {
        let mut accounts = PerpAccountStore::new(PerpClearingConfig::default(), i64::MAX).unwrap();
        accounts.create_account(1, i128::MAX);
        accounts
            .reserve_resting_order(1, 1, Side::Buy, i64::MAX, u64::MAX)
            .unwrap();
        let risk = PerpRiskEngine::new(PerpRiskConfig::default());

        assert_eq!(
            risk.check(
                &limit(1, Side::Buy, i64::MAX, 4),
                &accounts,
                RiskContext {
                    best_bid: None,
                    best_ask: None,
                    fill_quote: FillQuote::default(),
                },
            ),
            Err(RiskRejectReason::MaxOrderNotionalExceeded)
        );
    }
}
