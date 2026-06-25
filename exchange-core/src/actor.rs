use std::collections::BTreeMap;

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
    model::{AccountId, BookSnapshot, Command},
    perp::{PerpAccountSnapshot, PerpClearingEvent},
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum MarketStatus {
    Running,
    Paused,
    Closed,
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
    room_id: RoomId,
    config: ExchangeConfig,
    markets: BTreeMap<InstrumentId, MarketActor>,
    venue_accounts: VenueAccountStore,
    #[serde(default)]
    portfolios: PortfolioStore,
    venue_rules: VenueRuleEngine,
    clock: SimulationClock,
    transfers: VenueTransferStore,
    next_command_seq: ActorSeq,
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
            portfolios: PortfolioStore::new(),
            next_command_seq: 0,
            clock: SimulationClock::default(),
            transfers: VenueTransferStore::new(),
        })
    }

    pub fn from_market(actor: MarketActor) -> Result<Self, MarketConfigError> {
        let room_id = actor.room_id().to_string();
        let config = ExchangeConfig::new_single(actor.config().clone())?;
        let next_command_seq = actor.next_command_seq();
        let mut markets = BTreeMap::new();
        markets.insert(actor.config().instrument_id().to_string(), actor);

        Ok(Self {
            room_id,
            venue_rules: VenueRuleEngine::new(config.venue_rules.clone())
                .map_err(MarketConfigError::VenueRule)?,
            config,
            markets,
            venue_accounts: VenueAccountStore::new(),
            portfolios: PortfolioStore::new(),
            next_command_seq,
            clock: SimulationClock::default(),
            transfers: VenueTransferStore::new(),
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
        self.venue_accounts
            .apply_signed_delta(account_id, asset_id, amount)
            .map(|_| ())
            .map_err(reject_reason_from_venue_account_error)
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

    pub fn advance_clock(&mut self, steps: u64) -> Vec<VenueTransfer> {
        let mut completed = Vec::new();
        for _ in 0..steps {
            self.clock.advance_step();
            let due = self
                .transfers
                .process_due(&mut self.venue_accounts, self.clock.step());
            for transfer in &due {
                self.apply_portfolio_effect_for_finished_transfer(transfer);
            }
            completed.extend(due);
        }
        completed
    }

    pub fn advance_clock_venue_only(&mut self, steps: u64) -> Vec<VenueTransfer> {
        let mut completed = Vec::new();
        for _ in 0..steps {
            self.clock.advance_step();
            completed.extend(
                self.transfers
                    .process_due(&mut self.venue_accounts, self.clock.step()),
            );
        }
        completed
    }

    pub fn submit_deposit(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<String>,
        amount: Money,
    ) -> VenueTransfer {
        let asset_id = asset_id.into();
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
    ) -> VenueTransfer {
        let asset_id = asset_id.into();
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
    ) -> VenueTransfer {
        let asset_id = asset_id.into();
        if amount > 0 && !self.config.accepts_withdrawal_asset(&asset_id) {
            return self.transfers.submit_rejected_withdrawal(
                account_id,
                asset_id,
                amount,
                self.clock.step(),
                VenueTransferRejectReason::AssetNotWithdrawableFromVenue,
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
    ) -> VenueTransfer {
        let asset_id = asset_id.into();
        if amount > 0 && !self.config.accepts_withdrawal_asset(&asset_id) {
            return self.transfers.submit_rejected_withdrawal(
                account_id,
                asset_id,
                amount,
                self.clock.step(),
                VenueTransferRejectReason::AssetNotWithdrawableFromVenue,
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

    pub fn primary_market(&self) -> &MarketActor {
        self.markets
            .get(self.primary_instrument_id())
            .expect("validated exchange config must have a primary market")
    }

    pub fn primary_market_mut(&mut self) -> &mut MarketActor {
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
        if !self.markets.contains_key(instrument_id) {
            return Err(ActorRejectReason::InstrumentNotFound {
                instrument_id: instrument_id.to_string(),
            });
        }

        let command_seq = self.take_command_seq();
        if let Some(rejection) = self.check_venue_rules(command_seq, instrument_id, &command)? {
            return Ok(ActorExecution {
                room_id: self.room_id.clone(),
                instrument_id: instrument_id.to_string(),
                command_seq,
                status: self.status(),
                result: ActorExecutionResult::Rejected(ActorRejectReason::VenueRule(rejection)),
            });
        }

        let mut execution = self.market_mut(instrument_id)?.apply(command);
        execution.command_seq = command_seq;
        execution.instrument_id = instrument_id.to_string();
        self.sync_venue_accounts_from_execution(instrument_id, &execution);
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
        command_seq: ActorSeq,
        instrument_id: &str,
        command: &Command,
    ) -> Result<Option<VenueRuleRejectReason>, ActorRejectReason> {
        let market = self.market(instrument_id)?;
        let instrument = market.config().instrument();
        let market_kind = market.kind();

        Ok(self
            .venue_rules
            .check_order(VenueRuleOrderContext {
                command_seq,
                market_time_ms: self.clock.market_time_ms(),
                instrument_id,
                instrument,
                market_kind,
                command,
                venue_accounts: &self.venue_accounts,
            })
            .err())
    }

    fn sync_venue_accounts_from_execution(
        &mut self,
        instrument_id: &str,
        execution: &ActorExecution,
    ) {
        let Some(instrument) = self
            .markets
            .get(instrument_id)
            .map(|market| market.config().instrument().clone())
        else {
            return;
        };

        let ActorExecutionResult::Accepted(market_execution) = &execution.result else {
            return;
        };
        let command_seq = execution.command_seq;

        match market_execution {
            MarketExecution::Spot(spot_execution) => {
                for event in &spot_execution.clearing_events {
                    self.apply_spot_clearing_to_venue(
                        event,
                        &instrument.base_asset,
                        &instrument.quote_asset,
                    );
                }
                self.venue_rules.record_spot_clearing(
                    command_seq,
                    &instrument.instrument_id,
                    &spot_execution.clearing_events,
                );
            }
            MarketExecution::Perp(perp_execution) => {
                for event in &perp_execution.clearing_events {
                    self.apply_perp_clearing_to_venue(event, &instrument.quote_asset);
                }
            }
        }
    }

    fn apply_spot_clearing_to_venue(
        &mut self,
        event: &SpotClearingEvent,
        base_asset: &str,
        quote_asset: &str,
    ) {
        let SpotClearingEvent::TradeSettled {
            buyer_account_id,
            seller_account_id,
            qty,
            notional,
            buyer_fee,
            seller_fee,
            ..
        } = event;
        let _ = self.venue_accounts.apply_delta(
            *buyer_account_id,
            quote_asset.to_string(),
            -(*notional + *buyer_fee),
        );
        let _ = self.venue_accounts.apply_delta(
            *buyer_account_id,
            base_asset.to_string(),
            PositionQty::from(*qty),
        );
        let _ = self.venue_accounts.apply_delta(
            *seller_account_id,
            quote_asset.to_string(),
            *notional - *seller_fee,
        );
        let _ = self.venue_accounts.apply_delta(
            *seller_account_id,
            base_asset.to_string(),
            -PositionQty::from(*qty),
        );
    }

    fn apply_perp_clearing_to_venue(&mut self, event: &PerpClearingEvent, quote_asset: &str) {
        let PerpClearingEvent::TradeSettled {
            buyer_account_id,
            seller_account_id,
            buyer_fee,
            seller_fee,
            buyer_realized_pnl,
            seller_realized_pnl,
            ..
        } = event;
        let _ = self.venue_accounts.apply_signed_delta(
            *buyer_account_id,
            quote_asset.to_string(),
            *buyer_realized_pnl - *buyer_fee,
        );
        let _ = self.venue_accounts.apply_signed_delta(
            *seller_account_id,
            quote_asset.to_string(),
            *seller_realized_pnl - *seller_fee,
        );
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

    pub fn apply(&mut self, command: Command) -> ActorExecution {
        let seq = self.take_command_seq();

        if self.status == MarketStatus::Closed {
            return ActorExecution {
                room_id: self.room_id.clone(),
                instrument_id: self.config.instrument_id().to_string(),
                command_seq: seq,
                status: self.status,
                result: ActorExecutionResult::Rejected(ActorRejectReason::MarketClosed),
            };
        }

        if self.status == MarketStatus::Paused && matches!(command, Command::NewOrder(_)) {
            return ActorExecution {
                room_id: self.room_id.clone(),
                instrument_id: self.config.instrument_id().to_string(),
                command_seq: seq,
                status: self.status,
                result: ActorExecutionResult::Rejected(ActorRejectReason::MarketPaused),
            };
        }

        let result = match self.engine.apply(command) {
            Ok(result) => ActorExecutionResult::Accepted(result),
            Err(error) => ActorExecutionResult::Rejected(ActorRejectReason::Clearing(error)),
        };

        ActorExecution {
            room_id: self.room_id.clone(),
            instrument_id: self.config.instrument_id().to_string(),
            command_seq: seq,
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

    fn take_command_seq(&mut self) -> ActorSeq {
        let seq = self.next_command_seq;
        self.next_command_seq += 1;
        seq
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActorExecution {
    pub room_id: RoomId,
    pub instrument_id: InstrumentId,
    pub command_seq: ActorSeq,
    pub status: MarketStatus,
    pub result: ActorExecutionResult,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActorExecutionResult {
    Accepted(MarketExecution),
    Rejected(ActorRejectReason),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActorRejectReason {
    MarketPaused,
    MarketClosed,
    InstrumentNotFound { instrument_id: InstrumentId },
    WrongMarketKind,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        PerpRiskConfig, SpotRiskConfig,
        market::{ExchangeConfig, InstrumentConfig, PerpMarketConfig, SpotMarketConfig},
        model::{BookLevel, CancelOrder, Event, NewOrder, OrderKind, Side},
        perp::PerpClearingConfig,
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

    fn limit(order_id: u64, account_id: u64, side: Side, price_tick: i64, qty: u64) -> Command {
        Command::NewOrder(NewOrder {
            order_id,
            account_id,
            side,
            kind: OrderKind::Limit { price_tick },
            qty,
        })
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

        let completed_transfers = exchange.advance_clock(1);
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

        let deposit = exchange.submit_deposit(20, "USDT", 500);
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

        assert!(exchange.advance_clock(1).is_empty());
        assert_eq!(
            exchange.venue_balance_snapshot(20, "USDT").unwrap().total,
            1_000
        );

        let completed = exchange.advance_clock(1);
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

        let withdrawal = exchange.submit_withdrawal(20, "USDT", 300);
        assert_eq!(withdrawal.status, VenueTransferStatus::Pending);
        let reserved = exchange.venue_balance_snapshot(20, "USDT").unwrap();
        assert_eq!(reserved.total, 1_500);
        assert_eq!(reserved.reserved, 300);

        let completed = exchange.advance_clock(1);
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

        let transfer = exchange.submit_deposit(20, "USDT", 101);

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

        let deposit = exchange.submit_deposit(20, "USDT", 100);
        assert_eq!(deposit.status, VenueTransferStatus::Rejected);
        assert_eq!(
            deposit.reject_reason,
            Some(VenueTransferRejectReason::AssetNotAcceptedByVenue)
        );

        let withdrawal = exchange.submit_withdrawal(20, "USDT", 100);
        assert_eq!(withdrawal.status, VenueTransferStatus::Rejected);
        assert_eq!(
            withdrawal.reject_reason,
            Some(VenueTransferRejectReason::AssetNotWithdrawableFromVenue)
        );
    }
}
