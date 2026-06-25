use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{account::Money, market::AssetId, model::AccountId};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PortfolioStore {
    balances: BTreeMap<AccountId, BTreeMap<AssetId, PortfolioAssetBalance>>,
}

impl PortfolioStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_balance(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<AssetId>,
        total: Money,
    ) -> PortfolioBalanceSnapshot {
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
    ) -> Result<PortfolioBalanceSnapshot, PortfolioError> {
        let asset_id = asset_id.into();
        let balance = self.balance_mut(account_id, asset_id.clone());
        let next_total = balance
            .total
            .checked_add(delta)
            .ok_or(PortfolioError::BalanceOverflow)?;
        if next_total < balance.reserved {
            return Err(PortfolioError::InsufficientAvailableBalance);
        }
        balance.total = next_total;
        Ok(balance.snapshot(account_id, asset_id))
    }

    pub fn reserve(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<AssetId>,
        amount: Money,
    ) -> Result<PortfolioBalanceSnapshot, PortfolioError> {
        if amount < 0 {
            return Err(PortfolioError::NegativeAmount);
        }
        let asset_id = asset_id.into();
        let balance = self.balance_mut(account_id, asset_id.clone());
        if balance.available() < amount {
            return Err(PortfolioError::InsufficientAvailableBalance);
        }
        balance.reserved += amount;
        Ok(balance.snapshot(account_id, asset_id))
    }

    pub fn release(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<AssetId>,
        amount: Money,
    ) -> Result<PortfolioBalanceSnapshot, PortfolioError> {
        if amount < 0 {
            return Err(PortfolioError::NegativeAmount);
        }
        let asset_id = asset_id.into();
        let balance = self.balance_mut(account_id, asset_id.clone());
        if balance.reserved < amount {
            return Err(PortfolioError::InsufficientReservedBalance);
        }
        balance.reserved -= amount;
        Ok(balance.snapshot(account_id, asset_id))
    }

    pub fn balance_snapshot(
        &self,
        account_id: AccountId,
        asset_id: &str,
    ) -> Option<PortfolioBalanceSnapshot> {
        self.balances
            .get(&account_id)
            .and_then(|balances| balances.get(asset_id))
            .map(|balance| balance.snapshot(account_id, asset_id.to_string()))
    }

    pub fn account_snapshot(&self, account_id: AccountId) -> PortfolioAccountSnapshot {
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

        PortfolioAccountSnapshot {
            account_id,
            balances,
        }
    }

    pub fn account_snapshots(&self) -> Vec<PortfolioAccountSnapshot> {
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

    fn balance_mut(
        &mut self,
        account_id: AccountId,
        asset_id: AssetId,
    ) -> &mut PortfolioAssetBalance {
        self.balances
            .entry(account_id)
            .or_default()
            .entry(asset_id)
            .or_default()
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PortfolioAssetBalance {
    pub total: Money,
    pub reserved: Money,
}

impl PortfolioAssetBalance {
    pub fn available(&self) -> Money {
        self.total - self.reserved
    }

    fn snapshot(&self, account_id: AccountId, asset_id: AssetId) -> PortfolioBalanceSnapshot {
        PortfolioBalanceSnapshot {
            account_id,
            asset_id,
            total: self.total,
            reserved: self.reserved,
            available: self.available(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PortfolioBalanceSnapshot {
    pub account_id: AccountId,
    pub asset_id: AssetId,
    pub total: Money,
    pub reserved: Money,
    pub available: Money,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PortfolioAccountSnapshot {
    pub account_id: AccountId,
    pub balances: Vec<PortfolioBalanceSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PortfolioError {
    BalanceOverflow,
    NegativeAmount,
    InsufficientAvailableBalance,
    InsufficientReservedBalance,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portfolio_store_reserves_releases_and_applies_deltas() {
        let mut store = PortfolioStore::new();

        let initial = store.set_balance(10, "USDT", 1_000);
        assert_eq!(initial.available, 1_000);

        let reserved = store.reserve(10, "USDT", 400).unwrap();
        assert_eq!(reserved.reserved, 400);
        assert_eq!(reserved.available, 600);

        assert_eq!(
            store.apply_delta(10, "USDT", -700),
            Err(PortfolioError::InsufficientAvailableBalance)
        );

        let released = store.release(10, "USDT", 150).unwrap();
        assert_eq!(released.reserved, 250);

        let debited = store.apply_delta(10, "USDT", -500).unwrap();
        assert_eq!(debited.total, 500);
        assert_eq!(debited.available, 250);
        assert_eq!(store.asset_ids(), vec!["USDT"]);
    }
}
