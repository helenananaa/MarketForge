use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    account::{
        ClearingError, Money, PositionQty, VenueAccountError, VenueAccountSnapshot,
        VenueAccountStore, VenueBalanceSnapshot,
    },
    clock::SimulationClock,
    market::{
        ExchangeConfig, InstrumentId, MarketConfig, MarketConfigError, MarketEngine, MarketKind,
    },
    model::{AccountId, BookSnapshot, Command, OrderId, Side},
    perp::{PerpAccountSnapshot, PerpClearingEvent, PerpCrossMarginContext},
    portfolio::{PortfolioAccountSnapshot, PortfolioStore},
    spot::{SpotAccountSnapshot, SpotClearingEvent},
    trading::{PerpTradingExecution, SpotTradingExecution},
    transfer::{
        VenueTransfer, VenueTransferKind, VenueTransferRejectReason, VenueTransferStatus,
        VenueTransferStore,
    },
    venue_rules::{VenueRuleEngine, VenueRuleOrderContext, VenueRuleRejectReason},
};

pub type RoomId = String;
pub type ActorSeq = u64;
type MarketReservations = BTreeMap<AccountId, BTreeMap<String, Money>>;

fn ordered_available_balance<'a>(
    cursor: &mut std::iter::Peekable<
        impl Iterator<Item = (AccountId, &'a crate::VenueAssetBalance)>,
    >,
    account: AccountId,
) -> Option<Money> {
    while cursor.peek().is_some_and(|(id, _)| *id < account) {
        cursor.next();
    }
    cursor
        .peek()
        .filter(|(id, _)| *id == account)
        .map(|(_, balance)| balance.available())
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum MarketStatus {
    Running,
    Paused,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandOrigin {
    External,
    Scheduler,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MarketActor {
    room_id: RoomId,
    config: MarketConfig,
    engine: MarketEngine,
    status: MarketStatus,
    next_command_seq: ActorSeq,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExchangeActor {
    #[serde(default)]
    conditional_orders: BTreeMap<String, crate::conditional_orders::ConditionalOrder>,
    #[serde(default)]
    position_protections: BTreeMap<String, crate::PositionProtection>,
    room_id: RoomId,
    config: ExchangeConfig,
    markets: BTreeMap<InstrumentId, MarketActor>,
    venue_accounts: VenueAccountStore,
    #[serde(default)]
    market_reservations: MarketReservations,
    #[serde(default)]
    portfolios: PortfolioStore,
    venue_rules: VenueRuleEngine,
    clock: SimulationClock,
    transfers: VenueTransferStore,
    next_command_seq: ActorSeq,
    #[serde(default)]
    price_links: BTreeMap<InstrumentId, crate::PerpPriceSnapshot>,
    #[serde(default)]
    spot_trade_prices: BTreeMap<InstrumentId, crate::price_link::SpotTradePrice>,
    #[serde(default)]
    funding_states: BTreeMap<InstrumentId, crate::funding::FundingState>,
    #[serde(skip)]
    pending_clock_executions: Vec<ActorExecution>,
    /// The unpaid part of logical collateral after funding. Risk requirements
    /// remain unchanged; only the cash that can actually be frozen is capped.
    #[serde(default)]
    funding_collateral_shortfalls: MarketReservations,
}

impl ExchangeActor {
    pub fn new(
        room_id: impl Into<RoomId>,
        config: ExchangeConfig,
    ) -> Result<Self, MarketConfigError> {
        config.validate()?;
        let room_id = room_id.into();
        let mut markets = BTreeMap::new();

        for market in &config.markets {
            markets.insert(
                market.instrument_id().to_string(),
                MarketActor::new(room_id.clone(), market.clone())?,
            );
        }

        Ok(Self {
            room_id,
            venue_rules: VenueRuleEngine::new(config.venue_rules.clone())
                .map_err(MarketConfigError::VenueRule)?,
            config,
            markets,
            venue_accounts: VenueAccountStore::new(),
            market_reservations: BTreeMap::new(),
            portfolios: PortfolioStore::new(),
            next_command_seq: 0,
            clock: SimulationClock::default(),
            transfers: VenueTransferStore::new(),
            price_links: BTreeMap::new(),
            spot_trade_prices: BTreeMap::new(),
            funding_states: BTreeMap::new(),
            position_protections: BTreeMap::new(),
            conditional_orders: BTreeMap::new(),
            pending_clock_executions: Vec::new(),
            funding_collateral_shortfalls: BTreeMap::new(),
        })
    }

    pub fn from_market(actor: MarketActor) -> Result<Self, MarketConfigError> {
        let room_id = actor.room_id().to_string();
        let config = ExchangeConfig::new_single(actor.config().clone())?;
        let next_command_seq = actor.next_command_seq();
        let instrument = actor.config().instrument().clone();
        let account_snapshots = actor.account_snapshots();
        let mut venue_accounts = VenueAccountStore::new();
        match account_snapshots {
            AccountSnapshots::Spot(accounts) => {
                for account in accounts {
                    venue_accounts.set_balance(
                        account.account_id,
                        instrument.quote_asset.clone(),
                        account.cash_balance,
                    );
                    venue_accounts.set_balance(
                        account.account_id,
                        instrument.base_asset.clone(),
                        account.position_qty,
                    );
                }
            }
            AccountSnapshots::Perp(accounts) => {
                for account in accounts {
                    venue_accounts.set_balance(
                        account.account_id,
                        instrument.quote_asset.clone(),
                        account.cash_balance,
                    );
                }
            }
        }
        let mut markets = BTreeMap::new();
        markets.insert(actor.config().instrument_id().to_string(), actor);

        Ok(Self {
            room_id,
            venue_rules: VenueRuleEngine::new(config.venue_rules.clone())
                .map_err(MarketConfigError::VenueRule)?,
            config,
            markets,
            venue_accounts,
            market_reservations: BTreeMap::new(),
            portfolios: PortfolioStore::new(),
            next_command_seq,
            clock: SimulationClock::default(),
            transfers: VenueTransferStore::new(),
            price_links: BTreeMap::new(),
            spot_trade_prices: BTreeMap::new(),
            funding_states: BTreeMap::new(),
            position_protections: BTreeMap::new(),
            conditional_orders: BTreeMap::new(),
            pending_clock_executions: Vec::new(),
            funding_collateral_shortfalls: BTreeMap::new(),
        })
    }

    pub fn room_id(&self) -> &str {
        &self.room_id
    }

    pub fn config(&self) -> &ExchangeConfig {
        &self.config
    }

    pub fn venue_id(&self) -> &str {
        &self.config.venue_id
    }

    pub fn primary_instrument_id(&self) -> &str {
        self.config.primary_instrument_id()
    }

    pub fn next_command_seq(&self) -> ActorSeq {
        self.next_command_seq
    }

    pub fn instrument_ids(&self) -> Vec<&str> {
        self.markets.keys().map(String::as_str).collect()
    }

    pub fn venue_account_snapshot(&self, account_id: AccountId) -> VenueAccountSnapshot {
        self.venue_accounts.account_snapshot(account_id)
    }

    pub fn venue_account_snapshots(&self) -> Vec<VenueAccountSnapshot> {
        self.venue_accounts.account_snapshots()
    }

    pub fn portfolio_snapshot(&self, account_id: AccountId) -> PortfolioAccountSnapshot {
        self.portfolios.account_snapshot(account_id)
    }

    pub fn portfolio_snapshots(&self) -> Vec<PortfolioAccountSnapshot> {
        self.portfolios.account_snapshots()
    }

    pub fn set_portfolio_balance(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<String>,
        total: Money,
    ) {
        self.portfolios.set_balance(account_id, asset_id, total);
    }

    pub fn portfolio_store(&self) -> &PortfolioStore {
        &self.portfolios
    }

    pub fn allocate_from_portfolio(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<String>,
        amount: Money,
    ) -> Result<(), VenueTransferRejectReason> {
        if amount <= 0 {
            return Err(VenueTransferRejectReason::NonPositiveAmount);
        }
        let asset_id = asset_id.into();
        if !self.config.accepts_deposit_asset(&asset_id) {
            return Err(VenueTransferRejectReason::AssetNotAcceptedByVenue);
        }
        self.portfolios
            .apply_delta(account_id, asset_id.clone(), -amount)
            .map_err(|_| VenueTransferRejectReason::InsufficientPortfolioBalance)?;
        if let Err(error) =
            self.venue_accounts
                .apply_signed_delta(account_id, asset_id.clone(), amount)
        {
            let _ = self.portfolios.apply_delta(account_id, asset_id, amount);
            return Err(match error {
                crate::VenueAccountError::BalanceOverflow => {
                    VenueTransferRejectReason::BalanceOverflow
                }
                crate::VenueAccountError::NegativeAmount
                | crate::VenueAccountError::InsufficientAvailableBalance
                | crate::VenueAccountError::InsufficientReservedBalance => {
                    VenueTransferRejectReason::InsufficientAvailableBalance
                }
            });
        }
        Ok(())
    }

    pub fn apply_venue_asset_delta(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<String>,
        amount: Money,
    ) -> Result<(), VenueTransferRejectReason> {
        let asset_id = asset_id.into();
        if amount > 0 && !self.config.accepts_deposit_asset(&asset_id) {
            return Err(VenueTransferRejectReason::AssetNotAcceptedByVenue);
        }
        let mut staged = self.clone();
        staged
            .venue_accounts
            .apply_delta(account_id, asset_id, amount)
            .map_err(reject_reason_from_venue_account_error)?;
        staged
            .sync_all_accounts_after_external_balance_change_inner()
            .map_err(|_| VenueTransferRejectReason::BalanceOverflow)?;
        *self = staged;
        Ok(())
    }

    pub fn venue_balance_snapshot(
        &self,
        account_id: AccountId,
        asset_id: &str,
    ) -> Option<VenueBalanceSnapshot> {
        self.venue_accounts.balance_snapshot(account_id, asset_id)
    }

    pub fn clock(&self) -> SimulationClock {
        self.clock
    }

    pub fn advance_clock(&mut self, steps: u64) -> Result<Vec<VenueTransfer>, ClearingError> {
        let mut staged = self.clone();
        let completed = staged.advance_clock_venue_only(steps)?;
        for transfer in &completed {
            staged.apply_portfolio_effect_for_finished_transfer(transfer);
        }
        *self = staged;
        Ok(completed)
    }

    pub fn advance_clock_venue_only(
        &mut self,
        steps: u64,
    ) -> Result<Vec<VenueTransfer>, ClearingError> {
        let mut staged = self.clone();
        staged
            .clock
            .checked_time_after(steps)
            .map_err(|_| ClearingError::BalanceOverflow)?;
        let has_funding = staged
            .config
            .markets
            .iter()
            .any(|market| matches!(market, MarketConfig::Perp(perp) if perp.funding.is_some()));
        let mut completed = Vec::new();
        for _ in 0..steps {
            staged.clock.advance_step();
            staged.expire_orders()?;
            let due = staged
                .transfers
                .process_due(&mut staged.venue_accounts, staged.clock.step());
            completed.extend(due);
            if has_funding {
                staged.sync_all_accounts_after_external_balance_change_inner()?;
                staged.advance_funding()?;
            }
        }
        staged.sync_all_accounts_after_external_balance_change_inner()?;
        *self = staged;
        Ok(completed)
    }

    pub(crate) fn take_clock_executions(&mut self) -> Vec<ActorExecution> {
        std::mem::take(&mut self.pending_clock_executions)
    }

    fn expire_orders(&mut self) -> Result<(), ClearingError> {
        let now = self.clock.market_time_ms();
        let due: Vec<_> = self
            .markets
            .iter()
            .flat_map(|(instrument, market)| {
                let ids = match &market.engine {
                    MarketEngine::Spot(engine) => engine.expiring_order_ids(now),
                    MarketEngine::Perp(engine) => engine.expiring_order_ids(now),
                };
                ids.into_iter().map(move |id| (instrument.clone(), id))
            })
            .collect();
        for (instrument, order_id) in due {
            let execution = self
                .apply_to_instrument_from(
                    &instrument,
                    Command::ExpireOrder {
                        order_id,
                        market_time_ms: now,
                    },
                    CommandOrigin::Scheduler,
                )
                .map_err(|_| ClearingError::BalanceOverflow)?;
            if let ActorExecutionResult::Rejected(ActorRejectReason::Clearing(error)) =
                &execution.result
            {
                return Err(error.clone());
            }
            if matches!(execution.result, ActorExecutionResult::Rejected(_)) {
                return Err(ClearingError::BalanceOverflow);
            }
            self.pending_clock_executions.push(execution);
        }
        Ok(())
    }

    fn funding_snapshot(
        &self,
        id: &str,
        price: &crate::PerpPriceSnapshot,
    ) -> Option<crate::FundingSnapshot> {
        let MarketConfig::Perp(config) = self.markets.get(id)?.config() else {
            return None;
        };
        let funding = config.funding.as_ref()?;
        let initial = crate::funding::FundingState::new(funding);
        let state = self.funding_states.get(id).unwrap_or(&initial);
        let instantaneous =
            crate::funding::sample_rate(funding, price, &self.markets[id].book_snapshot());
        Some(crate::FundingSnapshot {
            market_time_ms: self.clock.market_time_ms(),
            interval_ms: funding.interval_ms,
            base_rate_ppm: funding.base_rate_ppm,
            max_rate_ppm: funding.max_rate_ppm,
            min_coverage_ppm: funding.min_coverage_ppm,
            estimated_rate_ppm: instantaneous.map(|_| {
                if state.covered_ms > 0 {
                    (state.rate_time_sum / i128::from(state.covered_ms)).clamp(
                        -i128::from(funding.max_rate_ppm),
                        i128::from(funding.max_rate_ppm),
                    ) as i32
                } else {
                    instantaneous.expect("valid sample").clamp(
                        -i128::from(funding.max_rate_ppm),
                        i128::from(funding.max_rate_ppm),
                    ) as i32
                }
            }),
            next_funding_time_ms: state.next_funding_time_ms,
            covered_ms: state.covered_ms,
            last_settlement: state.last_settlement.clone(),
        })
    }

    fn advance_funding(&mut self) -> Result<(), ClearingError> {
        let configs: Vec<_> = self
            .config
            .markets
            .iter()
            .filter_map(|market| {
                let MarketConfig::Perp(config) = market else {
                    return None;
                };
                Some((
                    config.instrument.instrument_id.clone(),
                    config.funding.clone()?,
                ))
            })
            .collect();
        let now = self.clock.market_time_ms();
        for (id, config) in configs {
            let price = self
                .perp_price_snapshot(&id)
                .expect("configured market")
                .expect("funding requires a link");
            let sample =
                crate::funding::sample_rate(&config, &price, &self.markets[&id].book_snapshot());
            let state = self
                .funding_states
                .entry(id.clone())
                .or_insert_with(|| crate::funding::FundingState::new(&config));
            if let Some(rate) = sample {
                state.covered_ms = state
                    .covered_ms
                    .checked_add(self.clock.step_duration_ms())
                    .ok_or(ClearingError::BalanceOverflow)?;
                state.rate_time_sum = state
                    .rate_time_sum
                    .checked_add(
                        rate.checked_mul(i128::from(self.clock.step_duration_ms()))
                            .ok_or(ClearingError::BalanceOverflow)?,
                    )
                    .ok_or(ClearingError::BalanceOverflow)?;
            }
            if now < state.next_funding_time_ms {
                continue;
            }
            let status = if sample.is_some()
                && u128::from(state.covered_ms) * 1_000_000
                    >= u128::from(config.interval_ms) * u128::from(config.min_coverage_ppm)
            {
                crate::FundingStatus::Settled
            } else {
                crate::FundingStatus::SkippedPrices
            };
            let rate_ppm = if state.covered_ms > 0 {
                (state.rate_time_sum / i128::from(state.covered_ms)).clamp(
                    -i128::from(config.max_rate_ppm),
                    i128::from(config.max_rate_ppm),
                ) as i32
            } else {
                0
            };
            let settlement = crate::FundingSettlement {
                instrument_id: id.clone(),
                funding_time_ms: state.next_funding_time_ms,
                interval_ms: config.interval_ms,
                covered_ms: state.covered_ms,
                rate_ppm,
                mark_price_tick: price.mark_price_tick,
                status,
                total_transfer: 0,
            };
            let command_seq = self.take_command_seq();
            self.sync_market_accounts_from_venue(&id)?;
            let MarketEngine::Perp(engine) =
                &mut self.markets.get_mut(&id).expect("configured market").engine
            else {
                unreachable!();
            };
            let result = engine.apply(Command::SettleFunding(settlement))?;
            let Command::SettleFunding(settlement) = &result.command.command else {
                unreachable!();
            };
            let execution = ActorExecution {
                rejected_command: None,
                room_id: self.room_id.clone(),
                instrument_id: id.clone(),
                command_seq,
                market_time_ms: now,
                status: self.status(),
                price_updates: Vec::new(),
                funding_settlement: Some(settlement.clone()),
                result: ActorExecutionResult::Accepted(MarketExecution::Perp(result)),
            };
            self.sync_venue_accounts_from_execution(&id, &execution)?;
            self.sync_all_accounts_after_external_balance_change_inner()?;
            let state = self
                .funding_states
                .get_mut(&id)
                .expect("funding state initialized");
            state.last_settlement = execution.funding_settlement.clone();
            state.next_funding_time_ms = state
                .next_funding_time_ms
                .checked_add(config.interval_ms)
                .ok_or(ClearingError::BalanceOverflow)?;
            state.covered_ms = 0;
            state.rate_time_sum = 0;
            self.pending_clock_executions.push(execution);
        }
        Ok(())
    }

    pub fn submit_deposit(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<String>,
        amount: Money,
    ) -> Result<VenueTransfer, ClearingError> {
        let mut staged = self.clone();
        let transfer = staged.submit_deposit_inner(account_id, asset_id.into(), amount);
        if transfer.status != VenueTransferStatus::Rejected {
            staged.sync_all_accounts_after_external_balance_change_inner()?;
        }
        *self = staged;
        Ok(transfer)
    }

    fn submit_deposit_inner(
        &mut self,
        account_id: AccountId,
        asset_id: String,
        amount: Money,
    ) -> VenueTransfer {
        if amount <= 0 {
            return self.transfers.submit_deposit(
                &mut self.venue_accounts,
                account_id,
                asset_id,
                amount,
                self.clock.step(),
                self.venue_rules.config().transfers.deposit_delay_steps,
            );
        }
        if !self.config.accepts_deposit_asset(&asset_id) {
            return self.transfers.submit_rejected_deposit(
                account_id,
                asset_id,
                amount,
                self.clock.step(),
                VenueTransferRejectReason::AssetNotAcceptedByVenue,
            );
        }
        if self
            .portfolios
            .reserve(account_id, asset_id.clone(), amount)
            .is_err()
        {
            return self.transfers.submit_rejected_deposit(
                account_id,
                asset_id,
                amount,
                self.clock.step(),
                VenueTransferRejectReason::InsufficientPortfolioBalance,
            );
        }

        let transfer = self.transfers.submit_deposit(
            &mut self.venue_accounts,
            account_id,
            asset_id,
            amount,
            self.clock.step(),
            self.venue_rules.config().transfers.deposit_delay_steps,
        );
        if transfer.status != VenueTransferStatus::Pending {
            self.apply_portfolio_effect_for_finished_transfer(&transfer);
        }
        transfer
    }

    pub fn submit_venue_deposit(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<String>,
        amount: Money,
    ) -> Result<VenueTransfer, ClearingError> {
        let mut staged = self.clone();
        let transfer = staged.submit_venue_deposit_inner(account_id, asset_id.into(), amount);
        if transfer.status != VenueTransferStatus::Rejected {
            staged.sync_all_accounts_after_external_balance_change_inner()?;
        }
        *self = staged;
        Ok(transfer)
    }

    fn submit_venue_deposit_inner(
        &mut self,
        account_id: AccountId,
        asset_id: String,
        amount: Money,
    ) -> VenueTransfer {
        if amount > 0 && !self.config.accepts_deposit_asset(&asset_id) {
            return self.transfers.submit_rejected_deposit(
                account_id,
                asset_id,
                amount,
                self.clock.step(),
                VenueTransferRejectReason::AssetNotAcceptedByVenue,
            );
        }
        self.transfers.submit_deposit(
            &mut self.venue_accounts,
            account_id,
            asset_id,
            amount,
            self.clock.step(),
            self.venue_rules.config().transfers.deposit_delay_steps,
        )
    }

    pub fn submit_rejected_venue_deposit(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<String>,
        amount: Money,
        reject_reason: VenueTransferRejectReason,
    ) -> VenueTransfer {
        self.transfers.submit_rejected_deposit(
            account_id,
            asset_id,
            amount,
            self.clock.step(),
            reject_reason,
        )
    }

    pub fn submit_withdrawal(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<String>,
        amount: Money,
    ) -> Result<VenueTransfer, ClearingError> {
        let mut staged = self.clone();
        let transfer = staged.submit_withdrawal_inner(account_id, asset_id.into(), amount);
        if transfer.status != VenueTransferStatus::Rejected {
            staged.sync_all_accounts_after_external_balance_change_inner()?;
        }
        *self = staged;
        Ok(transfer)
    }

    fn submit_withdrawal_inner(
        &mut self,
        account_id: AccountId,
        asset_id: String,
        amount: Money,
    ) -> VenueTransfer {
        if amount > 0 && !self.config.accepts_withdrawal_asset(&asset_id) {
            return self.transfers.submit_rejected_withdrawal(
                account_id,
                asset_id,
                amount,
                self.clock.step(),
                VenueTransferRejectReason::AssetNotWithdrawableFromVenue,
            );
        }
        if let Some(reject_reason) =
            self.perp_withdrawal_reject_reason(account_id, &asset_id, amount)
        {
            return self.transfers.submit_rejected_withdrawal(
                account_id,
                asset_id,
                amount,
                self.clock.step(),
                reject_reason,
            );
        }
        let transfer = self.transfers.submit_withdrawal(
            &mut self.venue_accounts,
            account_id,
            asset_id,
            amount,
            self.clock.step(),
            self.venue_rules.config().transfers.withdrawal_delay_steps,
        );
        if transfer.status != VenueTransferStatus::Pending {
            self.apply_portfolio_effect_for_finished_transfer(&transfer);
        }
        transfer
    }

    pub fn submit_venue_withdrawal(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<String>,
        amount: Money,
    ) -> Result<VenueTransfer, ClearingError> {
        let mut staged = self.clone();
        let transfer = staged.submit_venue_withdrawal_inner(account_id, asset_id.into(), amount);
        if transfer.status != VenueTransferStatus::Rejected {
            staged.sync_all_accounts_after_external_balance_change_inner()?;
        }
        *self = staged;
        Ok(transfer)
    }

    fn submit_venue_withdrawal_inner(
        &mut self,
        account_id: AccountId,
        asset_id: String,
        amount: Money,
    ) -> VenueTransfer {
        if amount > 0 && !self.config.accepts_withdrawal_asset(&asset_id) {
            return self.transfers.submit_rejected_withdrawal(
                account_id,
                asset_id,
                amount,
                self.clock.step(),
                VenueTransferRejectReason::AssetNotWithdrawableFromVenue,
            );
        }
        if let Some(reject_reason) =
            self.perp_withdrawal_reject_reason(account_id, &asset_id, amount)
        {
            return self.transfers.submit_rejected_withdrawal(
                account_id,
                asset_id,
                amount,
                self.clock.step(),
                reject_reason,
            );
        }
        self.transfers.submit_withdrawal(
            &mut self.venue_accounts,
            account_id,
            asset_id,
            amount,
            self.clock.step(),
            self.venue_rules.config().transfers.withdrawal_delay_steps,
        )
    }

    pub fn transfers(&self) -> Vec<VenueTransfer> {
        self.transfers.transfers()
    }

    fn perp_withdrawal_reject_reason(
        &self,
        account_id: AccountId,
        asset_id: &str,
        amount: Money,
    ) -> Option<VenueTransferRejectReason> {
        if amount <= 0 {
            return None;
        }

        let mut net_unrealized_pnl = 0i128;
        for market in self.markets.values() {
            if market.config().instrument().quote_asset != asset_id {
                continue;
            }
            let AccountSnapshots::Perp(accounts) = market.account_snapshots() else {
                continue;
            };
            let Some(account) = accounts
                .into_iter()
                .find(|account| account.account_id == account_id)
            else {
                continue;
            };
            net_unrealized_pnl = match net_unrealized_pnl.checked_add(account.unrealized_pnl) {
                Some(total) => total,
                None => return Some(VenueTransferRejectReason::BalanceOverflow),
            };
        }

        let unrealized_loss_buffer = if net_unrealized_pnl < 0 {
            match net_unrealized_pnl.checked_neg() {
                Some(loss) => loss,
                None => return Some(VenueTransferRejectReason::BalanceOverflow),
            }
        } else {
            0
        };
        let venue_available = self
            .venue_accounts
            .balance_snapshot(account_id, asset_id)
            .map(|balance| balance.available)
            .unwrap_or_default();
        match venue_available.checked_sub(unrealized_loss_buffer) {
            Some(withdrawable) if withdrawable >= amount => None,
            Some(_) => Some(VenueTransferRejectReason::InsufficientAvailableBalance),
            None => Some(VenueTransferRejectReason::BalanceOverflow),
        }
    }

    pub(crate) fn normalize_after_restore(&mut self) -> Result<(), ClearingError> {
        let mut staged = self.clone();
        staged
            .venue_rules
            .normalize_after_restore(staged.next_command_seq, staged.clock.step())?;
        staged.reconcile_market_reservations()?;
        let perp_quote_assets = staged
            .markets
            .values()
            .filter(|market| market.kind() == MarketKind::Perp)
            .map(|market| market.config().instrument().quote_asset.clone())
            .collect::<std::collections::BTreeSet<_>>();
        for quote_asset in &perp_quote_assets {
            staged.sync_perp_cross_margin_group(quote_asset)?;
        }
        staged.reconcile_market_reservations()?;
        // The first pass may rebuild local reservations from a legacy
        // snapshot. Refresh once more so every peer context observes those
        // normalized values rather than the serialized stale values.
        for quote_asset in &perp_quote_assets {
            staged.sync_perp_cross_margin_group(quote_asset)?;
        }
        *self = staged;
        Ok(())
    }

    fn sync_all_accounts_after_external_balance_change_inner(
        &mut self,
    ) -> Result<(), ClearingError> {
        self.reconcile_market_reservations()?;
        let spot_instrument_ids = self
            .markets
            .iter()
            .filter(|(_, market)| market.kind() == MarketKind::Spot)
            .map(|(instrument_id, _)| instrument_id.clone())
            .collect::<Vec<_>>();
        for instrument_id in spot_instrument_ids {
            self.sync_market_accounts_from_venue(&instrument_id)?;
        }
        let perp_quote_assets = self
            .markets
            .values()
            .filter(|market| market.kind() == MarketKind::Perp)
            .map(|market| market.config().instrument().quote_asset.clone())
            .collect::<std::collections::BTreeSet<_>>();
        for quote_asset in perp_quote_assets {
            self.sync_perp_cross_margin_group(&quote_asset)?;
        }
        self.reconcile_market_reservations()
    }

    pub fn primary_market(&self) -> &MarketActor {
        self.markets
            .get(self.primary_instrument_id())
            .expect("validated exchange config must have a primary market")
    }

    pub fn primary_market_mut(&mut self) -> &mut MarketActor {
        self.venue_accounts.reservation_changes.invalidate();
        let instrument_id = self.primary_instrument_id().to_string();
        self.markets
            .get_mut(&instrument_id)
            .expect("validated exchange config must have a primary market")
    }

    pub fn market(&self, instrument_id: &str) -> Result<&MarketActor, ActorRejectReason> {
        self.markets
            .get(instrument_id)
            .ok_or_else(|| ActorRejectReason::InstrumentNotFound {
                instrument_id: instrument_id.to_string(),
            })
    }

    pub fn market_mut(
        &mut self,
        instrument_id: &str,
    ) -> Result<&mut MarketActor, ActorRejectReason> {
        self.venue_accounts.reservation_changes.invalidate();
        self.markets
            .get_mut(instrument_id)
            .ok_or_else(|| ActorRejectReason::InstrumentNotFound {
                instrument_id: instrument_id.to_string(),
            })
    }

    pub fn status(&self) -> MarketStatus {
        self.primary_market().status()
    }

    pub fn pause(&mut self) {
        for market in self.markets.values_mut() {
            market.pause();
        }
    }

    pub fn resume(&mut self) {
        for market in self.markets.values_mut() {
            market.resume();
        }
    }

    pub fn close(&mut self) {
        for market in self.markets.values_mut() {
            market.close();
        }
    }

    pub fn restore_status(&mut self, status: MarketStatus) {
        for market in self.markets.values_mut() {
            market.restore_status(status);
        }
    }

    pub fn create_account(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
    ) -> AccountSnapshot {
        let primary_id = self.config.primary_instrument_id().to_string();
        let mut primary = None;
        for (instrument_id, market) in &mut self.markets {
            let snapshot = market.create_account(account_id, cash_balance);
            if instrument_id == &primary_id {
                primary = Some(snapshot);
            }
        }
        let quote_assets = self
            .config
            .markets
            .iter()
            .map(|market| market.instrument().quote_asset.clone())
            .collect::<std::collections::BTreeSet<_>>();
        for asset_id in quote_assets {
            self.venue_accounts
                .set_balance(account_id, asset_id, cash_balance);
        }
        primary.expect("validated exchange config must have a primary market")
    }

    pub fn create_spot_account_with_position(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
        position_qty: PositionQty,
    ) -> Result<SpotAccountSnapshot, ActorRejectReason> {
        let primary = self.primary_instrument_id().to_string();
        let snapshot = self
            .market_mut(&primary)?
            .create_spot_account_with_position(account_id, cash_balance, position_qty)?;

        for (instrument_id, market) in &mut self.markets {
            if instrument_id != &primary {
                market.create_account(account_id, cash_balance);
            }
        }
        let instrument = self.primary_market().config().instrument().clone();
        self.venue_accounts
            .set_balance(account_id, instrument.quote_asset.clone(), cash_balance);
        self.venue_accounts
            .set_balance(account_id, instrument.base_asset.clone(), position_qty);

        Ok(snapshot)
    }

    pub fn apply(&mut self, command: Command) -> ActorExecution {
        let instrument_id = self.primary_instrument_id().to_string();
        self.apply_to_instrument(&instrument_id, command)
            .expect("validated exchange config must have a primary market")
    }

    pub fn apply_to_instrument(
        &mut self,
        instrument_id: &str,
        command: Command,
    ) -> Result<ActorExecution, ActorRejectReason> {
        self.apply_to_instrument_from(instrument_id, command, CommandOrigin::External)
    }

    pub fn apply_to_instrument_from(
        &mut self,
        instrument_id: &str,
        command: Command,
        origin: CommandOrigin,
    ) -> Result<ActorExecution, ActorRejectReason> {
        if matches!(command, Command::SetConditionalOrder { .. }) {
            return self.apply_conditional_command(instrument_id, command);
        }
        if matches!(
            command,
            Command::SetPositionProtection { .. } | Command::NewOrderWithProtection { .. }
        ) {
            return self.apply_position_protection_command(instrument_id, command, origin);
        }
        let retained = command.clone();
        let mut execution = self.apply_to_instrument_inner(instrument_id, command, origin)?;
        if matches!(execution.result, ActorExecutionResult::Rejected(_)) {
            execution.rejected_command = Some(retained);
        }
        Ok(execution)
    }

    fn apply_to_instrument_inner(
        &mut self,
        instrument_id: &str,
        command: Command,
        origin: CommandOrigin,
    ) -> Result<ActorExecution, ActorRejectReason> {
        self.apply_to_instrument_synced(instrument_id, command, origin, true)
    }

    fn apply_to_instrument_synced(
        &mut self,
        instrument_id: &str,
        command: Command,
        origin: CommandOrigin,
        incremental: bool,
    ) -> Result<ActorExecution, ActorRejectReason> {
        if !self.markets.contains_key(instrument_id) {
            return Err(ActorRejectReason::InstrumentNotFound {
                instrument_id: instrument_id.to_string(),
            });
        }

        let command_seq = self.take_command_seq();
        if let Command::NewOrder(order) = &command {
            let valid_until_market_time_ms = order.kind.valid_until_market_time_ms();
            let expires_at_market_time_ms = order.kind.expires_at_market_time_ms();
            let now = self.clock.market_time_ms();
            let reason = if expires_at_market_time_ms.is_some() && !order.kind.rests_remainder() {
                Some(ActorRejectReason::InvalidOrderProtection)
            } else {
                [valid_until_market_time_ms, expires_at_market_time_ms]
                    .into_iter()
                    .flatten()
                    .filter(|&deadline| now >= deadline)
                    .min()
                    .map(|deadline| ActorRejectReason::OrderProtectionExpired {
                        deadline_market_time_ms: deadline,
                        market_time_ms: now,
                    })
            };
            if let Some(reason) = reason {
                return Ok(ActorExecution {
                    rejected_command: None,
                    room_id: self.room_id.clone(),
                    instrument_id: instrument_id.into(),
                    command_seq,
                    market_time_ms: now,
                    status: self.status(),
                    result: ActorExecutionResult::Rejected(reason),
                    price_updates: Vec::new(),
                    funding_settlement: None,
                });
            }
        }
        if matches!(command, Command::SettleFunding(_)) {
            return Ok(ActorExecution {
                rejected_command: None,
                room_id: self.room_id.clone(),
                instrument_id: instrument_id.into(),
                command_seq,
                market_time_ms: self.clock.market_time_ms(),
                status: self.status(),
                result: ActorExecutionResult::Rejected(ActorRejectReason::FundingManaged),
                price_updates: Vec::new(),
                funding_settlement: None,
            });
        }
        if let Some(price) = self.perp_price_snapshot(instrument_id)? {
            let rejection = match &command {
                Command::SetMarkPrice(_) => Some(ActorRejectReason::LinkedMarkPriceManaged),
                Command::NewOrder(order)
                    if !order.reduce_only && price.status != crate::PriceLinkStatus::Live =>
                {
                    Some(ActorRejectReason::PriceLinkNotReady)
                }
                Command::AmendOrder(_) if price.status != crate::PriceLinkStatus::Live => {
                    Some(ActorRejectReason::PriceLinkNotReady)
                }
                _ => None,
            };
            if let Some(reason) = rejection {
                return Ok(ActorExecution {
                    rejected_command: None,
                    price_updates: Vec::new(),
                    funding_settlement: None,
                    room_id: self.room_id.clone(),
                    instrument_id: instrument_id.to_string(),
                    command_seq,
                    market_time_ms: self.clock.market_time_ms(),
                    status: self.status(),
                    result: ActorExecutionResult::Rejected(reason),
                });
            }
        }
        let before_book = self.config.markets.iter().any(|market| {
            matches!(market, MarketConfig::Perp(perp) if perp.price_link.as_ref().is_some_and(|link| link.spot_instrument_id == instrument_id))
        }).then(|| self.markets[instrument_id].book_snapshot());
        let affected_perp_quote_assets = self
            .perp_quote_assets_affected_by_instrument(instrument_id)
            .expect("instrument existence was checked above");
        if let Some(rejection) = self.check_venue_rules(instrument_id, &command)? {
            return Ok(ActorExecution {
                rejected_command: None,
                price_updates: Vec::new(),
                funding_settlement: None,
                room_id: self.room_id.clone(),
                instrument_id: instrument_id.to_string(),
                command_seq,
                market_time_ms: self.clock.market_time_ms(),
                status: self.status(),
                result: ActorExecutionResult::Rejected(ActorRejectReason::VenueRule(rejection)),
            });
        }

        let mut staged = {
            let _timer = crate::performance::Timer::start(0);
            self.clone()
        };
        if let Err(error) = staged.reconcile_market_reservations() {
            return Ok(self.clearing_rejection(instrument_id, command_seq, error));
        }
        if let Err(error) = staged.sync_market_accounts_from_venue(instrument_id) {
            return Ok(self.clearing_rejection(instrument_id, command_seq, error));
        }
        // Pre-command reconciliation and full margin synchronization establish a baseline.
        // Only ordinary perp orders have account-local effects; mark/funding
        // changes and spot collateral changes retain the full group path.
        let mut affected_accounts =
            if incremental && staged.markets[instrument_id].kind() == MarketKind::Perp {
                match &command {
                    Command::NewOrder(order) => Some(BTreeSet::from([order.account_id])),
                    Command::CancelOrder(_)
                    | Command::AmendOrder(_)
                    | Command::ExpireOrder { .. } => Some(
                        staged.markets[instrument_id]
                            .order_owner(command.order_id())
                            .into_iter()
                            .collect(),
                    ),
                    Command::SetConditionalOrder { .. }
                    | Command::SetPositionProtection { .. }
                    | Command::NewOrderWithProtection { .. }
                    | Command::SetMarkPrice(_)
                    | Command::SettleFunding(_) => None,
                }
            } else {
                None
            };
        let mut execution = staged
            .markets
            .get_mut(instrument_id)
            .expect("validated instrument")
            .apply_from(command, origin);
        execution.command_seq = command_seq;
        execution.instrument_id = instrument_id.to_string();
        execution.market_time_ms = staged.clock.market_time_ms();
        if matches!(execution.result, ActorExecutionResult::Accepted(_)) {
            if let (Some(accounts), ActorExecutionResult::Accepted(MarketExecution::Perp(result))) =
                (&mut affected_accounts, &execution.result)
            {
                for record in &result.events {
                    if let crate::Event::TradePrinted(trade) = &record.event {
                        accounts.insert(trade.maker_account_id);
                        accounts.insert(trade.taker_account_id);
                    }
                }
            }
            if let Err(error) = staged.reconcile_market_reservations_for(affected_accounts.as_ref())
            {
                return Ok(self.clearing_rejection(instrument_id, command_seq, error));
            }
            if let Err(error) = staged.sync_venue_accounts_from_execution(instrument_id, &execution)
            {
                return Ok(self.clearing_rejection(instrument_id, command_seq, error));
            }
            if let Err(error) =
                staged.refresh_price_links(instrument_id, before_book.as_ref(), &mut execution)
            {
                return Ok(self.clearing_rejection(instrument_id, command_seq, error));
            }
            if !execution.price_updates.is_empty()
                && let Err(error) = staged.reconcile_market_reservations()
            {
                return Ok(self.clearing_rejection(instrument_id, command_seq, error));
            }
            if !execution.price_updates.is_empty() {
                affected_accounts = None;
            }
            for quote_asset in &affected_perp_quote_assets {
                if let Err(error) =
                    staged.sync_perp_cross_margin_group_for(quote_asset, affected_accounts.as_ref())
                {
                    return Ok(self.clearing_rejection(instrument_id, command_seq, error));
                }
            }
        }
        *self = staged;
        Ok(execution)
    }

    pub fn perp_price_snapshot(
        &self,
        instrument_id: &str,
    ) -> Result<Option<crate::PerpPriceSnapshot>, ActorRejectReason> {
        let market = self.market(instrument_id)?;
        let MarketConfig::Perp(config) = market.config() else {
            return Ok(None);
        };
        let Some(link) = &config.price_link else {
            return Ok(None);
        };
        let snapshot =
            self.price_links
                .get(instrument_id)
                .cloned()
                .unwrap_or(crate::PerpPriceSnapshot {
                    instrument_id: instrument_id.to_string(),
                    spot_instrument_id: link.spot_instrument_id.clone(),
                    index_price_tick: None,
                    mark_price_tick: config.initial_mark_price_tick,
                    source: None,
                    source_time_ms: None,
                    max_age_ms: link.max_age_ms,
                    status: crate::PriceLinkStatus::AwaitingPrice,
                    funding: None,
                });
        let mut snapshot = snapshot.at_time(self.clock.market_time_ms());
        snapshot.funding = self.funding_snapshot(instrument_id, &snapshot);
        Ok(Some(snapshot))
    }

    fn refresh_price_links(
        &mut self,
        source_id: &str,
        before_book: Option<&BookSnapshot>,
        execution: &mut ActorExecution,
    ) -> Result<(), ClearingError> {
        let linked: Vec<_> = self
            .config
            .markets
            .iter()
            .filter_map(|market| {
                let MarketConfig::Perp(perp) = market else {
                    return None;
                };
                let link = perp.price_link.as_ref()?;
                (link.spot_instrument_id == source_id).then(|| {
                    (
                        perp.instrument.instrument_id.clone(),
                        perp.instrument.tick_size,
                        link.clone(),
                    )
                })
            })
            .collect();
        if linked.is_empty() {
            return Ok(());
        }
        let before_book = before_book.expect("linked source captured before command");
        let now = self.clock.market_time_ms();
        let trade = match &execution.result {
            ActorExecutionResult::Accepted(MarketExecution::Spot(spot)) => {
                spot.events.iter().rev().find_map(|record| {
                    if let crate::model::Event::TradePrinted(trade) = &record.event {
                        Some(trade.price_tick)
                    } else {
                        None
                    }
                })
            }
            _ => None,
        };
        if let Some(price_tick) = trade {
            self.spot_trade_prices.insert(
                source_id.to_string(),
                crate::price_link::SpotTradePrice {
                    price_tick,
                    market_time_ms: now,
                },
            );
        }
        let book = self
            .markets
            .get(source_id)
            .expect("validated index source")
            .book_snapshot();
        if trade.is_none()
            && before_book.bids.first() == book.bids.first()
            && before_book.asks.first() == book.asks.first()
        {
            return Ok(());
        }
        for (perp_id, tick_size, link) in linked {
            let mut price = self
                .perp_price_snapshot(&perp_id)
                .expect("validated perpetual")
                .expect("configured price link");
            let sample = match (book.bids.first(), book.asks.first()) {
                (Some(bid), Some(ask))
                    if bid.price_tick > 0 && ask.price_tick >= bid.price_tick =>
                {
                    Some((
                        bid.price_tick + (ask.price_tick - bid.price_tick) / 2,
                        now,
                        crate::IndexPriceSource::SpotMid,
                    ))
                }
                _ => self
                    .spot_trade_prices
                    .get(source_id)
                    .filter(|trade| now.saturating_sub(trade.market_time_ms) <= link.max_age_ms)
                    .map(|trade| {
                        (
                            trade.price_tick,
                            trade.market_time_ms,
                            crate::IndexPriceSource::SpotTrade,
                        )
                    }),
            };
            if let Some((index, time, source)) = sample {
                price.index_price_tick = Some(index);
                price.mark_price_tick = crate::price_link::mark_on_grid(index, tick_size);
                price.source = Some(source);
                price.source_time_ms = Some(time);
                price.status = crate::PriceLinkStatus::Live;
                let market = self.markets.get_mut(&perp_id).expect("validated perpetual");
                let MarketEngine::Perp(engine) = &mut market.engine else {
                    unreachable!();
                };
                engine.set_mark_price_tick(price.mark_price_tick)?;
            } else {
                price.status = crate::PriceLinkStatus::Unavailable;
            }
            self.price_links.insert(perp_id, price.clone());
            execution.price_updates.push(price);
        }
        Ok(())
    }

    pub fn liquidate_account(
        &mut self,
        instrument_id: &str,
        account_id: AccountId,
        order_id: OrderId,
    ) -> Result<ActorExecution, ActorRejectReason> {
        if !self.markets.contains_key(instrument_id) {
            return Err(ActorRejectReason::InstrumentNotFound {
                instrument_id: instrument_id.to_string(),
            });
        }

        let command_seq = self.take_command_seq();
        let perp_quote_asset = self
            .markets
            .get(instrument_id)
            .filter(|market| market.kind() == MarketKind::Perp)
            .map(|market| market.config().instrument().quote_asset.clone());
        let mut staged = self.clone();
        if let Err(error) = staged.reconcile_market_reservations() {
            return Ok(self.clearing_rejection(instrument_id, command_seq, error));
        }
        if let Err(error) = staged.sync_market_accounts_from_venue(instrument_id) {
            return Ok(self.clearing_rejection(instrument_id, command_seq, error));
        }
        let mut execution = staged
            .market_mut(instrument_id)?
            .liquidate_account(account_id, order_id);
        execution.command_seq = command_seq;
        execution.instrument_id = instrument_id.to_string();
        execution.market_time_ms = staged.clock.market_time_ms();
        if matches!(execution.result, ActorExecutionResult::Accepted(_)) {
            if let Err(error) = staged.reconcile_market_reservations() {
                return Ok(self.clearing_rejection(instrument_id, command_seq, error));
            }
            if let Err(error) = staged.sync_venue_accounts_from_execution(instrument_id, &execution)
            {
                return Ok(self.clearing_rejection(instrument_id, command_seq, error));
            }
            if let Some(quote_asset) = &perp_quote_asset
                && let Err(error) = staged.sync_perp_cross_margin_group(quote_asset)
            {
                return Ok(self.clearing_rejection(instrument_id, command_seq, error));
            }
        }
        *self = staged;
        Ok(execution)
    }

    pub fn book_snapshot(&self) -> BookSnapshot {
        self.primary_market().book_snapshot()
    }

    pub fn book_snapshot_for(
        &self,
        instrument_id: &str,
    ) -> Result<BookSnapshot, ActorRejectReason> {
        Ok(self.market(instrument_id)?.book_snapshot())
    }

    pub fn account_snapshot(&self, account_id: AccountId) -> Option<AccountSnapshot> {
        self.primary_market().account_snapshot(account_id)
    }

    pub fn account_snapshot_for(
        &self,
        instrument_id: &str,
        account_id: AccountId,
    ) -> Result<Option<AccountSnapshot>, ActorRejectReason> {
        Ok(self.market(instrument_id)?.account_snapshot(account_id))
    }

    pub fn account_snapshots(&self) -> AccountSnapshots {
        self.primary_market().account_snapshots()
    }

    pub fn account_snapshots_for(
        &self,
        instrument_id: &str,
    ) -> Result<AccountSnapshots, ActorRejectReason> {
        Ok(self.market(instrument_id)?.account_snapshots())
    }

    pub fn pending_liquidation_accounts_for(
        &self,
        instrument_id: &str,
    ) -> Result<Vec<AccountId>, ActorRejectReason> {
        Ok(self.market(instrument_id)?.pending_liquidation_accounts())
    }

    fn perp_quote_assets_affected_by_instrument(
        &self,
        instrument_id: &str,
    ) -> Option<std::collections::BTreeSet<String>> {
        let market = self.markets.get(instrument_id)?;
        let instrument = market.config().instrument();
        let affected_assets = match market.kind() {
            MarketKind::Spot => [
                instrument.base_asset.as_str(),
                instrument.quote_asset.as_str(),
            ]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
            MarketKind::Perp => [instrument.quote_asset.as_str()]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
        };
        Some(
            self.markets
                .values()
                .filter(|candidate| {
                    candidate.kind() == MarketKind::Perp
                        && affected_assets
                            .contains(candidate.config().instrument().quote_asset.as_str())
                })
                .map(|candidate| candidate.config().instrument().quote_asset.clone())
                .collect(),
        )
    }

    pub fn cross_margin_collateral_orders_for_liquidation(
        &self,
        instrument_id: &str,
        account_id: AccountId,
    ) -> Result<Vec<(InstrumentId, OrderId)>, ActorRejectReason> {
        let target = self.market(instrument_id)?;
        if target.kind() != MarketKind::Perp {
            return Err(ActorRejectReason::WrongMarketKind);
        }
        let quote_asset = &target.config().instrument().quote_asset;
        Ok(self
            .markets
            .iter()
            .filter(|(peer_instrument_id, market)| {
                if peer_instrument_id.as_str() == instrument_id {
                    return false;
                }
                let instrument = market.config().instrument();
                match market.kind() {
                    MarketKind::Perp => instrument.quote_asset == *quote_asset,
                    MarketKind::Spot => {
                        instrument.quote_asset == *quote_asset
                            || instrument.base_asset == *quote_asset
                    }
                }
            })
            .flat_map(|(peer_instrument_id, market)| {
                market
                    .collateral_resting_order_ids_for_account(account_id, quote_asset)
                    .into_iter()
                    .map(|order_id| (peer_instrument_id.clone(), order_id))
            })
            .collect())
    }

    pub fn order_owner_for(
        &self,
        instrument_id: &str,
        order_id: OrderId,
    ) -> Result<Option<AccountId>, ActorRejectReason> {
        Ok(self.market(instrument_id)?.order_owner(order_id))
    }

    pub fn resting_orders_for_account(
        &self,
        instrument_id: &str,
        account_id: AccountId,
    ) -> Result<Vec<crate::model::Order>, ActorRejectReason> {
        Ok(self
            .market(instrument_id)?
            .resting_orders_for_account(account_id))
    }

    fn take_command_seq(&mut self) -> ActorSeq {
        let seq = self.next_command_seq;
        self.next_command_seq += 1;
        seq
    }

    fn apply_portfolio_effect_for_finished_transfer(&mut self, transfer: &VenueTransfer) {
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
                let _ = self.portfolios.apply_delta(
                    transfer.account_id,
                    transfer.asset_id.clone(),
                    -transfer.amount,
                );
            }
            (VenueTransferKind::Deposit, VenueTransferStatus::Rejected) => {
                let _ = self.portfolios.release(
                    transfer.account_id,
                    transfer.asset_id.clone(),
                    transfer.amount,
                );
            }
            (VenueTransferKind::Withdrawal, VenueTransferStatus::Completed) => {
                let _ = self.portfolios.apply_delta(
                    transfer.account_id,
                    transfer.asset_id.clone(),
                    transfer.amount,
                );
            }
            (VenueTransferKind::Withdrawal, VenueTransferStatus::Rejected)
            | (_, VenueTransferStatus::Pending) => {}
        }
    }

    fn check_venue_rules(
        &self,
        instrument_id: &str,
        command: &Command,
    ) -> Result<Option<VenueRuleRejectReason>, ActorRejectReason> {
        let market = self.market(instrument_id)?;
        let instrument = market.config().instrument();
        let market_kind = market.kind();

        Ok(self
            .venue_rules
            .check_order(VenueRuleOrderContext {
                market_step: self.clock.step(),
                market_time_ms: self.clock.market_time_ms(),
                instrument_id,
                instrument,
                market_kind,
                command,
                venue_accounts: &self.venue_accounts,
            })
            .err())
    }

    fn clearing_rejection(
        &self,
        instrument_id: &str,
        command_seq: ActorSeq,
        error: ClearingError,
    ) -> ActorExecution {
        ActorExecution {
            rejected_command: None,
            price_updates: Vec::new(),
            funding_settlement: None,
            room_id: self.room_id.clone(),
            instrument_id: instrument_id.to_string(),
            command_seq,
            market_time_ms: self.clock.market_time_ms(),
            status: self.status(),
            result: ActorExecutionResult::Rejected(ActorRejectReason::Clearing(error)),
        }
    }

    fn sync_market_accounts_from_venue(
        &mut self,
        instrument_id: &str,
    ) -> Result<(), ClearingError> {
        #[cfg(test)]
        if tests::INDEXED_MARGIN_REFERENCE.get() {
            return self.sync_market_accounts_from_venue_indexed_reference(instrument_id);
        }
        let market = self
            .markets
            .get(instrument_id)
            .ok_or(ClearingError::AccountNotFound)?;
        let instrument = market.config().instrument().clone();
        if market.kind() == MarketKind::Perp {
            return self.sync_perp_cross_margin_group(&instrument.quote_asset);
        }
        let AccountSnapshots::Spot(accounts) = market.account_snapshots() else {
            return Err(ClearingError::WrongMarketKind);
        };
        let account_reservations = accounts
            .into_iter()
            .map(|account| {
                (
                    account.account_id,
                    account.reserved_cash,
                    account.reserved_position,
                )
            })
            .collect::<Vec<_>>();

        let merge = account_reservations.len() >= self.venue_accounts.account_count() / 4
            && account_reservations
                .windows(2)
                .all(|pair| pair[0].0 < pair[1].0);
        let mut quote = self
            .venue_accounts
            .balances_for_asset(&instrument.quote_asset)
            .peekable();
        let mut base = self
            .venue_accounts
            .balances_for_asset(&instrument.base_asset)
            .peekable();
        for (account_id, own_quote_reservation, own_base_reservation) in account_reservations {
            let quote_available = if merge {
                ordered_available_balance(&mut quote, account_id)
            } else {
                self.venue_available_balance(account_id, &instrument.quote_asset)
            };
            let quote_balance = quote_available
                .map(|available| {
                    available
                        .max(0)
                        .checked_add(own_quote_reservation)
                        .ok_or(ClearingError::BalanceOverflow)
                })
                .transpose()?
                .unwrap_or(own_quote_reservation);
            let base_available = if merge {
                ordered_available_balance(&mut base, account_id)
            } else {
                self.venue_available_balance(account_id, &instrument.base_asset)
            };
            let base_balance = base_available
                .map(|available| {
                    available
                        .checked_add(own_base_reservation)
                        .ok_or(ClearingError::BalanceOverflow)
                })
                .transpose()?
                .unwrap_or(own_base_reservation);
            self.markets
                .get_mut(instrument_id)
                .ok_or(ClearingError::AccountNotFound)?
                .sync_venue_balances(account_id, quote_balance, base_balance)?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn sync_market_accounts_from_venue_indexed_reference(
        &mut self,
        instrument_id: &str,
    ) -> Result<(), ClearingError> {
        let market = self
            .markets
            .get(instrument_id)
            .ok_or(ClearingError::AccountNotFound)?;
        let instrument = market.config().instrument().clone();
        if market.kind() == MarketKind::Perp {
            return self.sync_perp_cross_margin_group(&instrument.quote_asset);
        }
        let AccountSnapshots::Spot(accounts) = market.account_snapshots() else {
            return Err(ClearingError::WrongMarketKind);
        };
        let account_reservations = accounts
            .into_iter()
            .map(|account| {
                (
                    account.account_id,
                    account.reserved_cash,
                    account.reserved_position,
                )
            })
            .collect::<Vec<_>>();

        for (account_id, own_quote_reservation, own_base_reservation) in account_reservations {
            let quote_balance = self
                .venue_available_balance(account_id, &instrument.quote_asset)
                .map(|available| {
                    available
                        .max(0)
                        .checked_add(own_quote_reservation)
                        .ok_or(ClearingError::BalanceOverflow)
                })
                .transpose()?
                .unwrap_or(own_quote_reservation);
            let base_balance = self
                .venue_available_balance(account_id, &instrument.base_asset)
                .map(|available| {
                    available
                        .checked_add(own_base_reservation)
                        .ok_or(ClearingError::BalanceOverflow)
                })
                .transpose()?
                .unwrap_or(own_base_reservation);
            self.markets
                .get_mut(instrument_id)
                .ok_or(ClearingError::AccountNotFound)?
                .sync_venue_balances(account_id, quote_balance, base_balance)?;
        }
        Ok(())
    }

    fn sync_perp_cross_margin_group(&mut self, quote_asset: &str) -> Result<(), ClearingError> {
        self.sync_perp_cross_margin_group_for(quote_asset, None)
    }

    fn sync_perp_cross_margin_group_for(
        &mut self,
        quote_asset: &str,
        affected: Option<&BTreeSet<AccountId>>,
    ) -> Result<(), ClearingError> {
        #[cfg(test)]
        if tests::SCALAR_MARGIN_REFERENCE.get() {
            return self.sync_perp_cross_margin_group_reference(quote_asset, affected);
        }
        #[cfg(test)]
        if tests::INDEXED_MARGIN_REFERENCE.get() {
            return self.sync_perp_cross_margin_group_indexed_reference(quote_asset, affected);
        }
        let _timer = crate::performance::Timer::start(1);
        let market_accounts = self
            .markets
            .iter()
            .filter(|(_, market)| {
                market.kind() == MarketKind::Perp
                    && market.config().instrument().quote_asset == quote_asset
            })
            .map(|(instrument_id, market)| {
                let MarketEngine::Perp(engine) = &market.engine else {
                    unreachable!("filtered to perp markets");
                };
                let accounts = engine.margin_inputs(affected);
                // PerpAccountStore emits snapshots in BTreeMap account order.
                // Binary search avoids both linear peer scans and rebuilding
                // a separately allocated tree for every synchronization.
                debug_assert!(
                    accounts
                        .windows(2)
                        .all(|pair| pair[0].account_id < pair[1].account_id)
                );
                (instrument_id.clone(), accounts)
            })
            .collect::<BTreeMap<_, _>>();
        let liquidation_pending_accounts = self
            .markets
            .iter()
            .filter(|(_, market)| {
                market.kind() == MarketKind::Perp
                    && market.config().instrument().quote_asset == quote_asset
            })
            .flat_map(|(_, market)| market.pending_liquidation_accounts())
            .collect::<std::collections::BTreeSet<_>>();

        for (target_instrument_id, target_accounts) in &market_accounts {
            let sorted = target_accounts
                .windows(2)
                .all(|pair| pair[0].account_id < pair[1].account_id);
            let mut peers = market_accounts
                .iter()
                .filter(|(id, _)| *id != target_instrument_id)
                .map(|(_, accounts)| {
                    let ordered = sorted
                        && target_accounts.len() >= accounts.len() / 4
                        && accounts
                            .windows(2)
                            .all(|pair| pair[0].account_id < pair[1].account_id);
                    (accounts.as_slice(), 0usize, ordered)
                })
                .collect::<Vec<_>>();
            let merge_venue =
                sorted && target_accounts.len() >= self.venue_accounts.account_count() / 4;
            let mut venue = self
                .venue_accounts
                .balances_for_asset(quote_asset)
                .peekable();
            let mut requests = Vec::with_capacity(target_accounts.len());
            let mut request_error = None;
            for target_account in target_accounts {
                let request = (|| {
                    let mut context = PerpCrossMarginContext {
                        liquidation_pending: liquidation_pending_accounts
                            .contains(&target_account.account_id),
                        ..PerpCrossMarginContext::default()
                    };
                    for (accounts, cursor, ordered) in &mut peers {
                        let other_account = if *ordered {
                            while accounts.get(*cursor).is_some_and(|account| {
                                account.account_id < target_account.account_id
                            }) {
                                *cursor += 1;
                            }
                            accounts
                                .get(*cursor)
                                .filter(|account| account.account_id == target_account.account_id)
                        } else {
                            accounts
                                .binary_search_by_key(&target_account.account_id, |account| {
                                    account.account_id
                                })
                                .ok()
                                .map(|index| &accounts[index])
                        };
                        let Some(other_account) = other_account else {
                            continue;
                        };
                        context.other_unrealized_pnl = context
                            .other_unrealized_pnl
                            .checked_add(other_account.unrealized_pnl)
                            .ok_or(ClearingError::BalanceOverflow)?;
                        context.other_required_margin = context
                            .other_required_margin
                            .checked_add(other_account.collateral_reservation()?)
                            .ok_or(ClearingError::BalanceOverflow)?;
                        context.other_initial_margin = context
                            .other_initial_margin
                            .checked_add(other_account.initial_margin)
                            .ok_or(ClearingError::BalanceOverflow)?;
                        context.other_maintenance_margin = context
                            .other_maintenance_margin
                            .checked_add(other_account.maintenance_margin)
                            .ok_or(ClearingError::BalanceOverflow)?;
                        context.other_position_open |= other_account.position_open;
                    }

                    let cross_group_reservation = target_account
                        .collateral_reservation()?
                        .checked_add(context.other_required_margin)
                        .ok_or(ClearingError::BalanceOverflow)?;
                    let available = if merge_venue {
                        ordered_available_balance(&mut venue, target_account.account_id)
                    } else {
                        self.venue_available_balance(target_account.account_id, quote_asset)
                    };
                    let cash_balance = available
                        .map(|available| {
                            available
                                .checked_add(cross_group_reservation)
                                .and_then(|value| {
                                    value.checked_sub(reservation_amount(
                                        &self.funding_collateral_shortfalls,
                                        target_account.account_id,
                                        quote_asset,
                                    ))
                                })
                                .ok_or(ClearingError::BalanceOverflow)
                        })
                        .transpose()?
                        .unwrap_or(cross_group_reservation);
                    Ok((target_account.account_id, cash_balance, context))
                })();
                match request {
                    Ok(request) => requests.push(request),
                    Err(error) => {
                        request_error = Some(error);
                        break;
                    }
                }
            }
            let market = self
                .markets
                .get_mut(target_instrument_id)
                .ok_or(ClearingError::AccountNotFound)?;
            let MarketEngine::Perp(engine) = &mut market.engine else {
                unreachable!("filtered to perp markets");
            };
            // Preserve the scalar failure order: earlier account sync errors
            // take precedence over a later request-construction error.
            engine.sync_cross_margin_accounts(&requests)?;
            if let Some(error) = request_error {
                return Err(error);
            }
        }

        Ok(())
    }

    #[cfg(test)]
    fn sync_perp_cross_margin_group_indexed_reference(
        &mut self,
        quote_asset: &str,
        affected: Option<&BTreeSet<AccountId>>,
    ) -> Result<(), ClearingError> {
        let _timer = crate::performance::Timer::start(1);
        let market_accounts = self
            .markets
            .iter()
            .filter(|(_, market)| {
                market.kind() == MarketKind::Perp
                    && market.config().instrument().quote_asset == quote_asset
            })
            .map(|(instrument_id, market)| {
                let MarketEngine::Perp(engine) = &market.engine else {
                    unreachable!("filtered to perp markets");
                };
                let accounts = engine.margin_inputs(affected);
                // PerpAccountStore emits snapshots in BTreeMap account order.
                // Binary search avoids both linear peer scans and rebuilding
                // a separately allocated tree for every synchronization.
                debug_assert!(
                    accounts
                        .windows(2)
                        .all(|pair| pair[0].account_id < pair[1].account_id)
                );
                (instrument_id.clone(), accounts)
            })
            .collect::<BTreeMap<_, _>>();
        let liquidation_pending_accounts = self
            .markets
            .iter()
            .filter(|(_, market)| {
                market.kind() == MarketKind::Perp
                    && market.config().instrument().quote_asset == quote_asset
            })
            .flat_map(|(_, market)| market.pending_liquidation_accounts())
            .collect::<std::collections::BTreeSet<_>>();

        for (target_instrument_id, target_accounts) in &market_accounts {
            let mut requests = Vec::with_capacity(target_accounts.len());
            let mut request_error = None;
            for target_account in target_accounts {
                let request = (|| {
                    let mut context = PerpCrossMarginContext {
                        liquidation_pending: liquidation_pending_accounts
                            .contains(&target_account.account_id),
                        ..PerpCrossMarginContext::default()
                    };
                    for (other_instrument_id, other_accounts) in &market_accounts {
                        if other_instrument_id == target_instrument_id {
                            continue;
                        }
                        let Some(other_account) = other_accounts
                            .binary_search_by_key(&target_account.account_id, |account| {
                                account.account_id
                            })
                            .ok()
                            .map(|index| &other_accounts[index])
                        else {
                            continue;
                        };
                        context.other_unrealized_pnl = context
                            .other_unrealized_pnl
                            .checked_add(other_account.unrealized_pnl)
                            .ok_or(ClearingError::BalanceOverflow)?;
                        context.other_required_margin = context
                            .other_required_margin
                            .checked_add(other_account.collateral_reservation()?)
                            .ok_or(ClearingError::BalanceOverflow)?;
                        context.other_initial_margin = context
                            .other_initial_margin
                            .checked_add(other_account.initial_margin)
                            .ok_or(ClearingError::BalanceOverflow)?;
                        context.other_maintenance_margin = context
                            .other_maintenance_margin
                            .checked_add(other_account.maintenance_margin)
                            .ok_or(ClearingError::BalanceOverflow)?;
                        context.other_position_open |= other_account.position_open;
                    }

                    let cross_group_reservation = target_account
                        .collateral_reservation()?
                        .checked_add(context.other_required_margin)
                        .ok_or(ClearingError::BalanceOverflow)?;
                    let cash_balance = self
                        .venue_available_balance(target_account.account_id, quote_asset)
                        .map(|available| {
                            available
                                .checked_add(cross_group_reservation)
                                .and_then(|value| {
                                    value.checked_sub(reservation_amount(
                                        &self.funding_collateral_shortfalls,
                                        target_account.account_id,
                                        quote_asset,
                                    ))
                                })
                                .ok_or(ClearingError::BalanceOverflow)
                        })
                        .transpose()?
                        .unwrap_or(cross_group_reservation);
                    Ok((target_account.account_id, cash_balance, context))
                })();
                match request {
                    Ok(request) => requests.push(request),
                    Err(error) => {
                        request_error = Some(error);
                        break;
                    }
                }
            }
            let market = self
                .markets
                .get_mut(target_instrument_id)
                .ok_or(ClearingError::AccountNotFound)?;
            let MarketEngine::Perp(engine) = &mut market.engine else {
                unreachable!("filtered to perp markets");
            };
            // Preserve the scalar failure order: earlier account sync errors
            // take precedence over a later request-construction error.
            engine.sync_cross_margin_accounts(&requests)?;
            if let Some(error) = request_error {
                return Err(error);
            }
        }

        Ok(())
    }

    // Frozen pre-batch algorithm, used only as an independent test oracle.
    #[cfg(test)]
    fn sync_perp_cross_margin_group_reference(
        &mut self,
        quote_asset: &str,
        affected: Option<&BTreeSet<AccountId>>,
    ) -> Result<(), ClearingError> {
        let _timer = crate::performance::Timer::start(1);
        let market_accounts = self
            .markets
            .iter()
            .filter(|(_, market)| {
                market.kind() == MarketKind::Perp
                    && market.config().instrument().quote_asset == quote_asset
            })
            .map(|(instrument_id, market)| {
                let accounts = if let Some(affected) = affected {
                    affected
                        .iter()
                        .filter_map(|&id| match market.account_snapshot(id) {
                            Some(AccountSnapshot::Perp(account)) => Some(account),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                } else {
                    let AccountSnapshots::Perp(accounts) = market.account_snapshots() else {
                        unreachable!("perp market must expose perp accounts");
                    };
                    accounts
                };
                // PerpAccountStore emits snapshots in BTreeMap account order.
                // Binary search avoids both linear peer scans and rebuilding
                // a separately allocated tree for every synchronization.
                debug_assert!(
                    accounts
                        .windows(2)
                        .all(|pair| pair[0].account_id < pair[1].account_id)
                );
                (instrument_id.clone(), accounts)
            })
            .collect::<BTreeMap<_, _>>();
        let liquidation_pending_accounts = self
            .markets
            .iter()
            .filter(|(_, market)| {
                market.kind() == MarketKind::Perp
                    && market.config().instrument().quote_asset == quote_asset
            })
            .flat_map(|(_, market)| market.pending_liquidation_accounts())
            .collect::<std::collections::BTreeSet<_>>();

        for (target_instrument_id, target_accounts) in &market_accounts {
            for target_account in target_accounts {
                let mut context = PerpCrossMarginContext {
                    liquidation_pending: liquidation_pending_accounts
                        .contains(&target_account.account_id),
                    ..PerpCrossMarginContext::default()
                };
                for (other_instrument_id, other_accounts) in &market_accounts {
                    if other_instrument_id == target_instrument_id {
                        continue;
                    }
                    let Some(other_account) = other_accounts
                        .binary_search_by_key(&target_account.account_id, |account| {
                            account.account_id
                        })
                        .ok()
                        .map(|index| &other_accounts[index])
                    else {
                        continue;
                    };
                    context.other_unrealized_pnl = context
                        .other_unrealized_pnl
                        .checked_add(other_account.unrealized_pnl)
                        .ok_or(ClearingError::BalanceOverflow)?;
                    context.other_required_margin = context
                        .other_required_margin
                        .checked_add(perp_collateral_reservation(other_account)?)
                        .ok_or(ClearingError::BalanceOverflow)?;
                    context.other_initial_margin = context
                        .other_initial_margin
                        .checked_add(other_account.initial_margin)
                        .ok_or(ClearingError::BalanceOverflow)?;
                    context.other_maintenance_margin = context
                        .other_maintenance_margin
                        .checked_add(other_account.maintenance_margin)
                        .ok_or(ClearingError::BalanceOverflow)?;
                    context.other_position_open |= other_account.has_open_position();
                }

                let cross_group_reservation = perp_collateral_reservation(target_account)?
                    .checked_add(context.other_required_margin)
                    .ok_or(ClearingError::BalanceOverflow)?;
                let cash_balance = self
                    .venue_accounts
                    .balance_snapshot(target_account.account_id, quote_asset)
                    .map(|balance| {
                        balance
                            .available
                            .checked_add(cross_group_reservation)
                            .and_then(|value| {
                                value.checked_sub(reservation_amount(
                                    &self.funding_collateral_shortfalls,
                                    target_account.account_id,
                                    quote_asset,
                                ))
                            })
                            .ok_or(ClearingError::BalanceOverflow)
                    })
                    .transpose()?
                    .unwrap_or(cross_group_reservation);
                let market = self
                    .markets
                    .get_mut(target_instrument_id)
                    .ok_or(ClearingError::AccountNotFound)?;
                let MarketEngine::Perp(engine) = &mut market.engine else {
                    unreachable!()
                };
                engine.sync_cross_margin_account(
                    target_account.account_id,
                    cash_balance,
                    context,
                )?;
            }
        }

        Ok(())
    }

    fn reconcile_market_reservations(&mut self) -> Result<(), ClearingError> {
        #[cfg(test)]
        if tests::FULL_RESERVATION_REFERENCE.get() {
            return self.reconcile_market_reservations_for(None);
        }
        let mut changes = self.venue_accounts.reservation_changes.accounts().cloned();
        for market in self.markets.values() {
            let market_changes = match &market.engine {
                MarketEngine::Spot(engine) => engine.reservation_changes(),
                MarketEngine::Perp(engine) => engine.reservation_changes(),
            };
            match (&mut changes, market_changes.accounts()) {
                (Some(ids), Some(changed)) => ids.extend(changed),
                _ => {
                    changes = None;
                    break;
                }
            }
        }
        // A dense update is cheaper through raw linear projections than through
        // per-account snapshot lookups. This also establishes a full baseline.
        if changes
            .as_ref()
            .is_some_and(|ids| ids.len() > self.venue_accounts.account_count() / 4)
        {
            changes = None;
        }
        self.reconcile_market_reservations_for(changes.as_ref())
    }

    fn reconcile_market_reservations_for(
        &mut self,
        affected: Option<&BTreeSet<AccountId>>,
    ) -> Result<(), ClearingError> {
        let _timer = crate::performance::Timer::start(2);
        if affected.is_some_and(BTreeSet::is_empty) {
            return Ok(());
        }
        let mut next = if let Some(affected) = affected {
            let mut next = MarketReservations::new();
            for market in self.markets.values() {
                let instrument = market.config().instrument();
                for &id in affected {
                    match market.account_snapshot(id) {
                        Some(AccountSnapshot::Spot(account)) => {
                            add_market_reservation(
                                &mut next,
                                id,
                                &instrument.quote_asset,
                                account.reserved_cash,
                            )?;
                            add_market_reservation(
                                &mut next,
                                id,
                                &instrument.base_asset,
                                account.reserved_position,
                            )?;
                        }
                        Some(AccountSnapshot::Perp(account)) => {
                            add_market_reservation(
                                &mut next,
                                id,
                                &instrument.quote_asset,
                                perp_collateral_reservation(&account)?,
                            )?;
                        }
                        None => {}
                    }
                }
            }
            next
        } else {
            aggregate_market_reservations(&self.markets)?
        };
        let mut shortfalls = MarketReservations::new();
        for (account_id, asset_id) in self.funding_liability_accounts() {
            if affected.is_some_and(|ids| !ids.contains(&account_id)) {
                continue;
            }
            let logical = reservation_amount(&next, account_id, &asset_id);
            let previous = reservation_amount(&self.market_reservations, account_id, &asset_id);
            if let Some(balance) = self.venue_accounts.balance_snapshot(account_id, &asset_id) {
                let outside = balance
                    .reserved
                    .checked_sub(previous)
                    .ok_or(ClearingError::ReservationUnderflow)?;
                let budget = balance
                    .total
                    .checked_sub(outside)
                    .ok_or(ClearingError::BalanceOverflow)?
                    .max(0);
                let actual = logical.min(budget);
                if logical > actual {
                    next.entry(account_id)
                        .or_default()
                        .insert(asset_id.clone(), actual);
                    shortfalls
                        .entry(account_id)
                        .or_default()
                        .insert(asset_id, logical - actual);
                }
            }
        }
        let previous_accounts = if let Some(ids) = affected {
            ids.iter()
                .filter_map(|id| self.market_reservations.get_key_value(id))
                .collect::<Vec<_>>()
        } else {
            self.market_reservations.iter().collect()
        };
        for (account_id, balances) in previous_accounts {
            for (asset_id, &previous) in balances {
                let next_amount = reservation_amount(&next, *account_id, asset_id);
                if previous > next_amount {
                    self.venue_accounts
                        .release(*account_id, asset_id.clone(), previous - next_amount)
                        .map_err(clearing_error_from_venue_account_error)?;
                }
            }
        }
        for (account_id, balances) in &next {
            for (asset_id, &next_amount) in balances {
                let previous = reservation_amount(&self.market_reservations, *account_id, asset_id);
                if next_amount > previous {
                    self.venue_accounts
                        .reserve(*account_id, asset_id.clone(), next_amount - previous)
                        .map_err(clearing_error_from_venue_account_error)?;
                }
            }
        }
        if let Some(ids) = affected {
            for id in ids {
                self.market_reservations.remove(id);
                self.funding_collateral_shortfalls.remove(id);
            }
            self.market_reservations.extend(next);
            self.funding_collateral_shortfalls.extend(shortfalls);
        } else {
            self.market_reservations = next;
            self.funding_collateral_shortfalls = shortfalls;
        }
        // Clear only after every release/reserve succeeds. Partial scopes cannot
        // establish an unknown baseline; rollback clones retain their own tracking.
        self.venue_accounts.reservation_changes.reconciled(affected);
        for market in self.markets.values_mut() {
            match &mut market.engine {
                MarketEngine::Spot(engine) => engine.reservation_changes_mut().reconciled(affected),
                MarketEngine::Perp(engine) => engine.reservation_changes_mut().reconciled(affected),
            }
        }
        Ok(())
    }

    fn funding_liability_accounts(&self) -> std::collections::BTreeSet<(AccountId, String)> {
        let funded_quotes: std::collections::BTreeSet<_> = self
            .markets
            .values()
            .filter_map(|market| {
                let MarketConfig::Perp(config) = market.config() else {
                    return None;
                };
                // Hedge gross margin can exceed collateral after a mark move
                // even at zero net exposure. Preserve that liability so the
                // room can liquidate it instead of rolling back the mark.
                if config.clearing.position_mode == crate::PositionMode::Hedge {
                    return Some(config.instrument.quote_asset.clone());
                }
                config
                    .funding
                    .as_ref()
                    .map(|_| config.instrument.quote_asset.clone())
            })
            .collect();
        self.markets
            .values()
            .filter_map(|market| {
                let MarketConfig::Perp(config) = market.config() else {
                    return None;
                };
                if !funded_quotes.contains(&config.instrument.quote_asset) {
                    return None;
                }
                let MarketEngine::Perp(engine) = &market.engine else {
                    unreachable!();
                };
                Some(
                    engine
                        .open_position_accounts()
                        .map(|account_id| (account_id, config.instrument.quote_asset.clone()))
                        .collect::<Vec<_>>(),
                )
            })
            .flatten()
            .collect()
    }

    fn venue_available_balance(&self, account_id: AccountId, asset_id: &str) -> Option<Money> {
        #[cfg(test)]
        if tests::SNAPSHOT_VENUE_REFERENCE.get() {
            return self
                .venue_accounts
                .balance_snapshot(account_id, asset_id)
                .map(|balance| balance.available);
        }
        self.venue_accounts.available_balance(account_id, asset_id)
    }

    fn validate_venue_after_clearing(&self) -> Result<(), ClearingError> {
        #[cfg(test)]
        if tests::SNAPSHOT_VENUE_REFERENCE.get() {
            return self.validate_venue_after_clearing_reference();
        }
        // Check every raw balance. Only an underfunded balance can need the
        // funding-liability exception; ordinary orders avoid building either
        // public snapshots or an all-position liability projection.
        let mut liabilities = None;
        for (account, asset, balance) in self.venue_accounts.balance_entries() {
            if balance.reserved < 0 {
                return Err(ClearingError::InsufficientAvailableBalance);
            }
            if balance.total < balance.reserved
                && !liabilities
                    .get_or_insert_with(|| self.funding_liability_accounts())
                    .contains(&(account, asset.to_string()))
            {
                return Err(ClearingError::InsufficientAvailableBalance);
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn validate_venue_after_clearing_reference(&self) -> Result<(), ClearingError> {
        let liabilities = self.funding_liability_accounts();
        for account in self.venue_accounts.account_snapshots() {
            for balance in account.balances {
                if balance.reserved < 0
                    || (balance.total < balance.reserved
                        && !liabilities.contains(&(account.account_id, balance.asset_id)))
                {
                    return Err(ClearingError::InsufficientAvailableBalance);
                }
            }
        }
        Ok(())
    }

    fn sync_venue_accounts_from_execution(
        &mut self,
        instrument_id: &str,
        execution: &ActorExecution,
    ) -> Result<(), ClearingError> {
        let Some(instrument) = self
            .markets
            .get(instrument_id)
            .map(|market| market.config().instrument().clone())
        else {
            return Ok(());
        };

        let ActorExecutionResult::Accepted(market_execution) = &execution.result else {
            return Ok(());
        };
        match market_execution {
            MarketExecution::Spot(spot_execution) => {
                for event in &spot_execution.clearing_events {
                    self.apply_spot_clearing_to_venue(
                        event,
                        &instrument.base_asset,
                        &instrument.quote_asset,
                    )?;
                }
                self.venue_rules.record_spot_clearing(
                    self.clock.step(),
                    &instrument.instrument_id,
                    &spot_execution.clearing_events,
                );
            }
            MarketExecution::Perp(perp_execution) => {
                for event in &perp_execution.clearing_events {
                    self.apply_perp_clearing_to_venue(event, &instrument.quote_asset)?;
                }
            }
        }
        if execution.funding_settlement.is_some() {
            self.reconcile_market_reservations()?;
        }
        self.validate_venue_after_clearing()?;
        Ok(())
    }

    fn apply_spot_clearing_to_venue(
        &mut self,
        event: &SpotClearingEvent,
        base_asset: &str,
        quote_asset: &str,
    ) -> Result<(), ClearingError> {
        let SpotClearingEvent::TradeSettled {
            buyer_account_id,
            seller_account_id,
            qty,
            notional,
            buyer_fee,
            seller_fee,
            ..
        } = event;
        let buyer_quote_delta = notional
            .checked_add(*buyer_fee)
            .and_then(|amount| amount.checked_neg())
            .ok_or(ClearingError::BalanceOverflow)?;
        self.venue_accounts
            .apply_delta(
                *buyer_account_id,
                quote_asset.to_string(),
                buyer_quote_delta,
            )
            .map_err(clearing_error_from_venue_account_error)?;
        self.venue_accounts
            .apply_delta(
                *buyer_account_id,
                base_asset.to_string(),
                PositionQty::from(*qty),
            )
            .map_err(clearing_error_from_venue_account_error)?;
        let seller_quote_delta = notional
            .checked_sub(*seller_fee)
            .ok_or(ClearingError::BalanceOverflow)?;
        self.venue_accounts
            .apply_delta(
                *seller_account_id,
                quote_asset.to_string(),
                seller_quote_delta,
            )
            .map_err(clearing_error_from_venue_account_error)?;
        self.venue_accounts
            .apply_delta(
                *seller_account_id,
                base_asset.to_string(),
                -PositionQty::from(*qty),
            )
            .map_err(clearing_error_from_venue_account_error)?;
        Ok(())
    }

    fn apply_perp_clearing_to_venue(
        &mut self,
        event: &PerpClearingEvent,
        quote_asset: &str,
    ) -> Result<(), ClearingError> {
        match event {
            PerpClearingEvent::FundingSettled {
                account_id,
                cash_delta,
                ..
            } => {
                self.venue_accounts
                    .apply_signed_delta(*account_id, quote_asset.to_string(), *cash_delta)
                    .map_err(clearing_error_from_venue_account_error)?;
            }
            PerpClearingEvent::TradeSettled {
                buyer_account_id,
                seller_account_id,
                buyer_fee,
                seller_fee,
                buyer_realized_pnl,
                seller_realized_pnl,
                ..
            } => {
                let buyer_delta = buyer_realized_pnl
                    .checked_sub(*buyer_fee)
                    .ok_or(ClearingError::BalanceOverflow)?;
                self.venue_accounts
                    .apply_signed_delta(*buyer_account_id, quote_asset.to_string(), buyer_delta)
                    .map_err(clearing_error_from_venue_account_error)?;
                let seller_delta = seller_realized_pnl
                    .checked_sub(*seller_fee)
                    .ok_or(ClearingError::BalanceOverflow)?;
                self.venue_accounts
                    .apply_signed_delta(*seller_account_id, quote_asset.to_string(), seller_delta)
                    .map_err(clearing_error_from_venue_account_error)?;
            }
            PerpClearingEvent::LiquidationSettled {
                account_id,
                liquidation_fee,
                insurance_fund_payment,
                auto_deleveraging_loss,
                auto_deleveraging_allocations,
                socialized_loss,
                socialized_loss_allocations,
                bad_debt,
                ..
            } => {
                let liquidated_delta = liquidation_fee
                    .checked_neg()
                    .and_then(|delta| delta.checked_add(*insurance_fund_payment))
                    .and_then(|delta| delta.checked_add(*auto_deleveraging_loss))
                    .and_then(|delta| delta.checked_add(*socialized_loss))
                    .and_then(|delta| delta.checked_add(*bad_debt))
                    .ok_or(ClearingError::BalanceOverflow)?;
                self.venue_accounts
                    .apply_signed_delta(*account_id, quote_asset.to_string(), liquidated_delta)
                    .map_err(clearing_error_from_venue_account_error)?;
                for allocation in auto_deleveraging_allocations {
                    let allocation_delta = allocation
                        .realized_pnl
                        .checked_sub(allocation.loss)
                        .ok_or(ClearingError::BalanceOverflow)?;
                    self.venue_accounts
                        .apply_signed_delta(
                            allocation.account_id,
                            quote_asset.to_string(),
                            allocation_delta,
                        )
                        .map_err(clearing_error_from_venue_account_error)?;
                }
                for allocation in socialized_loss_allocations {
                    let allocation_delta = allocation
                        .loss
                        .checked_neg()
                        .ok_or(ClearingError::BalanceOverflow)?;
                    self.venue_accounts
                        .apply_signed_delta(
                            allocation.account_id,
                            quote_asset.to_string(),
                            allocation_delta,
                        )
                        .map_err(clearing_error_from_venue_account_error)?;
                }
            }
            PerpClearingEvent::MarginStatusChanged { .. } => {}
        }
        Ok(())
    }
}

fn aggregate_market_reservations(
    markets: &BTreeMap<InstrumentId, MarketActor>,
) -> Result<MarketReservations, ClearingError> {
    let mut reservations = MarketReservations::new();
    for market in markets.values() {
        let instrument = market.config().instrument();
        match &market.engine {
            MarketEngine::Spot(engine) => {
                for (account_id, reserved_cash, reserved_position) in engine.reservation_balances()
                {
                    add_market_reservation(
                        &mut reservations,
                        account_id,
                        &instrument.quote_asset,
                        reserved_cash,
                    )?;
                    add_market_reservation(
                        &mut reservations,
                        account_id,
                        &instrument.base_asset,
                        reserved_position,
                    )?;
                }
            }
            MarketEngine::Perp(engine) => {
                for reservation in engine.reservation_balances() {
                    let (account_id, amount) = reservation?;
                    add_market_reservation(
                        &mut reservations,
                        account_id,
                        &instrument.quote_asset,
                        amount,
                    )?;
                }
            }
        }
    }
    Ok(reservations)
}

fn perp_collateral_reservation(account: &PerpAccountSnapshot) -> Result<Money, ClearingError> {
    account
        .initial_margin
        .checked_add(account.reserved_margin)
        .ok_or(ClearingError::BalanceOverflow)
}

fn add_market_reservation(
    reservations: &mut MarketReservations,
    account_id: AccountId,
    asset_id: &str,
    amount: Money,
) -> Result<(), ClearingError> {
    if amount < 0 {
        return Err(ClearingError::ReservationUnderflow);
    }
    if amount == 0 {
        return Ok(());
    }
    let current = reservations
        .entry(account_id)
        .or_default()
        .entry(asset_id.to_string())
        .or_default();
    *current = current
        .checked_add(amount)
        .ok_or(ClearingError::BalanceOverflow)?;
    Ok(())
}

fn reservation_amount(
    reservations: &MarketReservations,
    account_id: AccountId,
    asset_id: &str,
) -> Money {
    reservations
        .get(&account_id)
        .and_then(|balances| balances.get(asset_id))
        .copied()
        .unwrap_or(0)
}

fn clearing_error_from_venue_account_error(error: VenueAccountError) -> ClearingError {
    match error {
        VenueAccountError::BalanceOverflow => ClearingError::BalanceOverflow,
        VenueAccountError::NegativeAmount
        | VenueAccountError::InsufficientAvailableBalance
        | VenueAccountError::InsufficientReservedBalance => {
            ClearingError::InsufficientAvailableBalance
        }
    }
}

impl MarketActor {
    pub fn new(
        room_id: impl Into<RoomId>,
        config: MarketConfig,
    ) -> Result<Self, MarketConfigError> {
        let engine = config.build_engine()?;
        Ok(Self {
            room_id: room_id.into(),
            config,
            engine,
            status: MarketStatus::Running,
            next_command_seq: 0,
        })
    }

    pub fn room_id(&self) -> &str {
        &self.room_id
    }

    pub fn config(&self) -> &MarketConfig {
        &self.config
    }

    pub fn kind(&self) -> MarketKind {
        self.engine.kind()
    }

    pub fn status(&self) -> MarketStatus {
        self.status
    }

    pub fn next_command_seq(&self) -> ActorSeq {
        self.next_command_seq
    }

    pub fn pause(&mut self) {
        if self.status == MarketStatus::Running {
            self.status = MarketStatus::Paused;
        }
    }

    pub fn resume(&mut self) {
        if self.status == MarketStatus::Paused {
            self.status = MarketStatus::Running;
        }
    }

    pub fn close(&mut self) {
        self.status = MarketStatus::Closed;
    }

    pub fn restore_status(&mut self, status: MarketStatus) {
        self.status = status;
    }

    pub fn create_account(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
    ) -> AccountSnapshot {
        self.engine.create_account(account_id, cash_balance)
    }

    pub fn create_spot_account_with_position(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
        position_qty: PositionQty,
    ) -> Result<SpotAccountSnapshot, ActorRejectReason> {
        match &mut self.engine {
            MarketEngine::Spot(engine) => {
                Ok(engine.create_account_with_position(account_id, cash_balance, position_qty))
            }
            MarketEngine::Perp(_) => Err(ActorRejectReason::WrongMarketKind),
        }
    }

    fn sync_venue_balances(
        &mut self,
        account_id: AccountId,
        quote_balance: Money,
        base_balance: PositionQty,
    ) -> Result<(), ClearingError> {
        match &mut self.engine {
            MarketEngine::Spot(engine) => {
                #[cfg(test)]
                if tests::SNAPSHOT_VENUE_REFERENCE.get() || tests::SPOT_SYNC_REFERENCE.get() {
                    engine.sync_account_balances_reference(
                        account_id,
                        quote_balance,
                        base_balance,
                    )?;
                    return Ok(());
                }
                engine.sync_account_balances(account_id, quote_balance, base_balance)?;
            }
            MarketEngine::Perp(engine) => {
                engine.sync_cash_balance(account_id, quote_balance)?;
            }
        }
        Ok(())
    }

    pub fn apply(&mut self, command: Command) -> ActorExecution {
        self.apply_from(command, CommandOrigin::External)
    }

    pub fn apply_from(&mut self, command: Command, origin: CommandOrigin) -> ActorExecution {
        let retained = command.clone();
        let mut execution = self.apply_from_inner(command, origin);
        if matches!(execution.result, ActorExecutionResult::Rejected(_)) {
            execution.rejected_command = Some(retained);
        }
        execution
    }

    fn apply_from_inner(&mut self, command: Command, origin: CommandOrigin) -> ActorExecution {
        let seq = self.take_command_seq();

        if matches!(command, Command::SettleFunding(_)) {
            return ActorExecution {
                rejected_command: None,
                room_id: self.room_id.clone(),
                instrument_id: self.config.instrument_id().into(),
                command_seq: seq,
                market_time_ms: 0,
                status: self.status,
                price_updates: Vec::new(),
                funding_settlement: None,
                result: ActorExecutionResult::Rejected(ActorRejectReason::FundingManaged),
            };
        }

        if self.status == MarketStatus::Closed {
            return ActorExecution {
                rejected_command: None,
                price_updates: Vec::new(),
                funding_settlement: None,
                room_id: self.room_id.clone(),
                instrument_id: self.config.instrument_id().to_string(),
                command_seq: seq,
                market_time_ms: 0,
                status: self.status,
                result: ActorExecutionResult::Rejected(ActorRejectReason::MarketClosed),
            };
        }

        if self.status == MarketStatus::Paused
            && matches!(command, Command::NewOrder(_))
            && origin != CommandOrigin::Scheduler
        {
            return ActorExecution {
                rejected_command: None,
                price_updates: Vec::new(),
                funding_settlement: None,
                room_id: self.room_id.clone(),
                instrument_id: self.config.instrument_id().to_string(),
                command_seq: seq,
                market_time_ms: 0,
                status: self.status,
                result: ActorExecutionResult::Rejected(ActorRejectReason::MarketPaused),
            };
        }

        let result = match self.engine.apply(command) {
            Ok(result) => ActorExecutionResult::Accepted(result),
            Err(error) => ActorExecutionResult::Rejected(ActorRejectReason::Clearing(error)),
        };

        ActorExecution {
            rejected_command: None,
            price_updates: Vec::new(),
            funding_settlement: None,
            room_id: self.room_id.clone(),
            instrument_id: self.config.instrument_id().to_string(),
            command_seq: seq,
            market_time_ms: 0,
            status: self.status,
            result,
        }
    }

    pub fn liquidate_account(
        &mut self,
        account_id: AccountId,
        order_id: OrderId,
    ) -> ActorExecution {
        let seq = self.take_command_seq();

        if self.status == MarketStatus::Closed {
            return ActorExecution {
                rejected_command: None,
                price_updates: Vec::new(),
                funding_settlement: None,
                room_id: self.room_id.clone(),
                instrument_id: self.config.instrument_id().to_string(),
                command_seq: seq,
                market_time_ms: 0,
                status: self.status,
                result: ActorExecutionResult::Rejected(ActorRejectReason::MarketClosed),
            };
        }

        let result = match &mut self.engine {
            MarketEngine::Spot(_) => {
                ActorExecutionResult::Rejected(ActorRejectReason::WrongMarketKind)
            }
            MarketEngine::Perp(engine) => match engine.liquidate_account(account_id, order_id) {
                Ok(result) => ActorExecutionResult::Accepted(MarketExecution::Perp(result)),
                Err(error) => ActorExecutionResult::Rejected(ActorRejectReason::Clearing(error)),
            },
        };

        ActorExecution {
            rejected_command: None,
            price_updates: Vec::new(),
            funding_settlement: None,
            room_id: self.room_id.clone(),
            instrument_id: self.config.instrument_id().to_string(),
            command_seq: seq,
            market_time_ms: 0,
            status: self.status,
            result,
        }
    }

    pub fn book_snapshot(&self) -> BookSnapshot {
        self.engine.book_snapshot()
    }

    pub fn account_snapshot(&self, account_id: AccountId) -> Option<AccountSnapshot> {
        self.engine.account_snapshot(account_id)
    }

    pub fn account_snapshots(&self) -> AccountSnapshots {
        self.engine.account_snapshots()
    }

    pub fn pending_liquidation_accounts(&self) -> Vec<AccountId> {
        self.engine.pending_liquidation_accounts()
    }

    fn collateral_resting_order_ids_for_account(
        &self,
        account_id: AccountId,
        collateral_asset: &str,
    ) -> Vec<OrderId> {
        match &self.engine {
            MarketEngine::Perp(engine) => engine.resting_order_ids_for_account(account_id),
            MarketEngine::Spot(engine) => {
                let instrument = self.config.instrument();
                let mut order_ids = Vec::new();
                if instrument.quote_asset == collateral_asset {
                    order_ids.extend(
                        engine.resting_order_ids_for_account_on_side(account_id, Side::Buy),
                    );
                }
                if instrument.base_asset == collateral_asset {
                    order_ids.extend(
                        engine.resting_order_ids_for_account_on_side(account_id, Side::Sell),
                    );
                }
                order_ids.sort_unstable();
                order_ids.dedup();
                order_ids
            }
        }
    }

    pub fn order_owner(&self, order_id: OrderId) -> Option<AccountId> {
        self.engine.order_owner(order_id)
    }

    pub fn resting_orders_for_account(&self, account_id: AccountId) -> Vec<crate::model::Order> {
        self.engine.resting_orders_for_account(account_id)
    }

    fn take_command_seq(&mut self) -> ActorSeq {
        let seq = self.next_command_seq;
        self.next_command_seq += 1;
        seq
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActorExecution {
    /// Original command for actor-level rejection, where no engine command log exists.
    pub rejected_command: Option<Command>,
    pub room_id: RoomId,
    pub instrument_id: InstrumentId,
    pub command_seq: ActorSeq,
    /// Authoritative simulation time when this command was evaluated.
    pub market_time_ms: u64,
    pub status: MarketStatus,
    pub result: ActorExecutionResult,
    /// Derived, public price changes, committed with the originating command.
    pub price_updates: Vec<crate::PerpPriceSnapshot>,
    pub funding_settlement: Option<crate::FundingSettlement>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActorExecutionResult {
    Accepted(MarketExecution),
    Rejected(ActorRejectReason),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActorRejectReason {
    InvalidOrderProtection,
    InvalidPositionProtection,
    OrderProtectionExpired {
        deadline_market_time_ms: u64,
        market_time_ms: u64,
    },
    MarketPaused,
    MarketClosed,
    InstrumentNotFound {
        instrument_id: InstrumentId,
    },
    WrongMarketKind,
    PriceLinkNotReady,
    LinkedMarkPriceManaged,
    FundingManaged,
    VenueRule(VenueRuleRejectReason),
    Clearing(ClearingError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarketExecution {
    Spot(SpotTradingExecution),
    Perp(PerpTradingExecution),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum AccountSnapshot {
    Spot(SpotAccountSnapshot),
    Perp(PerpAccountSnapshot),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum AccountSnapshots {
    Spot(Vec<SpotAccountSnapshot>),
    Perp(Vec<PerpAccountSnapshot>),
}

impl MarketEngine {
    pub fn create_account(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
    ) -> AccountSnapshot {
        match self {
            Self::Spot(engine) => {
                AccountSnapshot::Spot(engine.create_account(account_id, cash_balance))
            }
            Self::Perp(engine) => {
                AccountSnapshot::Perp(engine.create_account(account_id, cash_balance))
            }
        }
    }

    pub fn apply(&mut self, command: Command) -> Result<MarketExecution, ClearingError> {
        match self {
            Self::Spot(engine) => engine.apply(command).map(MarketExecution::Spot),
            Self::Perp(engine) => engine.apply(command).map(MarketExecution::Perp),
        }
    }

    pub fn book_snapshot(&self) -> BookSnapshot {
        match self {
            Self::Spot(engine) => engine.snapshot(),
            Self::Perp(engine) => engine.snapshot(),
        }
    }

    pub fn account_snapshot(&self, account_id: AccountId) -> Option<AccountSnapshot> {
        match self {
            Self::Spot(engine) => engine
                .account_snapshot(account_id)
                .map(AccountSnapshot::Spot),
            Self::Perp(engine) => engine
                .account_snapshot(account_id)
                .map(AccountSnapshot::Perp),
        }
    }

    pub fn account_snapshots(&self) -> AccountSnapshots {
        match self {
            Self::Spot(engine) => AccountSnapshots::Spot(engine.account_snapshots()),
            Self::Perp(engine) => AccountSnapshots::Perp(engine.account_snapshots()),
        }
    }

    pub fn pending_liquidation_accounts(&self) -> Vec<AccountId> {
        match self {
            Self::Spot(_) => Vec::new(),
            Self::Perp(engine) => engine.pending_liquidation_accounts(),
        }
    }

    pub fn order_owner(&self, order_id: OrderId) -> Option<AccountId> {
        match self {
            Self::Spot(engine) => engine.order_owner(order_id),
            Self::Perp(engine) => engine.order_owner(order_id),
        }
    }

    pub fn resting_orders_for_account(&self, account_id: AccountId) -> Vec<crate::model::Order> {
        match self {
            Self::Spot(engine) => engine.resting_orders_for_account(account_id),
            Self::Perp(engine) => engine.resting_orders_for_account(account_id),
        }
    }
}

fn reject_reason_from_venue_account_error(error: VenueAccountError) -> VenueTransferRejectReason {
    match error {
        VenueAccountError::BalanceOverflow => VenueTransferRejectReason::BalanceOverflow,
        VenueAccountError::NegativeAmount
        | VenueAccountError::InsufficientAvailableBalance
        | VenueAccountError::InsufficientReservedBalance => {
            VenueTransferRejectReason::InsufficientAvailableBalance
        }
    }
}

include!("position_protection_actor.rs");
include!("conditional_orders_actor.rs");

#[cfg(test)]
mod tests {
    thread_local! {
        pub(super) static FULL_RESERVATION_REFERENCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        pub(super) static SCALAR_MARGIN_REFERENCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        pub(super) static INDEXED_MARGIN_REFERENCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        pub(super) static SNAPSHOT_VENUE_REFERENCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        pub(super) static SPOT_SYNC_REFERENCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    use super::*;
    use crate::{
        PerpRiskConfig, SpotRiskConfig,
        market::{ExchangeConfig, InstrumentConfig, PerpMarketConfig, SpotMarketConfig},
        model::{BookLevel, CancelOrder, Event, NewOrder, OrderKind, SetMarkPrice, Side},
        perp::{PerpClearingConfig, PerpMarginStatus},
        spot::SpotClearingConfig,
        transfer::VenueTransferStatus,
        venue_rules::{
            PriceLimitRuleConfig, SettlementRuleConfig, TradingSessionRuleConfig,
            TradingSessionWindow, TransferPolicyConfig, VenueRuleConfig,
        },
    };

    fn spot_config() -> MarketConfig {
        MarketConfig::Spot(SpotMarketConfig {
            instrument: InstrumentConfig::new("V-BTC-SPOT", 1, 1).unwrap(),
            clearing: SpotClearingConfig::default(),
            risk: SpotRiskConfig::default(),
        })
    }

    fn btc_usdt_spot_config() -> MarketConfig {
        MarketConfig::Spot(SpotMarketConfig {
            instrument: InstrumentConfig::new_for_venue(
                "binance",
                "binance:btc-usdt:spot",
                "BTC",
                "USDT",
                "BTC-USDT Spot",
                1,
                1,
            )
            .unwrap(),
            clearing: SpotClearingConfig::default(),
            risk: SpotRiskConfig::default(),
        })
    }

    fn venue_spot_config(instrument_id: &str, base_asset: &str, quote_asset: &str) -> MarketConfig {
        MarketConfig::Spot(SpotMarketConfig {
            instrument: InstrumentConfig::new_for_venue(
                "binance",
                instrument_id,
                base_asset,
                quote_asset,
                instrument_id,
                1,
                1,
            )
            .unwrap(),
            clearing: SpotClearingConfig::default(),
            risk: SpotRiskConfig::default(),
        })
    }

    fn venue_perp_config(instrument_id: &str, base_asset: &str, quote_asset: &str) -> MarketConfig {
        MarketConfig::Perp(PerpMarketConfig {
            instrument: InstrumentConfig::new_for_venue(
                "binance",
                instrument_id,
                base_asset,
                quote_asset,
                instrument_id,
                1,
                1,
            )
            .unwrap(),
            clearing: PerpClearingConfig {
                leverage: 10,
                maintenance_margin_ppm: 50_000,
                ..PerpClearingConfig::default()
            },
            risk: PerpRiskConfig::default(),
            initial_mark_price_tick: 100,
            price_link: None,
            funding: None,
        })
    }

    fn limit(order_id: u64, account_id: u64, side: Side, price_tick: i64, qty: u64) -> Command {
        Command::NewOrder(NewOrder {
            position_side: crate::model::PositionSide::Both,
            order_id,
            account_id,
            side,
            kind: OrderKind::Limit { price_tick },
            qty,
            reduce_only: false,
        })
    }

    fn market(order_id: u64, account_id: u64, side: Side, qty: u64) -> Command {
        Command::NewOrder(NewOrder {
            position_side: crate::model::PositionSide::Both,
            order_id,
            account_id,
            side,
            kind: OrderKind::Market,
            qty,
            reduce_only: false,
        })
    }

    fn reduce_only_market(order_id: u64, account_id: u64, side: Side, qty: u64) -> Command {
        Command::NewOrder(NewOrder {
            position_side: crate::model::PositionSide::Both,
            order_id,
            account_id,
            side,
            kind: OrderKind::Market,
            qty,
            reduce_only: true,
        })
    }

    // Compare every durable field and execution with the original full-scan
    // transaction, including intermediate states and a serialized restart.
    #[test]
    fn incremental_perp_orders_match_full_sync_and_restore() {
        for hedge in [false, true] {
            let mut configs = vec![
                venue_perp_config("btc", "BTC", "USDT"),
                venue_perp_config("eth", "ETH", "USDT"),
            ];
            if hedge {
                for config in &mut configs {
                    let MarketConfig::Perp(config) = config else {
                        unreachable!()
                    };
                    config.clearing.position_mode = crate::PositionMode::Hedge;
                }
            }
            let mut incremental =
                ExchangeActor::new("parity", ExchangeConfig::new("binance", configs).unwrap())
                    .unwrap();
            for id in 1..=32 {
                incremental.create_account(id, if id == 32 { 5 } else { 100_000 });
            }
            for (instrument, mut command) in [
                ("eth", limit(1001, 1, Side::Sell, 120, 4)),
                ("btc", limit(1002, 2, Side::Buy, 80, 4)),
                ("btc", limit(1003, 3, Side::Buy, 70, 4)),
            ] {
                if let Command::NewOrder(order) = &mut command {
                    if hedge {
                        order.position_side = if order.side == Side::Buy {
                            crate::PositionSide::Long
                        } else {
                            crate::PositionSide::Short
                        };
                    }
                    if order.order_id == 1003 {
                        order.kind = OrderKind::Protected {
                            order_type: crate::model::ProtectedOrderType::Limit,
                            price_tick: 70,
                            valid_until_market_time_ms: None,
                            expires_at_market_time_ms: Some(1000),
                        };
                    }
                }
                incremental
                    .apply_to_instrument(instrument, command)
                    .unwrap();
            }
            let mut full = incremental.clone();
            let mut covered = [false; 4];
            for n in 0..180_u64 {
                let instrument = if n % 2 == 0 { "btc" } else { "eth" };
                let id = n + 1;
                let account = (n * 7) % 32 + 1;
                let mut command = match n % 12 {
                    6 => Command::CancelOrder(crate::CancelOrder { order_id: 1002 }),
                    7 => Command::AmendOrder(crate::model::AmendOrder {
                        order_id: 1001,
                        price_tick: Some(121),
                        qty: Some(2),
                    }),
                    8 => Command::ExpireOrder {
                        order_id: 1003,
                        market_time_ms: 1000,
                    },
                    9 => market(id, account, Side::Buy, 3),
                    10 => Command::SetMarkPrice(crate::SetMarkPrice {
                        price_tick: 95 + (n % 11) as i64,
                    }),
                    11 => limit(id, 32, Side::Buy, 100, 1000),
                    _ => limit(
                        id,
                        account,
                        if n % 4 < 2 { Side::Sell } else { Side::Buy },
                        100,
                        2,
                    ),
                };
                if hedge && let Command::NewOrder(order) = &mut command {
                    order.position_side = if order.side == Side::Buy {
                        crate::PositionSide::Long
                    } else {
                        crate::PositionSide::Short
                    };
                }
                let a = incremental.apply_to_instrument_synced(
                    instrument,
                    command.clone(),
                    CommandOrigin::External,
                    true,
                );
                crate::shared_map::EAGER_CLONE.set(true);
                INDEXED_MARGIN_REFERENCE.set(true);
                SCALAR_MARGIN_REFERENCE.set(true);
                FULL_RESERVATION_REFERENCE.set(true);
                SNAPSHOT_VENUE_REFERENCE.set(true);
                let b = full.apply_to_instrument_synced(
                    instrument,
                    command,
                    CommandOrigin::External,
                    false,
                );
                crate::shared_map::EAGER_CLONE.set(false);
                INDEXED_MARGIN_REFERENCE.set(false);
                SCALAR_MARGIN_REFERENCE.set(false);
                FULL_RESERVATION_REFERENCE.set(false);
                SNAPSHOT_VENUE_REFERENCE.set(false);
                assert_eq!(a, b, "execution {n}, hedge={hedge}");
                if let Ok(ActorExecution {
                    result: ActorExecutionResult::Accepted(MarketExecution::Perp(result)),
                    ..
                }) = &a
                {
                    for record in &result.events {
                        match record.event {
                            crate::Event::TradePrinted(_) => covered[0] = true,
                            crate::Event::OrderCanceled { .. } => covered[1] = true,
                            crate::Event::OrderAmended { .. } => covered[2] = true,
                            crate::Event::OrderExpired { order_id: 1003, .. } => covered[3] = true,
                            _ => {}
                        }
                    }
                }
                let a_state = serde_json::to_value(&incremental).unwrap();
                let b_state = serde_json::to_value(&full).unwrap();
                fn compare(a: &serde_json::Value, b: &serde_json::Value, path: &str) {
                    if let (Some(a), Some(b)) = (a.as_object(), b.as_object()) {
                        assert_eq!(
                            a.keys().collect::<Vec<_>>(),
                            b.keys().collect::<Vec<_>>(),
                            "{path}"
                        );
                        for (key, value) in a {
                            compare(value, &b[key], &format!("{path}/{key}"));
                        }
                    } else if path.ends_with("/seen_order_ids") {
                        // HashSet iteration order is randomized on deserialize.
                        let ids = |value: &serde_json::Value| {
                            value
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|id| id.as_u64().unwrap())
                                .collect::<BTreeSet<_>>()
                        };
                        assert_eq!(ids(a), ids(b), "{path}");
                    } else {
                        assert_eq!(a, b, "{path}");
                    }
                }
                compare(&a_state, &b_state, &format!("state {n}, hedge={hedge}"));
                if n == 90 {
                    incremental =
                        serde_json::from_value(serde_json::to_value(&incremental).unwrap())
                            .unwrap();
                    full = serde_json::from_value(serde_json::to_value(&full).unwrap()).unwrap();
                }
            }
            assert_eq!(covered, [true; 4], "hedge={hedge}");
        }
    }

    #[test]
    #[ignore = "controlled release performance comparison; run alone with --nocapture"]
    fn incremental_perp_fixed_work_benchmark() {
        let mut initial = ExchangeActor::new(
            "bench",
            ExchangeConfig::new(
                "binance",
                vec![
                    venue_perp_config("btc", "BTC", "USDT"),
                    venue_perp_config("eth", "ETH", "USDT"),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        for id in 1..=1000 {
            initial.create_account(id, 1_000_000);
        }
        let mut reference = None;
        for incremental in [false, true, true, false, false, true] {
            let mut actor = initial.clone();
            let before = crate::performance::snapshot();
            let start = std::time::Instant::now();
            let mut executions = Vec::new();
            for n in 0..600 {
                let instrument = if n % 4 < 2 { "btc" } else { "eth" };
                executions.push(
                    actor
                        .apply_to_instrument_synced(
                            instrument,
                            limit(
                                n + 1,
                                n % 1000 + 1,
                                if n % 2 == 0 { Side::Sell } else { Side::Buy },
                                100,
                                2,
                            ),
                            CommandOrigin::External,
                            incremental,
                        )
                        .unwrap(),
                );
            }
            let elapsed = start.elapsed().as_secs_f64();
            let after = crate::performance::snapshot();
            println!(
                "incremental={incremental} seconds={elapsed:.6} margin_us={} reservation_us={}",
                after[1].1 - before[1].1,
                after[2].1 - before[2].1
            );
            let state = serde_json::to_value(actor).unwrap();
            if let Some((ref_state, ref_executions)) = &reference {
                assert_eq!(&state, ref_state);
                assert_eq!(&executions, ref_executions);
            } else {
                reference = Some((state, executions));
            }
        }
    }

    #[test]
    #[ignore = "controlled release performance comparison; run alone with --nocapture"]
    fn ordered_margin_sync_fixed_work_benchmark() {
        let mut initial = ExchangeActor::new(
            "bench",
            ExchangeConfig::new(
                "binance",
                vec![
                    venue_perp_config("btc", "BTC", "USDT"),
                    venue_perp_config("eth", "ETH", "USDT"),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        for id in 1..=1000 {
            initial.create_account(id, 1_000_000);
        }
        let mut reference = None;
        for batch in [false, true, true, false, false, true, true, false] {
            SCALAR_MARGIN_REFERENCE.set(!batch);
            let mut actor = initial.clone();
            let before = crate::performance::snapshot();
            let start = std::time::Instant::now();
            let mut executions = Vec::new();
            for n in 0..600 {
                let instrument = if n % 4 < 2 { "btc" } else { "eth" };
                executions.push(
                    actor
                        .apply_to_instrument_synced(
                            instrument,
                            limit(
                                n + 1,
                                n % 1000 + 1,
                                if n % 2 == 0 { Side::Sell } else { Side::Buy },
                                100,
                                2,
                            ),
                            CommandOrigin::External,
                            true,
                        )
                        .unwrap(),
                );
            }
            let elapsed = start.elapsed().as_secs_f64();
            SCALAR_MARGIN_REFERENCE.set(false);
            let after = crate::performance::snapshot();
            println!(
                "batch={batch} seconds={elapsed:.6} margin_us={} reservation_us={}",
                after[1].1 - before[1].1,
                after[2].1 - before[2].1
            );
            let state = serde_json::to_value(actor).unwrap();
            if let Some((ref_state, ref_executions)) = &reference {
                assert_eq!(&state, ref_state);
                assert_eq!(&executions, ref_executions);
            } else {
                reference = Some((state, executions));
            }
        }
    }

    #[test]
    #[ignore = "controlled release performance comparison; run alone with --nocapture"]
    fn dirty_reservation_fixed_work_benchmark() {
        let mut initial = ExchangeActor::new(
            "bench",
            ExchangeConfig::new(
                "binance",
                vec![
                    venue_perp_config("btc", "BTC", "USDT"),
                    venue_perp_config("eth", "ETH", "USDT"),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        for id in 1..=1000 {
            initial.create_account(id, 1_000_000);
        }
        let mut reference = None;
        for dirty in [false, true, true, false, false, true, true, false] {
            FULL_RESERVATION_REFERENCE.set(!dirty);
            let mut actor = initial.clone();
            let before = crate::performance::snapshot();
            let start = std::time::Instant::now();
            let mut executions = Vec::new();
            for n in 0..600 {
                let instrument = if n % 4 < 2 { "btc" } else { "eth" };
                executions.push(
                    actor
                        .apply_to_instrument_synced(
                            instrument,
                            limit(
                                n + 1,
                                n % 1000 + 1,
                                if n % 2 == 0 { Side::Sell } else { Side::Buy },
                                100,
                                2,
                            ),
                            CommandOrigin::External,
                            true,
                        )
                        .unwrap(),
                );
            }
            let elapsed = start.elapsed().as_secs_f64();
            FULL_RESERVATION_REFERENCE.set(false);
            let after = crate::performance::snapshot();
            println!(
                "dirty={dirty} seconds={elapsed:.6} margin_us={} reservation_us={}",
                after[1].1 - before[1].1,
                after[2].1 - before[2].1
            );
            let state = serde_json::to_value(actor).unwrap();
            if let Some((ref_state, ref_executions)) = &reference {
                assert_eq!(&state, ref_state);
                assert_eq!(&executions, ref_executions);
            } else {
                reference = Some((state, executions));
            }
        }
    }

    #[test]
    #[ignore = "fixed-work release comparison; run explicitly"]
    fn raw_venue_fixed_work_benchmark() {
        let mut initial = ExchangeActor::new(
            "raw-venue-bench",
            ExchangeConfig::new(
                "binance",
                vec![
                    venue_perp_config("btc", "BTC", "USDT"),
                    venue_perp_config("eth", "ETH", "USDT"),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        for id in 1..=1000 {
            initial.create_account(id, 1_000_000);
        }
        let mut reference = None;
        for raw in [false, true, true, false, false, true, true, false] {
            SNAPSHOT_VENUE_REFERENCE.set(!raw);
            let mut actor = initial.clone();
            let start = std::time::Instant::now();
            let mut executions = Vec::new();
            for n in 0..600 {
                executions.push(
                    actor
                        .apply_to_instrument(
                            if n % 4 < 2 { "btc" } else { "eth" },
                            limit(
                                n + 1,
                                n % 1000 + 1,
                                if n % 2 == 0 { Side::Sell } else { Side::Buy },
                                100,
                                2,
                            ),
                        )
                        .unwrap(),
                );
            }
            println!("raw={raw} seconds={:.6}", start.elapsed().as_secs_f64());
            SNAPSHOT_VENUE_REFERENCE.set(false);
            let result = (canonical_actor(&actor), executions);
            if let Some(prior) = &reference {
                assert_eq!(&result, prior);
            } else {
                reference = Some(result);
            }
        }
    }

    #[test]
    fn margin_merge_matches_indexed_with_disjoint_accounts_gaps_restore_and_overflow() {
        let mut initial = ExchangeActor::new(
            "margin-merge",
            ExchangeConfig::new(
                "binance",
                vec![
                    venue_perp_config("a", "A", "USDT"),
                    venue_perp_config("b", "B", "USDT"),
                    venue_perp_config("c", "C", "USDT"),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        for id in 1..=96 {
            if id == 77 {
                continue;
            }
            initial.venue_accounts.set_balance(
                id,
                if id % 5 == 0 { "OTHER" } else { "USDT" },
                1_000_000,
            );
            for (index, name) in ["a", "b", "c"].into_iter().enumerate() {
                if id % (index as u64 + 2) != 0 {
                    initial
                        .market_mut(name)
                        .unwrap()
                        .create_account(id, 1_000_000);
                }
            }
        }
        initial
            .apply_to_instrument("a", limit(1, 1, Side::Sell, 100, 2))
            .unwrap();
        initial
            .apply_to_instrument("a", limit(2, 3, Side::Buy, 100, 2))
            .unwrap();
        // Account 77 has no local market account. A dense merge crosses its
        // venue entry but must not evaluate overflowing available arithmetic.
        let mut venue = serde_json::to_value(&initial.venue_accounts).unwrap();
        venue["balances"]["77"] = serde_json::json!({"USDT":{"total":"MIN_SENTINEL","reserved":1}});
        let encoded = serde_json::to_string(&venue)
            .unwrap()
            .replace("\"MIN_SENTINEL\"", &Money::MIN.to_string());
        initial.venue_accounts = serde_json::from_str(&encoded).unwrap();
        for affected in [
            None,
            Some(BTreeSet::new()),
            Some(BTreeSet::from([1, 19, 96])),
            Some(BTreeSet::from([9999])),
        ] {
            for restored in [false, true] {
                let mut merged = if restored {
                    serde_json::from_str::<ExchangeActor>(&serde_json::to_string(&initial).unwrap())
                        .unwrap()
                } else {
                    initial.clone()
                };
                let mut indexed = merged.clone();
                let result = merged.sync_perp_cross_margin_group_for("USDT", affected.as_ref());
                INDEXED_MARGIN_REFERENCE.set(true);
                let reference = indexed.sync_perp_cross_margin_group_for("USDT", affected.as_ref());
                INDEXED_MARGIN_REFERENCE.set(false);
                assert_eq!(result, reference);
                assert_eq!(
                    serde_json::to_string(&merged).unwrap(),
                    serde_json::to_string(&indexed).unwrap()
                );
            }
        }
        let mut venue = serde_json::to_value(
            serde_json::from_str::<VenueAccountStore>(
                &encoded.replace(&Money::MIN.to_string(), "0"),
            )
            .unwrap(),
        )
        .unwrap();
        venue["balances"]["1"]["USDT"] = serde_json::json!({"total":"MAX_SENTINEL","reserved":0});
        initial.venue_accounts = serde_json::from_str(
            &serde_json::to_string(&venue)
                .unwrap()
                .replace("\"MAX_SENTINEL\"", &Money::MAX.to_string()),
        )
        .unwrap();
        let mut indexed = initial.clone();
        let result = initial.sync_perp_cross_margin_group_for("USDT", None);
        INDEXED_MARGIN_REFERENCE.set(true);
        let reference = indexed.sync_perp_cross_margin_group_for("USDT", None);
        INDEXED_MARGIN_REFERENCE.set(false);
        assert_eq!(result, Err(ClearingError::BalanceOverflow));
        assert_eq!(result, reference);
        assert_eq!(
            serde_json::to_string(&initial).unwrap(),
            serde_json::to_string(&indexed).unwrap()
        );
    }

    #[test]
    #[ignore = "fixed-work release comparison; run explicitly"]
    fn margin_merge_fixed_work_benchmark() {
        for count in [0usize, 1, 2, 4] {
            let configs = (0..count)
                .map(|n| venue_perp_config(&format!("m{n}"), &format!("ASSET{n}"), "USDT"))
                .collect();
            let config = if count == 0 {
                ExchangeConfig::new_single(spot_config()).unwrap()
            } else {
                ExchangeConfig::new("binance", configs).unwrap()
            };
            let mut initial = ExchangeActor::new("margin-merge-bench", config).unwrap();
            for id in 1..=1000 {
                if count == 0 {
                    initial
                        .create_spot_account_with_position(id, 1_000_000, 100)
                        .unwrap();
                } else {
                    initial.create_account(id, 1_000_000);
                }
            }
            let instruments = if count == 0 {
                vec!["V-BTC-SPOT".to_string()]
            } else {
                (0..count).map(|n| format!("m{n}")).collect::<Vec<_>>()
            };
            let mut reference = None;
            for merge in [false, true, true, false, false, true, true, false] {
                INDEXED_MARGIN_REFERENCE.set(!merge);
                let mut actor = initial.clone();
                let phases_before = crate::performance::snapshot();
                let start = std::time::Instant::now();
                let mut executions = Vec::new();
                for n in 0..600 {
                    executions.push(
                        actor
                            .apply_to_instrument(
                                &instruments[(n as usize / 2) % instruments.len()],
                                limit(
                                    n + 1,
                                    n % 1000 + 1,
                                    if n % 2 == 0 { Side::Sell } else { Side::Buy },
                                    100,
                                    2,
                                ),
                            )
                            .unwrap(),
                    );
                }
                let seconds = start.elapsed().as_secs_f64();
                INDEXED_MARGIN_REFERENCE.set(false);
                let phases_after = crate::performance::snapshot();
                println!(
                    "markets={count} merge={merge} seconds={seconds:.6} margin_us={}",
                    phases_after[1].1 - phases_before[1].1
                );
                let result = (canonical_actor(&actor), executions);
                if let Some(prior) = &reference {
                    assert_eq!(&result, prior);
                } else {
                    reference = Some(result);
                }
            }
        }
    }

    #[test]
    #[ignore = "fixed-work release comparison; run explicitly"]
    fn shared_tables_fixed_work_benchmark() {
        for perp in [true, false] {
            let config = if perp {
                ExchangeConfig::new(
                    "binance",
                    vec![
                        venue_perp_config("btc", "BTC", "USDT"),
                        venue_perp_config("eth", "ETH", "USDT"),
                    ],
                )
                .unwrap()
            } else {
                ExchangeConfig::new_single(spot_config()).unwrap()
            };
            let mut initial = ExchangeActor::new("shared-tables-bench", config).unwrap();
            for id in 1..=1000 {
                if perp {
                    initial.create_account(id, 1_000_000);
                } else {
                    initial
                        .create_spot_account_with_position(id, 1_000_000, 100)
                        .unwrap();
                }
            }
            let frozen = canonical_actor(&initial);
            let mut reference = None;
            for shared in [false, true, true, false, false, true, true, false] {
                crate::shared_map::EAGER_CLONE.set(!shared);
                let mut actor = initial.clone();
                let phases_before = crate::performance::snapshot();
                let start = std::time::Instant::now();
                let mut executions = Vec::new();
                for n in 0..600 {
                    let instrument = if !perp {
                        "V-BTC-SPOT"
                    } else if n % 4 < 2 {
                        "btc"
                    } else {
                        "eth"
                    };
                    executions.push(
                        actor
                            .apply_to_instrument(
                                instrument,
                                limit(
                                    n + 1,
                                    n % 1000 + 1,
                                    if n % 2 == 0 { Side::Sell } else { Side::Buy },
                                    100,
                                    2,
                                ),
                            )
                            .unwrap(),
                    );
                }
                let seconds = start.elapsed().as_secs_f64();
                crate::shared_map::EAGER_CLONE.set(false);
                let phases_after = crate::performance::snapshot();
                println!(
                    "perp={perp} shared={shared} seconds={seconds:.6} clone_us={} margin_us={} reservation_us={}",
                    phases_after[0].1 - phases_before[0].1,
                    phases_after[1].1 - phases_before[1].1,
                    phases_after[2].1 - phases_before[2].1
                );
                let result = (canonical_actor(&actor), executions);
                if let Some(prior) = &reference {
                    assert_eq!(&result, prior);
                } else {
                    reference = Some(result);
                }
                assert_eq!(canonical_actor(&initial), frozen);
            }
        }
    }

    #[test]
    #[ignore = "fixed-work release comparison; run explicitly"]
    fn spot_balance_sync_fixed_work_benchmark() {
        let mut initial = ExchangeActor::new(
            "spot-sync-bench",
            ExchangeConfig::new_single(spot_config()).unwrap(),
        )
        .unwrap();
        for id in 1..=1000 {
            initial
                .create_spot_account_with_position(id, 1_000_000, 100)
                .unwrap();
        }
        let mut reference = None;
        for exact in [false, true, true, false, false, true, true, false] {
            SPOT_SYNC_REFERENCE.set(!exact);
            let mut actor = initial.clone();
            let start = std::time::Instant::now();
            let mut executions = Vec::new();
            for n in 0..600 {
                executions.push(actor.apply(limit(
                    n + 1,
                    n % 1000 + 1,
                    if n % 2 == 0 { Side::Sell } else { Side::Buy },
                    100,
                    2,
                )));
            }
            println!("exact={exact} seconds={:.6}", start.elapsed().as_secs_f64());
            SPOT_SYNC_REFERENCE.set(false);
            let result = (canonical_actor(&actor), executions);
            if let Some(prior) = &reference {
                assert_eq!(&result, prior);
            } else {
                reference = Some(result);
            }
        }
    }

    #[test]
    fn raw_venue_checks_preserve_funding_exception_and_reject_other_deficits() {
        let mut actor = crate::funding_tests::scenario(100_000)
            .bootstrap()
            .unwrap()
            .exchange;
        actor
            .apply_to_instrument("V-USD-PERP", limit(3, 10, Side::Sell, 10000, 1))
            .unwrap();
        actor
            .apply_to_instrument("V-USD-PERP", limit(4, 20, Side::Buy, 10000, 1))
            .unwrap();
        let assert_same = |actor: &ExchangeActor| {
            assert_eq!(
                actor.validate_venue_after_clearing(),
                actor.validate_venue_after_clearing_reference()
            );
        };
        assert_same(&actor);
        actor
            .venue_accounts
            .apply_signed_delta(10, "USD", -200_000)
            .unwrap();
        assert_same(&actor);
        assert!(actor.validate_venue_after_clearing().is_ok());
        actor.venue_accounts.set_balance(99, "USD", -1);
        assert_same(&actor);
        assert!(actor.validate_venue_after_clearing().is_err());
        let restored: ExchangeActor = serde_json::from_value(canonical_actor(&actor)).unwrap();
        assert_same(&restored);
        let mut malformed = canonical_actor(&actor);
        malformed["venue_accounts"]["balances"]["10"]["USD"]["reserved"] = serde_json::json!(-1);
        let malformed: ExchangeActor = serde_json::from_value(malformed).unwrap();
        assert_same(&malformed);
        assert!(malformed.validate_venue_after_clearing().is_err());
    }

    fn canonical_actor(actor: &ExchangeActor) -> serde_json::Value {
        fn normalize(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::Object(fields) => {
                    for (key, value) in fields {
                        if key == "seen_order_ids" {
                            value.as_array_mut().unwrap().sort_by_key(|id| id.as_u64());
                        } else {
                            normalize(value);
                        }
                    }
                }
                serde_json::Value::Array(values) => values.iter_mut().for_each(normalize),
                _ => {}
            }
        }
        let mut state = serde_json::to_value(actor).unwrap();
        normalize(&mut state);
        state
    }

    #[test]
    fn dirty_reservations_match_full_with_funding_transfers_and_restore() {
        let mut dirty = crate::funding_tests::scenario(100_000)
            .bootstrap()
            .unwrap()
            .exchange;
        let mut full = dirty.clone();
        let mut funding_count = 0;
        for n in 0..120_u64 {
            let action = |actor: &mut ExchangeActor| -> String {
                match n % 20 {
                    0 => format!(
                        "{:?}",
                        actor.apply_to_instrument(
                            "V-USD-PERP",
                            limit(n + 100, 10, Side::Sell, 10_000, 1)
                        )
                    ),
                    1 => format!(
                        "{:?}",
                        actor.apply_to_instrument(
                            "V-USD-PERP",
                            limit(n + 100, 20, Side::Buy, 10_000, 1)
                        )
                    ),
                    2 | 3 => format!("{:?}", actor.advance_clock(1)),
                    4 => format!("{:?}", actor.apply_venue_asset_delta(10, "USD", 1000)),
                    5 => format!("{:?}", actor.submit_venue_withdrawal(20, "USD", 100)),
                    6 => format!("{:?}", actor.submit_venue_deposit(20, "USD", 200)),
                    7 => format!(
                        "{:?}",
                        actor.apply_to_instrument(
                            "V-USD-SPOT",
                            limit(n + 100, 10, Side::Buy, 10_001, 1)
                        )
                    ),
                    8 => format!(
                        "{:?}",
                        actor.apply_venue_asset_delta(10, "USD", -100_000_000)
                    ),
                    9 => {
                        *actor =
                            serde_json::from_value(serde_json::to_value(&*actor).unwrap()).unwrap();
                        assert!(
                            actor
                                .venue_accounts
                                .reservation_changes
                                .accounts()
                                .is_none()
                        );
                        format!("{:?}", actor.normalize_after_restore())
                    }
                    10 => {
                        actor
                            .market_mut("V-USD-SPOT")
                            .unwrap()
                            .create_account(99, 500);
                        assert!(
                            actor
                                .venue_accounts
                                .reservation_changes
                                .accounts()
                                .is_none()
                        );
                        format!("{:?}", actor.reconcile_market_reservations())
                    }
                    11 => {
                        actor.primary_market_mut().create_account(99, 700);
                        format!("{:?}", actor.reconcile_market_reservations())
                    }
                    12 => format!(
                        "{:?}",
                        actor.apply_to_instrument(
                            "V-USD-SPOT",
                            Command::CancelOrder(crate::CancelOrder { order_id: n + 95 })
                        )
                    ),
                    13 => format!("{:?}", actor.advance_clock(2)),
                    14 => format!("{:?}", actor.liquidate_account("V-USD-PERP", 10, n + 1000)),
                    15 => {
                        actor.create_account(100 + n, 1000);
                        format!("{:?}", actor.reconcile_market_reservations())
                    }
                    _ => format!(
                        "{:?}",
                        actor.apply_to_instrument(
                            "V-USD-PERP",
                            limit(n + 100, 30, Side::Buy, 9_900, 1)
                        )
                    ),
                }
            };
            let a = action(&mut dirty);
            INDEXED_MARGIN_REFERENCE.set(true);
            crate::shared_map::EAGER_CLONE.set(true);
            FULL_RESERVATION_REFERENCE.set(true);
            SNAPSHOT_VENUE_REFERENCE.set(true);
            let b = action(&mut full);
            INDEXED_MARGIN_REFERENCE.set(false);
            crate::shared_map::EAGER_CLONE.set(false);
            FULL_RESERVATION_REFERENCE.set(false);
            SNAPSHOT_VENUE_REFERENCE.set(false);
            assert_eq!(a, b, "action {n}");
            assert_eq!(canonical_actor(&dirty), canonical_actor(&full), "state {n}");
            funding_count += dirty
                .take_clock_executions()
                .iter()
                .filter(|e| e.funding_settlement.is_some())
                .count();
            full.take_clock_executions();
        }
        assert!(
            funding_count > 0,
            "must exercise actual funding settlements"
        );
    }

    #[test]
    fn dirty_reservations_preserve_pending_changes_and_failed_reconciliation() {
        let mut actor = ExchangeActor::new(
            "dirty",
            ExchangeConfig::new_single(venue_perp_config("btc", "BTC", "USDT")).unwrap(),
        )
        .unwrap();
        actor.create_account(1, 1000);
        actor.create_account(2, 1000);
        actor.reconcile_market_reservations().unwrap();
        actor.venue_accounts.apply_delta(1, "USDT", 1).unwrap();
        actor.venue_accounts.apply_delta(2, "USDT", 1).unwrap();
        actor
            .reconcile_market_reservations_for(Some(&BTreeSet::from([1])))
            .unwrap();
        assert_eq!(
            actor.venue_accounts.reservation_changes.accounts(),
            Some(&BTreeSet::from([2]))
        );
        actor.reconcile_market_reservations().unwrap();
        assert!(
            actor
                .venue_accounts
                .reservation_changes
                .accounts()
                .unwrap()
                .is_empty()
        );
        // Replacing a market with an independently reconciled clone must still
        // invalidate the exchange baseline, even if that clone has no dirty ids.
        let replacement = actor.primary_market().clone();
        *actor.primary_market_mut() = replacement;
        assert!(
            actor
                .venue_accounts
                .reservation_changes
                .accounts()
                .is_none()
        );
        actor.reconcile_market_reservations().unwrap();
        actor
            .primary_market_mut()
            .apply(limit(1, 1, Side::Buy, 100, 50));
        actor.venue_accounts.set_balance(1, "USDT", 0);
        let before = canonical_actor(&actor);
        assert!(actor.reconcile_market_reservations().is_err());
        assert!(
            actor
                .venue_accounts
                .reservation_changes
                .accounts()
                .is_none()
        );
        assert_eq!(canonical_actor(&actor), before);
    }

    #[test]
    fn actor_runs_spot_market_and_returns_room_scoped_execution() {
        let mut actor = MarketActor::new("room-1", spot_config()).unwrap();
        actor
            .create_spot_account_with_position(10, 1_000, 5)
            .unwrap();
        actor.create_account(20, 1_000);

        actor.apply(limit(1, 10, Side::Sell, 100, 5));
        let execution = actor.apply(limit(2, 20, Side::Buy, 100, 2));

        assert_eq!(execution.room_id, "room-1");
        assert_eq!(execution.command_seq, 1);
        let ActorExecutionResult::Accepted(MarketExecution::Spot(result)) = execution.result else {
            panic!("expected accepted spot execution");
        };
        assert_eq!(result.clearing_events.len(), 1);
        assert!(
            result
                .events
                .iter()
                .any(|record| matches!(record.event, Event::TradePrinted(_)))
        );
    }

    #[test]
    fn paused_actor_rejects_new_orders_without_changing_book() {
        let mut actor = MarketActor::new("room-1", spot_config()).unwrap();
        actor.create_account(20, 1_000);
        actor.pause();

        let execution = actor.apply(limit(1, 20, Side::Buy, 100, 1));

        assert_eq!(actor.status(), MarketStatus::Paused);
        assert_eq!(
            execution.result,
            ActorExecutionResult::Rejected(ActorRejectReason::MarketPaused)
        );
        assert!(actor.book_snapshot().bids.is_empty());
    }

    #[test]
    fn paused_actor_allows_cancel_orders() {
        let mut actor = MarketActor::new("room-1", spot_config()).unwrap();
        actor.create_account(20, 1_000);
        actor.apply(limit(1, 20, Side::Buy, 100, 1));
        actor.pause();

        let execution = actor.apply(Command::CancelOrder(CancelOrder { order_id: 1 }));

        let ActorExecutionResult::Accepted(MarketExecution::Spot(result)) = execution.result else {
            panic!("expected accepted cancel");
        };
        assert!(
            result
                .events
                .iter()
                .any(|record| matches!(record.event, Event::OrderCanceled { order_id: 1, .. }))
        );
        assert!(actor.book_snapshot().bids.is_empty());
    }

    #[test]
    fn closed_actor_rejects_all_commands() {
        let mut actor = MarketActor::new("room-1", spot_config()).unwrap();
        actor.close();

        let execution = actor.apply(limit(1, 20, Side::Buy, 100, 1));

        assert_eq!(
            execution.result,
            ActorExecutionResult::Rejected(ActorRejectReason::MarketClosed)
        );
    }

    #[test]
    fn actor_exposes_account_snapshots() {
        let mut actor = MarketActor::new("room-1", spot_config()).unwrap();
        actor.create_account(20, 1_000);

        assert!(matches!(
            actor.account_snapshot(20),
            Some(AccountSnapshot::Spot(SpotAccountSnapshot {
                account_id: 20,
                cash_balance: 1_000,
                ..
            }))
        ));
        assert!(matches!(
            actor.account_snapshots(),
            AccountSnapshots::Spot(accounts) if accounts.len() == 1
        ));
    }

    #[test]
    fn exchange_actor_routes_commands_to_explicit_instrument() {
        let spot = MarketConfig::Spot(SpotMarketConfig {
            instrument: InstrumentConfig::new_for_venue(
                "binance",
                "binance:btc-usdt:spot",
                "BTC",
                "USDT",
                "BTC-USDT Spot",
                1,
                1,
            )
            .unwrap(),
            clearing: SpotClearingConfig::default(),
            risk: SpotRiskConfig::default(),
        });
        let perp = MarketConfig::Perp(PerpMarketConfig {
            instrument: InstrumentConfig::new_for_venue(
                "binance",
                "binance:btc-usdt:perp",
                "BTC",
                "USDT",
                "BTC-USDT Perp",
                1,
                1,
            )
            .unwrap(),
            clearing: PerpClearingConfig {
                leverage: 10,
                ..PerpClearingConfig::default()
            },
            risk: PerpRiskConfig::default(),
            initial_mark_price_tick: 100,
            price_link: None,
            funding: None,
        });
        let config = ExchangeConfig::new("binance", vec![spot, perp]).unwrap();
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange.create_account(20, 1_000);

        let perp_execution = exchange
            .apply_to_instrument("binance:btc-usdt:perp", limit(1, 20, Side::Sell, 100, 2))
            .unwrap();
        let spot_execution = exchange
            .apply_to_instrument("binance:btc-usdt:spot", limit(2, 20, Side::Buy, 90, 1))
            .unwrap();

        assert_eq!(perp_execution.command_seq, 0);
        assert_eq!(perp_execution.instrument_id, "binance:btc-usdt:perp");
        assert_eq!(spot_execution.command_seq, 1);
        assert_eq!(spot_execution.instrument_id, "binance:btc-usdt:spot");

        assert_eq!(
            exchange
                .book_snapshot_for("binance:btc-usdt:spot")
                .unwrap()
                .bids,
            vec![BookLevel {
                price_tick: 90,
                qty: 1,
            }]
        );
        assert_eq!(
            exchange
                .book_snapshot_for("binance:btc-usdt:perp")
                .unwrap()
                .asks,
            vec![BookLevel {
                price_tick: 100,
                qty: 2,
            }]
        );
    }

    #[test]
    fn exchange_actor_liquidates_perp_account_and_syncs_venue_balance() {
        let perp = MarketConfig::Perp(PerpMarketConfig {
            instrument: InstrumentConfig::new_for_venue(
                "binance",
                "binance:btc-usdt:perp",
                "BTC",
                "USDT",
                "BTC-USDT Perp",
                1,
                1,
            )
            .unwrap(),
            clearing: PerpClearingConfig {
                leverage: 10,
                maintenance_margin_ppm: 50_000,
                ..PerpClearingConfig::default()
            },
            risk: PerpRiskConfig::default(),
            initial_mark_price_tick: 80,
            price_link: None,
            funding: None,
        });
        let config = ExchangeConfig::new("binance", vec![perp]).unwrap();
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange.create_account(10, 10_000);
        exchange.create_account(20, 200);
        exchange.create_account(30, 10_000);

        exchange
            .apply_to_instrument("binance:btc-usdt:perp", limit(1, 10, Side::Sell, 100, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:btc-usdt:perp", market(2, 20, Side::Buy, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:btc-usdt:perp", limit(3, 30, Side::Buy, 80, 10))
            .unwrap();

        let execution = exchange
            .liquidate_account("binance:btc-usdt:perp", 20, 4)
            .unwrap();

        let ActorExecutionResult::Accepted(MarketExecution::Perp(result)) = execution.result else {
            panic!("expected accepted perp liquidation: {execution:?}");
        };
        assert_eq!(execution.command_seq, 3);
        assert_eq!(result.clearing_events.len(), 2);
        assert!(matches!(
            exchange
                .account_snapshot_for("binance:btc-usdt:perp", 20)
                .unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                position_qty: 0,
                cash_balance: 0,
                margin_status: PerpMarginStatus::Flat,
                ..
            }))
        ));
        assert_eq!(
            exchange
                .venue_account_snapshot(20)
                .balances
                .into_iter()
                .find(|balance| balance.asset_id == "USDT")
                .unwrap()
                .total,
            0
        );
    }

    #[test]
    fn exchange_actor_liquidation_cancels_resting_orders_and_releases_margin() {
        let perp = MarketConfig::Perp(PerpMarketConfig {
            instrument: InstrumentConfig::new_for_venue(
                "binance",
                "binance:btc-usdt:perp",
                "BTC",
                "USDT",
                "BTC-USDT Perp",
                1,
                1,
            )
            .unwrap(),
            clearing: PerpClearingConfig {
                leverage: 10,
                maintenance_margin_ppm: 50_000,
                ..PerpClearingConfig::default()
            },
            risk: PerpRiskConfig::default(),
            initial_mark_price_tick: 100,
            price_link: None,
            funding: None,
        });
        let config = ExchangeConfig::new("binance", vec![perp]).unwrap();
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange.create_account(10, 10_000);
        exchange.create_account(20, 200);
        exchange.create_account(30, 10_000);

        exchange
            .apply_to_instrument("binance:btc-usdt:perp", limit(1, 10, Side::Sell, 100, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:btc-usdt:perp", market(2, 20, Side::Buy, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:btc-usdt:perp", limit(3, 20, Side::Buy, 70, 1))
            .unwrap();
        exchange
            .apply_to_instrument("binance:btc-usdt:perp", limit(4, 30, Side::Buy, 80, 10))
            .unwrap();
        exchange
            .apply_to_instrument(
                "binance:btc-usdt:perp",
                Command::SetMarkPrice(SetMarkPrice { price_tick: 80 }),
            )
            .unwrap();

        assert!(matches!(
            exchange
                .account_snapshot_for("binance:btc-usdt:perp", 20)
                .unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                reserved_margin: 0,
                margin_status: PerpMarginStatus::Liquidatable,
                ..
            }))
        ));
        assert_eq!(
            exchange
                .venue_balance_snapshot(20, "USDT")
                .unwrap()
                .reserved,
            100
        );

        let execution = exchange
            .liquidate_account("binance:btc-usdt:perp", 20, 5)
            .unwrap();
        let ActorExecutionResult::Accepted(MarketExecution::Perp(result)) = execution.result else {
            panic!("expected accepted perp liquidation: {execution:?}");
        };
        assert!(result.events.iter().any(|record| {
            matches!(
                record.event,
                Event::OrderCanceled {
                    order_id: 3,
                    remaining_qty: 1,
                }
            )
        }));
        assert!(matches!(
            exchange
                .account_snapshot_for("binance:btc-usdt:perp", 20)
                .unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                position_qty: 0,
                reserved_margin: 0,
                margin_status: PerpMarginStatus::Flat,
                ..
            }))
        ));
        assert_eq!(
            exchange
                .venue_balance_snapshot(20, "USDT")
                .unwrap()
                .reserved,
            0
        );
        assert!(
            exchange
                .book_snapshot_for("binance:btc-usdt:perp")
                .unwrap()
                .bids
                .is_empty()
        );
        assert_eq!(
            exchange
                .order_owner_for("binance:btc-usdt:perp", 3)
                .unwrap(),
            None
        );
    }

    #[test]
    fn exchange_actor_syncs_liquidation_fee_and_bad_debt_to_venue_balance() {
        let perp = MarketConfig::Perp(PerpMarketConfig {
            instrument: InstrumentConfig::new_for_venue(
                "binance",
                "binance:btc-usdt:perp",
                "BTC",
                "USDT",
                "BTC-USDT Perp",
                1,
                1,
            )
            .unwrap(),
            clearing: PerpClearingConfig {
                leverage: 10,
                maintenance_margin_ppm: 50_000,
                liquidation_fee_ppm: 10_000,
                ..PerpClearingConfig::default()
            },
            risk: PerpRiskConfig::default(),
            initial_mark_price_tick: 80,
            price_link: None,
            funding: None,
        });
        let config = ExchangeConfig::new("binance", vec![perp]).unwrap();
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange.create_account(10, 10_000);
        exchange.create_account(20, 190);
        exchange.create_account(30, 10_000);

        exchange
            .apply_to_instrument("binance:btc-usdt:perp", limit(1, 10, Side::Sell, 100, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:btc-usdt:perp", market(2, 20, Side::Buy, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:btc-usdt:perp", limit(3, 30, Side::Buy, 80, 10))
            .unwrap();

        let execution = exchange
            .liquidate_account("binance:btc-usdt:perp", 20, 4)
            .unwrap();

        let ActorExecutionResult::Accepted(MarketExecution::Perp(result)) = execution.result else {
            panic!("expected accepted perp liquidation: {execution:?}");
        };
        assert!(matches!(
            result.clearing_events[1],
            PerpClearingEvent::LiquidationSettled {
                account_id: 20,
                liquidation_fee: 8,
                insurance_fund_payment: 8,
                bad_debt: 10,
                ..
            }
        ));
        assert_eq!(
            exchange
                .venue_account_snapshot(20)
                .balances
                .into_iter()
                .find(|balance| balance.asset_id == "USDT")
                .unwrap()
                .total,
            0
        );
    }

    #[test]
    fn exchange_actor_syncs_socialized_loss_to_contributor_venue_balance() {
        let perp = MarketConfig::Perp(PerpMarketConfig {
            instrument: InstrumentConfig::new_for_venue(
                "binance",
                "binance:btc-usdt:perp",
                "BTC",
                "USDT",
                "BTC-USDT Perp",
                1,
                1,
            )
            .unwrap(),
            clearing: PerpClearingConfig {
                leverage: 10,
                maintenance_margin_ppm: 50_000,
                liquidation_fee_ppm: 10_000,
                socialized_loss_enabled: true,
                ..PerpClearingConfig::default()
            },
            risk: PerpRiskConfig::default(),
            initial_mark_price_tick: 80,
            price_link: None,
            funding: None,
        });
        let config = ExchangeConfig::new("binance", vec![perp]).unwrap();
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange.create_account(10, 10_000);
        exchange.create_account(20, 190);
        exchange.create_account(30, 10_000);

        exchange
            .apply_to_instrument("binance:btc-usdt:perp", limit(1, 10, Side::Sell, 100, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:btc-usdt:perp", market(2, 20, Side::Buy, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:btc-usdt:perp", limit(3, 30, Side::Buy, 80, 10))
            .unwrap();

        let execution = exchange
            .liquidate_account("binance:btc-usdt:perp", 20, 4)
            .unwrap();

        let ActorExecutionResult::Accepted(MarketExecution::Perp(result)) = execution.result else {
            panic!("expected accepted perp liquidation: {execution:?}");
        };
        assert!(matches!(
            &result.clearing_events[1],
            PerpClearingEvent::LiquidationSettled {
                socialized_loss: 10,
                bad_debt: 0,
                socialized_loss_allocations,
                ..
            } if socialized_loss_allocations.len() == 1
                && socialized_loss_allocations[0].account_id == 10
                && socialized_loss_allocations[0].loss == 10
        ));
        assert_eq!(
            exchange
                .venue_account_snapshot(10)
                .balances
                .into_iter()
                .find(|balance| balance.asset_id == "USDT")
                .unwrap()
                .total,
            9_990
        );
        assert_eq!(
            exchange
                .venue_account_snapshot(20)
                .balances
                .into_iter()
                .find(|balance| balance.asset_id == "USDT")
                .unwrap()
                .total,
            0
        );
    }

    #[test]
    fn exchange_actor_syncs_auto_deleveraging_to_contributor_venue_balance() {
        let perp = MarketConfig::Perp(PerpMarketConfig {
            instrument: InstrumentConfig::new_for_venue(
                "binance",
                "binance:btc-usdt:perp",
                "BTC",
                "USDT",
                "BTC-USDT Perp",
                1,
                1,
            )
            .unwrap(),
            clearing: PerpClearingConfig {
                leverage: 10,
                maintenance_margin_ppm: 50_000,
                liquidation_fee_ppm: 10_000,
                auto_deleveraging_enabled: true,
                socialized_loss_enabled: true,
                ..PerpClearingConfig::default()
            },
            risk: PerpRiskConfig::default(),
            initial_mark_price_tick: 80,
            price_link: None,
            funding: None,
        });
        let config = ExchangeConfig::new("binance", vec![perp]).unwrap();
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange.create_account(10, 10_000);
        exchange.create_account(20, 190);
        exchange.create_account(30, 10_000);

        exchange
            .apply_to_instrument("binance:btc-usdt:perp", limit(1, 10, Side::Sell, 100, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:btc-usdt:perp", market(2, 20, Side::Buy, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:btc-usdt:perp", limit(3, 30, Side::Buy, 80, 10))
            .unwrap();

        let execution = exchange
            .liquidate_account("binance:btc-usdt:perp", 20, 4)
            .unwrap();

        let ActorExecutionResult::Accepted(MarketExecution::Perp(result)) = execution.result else {
            panic!("expected accepted perp liquidation");
        };
        assert!(matches!(
            &result.clearing_events[1],
            PerpClearingEvent::LiquidationSettled {
                auto_deleveraging_loss: 10,
                socialized_loss: 0,
                auto_deleveraging_allocations,
                ..
            } if auto_deleveraging_allocations.len() == 2
                && auto_deleveraging_allocations[0].account_id == 10
                && auto_deleveraging_allocations[0].position_delta == 1
                && auto_deleveraging_allocations[0].realized_pnl == 20
                && auto_deleveraging_allocations[0].loss == 10
                && auto_deleveraging_allocations[1].account_id == 30
                && auto_deleveraging_allocations[1].position_delta == -1
                && auto_deleveraging_allocations[1].realized_pnl == 0
                && auto_deleveraging_allocations[1].loss == 0
        ));
        assert_eq!(
            exchange
                .venue_account_snapshot(10)
                .balances
                .into_iter()
                .find(|balance| balance.asset_id == "USDT")
                .unwrap()
                .total,
            10_010
        );
    }

    #[test]
    fn exchange_actor_tracks_venue_asset_balances_across_spot_fills() {
        let config = ExchangeConfig::new("binance", vec![btc_usdt_spot_config()]).unwrap();
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange
            .create_spot_account_with_position(10, 1_000, 10)
            .unwrap();
        exchange.create_account(20, 1_000);

        exchange.apply(limit(1, 10, Side::Sell, 100, 5));
        let execution = exchange.apply(limit(2, 20, Side::Buy, 100, 2));

        assert!(matches!(
            execution.result,
            ActorExecutionResult::Accepted(_)
        ));
        assert_eq!(exchange.venue_balance_snapshot(10, "BTC").unwrap().total, 8);
        assert_eq!(
            exchange.venue_balance_snapshot(10, "USDT").unwrap().total,
            1_200
        );
        assert_eq!(exchange.venue_balance_snapshot(20, "BTC").unwrap().total, 2);
        assert_eq!(
            exchange.venue_balance_snapshot(20, "USDT").unwrap().total,
            800
        );
    }

    #[test]
    fn exchange_actor_aggregates_same_asset_reservations_across_markets() {
        let config = ExchangeConfig::new(
            "binance",
            vec![
                venue_spot_config("binance:btc-usdt:spot", "BTC", "USDT"),
                venue_spot_config("binance:eth-usdt:spot", "ETH", "USDT"),
            ],
        )
        .unwrap();
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange.create_account(20, 1_000);

        exchange
            .apply_to_instrument("binance:btc-usdt:spot", limit(1, 20, Side::Buy, 100, 6))
            .unwrap();
        assert_eq!(
            exchange
                .venue_balance_snapshot(20, "USDT")
                .unwrap()
                .reserved,
            600
        );

        let rejected = exchange
            .apply_to_instrument("binance:eth-usdt:spot", limit(2, 20, Side::Buy, 100, 5))
            .unwrap();
        let ActorExecutionResult::Accepted(MarketExecution::Spot(rejected)) = rejected.result
        else {
            panic!("expected recorded risk rejection");
        };
        assert!(matches!(
            rejected.events[0].event,
            Event::RiskRejected {
                reason: crate::model::RiskRejectReason::InsufficientCash,
                ..
            }
        ));
        assert!(
            exchange
                .book_snapshot_for("binance:eth-usdt:spot")
                .unwrap()
                .bids
                .is_empty()
        );
        assert_eq!(
            exchange
                .venue_balance_snapshot(20, "USDT")
                .unwrap()
                .reserved,
            600
        );

        exchange
            .apply_to_instrument("binance:eth-usdt:spot", limit(3, 20, Side::Buy, 100, 4))
            .unwrap();
        let balance = exchange.venue_balance_snapshot(20, "USDT").unwrap();
        assert_eq!(balance.total, 1_000);
        assert_eq!(balance.reserved, 1_000);
        assert_eq!(balance.available, 0);
    }

    #[test]
    fn perp_position_collateral_blocks_withdrawal_and_releases_after_close() {
        let config = ExchangeConfig::new(
            "binance",
            vec![venue_perp_config("binance:btc-usdt:perp", "BTC", "USDT")],
        )
        .unwrap();
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange.create_account(10, 10_000);
        exchange.create_account(20, 200);
        exchange.create_account(30, 10_000);

        exchange
            .apply_to_instrument("binance:btc-usdt:perp", limit(1, 10, Side::Sell, 100, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:btc-usdt:perp", market(2, 20, Side::Buy, 10))
            .unwrap();

        assert!(matches!(
            exchange
                .account_snapshot_for("binance:btc-usdt:perp", 20)
                .unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                position_qty: 10,
                initial_margin: 100,
                reserved_margin: 0,
                ..
            }))
        ));
        let open_balance = exchange.venue_balance_snapshot(20, "USDT").unwrap();
        assert_eq!(open_balance.total, 200);
        assert_eq!(open_balance.reserved, 100);
        assert_eq!(open_balance.available, 100);

        let blocked = exchange.submit_withdrawal(20, "USDT", 200).unwrap();
        assert_eq!(blocked.status, VenueTransferStatus::Rejected);
        assert_eq!(
            blocked.reject_reason,
            Some(VenueTransferRejectReason::InsufficientAvailableBalance)
        );
        let allowed = exchange.submit_withdrawal(20, "USDT", 100).unwrap();
        assert_eq!(allowed.status, VenueTransferStatus::Completed);
        let after_withdrawal = exchange.venue_balance_snapshot(20, "USDT").unwrap();
        assert_eq!(after_withdrawal.total, 100);
        assert_eq!(after_withdrawal.reserved, 100);
        assert_eq!(after_withdrawal.available, 0);

        exchange
            .apply_to_instrument("binance:btc-usdt:perp", limit(3, 30, Side::Buy, 100, 10))
            .unwrap();
        let close = exchange
            .apply_to_instrument(
                "binance:btc-usdt:perp",
                reduce_only_market(4, 20, Side::Sell, 10),
            )
            .unwrap();
        assert!(matches!(
            close.result,
            ActorExecutionResult::Accepted(MarketExecution::Perp(_))
        ));
        assert!(matches!(
            exchange
                .account_snapshot_for("binance:btc-usdt:perp", 20)
                .unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                position_qty: 0,
                initial_margin: 0,
                reserved_margin: 0,
                ..
            }))
        ));
        let closed_balance = exchange.venue_balance_snapshot(20, "USDT").unwrap();
        assert_eq!(closed_balance.total, 100);
        assert_eq!(closed_balance.reserved, 0);
        assert_eq!(closed_balance.available, 100);
    }

    #[test]
    fn perp_unrealized_loss_reduces_withdrawable_collateral() {
        let config = ExchangeConfig::new(
            "binance",
            vec![venue_perp_config("binance:btc-usdt:perp", "BTC", "USDT")],
        )
        .unwrap();
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange.create_account(10, 10_000);
        exchange.create_account(20, 200);

        exchange
            .apply_to_instrument("binance:btc-usdt:perp", limit(1, 10, Side::Sell, 100, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:btc-usdt:perp", market(2, 20, Side::Buy, 10))
            .unwrap();
        let mark = exchange
            .apply_to_instrument(
                "binance:btc-usdt:perp",
                Command::SetMarkPrice(SetMarkPrice { price_tick: 95 }),
            )
            .unwrap();
        assert!(matches!(
            mark.result,
            ActorExecutionResult::Accepted(MarketExecution::Perp(_))
        ));
        assert!(matches!(
            exchange
                .account_snapshot_for("binance:btc-usdt:perp", 20)
                .unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                unrealized_pnl: -50,
                initial_margin: 100,
                ..
            }))
        ));

        let blocked = exchange.submit_withdrawal(20, "USDT", 51).unwrap();
        assert_eq!(blocked.status, VenueTransferStatus::Rejected);
        assert_eq!(
            blocked.reject_reason,
            Some(VenueTransferRejectReason::InsufficientAvailableBalance)
        );
        let allowed = exchange.submit_withdrawal(20, "USDT", 50).unwrap();
        assert_eq!(allowed.status, VenueTransferStatus::Completed);
        let balance = exchange.venue_balance_snapshot(20, "USDT").unwrap();
        assert_eq!(balance.total, 150);
        assert_eq!(balance.reserved, 100);
        assert_eq!(balance.available, 50);
    }

    #[test]
    fn exchange_actor_aggregates_position_and_order_collateral_across_perp_markets() {
        let config = ExchangeConfig::new(
            "binance",
            vec![
                venue_perp_config("binance:btc-usdt:perp", "BTC", "USDT"),
                venue_perp_config("binance:eth-usdt:perp", "ETH", "USDT"),
            ],
        )
        .unwrap();
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange.create_account(10, 10_000);
        exchange.create_account(11, 10_000);
        exchange.create_account(20, 300);

        exchange
            .apply_to_instrument("binance:btc-usdt:perp", limit(1, 10, Side::Sell, 100, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:btc-usdt:perp", market(2, 20, Side::Buy, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:eth-usdt:perp", limit(3, 11, Side::Sell, 100, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:eth-usdt:perp", market(4, 20, Side::Buy, 10))
            .unwrap();

        let two_positions = exchange.venue_balance_snapshot(20, "USDT").unwrap();
        assert_eq!(two_positions.total, 300);
        assert_eq!(two_positions.reserved, 200);
        assert_eq!(two_positions.available, 100);

        exchange
            .apply_to_instrument("binance:btc-usdt:perp", limit(5, 20, Side::Buy, 100, 1))
            .unwrap();
        assert!(matches!(
            exchange
                .account_snapshot_for("binance:btc-usdt:perp", 20)
                .unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                initial_margin: 100,
                reserved_margin: 10,
                ..
            }))
        ));
        let combined = exchange.venue_balance_snapshot(20, "USDT").unwrap();
        assert_eq!(combined.total, 300);
        assert_eq!(combined.reserved, 210);
        assert_eq!(combined.available, 90);
    }

    #[test]
    fn perp_mark_move_keeps_venue_collateral_at_or_above_risk_requirement() {
        let config = ExchangeConfig::new(
            "binance",
            vec![venue_perp_config("binance:btc-usdt:perp", "BTC", "USDT")],
        )
        .unwrap();
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange.create_account(10, 10_000);
        exchange.create_account(20, 300);

        exchange
            .apply_to_instrument("binance:btc-usdt:perp", limit(1, 10, Side::Sell, 100, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:btc-usdt:perp", market(2, 20, Side::Buy, 10))
            .unwrap();
        exchange
            .apply_to_instrument(
                "binance:btc-usdt:perp",
                Command::SetMarkPrice(SetMarkPrice { price_tick: 110 }),
            )
            .unwrap();

        assert!(matches!(
            exchange
                .account_snapshot_for("binance:btc-usdt:perp", 20)
                .unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                initial_margin: 100,
                reserved_margin: 10,
                equity: 400,
                ..
            }))
        ));
        let marked_up = exchange.venue_balance_snapshot(20, "USDT").unwrap();
        assert_eq!(marked_up.reserved, 110);
        assert_eq!(marked_up.available, 190);

        exchange
            .apply_to_instrument(
                "binance:btc-usdt:perp",
                Command::SetMarkPrice(SetMarkPrice { price_tick: 90 }),
            )
            .unwrap();
        assert!(matches!(
            exchange
                .account_snapshot_for("binance:btc-usdt:perp", 20)
                .unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                initial_margin: 100,
                reserved_margin: 0,
                equity: 200,
                ..
            }))
        ));
        let marked_down = exchange.venue_balance_snapshot(20, "USDT").unwrap();
        assert_eq!(marked_down.reserved, 100);
        assert_eq!(marked_down.available, 200);
    }

    #[test]
    fn cross_margin_status_uses_portfolio_equity_and_aggregate_thresholds() {
        let config = ExchangeConfig::new(
            "binance",
            vec![
                venue_perp_config("binance:btc-usdt:perp", "BTC", "USDT"),
                venue_perp_config("binance:eth-usdt:perp", "ETH", "USDT"),
            ],
        )
        .unwrap();
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange.create_account(10, 10_000);
        exchange.create_account(11, 10_000);
        exchange.create_account(20, 300);

        exchange
            .apply_to_instrument("binance:btc-usdt:perp", limit(1, 10, Side::Sell, 100, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:btc-usdt:perp", market(2, 20, Side::Buy, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:eth-usdt:perp", limit(3, 11, Side::Sell, 100, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:eth-usdt:perp", market(4, 20, Side::Buy, 10))
            .unwrap();

        exchange
            .apply_to_instrument(
                "binance:btc-usdt:perp",
                Command::SetMarkPrice(SetMarkPrice { price_tick: 80 }),
            )
            .unwrap();
        for instrument_id in ["binance:btc-usdt:perp", "binance:eth-usdt:perp"] {
            assert!(matches!(
                exchange.account_snapshot_for(instrument_id, 20).unwrap(),
                Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                    equity: 100,
                    available_cash: -100,
                    portfolio_initial_margin: 200,
                    portfolio_maintenance_margin: 90,
                    margin_status: PerpMarginStatus::MarginCall,
                    ..
                }))
            ));
        }

        exchange
            .apply_to_instrument(
                "binance:btc-usdt:perp",
                Command::SetMarkPrice(SetMarkPrice { price_tick: 78 }),
            )
            .unwrap();
        for instrument_id in ["binance:btc-usdt:perp", "binance:eth-usdt:perp"] {
            assert!(matches!(
                exchange.account_snapshot_for(instrument_id, 20).unwrap(),
                Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                    equity: 80,
                    portfolio_initial_margin: 200,
                    portfolio_maintenance_margin: 89,
                    margin_status: PerpMarginStatus::Liquidatable,
                    ..
                }))
            ));
        }
    }

    #[test]
    fn cross_market_unrealized_loss_and_open_orders_reduce_perp_buying_power() {
        let config = ExchangeConfig::new(
            "binance",
            vec![
                venue_perp_config("binance:btc-usdt:perp", "BTC", "USDT"),
                venue_perp_config("binance:eth-usdt:perp", "ETH", "USDT"),
            ],
        )
        .unwrap();
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange.create_account(10, 10_000);
        exchange.create_account(11, 10_000);
        exchange.create_account(20, 300);
        exchange.create_account(30, 10_000);

        exchange
            .apply_to_instrument("binance:btc-usdt:perp", limit(1, 10, Side::Sell, 100, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:btc-usdt:perp", market(2, 20, Side::Buy, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:eth-usdt:perp", limit(3, 11, Side::Sell, 100, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:eth-usdt:perp", market(4, 20, Side::Buy, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:eth-usdt:perp", limit(5, 20, Side::Buy, 100, 5))
            .unwrap();
        assert_eq!(
            exchange
                .venue_balance_snapshot(20, "USDT")
                .unwrap()
                .reserved,
            250
        );

        exchange
            .apply_to_instrument(
                "binance:btc-usdt:perp",
                Command::SetMarkPrice(SetMarkPrice { price_tick: 90 }),
            )
            .unwrap();
        let rejected = exchange
            .apply_to_instrument("binance:eth-usdt:perp", limit(6, 20, Side::Buy, 100, 1))
            .unwrap();
        let ActorExecutionResult::Accepted(MarketExecution::Perp(rejected)) = rejected.result
        else {
            panic!("expected a recorded perp risk rejection");
        };
        assert!(matches!(
            rejected.events[0].event,
            Event::RiskRejected {
                order_id: 6,
                reason: crate::model::RiskRejectReason::InsufficientMargin,
            }
        ));

        let canceled = exchange
            .apply_to_instrument(
                "binance:eth-usdt:perp",
                Command::CancelOrder(CancelOrder { order_id: 5 }),
            )
            .unwrap();
        assert!(matches!(
            canceled.result,
            ActorExecutionResult::Accepted(MarketExecution::Perp(_))
        ));
        exchange
            .apply_to_instrument("binance:eth-usdt:perp", limit(7, 30, Side::Buy, 100, 1))
            .unwrap();
        let reduced = exchange
            .apply_to_instrument("binance:eth-usdt:perp", market(8, 20, Side::Sell, 1))
            .unwrap();
        assert!(matches!(
            reduced.result,
            ActorExecutionResult::Accepted(MarketExecution::Perp(_))
        ));
        assert!(matches!(
            exchange
                .account_snapshot_for("binance:eth-usdt:perp", 20)
                .unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                position_qty: 9,
                ..
            }))
        ));
    }

    #[test]
    fn spot_reservations_reduce_same_quote_perp_buying_power() {
        let config = ExchangeConfig::new(
            "binance",
            vec![
                venue_spot_config("binance:btc-usdt:spot", "BTC", "USDT"),
                venue_perp_config("binance:eth-usdt:perp", "ETH", "USDT"),
            ],
        )
        .unwrap();
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange.create_account(20, 200);

        exchange
            .apply_to_instrument("binance:btc-usdt:spot", limit(1, 20, Side::Buy, 100, 1))
            .unwrap();
        assert_eq!(
            exchange
                .venue_balance_snapshot(20, "USDT")
                .unwrap()
                .reserved,
            100
        );

        let rejected = exchange
            .apply_to_instrument("binance:eth-usdt:perp", limit(2, 20, Side::Buy, 100, 11))
            .unwrap();
        let ActorExecutionResult::Accepted(MarketExecution::Perp(rejected)) = rejected.result
        else {
            panic!("expected an auditable perp risk rejection");
        };
        assert!(rejected.events.iter().any(|event| matches!(
            event.event,
            Event::RiskRejected {
                order_id: 2,
                reason: crate::model::RiskRejectReason::InsufficientMargin,
            }
        )));
    }

    #[test]
    fn spot_reservation_immediately_refreshes_same_asset_perp_margin_status() {
        let config = ExchangeConfig::new(
            "binance",
            vec![
                venue_spot_config("binance:btc-usdt:spot", "BTC", "USDT"),
                venue_perp_config("binance:eth-usdt:perp", "ETH", "USDT"),
            ],
        )
        .unwrap();
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange.create_account(10, 10_000);
        exchange.create_account(20, 200);

        exchange
            .apply_to_instrument("binance:eth-usdt:perp", limit(1, 10, Side::Sell, 100, 10))
            .unwrap();
        exchange
            .apply_to_instrument("binance:eth-usdt:perp", market(2, 20, Side::Buy, 10))
            .unwrap();
        exchange
            .apply_to_instrument(
                "binance:eth-usdt:perp",
                Command::SetMarkPrice(SetMarkPrice { price_tick: 94 }),
            )
            .unwrap();
        assert!(matches!(
            exchange
                .account_snapshot_for("binance:eth-usdt:perp", 20)
                .unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                cash_balance: 200,
                equity: 140,
                margin_status: PerpMarginStatus::Healthy,
                ..
            }))
        ));

        exchange
            .apply_to_instrument("binance:btc-usdt:spot", limit(3, 20, Side::Buy, 100, 1))
            .unwrap();

        assert!(matches!(
            exchange
                .account_snapshot_for("binance:eth-usdt:perp", 20)
                .unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                cash_balance: 100,
                equity: 40,
                portfolio_maintenance_margin: 47,
                margin_status: PerpMarginStatus::Liquidatable,
                ..
            }))
        ));
    }

    #[test]
    fn completed_withdrawal_is_visible_to_market_risk_and_reserved_orders_block_withdrawal() {
        let config = ExchangeConfig::new(
            "binance",
            vec![venue_spot_config("binance:btc-usdt:spot", "BTC", "USDT")],
        )
        .unwrap();
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange.create_account(20, 1_000);

        let withdrawal = exchange.submit_withdrawal(20, "USDT", 1_000).unwrap();
        assert_eq!(withdrawal.status, VenueTransferStatus::Completed);
        assert_eq!(
            exchange.venue_balance_snapshot(20, "USDT").unwrap().total,
            0
        );

        let rejected = exchange
            .apply_to_instrument("binance:btc-usdt:spot", limit(1, 20, Side::Buy, 100, 1))
            .unwrap();
        let ActorExecutionResult::Accepted(MarketExecution::Spot(rejected)) = rejected.result
        else {
            panic!("expected recorded risk rejection");
        };
        assert!(matches!(
            rejected.events[0].event,
            Event::RiskRejected {
                reason: crate::model::RiskRejectReason::InsufficientCash,
                ..
            }
        ));

        exchange.apply_venue_asset_delta(20, "USDT", 1_000).unwrap();
        exchange
            .apply_to_instrument("binance:btc-usdt:spot", limit(2, 20, Side::Buy, 100, 10))
            .unwrap();
        let blocked = exchange.submit_withdrawal(20, "USDT", 1).unwrap();
        assert_eq!(blocked.status, VenueTransferStatus::Rejected);
        assert_eq!(
            blocked.reject_reason,
            Some(VenueTransferRejectReason::InsufficientAvailableBalance)
        );
    }

    #[test]
    fn exchange_actor_rejects_orders_outside_venue_price_limit() {
        let mut config = ExchangeConfig::new("binance", vec![btc_usdt_spot_config()]).unwrap();
        config.venue_rules = VenueRuleConfig {
            price_limits: vec![PriceLimitRuleConfig {
                instrument_id: "binance:btc-usdt:spot".to_string(),
                reference_price_tick: 100,
                limit_up_ppm: 100_000,
                limit_down_ppm: 100_000,
            }],
            ..VenueRuleConfig::default()
        };
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange.create_account(20, 1_000);

        let rejected = exchange.apply(limit(1, 20, Side::Buy, 111, 1));
        assert!(matches!(
            rejected.result,
            ActorExecutionResult::Rejected(ActorRejectReason::VenueRule(_))
        ));

        let accepted = exchange.apply(limit(2, 20, Side::Buy, 110, 1));
        assert!(matches!(
            accepted.result,
            ActorExecutionResult::Accepted(MarketExecution::Spot(_))
        ));
    }

    #[test]
    fn exchange_actor_rejects_unsettled_spot_resale_when_venue_uses_t_plus_one() {
        let mut config = ExchangeConfig::new("binance", vec![btc_usdt_spot_config()]).unwrap();
        config.venue_rules = VenueRuleConfig {
            settlement: SettlementRuleConfig {
                spot_sell_delay_steps: 10,
            },
            ..VenueRuleConfig::default()
        };
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange
            .create_spot_account_with_position(10, 1_000, 10)
            .unwrap();
        exchange.create_account(20, 1_000);

        exchange.apply(limit(1, 10, Side::Sell, 100, 5));
        let buy = exchange.apply(limit(2, 20, Side::Buy, 100, 5));
        assert!(matches!(
            buy.result,
            ActorExecutionResult::Accepted(MarketExecution::Spot(_))
        ));

        let resale = exchange.apply(limit(3, 20, Side::Sell, 100, 5));
        assert!(matches!(
            resale.result,
            ActorExecutionResult::Rejected(ActorRejectReason::VenueRule(_))
        ));
        assert!(exchange.book_snapshot().asks.is_empty());
    }

    #[test]
    fn t_plus_n_counts_existing_sell_reservations_against_settled_position() {
        let mut config = ExchangeConfig::new("binance", vec![btc_usdt_spot_config()]).unwrap();
        config.venue_rules = VenueRuleConfig {
            settlement: SettlementRuleConfig {
                spot_sell_delay_steps: 10,
            },
            ..VenueRuleConfig::default()
        };
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange
            .create_spot_account_with_position(10, 1_000, 10)
            .unwrap();
        exchange
            .create_spot_account_with_position(20, 1_000, 10)
            .unwrap();

        exchange.apply(limit(1, 10, Side::Sell, 100, 5));
        exchange.apply(limit(2, 20, Side::Buy, 100, 5));

        let settled_sale = exchange.apply(limit(3, 20, Side::Sell, 110, 10));
        assert!(matches!(
            settled_sale.result,
            ActorExecutionResult::Accepted(MarketExecution::Spot(_))
        ));
        let unsettled_sale = exchange.apply(limit(4, 20, Side::Sell, 110, 1));
        assert!(matches!(
            unsettled_sale.result,
            ActorExecutionResult::Rejected(ActorRejectReason::VenueRule(
                VenueRuleRejectReason::SpotPositionUnsettled { .. }
            ))
        ));
        assert_eq!(exchange.book_snapshot().asks[0].qty, 10);
    }

    #[test]
    fn rejected_or_cancel_commands_do_not_advance_spot_settlement_time() {
        let mut config = ExchangeConfig::new("binance", vec![btc_usdt_spot_config()]).unwrap();
        config.venue_rules = VenueRuleConfig {
            settlement: SettlementRuleConfig {
                spot_sell_delay_steps: 2,
            },
            ..VenueRuleConfig::default()
        };
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange
            .create_spot_account_with_position(10, 1_000, 10)
            .unwrap();
        exchange.create_account(20, 1_000);

        exchange.apply(limit(1, 10, Side::Sell, 100, 5));
        exchange.apply(limit(2, 20, Side::Buy, 100, 5));
        for order_id in 100..120 {
            exchange.apply(Command::CancelOrder(CancelOrder { order_id }));
        }

        let still_unsettled = exchange.apply(limit(3, 20, Side::Sell, 100, 5));
        assert!(matches!(
            still_unsettled.result,
            ActorExecutionResult::Rejected(ActorRejectReason::VenueRule(
                VenueRuleRejectReason::SpotPositionUnsettled { .. }
            ))
        ));

        exchange.advance_clock(2).unwrap();
        let settled = exchange.apply(limit(4, 20, Side::Sell, 100, 5));
        assert!(matches!(
            settled.result,
            ActorExecutionResult::Accepted(MarketExecution::Spot(_))
        ));
    }

    #[test]
    fn exchange_actor_enforces_trading_session_against_market_time() {
        let mut config = ExchangeConfig::new("binance", vec![btc_usdt_spot_config()]).unwrap();
        config.venue_rules = VenueRuleConfig {
            trading_session: TradingSessionRuleConfig {
                sessions: vec![TradingSessionWindow {
                    open_time_ms: 1_000,
                    close_time_ms: 2_000,
                }],
            },
            ..VenueRuleConfig::default()
        };
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange.create_account(20, 1_000);

        let closed = exchange.apply(limit(1, 20, Side::Buy, 100, 1));
        assert!(matches!(
            closed.result,
            ActorExecutionResult::Rejected(ActorRejectReason::VenueRule(_))
        ));

        let completed_transfers = exchange.advance_clock(1).unwrap();
        assert!(completed_transfers.is_empty());

        let open = exchange.apply(limit(2, 20, Side::Buy, 100, 1));
        assert!(matches!(
            open.result,
            ActorExecutionResult::Accepted(MarketExecution::Spot(_))
        ));
    }

    #[test]
    fn exchange_actor_applies_deposit_and_withdrawal_after_configured_delay() {
        let mut config = ExchangeConfig::new("binance", vec![btc_usdt_spot_config()]).unwrap();
        config.venue_rules = VenueRuleConfig {
            transfers: TransferPolicyConfig {
                deposit_delay_steps: 2,
                withdrawal_delay_steps: 1,
            },
            ..VenueRuleConfig::default()
        };
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange.create_account(20, 1_000);
        exchange.set_portfolio_balance(20, "USDT", 2_000);

        let deposit = exchange.submit_deposit(20, "USDT", 500).unwrap();
        assert_eq!(deposit.status, VenueTransferStatus::Pending);
        let pending_wallet = exchange.portfolio_snapshot(20);
        let usdt_wallet = pending_wallet
            .balances
            .iter()
            .find(|balance| balance.asset_id == "USDT")
            .unwrap();
        assert_eq!(usdt_wallet.total, 2_000);
        assert_eq!(usdt_wallet.reserved, 500);
        assert_eq!(
            exchange.venue_balance_snapshot(20, "USDT").unwrap().total,
            1_000
        );

        assert!(exchange.advance_clock(1).unwrap().is_empty());
        assert_eq!(
            exchange.venue_balance_snapshot(20, "USDT").unwrap().total,
            1_000
        );

        let completed = exchange.advance_clock(1).unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].status, VenueTransferStatus::Completed);
        let wallet_after_deposit = exchange.portfolio_snapshot(20);
        let usdt_wallet = wallet_after_deposit
            .balances
            .iter()
            .find(|balance| balance.asset_id == "USDT")
            .unwrap();
        assert_eq!(usdt_wallet.total, 1_500);
        assert_eq!(usdt_wallet.reserved, 0);
        assert_eq!(
            exchange.venue_balance_snapshot(20, "USDT").unwrap().total,
            1_500
        );

        let withdrawal = exchange.submit_withdrawal(20, "USDT", 300).unwrap();
        assert_eq!(withdrawal.status, VenueTransferStatus::Pending);
        let reserved = exchange.venue_balance_snapshot(20, "USDT").unwrap();
        assert_eq!(reserved.total, 1_500);
        assert_eq!(reserved.reserved, 300);

        let completed = exchange.advance_clock(1).unwrap();
        assert_eq!(completed.len(), 1);
        let debited = exchange.venue_balance_snapshot(20, "USDT").unwrap();
        assert_eq!(debited.total, 1_200);
        assert_eq!(debited.reserved, 0);
        let wallet_after_withdrawal = exchange.portfolio_snapshot(20);
        let usdt_wallet = wallet_after_withdrawal
            .balances
            .iter()
            .find(|balance| balance.asset_id == "USDT")
            .unwrap();
        assert_eq!(usdt_wallet.total, 1_800);
    }

    #[test]
    fn exchange_actor_rejects_deposit_when_portfolio_lacks_available_balance() {
        let config = ExchangeConfig::new("binance", vec![btc_usdt_spot_config()]).unwrap();
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange.create_account(20, 1_000);
        exchange.set_portfolio_balance(20, "USDT", 100);

        let transfer = exchange.submit_deposit(20, "USDT", 101).unwrap();

        assert_eq!(transfer.status, VenueTransferStatus::Rejected);
        assert_eq!(
            transfer.reject_reason,
            Some(VenueTransferRejectReason::InsufficientPortfolioBalance)
        );
        let wallet = exchange.portfolio_snapshot(20);
        let usdt_wallet = wallet
            .balances
            .iter()
            .find(|balance| balance.asset_id == "USDT")
            .unwrap();
        assert_eq!(usdt_wallet.total, 100);
        assert_eq!(usdt_wallet.reserved, 0);
        assert_eq!(
            exchange.venue_balance_snapshot(20, "USDT").unwrap().total,
            1_000
        );
    }

    #[test]
    fn exchange_actor_rejects_transfers_when_asset_policy_disallows_asset() {
        let mut config = ExchangeConfig::new("binance", vec![btc_usdt_spot_config()]).unwrap();
        config.asset_policy = crate::VenueAssetPolicyConfig {
            deposit_assets: ["USD"].into_iter().map(str::to_string).collect(),
            withdrawal_assets: ["USD"].into_iter().map(str::to_string).collect(),
            settlement_assets: ["USD"].into_iter().map(str::to_string).collect(),
            margin_assets: ["USD"].into_iter().map(str::to_string).collect(),
            ..crate::VenueAssetPolicyConfig::default()
        };
        let mut exchange = ExchangeActor::new("room-1", config).unwrap();
        exchange.create_account(20, 1_000);
        exchange.set_portfolio_balance(20, "USDT", 1_000);

        let deposit = exchange.submit_deposit(20, "USDT", 100).unwrap();
        assert_eq!(deposit.status, VenueTransferStatus::Rejected);
        assert_eq!(
            deposit.reject_reason,
            Some(VenueTransferRejectReason::AssetNotAcceptedByVenue)
        );

        let withdrawal = exchange.submit_withdrawal(20, "USDT", 100).unwrap();
        assert_eq!(withdrawal.status, VenueTransferStatus::Rejected);
        assert_eq!(
            withdrawal.reject_reason,
            Some(VenueTransferRejectReason::AssetNotWithdrawableFromVenue)
        );
    }
}
