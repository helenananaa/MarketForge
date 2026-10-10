use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

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

/// Derived reconciliation inputs. Unknown after restore or untracked replacement;
/// only a successful full reconciliation establishes a known baseline.
#[derive(Clone, Debug, Default)]
pub(crate) struct ReservationChanges(Option<BTreeSet<AccountId>>);

impl ReservationChanges {
    pub(crate) fn mark(&mut self, id: AccountId) {
        if let Some(ids) = &mut self.0 {
            ids.insert(id);
        }
    }

    pub(crate) fn invalidate(&mut self) {
        self.0 = None;
    }

    pub(crate) fn accounts(&self) -> Option<&BTreeSet<AccountId>> {
        self.0.as_ref()
    }

    pub(crate) fn reconciled(&mut self, affected: Option<&BTreeSet<AccountId>>) {
        match affected {
            None => self.0 = Some(BTreeSet::new()),
            Some(affected) => {
                if let Some(ids) = &mut self.0 {
                    ids.retain(|id| !affected.contains(id));
                }
            }
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct VenueAccountStore {
    #[serde(skip)]
    pub(crate) reservation_changes: ReservationChanges,
    // Candidate transactions share untouched accounts; mutations detach only
    // the selected account. Serde retains the existing plain map wire format.
    balances: crate::shared_map::SharedMap<AccountId, Arc<BTreeMap<AssetId, VenueAssetBalance>>>,
}

impl VenueAccountStore {
    pub(crate) fn account_count(&self) -> usize {
        self.balances.len()
    }

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

    /// Internal risk reads need amounts, not owned public snapshot labels.
    pub(crate) fn available_balance(&self, account_id: AccountId, asset_id: &str) -> Option<Money> {
        self.balances
            .get(&account_id)
            .and_then(|balances| balances.get(asset_id))
            .map(VenueAssetBalance::available)
    }

    pub(crate) fn balance_entries(
        &self,
    ) -> impl Iterator<Item = (AccountId, &str, &VenueAssetBalance)> {
        self.balances.iter().flat_map(|(&account, balances)| {
            balances
                .iter()
                .map(move |(asset, balance)| (account, asset.as_str(), balance))
        })
    }

    /// Account-key order, with arithmetic deferred until a requested account
    /// is selected. Skipping a peer must not evaluate its available balance.
    pub(crate) fn balances_for_asset<'a>(
        &'a self,
        asset: &'a str,
    ) -> impl Iterator<Item = (AccountId, &'a VenueAssetBalance)> + 'a {
        self.balances
            .iter()
            .filter_map(move |(&id, balances)| balances.get(asset).map(|balance| (id, balance)))
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
        self.reservation_changes.mark(account_id);
        Arc::make_mut(self.balances.entry(account_id).or_default())
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
    #[test]
    fn venue_candidate_detaches_only_changed_accounts_and_restores_plain_maps() {
        let mut store = super::VenueAccountStore::new();
        for id in 1..=3 {
            store.set_balance(id, "USD", 1000);
            store.set_balance(id, "BTC", 10);
        }
        let before = serde_json::to_value(&store).unwrap();
        let mut candidate = store.clone();
        assert!(store.balances.shares_storage(&candidate.balances));
        candidate.available_balance(1, "USD");
        assert!(store.balances.shares_storage(&candidate.balances));
        candidate.reserve(1, "USD", 100).unwrap();
        candidate.apply_delta(2, "BTC", 2).unwrap();
        assert_eq!(serde_json::to_value(&store).unwrap(), before);
        assert!(!store.balances.shares_storage(&candidate.balances));
        assert!(!Arc::ptr_eq(&store.balances[&1], &candidate.balances[&1]));
        assert!(!Arc::ptr_eq(&store.balances[&2], &candidate.balances[&2]));
        assert!(Arc::ptr_eq(&store.balances[&3], &candidate.balances[&3]));
        assert!(candidate.reserve(3, "USD", 2000).is_err());
        assert_eq!(serde_json::to_value(&store).unwrap(), before);
        let restored: super::VenueAccountStore = serde_json::from_value(before.clone()).unwrap();
        assert_eq!(serde_json::to_value(&restored).unwrap(), before);
        assert_eq!(restored.account_snapshots(), store.account_snapshots());
        assert!(restored.reservation_changes.accounts().is_none());
        // A previous plain-map document is the same schema, without Arc tags.
        let legacy = serde_json::json!({"balances":{"7":{"USD":{"total":99,"reserved":9}}}});
        let restored: super::VenueAccountStore = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(serde_json::to_value(restored).unwrap(), legacy);
    }

    #[test]
    #[ignore = "controlled release account-copy comparison; run alone with --nocapture"]
    fn venue_candidate_copy_fixed_work_benchmark() {
        use std::{collections::BTreeMap, hint::black_box, time::Instant};
        let mut store = super::VenueAccountStore::new();
        for id in 1..=1000 {
            store.set_balance(id, "USD", 1000);
            store.set_balance(id, "BTC", 10);
        }
        let plain: BTreeMap<super::AccountId, BTreeMap<String, super::VenueAssetBalance>> = store
            .balances
            .iter()
            .map(|(&id, balances)| (id, (**balances).clone()))
            .collect();
        for cow in [false, true, true, false, false, true, true, false] {
            let start = Instant::now();
            for _ in 0..1000 {
                if cow {
                    let mut copy = black_box(&store).clone();
                    copy.reserve(1, "USD", 1).unwrap();
                    copy.reserve(2, "BTC", 1).unwrap();
                    black_box(copy);
                } else {
                    let mut copy = black_box(&plain).clone();
                    copy.get_mut(&1).unwrap().get_mut("USD").unwrap().reserved += 1;
                    copy.get_mut(&2).unwrap().get_mut("BTC").unwrap().reserved += 1;
                    black_box(copy);
                }
            }
            println!("cow={cow} seconds={:.6}", start.elapsed().as_secs_f64());
        }
        assert_eq!(store.balance_snapshot(1, "USD").unwrap().reserved, 0);
    }

    use super::*;

    #[test]
    fn ordered_asset_balances_skip_other_assets_and_defer_available_arithmetic() {
        let mut store = VenueAccountStore::new();
        store.set_balance(1, "USD", 100);
        store.set_balance(2, "OTHER", 200);
        store.set_balance(3, "USD", Money::MIN);
        store.balance_mut(3, "USD".into()).reserved = 1;
        let rows = store
            .balances_for_asset("USD")
            .map(|(id, balance)| (id, balance.total, balance.reserved))
            .collect::<Vec<_>>();
        assert_eq!(rows, vec![(1, 100, 0), (3, Money::MIN, 1)]);
        assert_eq!(
            store
                .balances_for_asset("USD")
                .next()
                .unwrap()
                .1
                .available(),
            100
        );
        assert!(store.balances_for_asset("MISSING").next().is_none());
    }

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
