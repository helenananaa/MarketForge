use crate::model::{PriceTick, Qty};

pub type Money = i128;
pub type PositionQty = i128;
pub type FeeRatePpm = u32;

const FEE_DENOMINATOR_PPM: Money = 1_000_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClearingError {
    InvalidPrice,
    InvalidLeverage,
    NotionalOverflow,
}

pub(crate) fn notional(price_tick: PriceTick, qty: Qty) -> Result<Money, ClearingError> {
    if price_tick <= 0 {
        return Err(ClearingError::InvalidPrice);
    }

    Money::from(price_tick)
        .checked_mul(Money::from(qty))
        .ok_or(ClearingError::NotionalOverflow)
}

pub(crate) fn fee_for(notional: Money, fee_rate_ppm: FeeRatePpm) -> Money {
    notional * Money::from(fee_rate_ppm) / FEE_DENOMINATOR_PPM
}
