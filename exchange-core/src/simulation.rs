use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    account::{Money, VenueAccountSnapshot},
    actor::{
        AccountSnapshot, AccountSnapshots, ActorExecution, ActorRejectReason, ExchangeActor,
        MarketStatus, RoomId,
    },
    clock::SimulationClock,
    market::{
        ExchangeConfig, MarketConfig, MarketConfigError, MarketKind, VenueAssetPolicyConfig,
        VenueId,
    },
    model::{AccountId, BookSnapshot, Command, OrderId},
    portfolio::{PortfolioAccountSnapshot, PortfolioStore},
    scenario::{
        ScenarioAccount, ScenarioConfig, ScenarioError, ScenarioPortfolio, ScenarioSeedOrder,
    },
    transfer::{VenueTransfer, VenueTransferKind, VenueTransferRejectReason, VenueTransferStatus},
    venue_rules::VenueRuleConfig,
};

pub type UserId = String;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SimulationRoom {
    room_id: RoomId,
    primary_venue_id: VenueId,
    exchanges: BTreeMap<VenueId, ExchangeActor>,
    portfolios: PortfolioStore,
    #[serde(default)]
    user_accounts: BTreeMap<UserId, BTreeSet<AccountId>>,
    #[serde(default)]
    asset_ledger: Vec<AssetLedgerEntry>,
    #[serde(default)]
    next_asset_ledger_seq: u64,
    #[serde(default)]
    pending_venue_transfers: Vec<PendingVenueTransfer>,
}

impl SimulationRoom {
    pub fn from_scenario(scenario: ScenarioConfig) -> Result<SimulationBootstrap, ScenarioError> {
        let room_id = scenario.room_id.clone();
        let primary_venue_id = scenario.market.venue_id().to_string();
        let mut markets_by_venue = BTreeMap::<VenueId, Vec<MarketConfig>>::new();
        markets_by_venue
            .entry(primary_venue_id.clone())
            .or_default()
            .push(scenario.market.clone());
        for market in &scenario.extra_markets {
            markets_by_venue
                .entry(market.venue_id().to_string())
                .or_default()
                .push(market.clone());
        }

        let mut exchanges = BTreeMap::new();
        for (venue_id, markets) in markets_by_venue {
            let mut config = ExchangeConfig::new(venue_id.clone(), markets)
                .map_err(ScenarioError::MarketConfig)?;
            config.merge_asset_metadata(&scenario.assets);
            config.venue_rules = rules_for_exchange(&scenario, &config)?;
            config.asset_policy = asset_policy_for_exchange(&scenario, &config);
            let mut exchange =
                ExchangeActor::new(room_id.clone(), config).map_err(ScenarioError::MarketConfig)?;
            for account in &scenario.accounts {
                apply_account_to_exchange(account, &mut exchange)?;
            }
            exchanges.insert(venue_id, exchange);
        }

        let mut room = Self {
            room_id: room_id.clone(),
            primary_venue_id,
            exchanges,
            portfolios: PortfolioStore::new(),
            user_accounts: scenario_user_accounts(&scenario),
            asset_ledger: Vec::new(),
            next_asset_ledger_seq: 0,
            pending_venue_transfers: Vec::new(),
        };

        for portfolio in &scenario.initial_portfolios {
            room.apply_initial_portfolio(portfolio);
        }
        for allocation in &scenario.initial_allocations {
            let venue_id = room.primary_venue_id.clone();
            room.apply_initial_allocation(
                &venue_id,
                allocation.account_id,
                allocation.asset_id.clone(),
                allocation.amount,
            )?;
        }
        for allocation in &scenario.routed_initial_allocations {
            room.apply_initial_allocation(
                &allocation.venue_id,
                allocation.account_id,
                allocation.asset_id.clone(),
                allocation.amount,
            )?;
        }

        let mut seed_executions = Vec::with_capacity(scenario.seed_order_count());
        for command in scenario.seed_orders {
            seed_executions.push(room.apply(command));
        }
        for seed_order in scenario.routed_seed_orders {
            seed_executions.push(room.apply_seed_order(seed_order)?);
        }

        Ok(SimulationBootstrap {
            room,
            seed_executions,
        })
    }

    pub fn from_exchange(exchange: ExchangeActor) -> Self {
        let room_id = exchange.room_id().to_string();
        let primary_venue_id = exchange.venue_id().to_string();
        let portfolios = exchange.portfolio_store().clone();
        let mut exchanges = BTreeMap::new();
        exchanges.insert(primary_venue_id.clone(), exchange);
        Self {
            room_id,
            primary_venue_id,
            exchanges,
            portfolios,
            user_accounts: BTreeMap::new(),
            asset_ledger: Vec::new(),
            next_asset_ledger_seq: 0,
            pending_venue_transfers: Vec::new(),
        }
    }

    pub fn room_id(&self) -> &str {
        &self.room_id
    }

    pub fn primary_venue_id(&self) -> &str {
        &self.primary_venue_id
    }

    pub fn next_command_seq(&self) -> crate::actor::ActorSeq {
        self.primary_exchange().next_command_seq()
    }

    pub fn venue_ids(&self) -> Vec<&str> {
        self.exchanges.keys().map(String::as_str).collect()
    }

    pub fn primary_exchange(&self) -> &ExchangeActor {
        self.exchanges
            .get(&self.primary_venue_id)
            .expect("simulation room must have a primary exchange")
    }

    pub fn primary_exchange_mut(&mut self) -> &mut ExchangeActor {
        self.exchanges
            .get_mut(&self.primary_venue_id)
            .expect("simulation room must have a primary exchange")
    }

    pub fn exchange(&self, venue_id: &str) -> Result<&ExchangeActor, SimulationRoomError> {
        self.exchanges
            .get(venue_id)
            .ok_or_else(|| SimulationRoomError::VenueNotFound {
                venue_id: venue_id.to_string(),
            })
    }

    pub fn status(&self) -> MarketStatus {
        self.primary_exchange().status()
    }

    pub fn pause(&mut self) {
        for exchange in self.exchanges.values_mut() {
            exchange.pause();
        }
    }

    pub fn resume(&mut self) {
        for exchange in self.exchanges.values_mut() {
            exchange.resume();
        }
    }

    pub fn close(&mut self) {
        for exchange in self.exchanges.values_mut() {
            exchange.close();
        }
    }

    pub fn restore_status(&mut self, status: MarketStatus) {
        for exchange in self.exchanges.values_mut() {
            exchange.restore_status(status);
        }
    }

    pub fn apply(&mut self, command: Command) -> ActorExecution {
        self.primary_exchange_mut().apply(command)
    }

    pub fn apply_to_instrument(
        &mut self,
        instrument_id: &str,
        command: Command,
    ) -> Result<ActorExecution, ActorRejectReason> {
        let venue_id = self.venue_id_for_instrument(instrument_id).ok_or_else(|| {
            ActorRejectReason::InstrumentNotFound {
                instrument_id: instrument_id.to_string(),
            }
        })?;
        self.exchanges
            .get_mut(&venue_id)
            .expect("venue id was resolved from exchanges")
            .apply_to_instrument(instrument_id, command)
    }

    pub fn liquidate_account(
        &mut self,
        instrument_id: &str,
        account_id: AccountId,
        order_id: OrderId,
    ) -> Result<ActorExecution, ActorRejectReason> {
        let venue_id = self.venue_id_for_instrument(instrument_id).ok_or_else(|| {
            ActorRejectReason::InstrumentNotFound {
                instrument_id: instrument_id.to_string(),
            }
        })?;
        self.exchanges
            .get_mut(&venue_id)
            .expect("venue id was resolved from exchanges")
            .liquidate_account(instrument_id, account_id, order_id)
    }

    pub fn book_snapshot(&self) -> BookSnapshot {
        self.primary_exchange().book_snapshot()
    }

    pub fn book_snapshot_for(
        &self,
        instrument_id: &str,
    ) -> Result<BookSnapshot, ActorRejectReason> {
        let venue_id = self.venue_id_for_instrument(instrument_id).ok_or_else(|| {
            ActorRejectReason::InstrumentNotFound {
                instrument_id: instrument_id.to_string(),
            }
        })?;
        self.exchanges
            .get(&venue_id)
            .expect("venue id was resolved from exchanges")
            .book_snapshot_for(instrument_id)
    }

    pub fn account_snapshot(&self, account_id: AccountId) -> Option<AccountSnapshot> {
        self.primary_exchange().account_snapshot(account_id)
    }

    pub fn account_snapshot_for(
        &self,
        instrument_id: &str,
        account_id: AccountId,
    ) -> Result<Option<AccountSnapshot>, ActorRejectReason> {
        let venue_id = self.venue_id_for_instrument(instrument_id).ok_or_else(|| {
            ActorRejectReason::InstrumentNotFound {
                instrument_id: instrument_id.to_string(),
            }
        })?;
        self.exchanges
            .get(&venue_id)
            .expect("venue id was resolved from exchanges")
            .account_snapshot_for(instrument_id, account_id)
    }

    pub fn account_snapshots(&self) -> AccountSnapshots {
        self.primary_exchange().account_snapshots()
    }

    pub fn account_snapshots_for(
        &self,
        instrument_id: &str,
    ) -> Result<AccountSnapshots, ActorRejectReason> {
        let venue_id = self.venue_id_for_instrument(instrument_id).ok_or_else(|| {
            ActorRejectReason::InstrumentNotFound {
                instrument_id: instrument_id.to_string(),
            }
        })?;
        self.exchanges
            .get(&venue_id)
            .expect("venue id was resolved from exchanges")
            .account_snapshots_for(instrument_id)
    }

    pub fn order_owner_for(
        &self,
        instrument_id: &str,
        order_id: OrderId,
    ) -> Result<Option<AccountId>, ActorRejectReason> {
        let venue_id = self.venue_id_for_instrument(instrument_id).ok_or_else(|| {
            ActorRejectReason::InstrumentNotFound {
                instrument_id: instrument_id.to_string(),
            }
        })?;
        self.exchanges
            .get(&venue_id)
            .expect("venue id was resolved from exchanges")
            .order_owner_for(instrument_id, order_id)
    }

    pub fn venue_account_snapshot(&self, account_id: AccountId) -> VenueAccountSnapshot {
        self.primary_exchange().venue_account_snapshot(account_id)
    }

    pub fn venue_account_snapshots(&self) -> Vec<VenueAccountSnapshot> {
        self.primary_exchange().venue_account_snapshots()
    }

    pub fn venue_account_snapshots_by_venue(&self) -> Vec<VenueAccountVenueSnapshot> {
        self.exchanges
            .iter()
            .flat_map(|(venue_id, exchange)| {
                exchange
                    .venue_account_snapshots()
                    .into_iter()
                    .map(|account| VenueAccountVenueSnapshot {
                        venue_id: venue_id.clone(),
                        account,
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    pub fn portfolio_snapshot(&self, account_id: AccountId) -> PortfolioAccountSnapshot {
        self.portfolios.account_snapshot(account_id)
    }

    pub fn portfolio_snapshots(&self) -> Vec<PortfolioAccountSnapshot> {
        self.portfolios.account_snapshots()
    }

    pub fn clock(&self) -> SimulationClock {
        self.primary_exchange().clock()
    }

    pub fn advance_clock(&mut self, steps: u64) -> Vec<VenueTransfer> {
        let mut completed = Vec::new();
        let venue_ids = self.exchanges.keys().cloned().collect::<Vec<_>>();
        for venue_id in venue_ids {
            let due = self
                .exchanges
                .get_mut(&venue_id)
                .expect("venue id was collected from exchanges")
                .advance_clock_venue_only(steps);
            for transfer in &due {
                self.apply_portfolio_effect_for_finished_transfer(&venue_id, transfer);
            }
            let triggered = self.process_completed_venue_transfer_links(&venue_id, &due);
            completed.extend(due);
            completed.extend(triggered);
        }
        completed
    }

    pub fn submit_deposit(
        &mut self,
        venue_id: Option<&str>,
        account_id: AccountId,
        asset_id: impl Into<String>,
        amount: Money,
    ) -> Result<VenueTransfer, SimulationRoomError> {
        let venue_id = venue_id.unwrap_or(&self.primary_venue_id).to_string();
        let asset_id = asset_id.into();
        if amount > 0
            && self
                .portfolios
                .reserve(account_id, asset_id.clone(), amount)
                .is_err()
        {
            let transfer = self.exchange_mut(&venue_id)?.submit_rejected_venue_deposit(
                account_id,
                asset_id,
                amount,
                VenueTransferRejectReason::InsufficientPortfolioBalance,
            );
            return Ok(transfer);
        }

        let transfer = self
            .exchange_mut(&venue_id)?
            .submit_venue_deposit(account_id, asset_id, amount);
        if transfer.status != VenueTransferStatus::Pending {
            self.apply_portfolio_effect_for_finished_transfer(&venue_id, &transfer);
        }
        Ok(transfer)
    }

    pub fn submit_withdrawal(
        &mut self,
        venue_id: Option<&str>,
        account_id: AccountId,
        asset_id: impl Into<String>,
        amount: Money,
    ) -> Result<VenueTransfer, SimulationRoomError> {
        let venue_id = venue_id.unwrap_or(&self.primary_venue_id).to_string();
        let transfer = self
            .exchange_mut(&venue_id)?
            .submit_venue_withdrawal(account_id, asset_id, amount);
        if transfer.status != VenueTransferStatus::Pending {
            self.apply_portfolio_effect_for_finished_transfer(&venue_id, &transfer);
        }
        Ok(transfer)
    }

    pub fn submit_venue_to_venue_transfer(
        &mut self,
        from_venue_id: &str,
        to_venue_id: &str,
        account_id: AccountId,
        asset_id: impl Into<String>,
        amount: Money,
    ) -> Result<VenueToVenueTransfer, SimulationRoomError> {
        if !self.exchanges.contains_key(to_venue_id) {
            return Err(SimulationRoomError::VenueNotFound {
                venue_id: to_venue_id.to_string(),
            });
        }
        let asset_id = asset_id.into();
        let withdrawal =
            self.submit_withdrawal(Some(from_venue_id), account_id, asset_id.clone(), amount)?;
        let mut deposit = None;
        if withdrawal.status == VenueTransferStatus::Completed {
            deposit = Some(self.submit_deposit(Some(to_venue_id), account_id, asset_id, amount)?);
        } else if withdrawal.status == VenueTransferStatus::Pending {
            self.pending_venue_transfers.push(PendingVenueTransfer {
                from_venue_id: from_venue_id.to_string(),
                to_venue_id: to_venue_id.to_string(),
                account_id,
                asset_id,
                amount,
                withdrawal_transfer_id: withdrawal.transfer_id,
            });
        }
        Ok(VenueToVenueTransfer {
            from_venue_id: from_venue_id.to_string(),
            to_venue_id: to_venue_id.to_string(),
            withdrawal,
            deposit,
        })
    }

    pub fn transfers(&self) -> Vec<VenueTransfer> {
        self.primary_exchange().transfers()
    }

    pub(crate) fn normalize_after_restore(&mut self) -> Result<(), ActorRejectReason> {
        for exchange in self.exchanges.values_mut() {
            exchange
                .normalize_after_restore()
                .map_err(ActorRejectReason::Clearing)?;
        }
        Ok(())
    }

    pub fn asset_ledger(&self) -> &[AssetLedgerEntry] {
        &self.asset_ledger
    }

    pub fn net_worth_snapshot(&self) -> RoomNetWorthSnapshot {
        let mut account_ids = BTreeSet::new();
        for account in self.portfolios.account_snapshots() {
            account_ids.insert(account.account_id);
        }
        for exchange in self.exchanges.values() {
            for account in exchange.venue_account_snapshots() {
                account_ids.insert(account.account_id);
            }
        }

        let accounts = account_ids
            .into_iter()
            .map(|account_id| self.account_net_worth_snapshot(account_id))
            .collect();

        RoomNetWorthSnapshot {
            room_id: self.room_id.clone(),
            accounts,
        }
    }

    fn apply_seed_order(
        &mut self,
        seed_order: ScenarioSeedOrder,
    ) -> Result<ActorExecution, ScenarioError> {
        match seed_order.instrument_id {
            Some(instrument_id) => self
                .apply_to_instrument(&instrument_id, seed_order.command)
                .map_err(ScenarioError::Actor),
            None => Ok(self.apply(seed_order.command)),
        }
    }

    fn exchange_mut(&mut self, venue_id: &str) -> Result<&mut ExchangeActor, SimulationRoomError> {
        self.exchanges
            .get_mut(venue_id)
            .ok_or_else(|| SimulationRoomError::VenueNotFound {
                venue_id: venue_id.to_string(),
            })
    }

    fn venue_id_for_instrument(&self, instrument_id: &str) -> Option<VenueId> {
        self.exchanges.iter().find_map(|(venue_id, exchange)| {
            exchange
                .instrument_ids()
                .contains(&instrument_id)
                .then(|| venue_id.clone())
        })
    }

    fn apply_initial_portfolio(&mut self, portfolio: &ScenarioPortfolio) {
        for (asset_id, total) in &portfolio.balances {
            let snapshot =
                self.portfolios
                    .set_balance(portfolio.account_id, asset_id.clone(), *total);
            self.record_asset_ledger(
                None,
                portfolio.account_id,
                asset_id.clone(),
                *total,
                snapshot.total,
                AssetLedgerKind::PortfolioSet,
            );
        }
    }

    fn apply_initial_allocation(
        &mut self,
        venue_id: &str,
        account_id: AccountId,
        asset_id: String,
        amount: Money,
    ) -> Result<(), ScenarioError> {
        if amount <= 0 {
            return Err(ScenarioError::Allocation(
                VenueTransferRejectReason::NonPositiveAmount,
            ));
        }
        let portfolio = self
            .portfolios
            .apply_delta(account_id, asset_id.clone(), -amount)
            .map_err(|_| {
                ScenarioError::Allocation(VenueTransferRejectReason::InsufficientPortfolioBalance)
            })?;
        let venue_result = self
            .exchange_mut(venue_id)
            .map_err(|_| {
                ScenarioError::Allocation(VenueTransferRejectReason::InsufficientAvailableBalance)
            })?
            .apply_venue_asset_delta(account_id, asset_id.clone(), amount);
        if let Err(error) = venue_result {
            let _ = self
                .portfolios
                .apply_delta(account_id, asset_id.clone(), amount);
            return Err(ScenarioError::Allocation(error));
        }
        self.record_asset_ledger(
            Some(venue_id.to_string()),
            account_id,
            asset_id,
            -amount,
            portfolio.total,
            AssetLedgerKind::InitialAllocation,
        );
        Ok(())
    }

    fn apply_portfolio_effect_for_finished_transfer(
        &mut self,
        venue_id: &str,
        transfer: &VenueTransfer,
    ) {
        if transfer.status == VenueTransferStatus::Pending {
            return;
        }

        match (transfer.kind, transfer.status) {
            (VenueTransferKind::Deposit, VenueTransferStatus::Completed) => {
                let _ = self.portfolios.release(
                    transfer.account_id,
                    transfer.asset_id.clone(),
                    transfer.amount,
                );
                if let Ok(snapshot) = self.portfolios.apply_delta(
                    transfer.account_id,
                    transfer.asset_id.clone(),
                    -transfer.amount,
                ) {
                    self.record_asset_ledger(
                        Some(venue_id.to_string()),
                        transfer.account_id,
                        transfer.asset_id.clone(),
                        -transfer.amount,
                        snapshot.total,
                        AssetLedgerKind::DepositCompleted,
                    );
                }
            }
            (VenueTransferKind::Deposit, VenueTransferStatus::Rejected) => {
                let _ = self.portfolios.release(
                    transfer.account_id,
                    transfer.asset_id.clone(),
                    transfer.amount,
                );
            }
            (VenueTransferKind::Withdrawal, VenueTransferStatus::Completed) => {
                if let Ok(snapshot) = self.portfolios.apply_delta(
                    transfer.account_id,
                    transfer.asset_id.clone(),
                    transfer.amount,
                ) {
                    self.record_asset_ledger(
                        Some(venue_id.to_string()),
                        transfer.account_id,
                        transfer.asset_id.clone(),
                        transfer.amount,
                        snapshot.total,
                        AssetLedgerKind::WithdrawalCompleted,
                    );
                }
            }
            (VenueTransferKind::Withdrawal, VenueTransferStatus::Rejected)
            | (_, VenueTransferStatus::Pending) => {}
        }
    }

    fn process_completed_venue_transfer_links(
        &mut self,
        venue_id: &str,
        completed: &[VenueTransfer],
    ) -> Vec<VenueTransfer> {
        let completed_withdrawals = completed
            .iter()
            .filter(|transfer| {
                transfer.kind == VenueTransferKind::Withdrawal
                    && transfer.status == VenueTransferStatus::Completed
            })
            .map(|transfer| transfer.transfer_id)
            .collect::<BTreeSet<_>>();
        if completed_withdrawals.is_empty() {
            return Vec::new();
        }

        let mut ready = Vec::new();
        self.pending_venue_transfers.retain(|pending| {
            let is_ready = pending.from_venue_id == venue_id
                && completed_withdrawals.contains(&pending.withdrawal_transfer_id);
            if is_ready {
                ready.push(pending.clone());
                false
            } else {
                true
            }
        });

        ready
            .into_iter()
            .filter_map(|pending| {
                self.submit_deposit(
                    Some(&pending.to_venue_id),
                    pending.account_id,
                    pending.asset_id,
                    pending.amount,
                )
                .ok()
            })
            .collect()
    }

    fn account_net_worth_snapshot(&self, account_id: AccountId) -> AccountNetWorthSnapshot {
        let mut asset_ids = BTreeSet::new();
        let portfolio = self.portfolios.account_snapshot(account_id);
        for balance in &portfolio.balances {
            asset_ids.insert(balance.asset_id.clone());
        }
        for exchange in self.exchanges.values() {
            for account in exchange.venue_account_snapshots() {
                if account.account_id == account_id {
                    for balance in account.balances {
                        asset_ids.insert(balance.asset_id);
                    }
                }
            }
        }

        let assets = asset_ids
            .into_iter()
            .map(|asset_id| {
                let portfolio_balance = self
                    .portfolios
                    .balance_snapshot(account_id, &asset_id)
                    .map(|balance| (balance.total, balance.reserved, balance.available))
                    .unwrap_or_default();
                let mut venue_total = 0;
                let mut venue_reserved = 0;
                for exchange in self.exchanges.values() {
                    if let Some(balance) = exchange.venue_balance_snapshot(account_id, &asset_id) {
                        venue_total += balance.total;
                        venue_reserved += balance.reserved;
                    }
                }
                AccountNetWorthAssetSnapshot {
                    asset_id,
                    portfolio_total: portfolio_balance.0,
                    portfolio_reserved: portfolio_balance.1,
                    portfolio_available: portfolio_balance.2,
                    venue_total,
                    venue_reserved,
                    venue_available: venue_total - venue_reserved,
                    total: portfolio_balance.0 + venue_total,
                    available: portfolio_balance.2 + venue_total - venue_reserved,
                }
            })
            .collect();

        AccountNetWorthSnapshot { account_id, assets }
    }

    fn record_asset_ledger(
        &mut self,
        venue_id: Option<VenueId>,
        account_id: AccountId,
        asset_id: String,
        delta: Money,
        portfolio_balance_after: Money,
        kind: AssetLedgerKind,
    ) {
        let seq = self.next_asset_ledger_seq;
        self.next_asset_ledger_seq += 1;
        self.asset_ledger.push(AssetLedgerEntry {
            seq,
            account_id,
            venue_id,
            asset_id,
            delta,
            portfolio_balance_after,
            kind,
            step: self.clock().step(),
        });
    }
}

#[derive(Clone, Debug)]
pub struct SimulationBootstrap {
    pub room: SimulationRoom,
    pub seed_executions: Vec<ActorExecution>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VenueAccountVenueSnapshot {
    pub venue_id: VenueId,
    pub account: VenueAccountSnapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PendingVenueTransfer {
    pub from_venue_id: VenueId,
    pub to_venue_id: VenueId,
    pub account_id: AccountId,
    pub asset_id: String,
    pub amount: Money,
    pub withdrawal_transfer_id: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VenueToVenueTransfer {
    pub from_venue_id: VenueId,
    pub to_venue_id: VenueId,
    pub withdrawal: VenueTransfer,
    pub deposit: Option<VenueTransfer>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AssetLedgerEntry {
    pub seq: u64,
    pub account_id: AccountId,
    pub venue_id: Option<VenueId>,
    pub asset_id: String,
    pub delta: Money,
    pub portfolio_balance_after: Money,
    pub kind: AssetLedgerKind,
    pub step: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum AssetLedgerKind {
    PortfolioSet,
    InitialAllocation,
    DepositCompleted,
    WithdrawalCompleted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RoomNetWorthSnapshot {
    pub room_id: RoomId,
    pub accounts: Vec<AccountNetWorthSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AccountNetWorthSnapshot {
    pub account_id: AccountId,
    pub assets: Vec<AccountNetWorthAssetSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AccountNetWorthAssetSnapshot {
    pub asset_id: String,
    pub portfolio_total: Money,
    pub portfolio_reserved: Money,
    pub portfolio_available: Money,
    pub venue_total: Money,
    pub venue_reserved: Money,
    pub venue_available: Money,
    pub total: Money,
    pub available: Money,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SimulationRoomError {
    VenueNotFound { venue_id: VenueId },
}

fn rules_for_exchange(
    scenario: &ScenarioConfig,
    exchange_config: &ExchangeConfig,
) -> Result<VenueRuleConfig, ScenarioError> {
    match &scenario.venue_preset {
        Some(preset) => preset
            .rules_for_markets(&exchange_config.markets)
            .map_err(|error| ScenarioError::MarketConfig(MarketConfigError::VenueRule(error)))
            .map(|rules| rules.merge_overrides(scenario.venue_rules.clone())),
        None => Ok(scenario.venue_rules.clone()),
    }
}

fn asset_policy_for_exchange(
    scenario: &ScenarioConfig,
    exchange_config: &ExchangeConfig,
) -> VenueAssetPolicyConfig {
    match &scenario.venue_preset {
        Some(preset) => preset
            .asset_policy_for_markets(&exchange_config.markets)
            .merge_overrides(scenario.venue_asset_policy.clone()),
        None => scenario.venue_asset_policy.clone(),
    }
}

fn scenario_user_accounts(scenario: &ScenarioConfig) -> BTreeMap<UserId, BTreeSet<AccountId>> {
    let accounts = scenario
        .accounts
        .iter()
        .map(|account| match account {
            ScenarioAccount::Basic { account_id, .. }
            | ScenarioAccount::Spot { account_id, .. } => *account_id,
        })
        .collect::<BTreeSet<_>>();
    BTreeMap::from([(String::from("default-user"), accounts)])
}

fn apply_account_to_exchange(
    account: &ScenarioAccount,
    exchange: &mut ExchangeActor,
) -> Result<(), ScenarioError> {
    match account {
        ScenarioAccount::Basic {
            account_id,
            cash_balance,
        } => {
            exchange.create_account(*account_id, *cash_balance);
            Ok(())
        }
        ScenarioAccount::Spot {
            account_id,
            cash_balance,
            position_qty,
        } => {
            if exchange.primary_market().kind() == MarketKind::Spot {
                exchange
                    .create_spot_account_with_position(*account_id, *cash_balance, *position_qty)
                    .map_err(ScenarioError::Actor)?;
            } else {
                exchange.create_account(*account_id, *cash_balance);
            }
            Ok(())
        }
    }
}
