use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    market::AssetId,
    model::{AccountId, PriceTick, Qty},
};

pub type Money = i128;
pub type PositionQty = i128;
pub type FeeRatePpm = u32;

const FEE_DENOMINATOR_PPM: Money = 1_000_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClearingError {
    InvalidPrice,
    InvalidLeverage,
    InvalidMarginRate,
    InvalidFeeRate,
    AccountNotFound,
    AccountNotLiquidatable,
    InvalidLiquidationQuantity,
    LiquidationUnfilled,
    WrongMarketKind,
    NotionalOverflow,
    BalanceOverflow,
    InsufficientAvailableBalance,
    ReservationUnderflow,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct VenueAccountStore {
    balances: BTreeMap<AccountId, BTreeMap<AssetId, VenueAssetBalance>>,
}

impl VenueAccountStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_balance(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<AssetId>,
        total: Money,
    ) -> VenueBalanceSnapshot {
        let asset_id = asset_id.into();
        let balance = self.balance_mut(account_id, asset_id.clone());
        balance.total = total;
        if balance.total < 0 {
            balance.reserved = 0;
        } else if balance.reserved > balance.total {
            balance.reserved = balance.total;
        }
        balance.snapshot(account_id, asset_id)
    }

    pub fn apply_delta(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<AssetId>,
        delta: Money,
    ) -> Result<VenueBalanceSnapshot, VenueAccountError> {
        let asset_id = asset_id.into();
        let balance = self.balance_mut(account_id, asset_id.clone());
        let next_total = balance
            .total
            .checked_add(delta)
            .ok_or(VenueAccountError::BalanceOverflow)?;
        if next_total < balance.reserved {
            return Err(VenueAccountError::InsufficientAvailableBalance);
        }
        balance.total = next_total;
        Ok(balance.snapshot(account_id, asset_id))
    }

    pub fn apply_signed_delta(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<AssetId>,
        delta: Money,
    ) -> Result<VenueBalanceSnapshot, VenueAccountError> {
        let asset_id = asset_id.into();
        let balance = self.balance_mut(account_id, asset_id.clone());
        balance.total = balance
            .total
            .checked_add(delta)
            .ok_or(VenueAccountError::BalanceOverflow)?;
        Ok(balance.snapshot(account_id, asset_id))
    }

    pub fn reserve(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<AssetId>,
        amount: Money,
    ) -> Result<VenueBalanceSnapshot, VenueAccountError> {
        if amount < 0 {
            return Err(VenueAccountError::NegativeAmount);
        }
        let asset_id = asset_id.into();
        let balance = self.balance_mut(account_id, asset_id.clone());
        if balance.available() < amount {
            return Err(VenueAccountError::InsufficientAvailableBalance);
        }
        balance.reserved = balance
            .reserved
            .checked_add(amount)
            .ok_or(VenueAccountError::BalanceOverflow)?;
        Ok(balance.snapshot(account_id, asset_id))
    }

    pub fn release(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<AssetId>,
        amount: Money,
    ) -> Result<VenueBalanceSnapshot, VenueAccountError> {
        if amount < 0 {
            return Err(VenueAccountError::NegativeAmount);
        }
        let asset_id = asset_id.into();
        let balance = self.balance_mut(account_id, asset_id.clone());
        if balance.reserved < amount {
            return Err(VenueAccountError::InsufficientReservedBalance);
        }
        balance.reserved -= amount;
        Ok(balance.snapshot(account_id, asset_id))
    }

    pub fn balance_snapshot(
        &self,
        account_id: AccountId,
        asset_id: &str,
    ) -> Option<VenueBalanceSnapshot> {
        self.balances
            .get(&account_id)
            .and_then(|balances| balances.get(asset_id))
            .map(|balance| balance.snapshot(account_id, asset_id.to_string()))
    }

    pub fn account_snapshot(&self, account_id: AccountId) -> VenueAccountSnapshot {
        let balances = self
            .balances
            .get(&account_id)
            .map(|balances| {
                balances
                    .iter()
                    .map(|(asset_id, balance)| balance.snapshot(account_id, asset_id.clone()))
                    .collect()
            })
            .unwrap_or_default();

        VenueAccountSnapshot {
            account_id,
            balances,
        }
    }

    pub fn account_snapshots(&self) -> Vec<VenueAccountSnapshot> {
        self.balances
            .keys()
            .map(|account_id| self.account_snapshot(*account_id))
            .collect()
    }

    pub fn asset_ids(&self) -> Vec<&str> {
        self.balances
            .values()
            .flat_map(|balances| balances.keys().map(String::as_str))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn validate(&self) -> Result<(), VenueAccountError> {
        if self
            .balances
            .values()
            .flat_map(|balances| balances.values())
            .any(|balance| balance.total < balance.reserved || balance.reserved < 0)
        {
            return Err(VenueAccountError::InsufficientAvailableBalance);
        }
        Ok(())
    }

    fn balance_mut(&mut self, account_id: AccountId, asset_id: AssetId) -> &mut VenueAssetBalance {
        self.balances
            .entry(account_id)
            .or_default()
            .entry(asset_id)
            .or_default()
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct VenueAssetBalance {
    pub total: Money,
    pub reserved: Money,
}

impl VenueAssetBalance {
    pub fn available(&self) -> Money {
        self.total - self.reserved
    }

    fn snapshot(&self, account_id: AccountId, asset_id: AssetId) -> VenueBalanceSnapshot {
        VenueBalanceSnapshot {
            account_id,
            asset_id,
            total: self.total,
            reserved: self.reserved,
            available: self.available(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VenueBalanceSnapshot {
    pub account_id: AccountId,
    pub asset_id: AssetId,
    pub total: Money,
    pub reserved: Money,
    pub available: Money,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VenueAccountSnapshot {
    pub account_id: AccountId,
    pub balances: Vec<VenueBalanceSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum VenueAccountError {
    BalanceOverflow,
    NegativeAmount,
    InsufficientAvailableBalance,
    InsufficientReservedBalance,
}

pub(crate) fn notional(price_tick: PriceTick, qty: Qty) -> Result<Money, ClearingError> {
    if price_tick <= 0 {
        return Err(ClearingError::InvalidPrice);
    }

    Money::from(price_tick)
        .checked_mul(Money::from(qty))
        .ok_or(ClearingError::NotionalOverflow)
}

pub(crate) fn fee_for(notional: Money, fee_rate_ppm: FeeRatePpm) -> Result<Money, ClearingError> {
    if fee_rate_ppm > 1_000_000 {
        return Err(ClearingError::InvalidFeeRate);
    }
    let rate = Money::from(fee_rate_ppm);
    let whole = notional / FEE_DENOMINATOR_PPM;
    let remainder = notional % FEE_DENOMINATOR_PPM;
    whole
        .checked_mul(rate)
        .and_then(|fee| {
            remainder.checked_mul(rate).and_then(|scaled_remainder| {
                fee.checked_add(scaled_remainder / FEE_DENOMINATOR_PPM)
            })
        })
        .ok_or(ClearingError::NotionalOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn venue_account_store_reserves_releases_and_snapshots_assets() {
        let mut store = VenueAccountStore::new();

        let initial = store.set_balance(10, "USDT", 1_000);
        assert_eq!(initial.available, 1_000);

        let reserved = store.reserve(10, "USDT", 250).unwrap();
        assert_eq!(reserved.reserved, 250);
        assert_eq!(reserved.available, 750);

        assert_eq!(
            store.reserve(10, "USDT", 751),
            Err(VenueAccountError::InsufficientAvailableBalance)
        );

        let released = store.release(10, "USDT", 100).unwrap();
        assert_eq!(released.reserved, 150);
        assert_eq!(released.available, 850);

        let updated = store.apply_delta(10, "USDT", -500).unwrap();
        assert_eq!(updated.total, 500);
        assert_eq!(updated.available, 350);
        assert_eq!(
            store.apply_delta(10, "USDT", -351),
            Err(VenueAccountError::InsufficientAvailableBalance)
        );

        let signed = store.apply_signed_delta(10, "USDT", -700).unwrap();
        assert_eq!(signed.total, -200);
        assert_eq!(signed.reserved, 150);
        assert_eq!(signed.available, -350);
        assert_eq!(store.asset_ids(), vec!["USDT"]);
    }

    #[test]
    fn fee_math_handles_extreme_notionals_without_intermediate_overflow() {
        assert_eq!(fee_for(Money::MAX, 1_000_000), Ok(Money::MAX));
        assert_eq!(
            fee_for(1_000, 1_000_001),
            Err(ClearingError::InvalidFeeRate)
        );
    }
}
