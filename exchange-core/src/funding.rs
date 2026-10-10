//! Deterministic, simulation-time funding for explicitly linked linear perps.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{ClearingError, Money, PerpAccountSnapshot, PriceLinkStatus, model::BookSnapshot};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct FundingConfig {
    pub interval_ms: u64,
    /// Signed rate per funding interval, not an annual interest rate.
    pub base_rate_ppm: i32,
    pub max_rate_ppm: u32,
    /// Minimum observed fraction of the interval. Missing samples are excluded,
    /// never substituted with zero; the settlement boundary must also be live.
    pub min_coverage_ppm: u32,
}

impl Default for FundingConfig {
    fn default() -> Self {
        Self {
            interval_ms: 28_800_000,
            base_rate_ppm: 100,
            max_rate_ppm: 1_000,
            min_coverage_ppm: 900_000,
        }
    }
}

impl FundingConfig {
    pub fn is_valid(&self) -> bool {
        self.interval_ms >= 1_000
            && self.interval_ms.is_multiple_of(1_000)
            && self.max_rate_ppm <= 1_000_000
            && self.base_rate_ppm.unsigned_abs() <= self.max_rate_ppm
            && self.min_coverage_ppm > 0
            && self.min_coverage_ppm <= 1_000_000
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FundingStatus {
    Settled,
    NoPositions,
    SkippedPrices,
    UnbalancedPositions,
}

/// Public receipt; account transfers belong to private clearing events.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FundingSettlement {
    pub instrument_id: String,
    pub funding_time_ms: u64,
    pub interval_ms: u64,
    pub covered_ms: u64,
    pub rate_ppm: i32,
    pub mark_price_tick: i64,
    pub status: FundingStatus,
    #[serde(with = "json_money")]
    pub total_transfer: Money,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FundingSnapshot {
    pub market_time_ms: u64,
    pub interval_ms: u64,
    pub base_rate_ppm: i32,
    pub max_rate_ppm: u32,
    pub min_coverage_ppm: u32,
    pub estimated_rate_ppm: Option<i32>,
    pub next_funding_time_ms: u64,
    pub covered_ms: u64,
    pub last_settlement: Option<FundingSettlement>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct FundingState {
    pub next_funding_time_ms: u64,
    pub covered_ms: u64,
    #[serde(with = "json_money")]
    pub rate_time_sum: i128,
    pub last_settlement: Option<FundingSettlement>,
}

/// Match http.v1's integer-or-decimal-string convention for wide amounts.
/// In particular, raw premium integrals may exceed JSON's u64 number range.
pub(crate) mod json_money {
    use serde::{
        Deserializer, Serializer,
        de::{self, Visitor},
    };
    pub fn serialize<S: Serializer>(value: &i128, serializer: S) -> Result<S::Ok, S::Error> {
        if let Ok(value) = i64::try_from(*value) {
            serializer.serialize_i64(value)
        } else if let Ok(value) = u64::try_from(*value) {
            serializer.serialize_u64(value)
        } else {
            serializer.serialize_str(&value.to_string())
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i128, D::Error> {
        struct Integer;
        impl Visitor<'_> for Integer {
            type Value = i128;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an integer or a base-10 i128 string")
            }
            fn visit_i64<E: de::Error>(self, value: i64) -> Result<i128, E> {
                Ok(value.into())
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> Result<i128, E> {
                Ok(value.into())
            }
            fn visit_i128<E: de::Error>(self, value: i128) -> Result<i128, E> {
                Ok(value)
            }
            fn visit_u128<E: de::Error>(self, value: u128) -> Result<i128, E> {
                value.try_into().map_err(E::custom)
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<i128, E> {
                value.parse().map_err(E::custom)
            }
        }
        deserializer.deserialize_any(Integer)
    }
}

impl FundingState {
    pub fn new(config: &FundingConfig) -> Self {
        Self {
            next_funding_time_ms: config.interval_ms,
            covered_ms: 0,
            rate_time_sum: 0,
            last_settlement: None,
        }
    }
}

pub(crate) fn sample_rate(
    config: &FundingConfig,
    price: &crate::PerpPriceSnapshot,
    book: &BookSnapshot,
) -> Option<i128> {
    if price.status != PriceLinkStatus::Live {
        return None;
    }
    let index = price.index_price_tick.filter(|value| *value > 0)?;
    let (bid, ask) = (book.bids.first()?, book.asks.first()?);
    if bid.price_tick <= 0 || ask.price_tick < bid.price_tick {
        return None;
    }
    // Keep half-tick midpoints until conversion to signed ppm.
    let premium = ((i128::from(bid.price_tick) + i128::from(ask.price_tick)
        - 2 * i128::from(index))
        * 1_000_000)
        / (2 * i128::from(index));
    Some(premium + i128::from(config.base_rate_ppm))
}

/// Payers round down. Receivers split the actual paid total by position size,
/// with largest remainder and ascending account id as the deterministic tie-break.
/// Cash is conserved even when small positions round differently.
#[cfg(test)]
pub(crate) fn funding_allocations(
    accounts: &[PerpAccountSnapshot],
    settlement: &mut FundingSettlement,
) -> Result<BTreeMap<u64, Money>, ClearingError> {
    let legs = funding_leg_allocations(accounts, settlement)?;
    let mut totals = BTreeMap::<u64, Money>::new();
    for ((id, _), delta) in legs {
        let total = totals.entry(id).or_default();
        *total = total
            .checked_add(delta)
            .ok_or(ClearingError::BalanceOverflow)?;
    }
    Ok(totals)
}

pub(crate) fn funding_leg_allocations(
    accounts: &[PerpAccountSnapshot],
    settlement: &mut FundingSettlement,
) -> Result<BTreeMap<(u64, crate::PositionSide), Money>, ClearingError> {
    use crate::PositionSide;
    let legs: Vec<_> = accounts
        .iter()
        .flat_map(|account| {
            if let Some(p) = &account.hedge_positions {
                vec![
                    (account.account_id, PositionSide::Long, p.long.qty),
                    (account.account_id, PositionSide::Short, -p.short.qty),
                ]
            } else {
                vec![(account.account_id, PositionSide::Both, account.position_qty)]
            }
        })
        .collect();
    let mut net = 0i128;
    let mut open = false;
    for (_, _, qty) in &legs {
        net = net
            .checked_add(*qty)
            .ok_or(ClearingError::BalanceOverflow)?;
        open |= *qty != 0;
    }
    if !open {
        settlement.status = FundingStatus::NoPositions;
    } else if net != 0 {
        settlement.status = FundingStatus::UnbalancedPositions;
    }
    if settlement.status != FundingStatus::Settled {
        return Ok(BTreeMap::new());
    }
    if settlement.mark_price_tick <= 0 {
        return Err(ClearingError::InvalidPrice);
    }
    let mut allocations = BTreeMap::new();
    let mut receivers = Vec::new();
    let mut receiver_qty = 0i128;
    let mut total = 0i128;
    for (id, side, signed_qty) in legs.into_iter().filter(|(_, _, qty)| *qty != 0) {
        let qty = signed_qty
            .checked_abs()
            .ok_or(ClearingError::NotionalOverflow)?;
        let pays = (signed_qty > 0) == (settlement.rate_ppm >= 0);
        if pays {
            let payment = qty
                .checked_mul(i128::from(settlement.mark_price_tick))
                .and_then(|value| value.checked_mul(i128::from(settlement.rate_ppm.unsigned_abs())))
                .ok_or(ClearingError::NotionalOverflow)?
                / 1_000_000;
            total = total
                .checked_add(payment)
                .ok_or(ClearingError::BalanceOverflow)?;
            allocations.insert((id, side), -payment);
        } else {
            receiver_qty = receiver_qty
                .checked_add(qty)
                .ok_or(ClearingError::NotionalOverflow)?;
            receivers.push(((id, side), qty));
        }
    }
    let mut allocated = 0i128;
    let mut remainders = Vec::new();
    for (id, qty) in receivers {
        let numerator = total
            .checked_mul(qty)
            .ok_or(ClearingError::NotionalOverflow)?;
        let share = numerator / receiver_qty;
        allocations.insert(id, share);
        allocated = allocated
            .checked_add(share)
            .ok_or(ClearingError::BalanceOverflow)?;
        remainders.push((numerator % receiver_qty, id));
    }
    remainders.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let leftover =
        usize::try_from(total - allocated).map_err(|_| ClearingError::BalanceOverflow)?;
    for (_, id) in remainders.into_iter().take(leftover) {
        *allocations.get_mut(&id).expect("receiver allocation") += 1;
    }
    settlement.total_transfer = total;
    Ok(allocations)
}
