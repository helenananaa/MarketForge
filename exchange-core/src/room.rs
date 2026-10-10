use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::{
    account::{Money, VenueAccountSnapshot},
    actor::{
        AccountSnapshot, AccountSnapshots, ActorExecution, ActorExecutionResult, ActorRejectReason,
        ExchangeActor, MarketActor, MarketExecution, MarketStatus, RoomId,
    },
    clock::SimulationClock,
    history::History,
    model::{AccountId, BookSnapshot, CancelOrder, Command, OrderId},
    portfolio::PortfolioAccountSnapshot,
    scenario::{ScenarioConfig, ScenarioError},
    simulation::{
        AssetLedgerEntry, RoomNetWorthSnapshot, SimulationBootstrap, SimulationRoom,
        SimulationRoomError, VenueAccountVenueSnapshot, VenueToVenueTransfer,
    },
    transfer::VenueTransfer,
};

const SYSTEM_LIQUIDATION_ORDER_ID_BASE: OrderId = 9_000_000_000_000_000_000;
const SYSTEM_LIQUIDATION_ORDER_ID_STRIDE: OrderId = 1_000;

#[derive(Clone, Debug, Default)]
pub struct RoomManager {
    rooms: BTreeMap<RoomId, SimulationRoom>,
    executions: BTreeMap<RoomId, History<ActorExecution>>,
    pending_liquidations: BTreeMap<RoomId, VecDeque<PendingRoomLiquidation>>,
    recovered_last_trades: BTreeMap<(RoomId, String), i64>,
    restored_bot_history: BTreeMap<
        RoomId,
        (
            std::sync::Arc<Vec<crate::observation::BotTradeReceipt>>,
            bool,
        ),
    >,
}

impl RoomManager {
    /// A borrow-scoped observation wave: public data is built once per market.
    /// Holding this immutable borrow prevents mutations from making the cache stale.
    pub fn observation_batch<'a>(&'a self, room_id: &'a str) -> RoomObservationBatch<'a> {
        RoomObservationBatch {
            rooms: self,
            room_id,
            public: BTreeMap::new(),
            history: BTreeMap::new(),
        }
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Move one complete room into a separate execution owner without replaying
    /// its history or changing liquidation, receipt, or last-trade state.
    pub fn take_room(&mut self, room_id: &str) -> Option<Self> {
        let room = self.rooms.remove(room_id)?;
        let mut result = Self::new();
        result.rooms.insert(room_id.to_owned(), room);
        if let Some(history) = self.executions.remove(room_id) {
            result.executions.insert(room_id.to_owned(), history);
        }
        if let Some(pending) = self.pending_liquidations.remove(room_id) {
            result
                .pending_liquidations
                .insert(room_id.to_owned(), pending);
        }
        if let Some(history) = self.restored_bot_history.remove(room_id) {
            result
                .restored_bot_history
                .insert(room_id.to_owned(), history);
        }
        let keys: Vec<_> = self
            .recovered_last_trades
            .keys()
            .filter(|(id, _)| id == room_id)
            .cloned()
            .collect();
        for key in keys {
            result.recovered_last_trades.insert(
                key.clone(),
                self.recovered_last_trades.remove(&key).unwrap(),
            );
        }
        Some(result)
    }

    /// Join disjoint execution owners for a coordinated administrative operation.
    /// Reject overlap before moving any state.
    pub fn join_disjoint(&mut self, mut other: Self) -> Result<(), RoomManagerError> {
        if let Some(room_id) = other.rooms.keys().find(|id| self.rooms.contains_key(*id)) {
            return Err(RoomManagerError::RoomAlreadyExists {
                room_id: room_id.clone(),
            });
        }
        self.rooms.append(&mut other.rooms);
        self.executions.append(&mut other.executions);
        self.pending_liquidations
            .append(&mut other.pending_liquidations);
        self.recovered_last_trades
            .append(&mut other.recovered_last_trades);
        self.restored_bot_history
            .append(&mut other.restored_bot_history);
        Ok(())
    }

    pub fn create_room(
        &mut self,
        scenario: ScenarioConfig,
    ) -> Result<RoomBootstrap, RoomManagerError> {
        if self.rooms.contains_key(&scenario.room_id) {
            return Err(RoomManagerError::RoomAlreadyExists {
                room_id: scenario.room_id,
            });
        }

        let SimulationBootstrap {
            room,
            seed_executions,
        } = SimulationRoom::from_scenario(scenario).map_err(RoomManagerError::Scenario)?;
        let room_id = room.room_id().to_string();
        self.rooms.insert(room_id.clone(), room);
        self.executions
            .insert(room_id.clone(), seed_executions.clone().into());
        self.pending_liquidations
            .insert(room_id.clone(), VecDeque::new());
        self.refresh_pending_liquidations(&room_id)?;

        Ok(RoomBootstrap {
            room_id,
            seed_executions,
        })
    }

    pub fn restore_room(
        &mut self,
        actor: MarketActor,
        executions: Vec<ActorExecution>,
    ) -> Result<(), RoomManagerError> {
        let room_id = actor.room_id().to_string();
        if self.rooms.contains_key(&room_id) {
            return Err(RoomManagerError::RoomAlreadyExists { room_id });
        }

        let exchange = ExchangeActor::from_market(actor).map_err(RoomManagerError::MarketConfig)?;
        let mut room = SimulationRoom::from_exchange(exchange);
        room.validate_market_events()
            .map_err(|e| RoomManagerError::Scenario(ScenarioError::InvalidMarketEvents(e)))?;
        room.normalize_after_restore()
            .map_err(RoomManagerError::Actor)?;
        self.rooms.insert(room_id.clone(), room);
        self.executions.insert(room_id.clone(), executions.into());
        self.pending_liquidations
            .insert(room_id.clone(), VecDeque::new());
        self.refresh_pending_liquidations(&room_id)?;
        Ok(())
    }

    pub fn restore_exchange_room(
        &mut self,
        exchange: ExchangeActor,
        executions: Vec<ActorExecution>,
    ) -> Result<(), RoomManagerError> {
        let room_id = exchange.room_id().to_string();
        if self.rooms.contains_key(&room_id) {
            return Err(RoomManagerError::RoomAlreadyExists { room_id });
        }

        let mut room = SimulationRoom::from_exchange(exchange);
        room.validate_market_events()
            .map_err(|e| RoomManagerError::Scenario(ScenarioError::InvalidMarketEvents(e)))?;
        room.normalize_after_restore()
            .map_err(RoomManagerError::Actor)?;
        self.rooms.insert(room_id.clone(), room);
        self.executions.insert(room_id.clone(), executions.into());
        self.pending_liquidations
            .insert(room_id.clone(), VecDeque::new());
        self.refresh_pending_liquidations(&room_id)?;
        Ok(())
    }

    pub fn restore_simulation_room(
        &mut self,
        mut room: SimulationRoom,
        executions: Vec<ActorExecution>,
    ) -> Result<(), RoomManagerError> {
        let room_id = room.room_id().to_string();
        if self.rooms.contains_key(&room_id) {
            return Err(RoomManagerError::RoomAlreadyExists { room_id });
        }

        room.validate_market_events()
            .map_err(|e| RoomManagerError::Scenario(ScenarioError::InvalidMarketEvents(e)))?;
        room.normalize_after_restore()
            .map_err(RoomManagerError::Actor)?;
        self.rooms.insert(room_id.clone(), room);
        self.executions.insert(room_id.clone(), executions.into());
        self.pending_liquidations
            .insert(room_id.clone(), VecDeque::new());
        self.refresh_pending_liquidations(&room_id)?;
        Ok(())
    }

    pub fn apply(
        &mut self,
        room_id: &str,
        command: Command,
    ) -> Result<ActorExecution, RoomManagerError> {
        let execution = self.simulation_room_mut(room_id)?.apply(command);
        self.record_execution_and_auto_liquidate(room_id, execution.clone(), true)?;
        Ok(execution)
    }

    pub fn apply_to_instrument(
        &mut self,
        room_id: &str,
        instrument_id: &str,
        command: Command,
    ) -> Result<ActorExecution, RoomManagerError> {
        self.apply_to_instrument_from(
            room_id,
            instrument_id,
            command,
            crate::actor::CommandOrigin::External,
        )
    }

    pub fn apply_to_instrument_from(
        &mut self,
        room_id: &str,
        instrument_id: &str,
        command: Command,
        origin: crate::actor::CommandOrigin,
    ) -> Result<ActorExecution, RoomManagerError> {
        let execution = self
            .simulation_room_mut(room_id)?
            .apply_to_instrument_from(instrument_id, command, origin)
            .map_err(RoomManagerError::Actor)?;
        self.record_execution_and_auto_liquidate(room_id, execution.clone(), true)?;
        Ok(execution)
    }

    pub fn liquidate_account(
        &mut self,
        room_id: &str,
        instrument_id: &str,
        account_id: AccountId,
        order_id: OrderId,
    ) -> Result<ActorExecution, RoomManagerError> {
        let trigger = PendingRoomLiquidation {
            instrument_id: instrument_id.to_string(),
            account_id,
        };
        if self.liquidation_trigger_is_active(room_id, &trigger)? {
            self.cancel_cross_margin_collateral_orders(room_id, instrument_id, account_id)?;
            if !self.liquidation_trigger_is_active(room_id, &trigger)? {
                return Err(RoomManagerError::Actor(ActorRejectReason::Clearing(
                    crate::ClearingError::AccountNotLiquidatable,
                )));
            }
        }
        let execution = self
            .simulation_room_mut(room_id)?
            .liquidate_account(instrument_id, account_id, order_id)
            .map_err(RoomManagerError::Actor)?;
        self.record_execution_and_auto_liquidate(room_id, execution.clone(), false)?;
        Ok(execution)
    }

    fn cancel_cross_margin_collateral_orders(
        &mut self,
        room_id: &str,
        instrument_id: &str,
        account_id: AccountId,
    ) -> Result<Vec<ActorExecution>, RoomManagerError> {
        let peer_orders = self
            .simulation_room(room_id)?
            .cross_margin_collateral_orders_for_liquidation(instrument_id, account_id)
            .map_err(RoomManagerError::Actor)?;
        let mut cancellations = Vec::with_capacity(peer_orders.len());
        for (peer_instrument_id, order_id) in peer_orders {
            let execution = self
                .simulation_room_mut(room_id)?
                .apply_to_instrument(
                    &peer_instrument_id,
                    Command::CancelOrder(CancelOrder { order_id }),
                )
                .map_err(RoomManagerError::Actor)?;
            let rejection = match &execution.result {
                ActorExecutionResult::Rejected(reason) => Some(reason.clone()),
                ActorExecutionResult::Accepted(_) => None,
            };
            self.executions
                .entry(room_id.to_string())
                .or_default()
                .push(execution.clone());
            cancellations.push(execution);
            if let Some(reason) = rejection {
                return Err(RoomManagerError::Actor(reason));
            }
        }
        Ok(cancellations)
    }

    fn record_execution_and_auto_liquidate(
        &mut self,
        room_id: &str,
        execution: ActorExecution,
        scan_liquidatable_accounts: bool,
    ) -> Result<(), RoomManagerError> {
        let accepted_market_execution = matches!(
            &execution.result,
            ActorExecutionResult::Accepted(MarketExecution::Spot(_))
                | ActorExecutionResult::Accepted(MarketExecution::Perp(_))
        );
        let instrument_id = execution.instrument_id.clone();
        self.executions
            .entry(room_id.to_string())
            .or_default()
            .push(execution);

        if !scan_liquidatable_accounts {
            let triggers =
                self.liquidation_candidates_for_instrument(room_id, &instrument_id, false)?;
            for trigger in triggers {
                self.enqueue_liquidation(room_id, trigger);
            }
        }
        if scan_liquidatable_accounts && accepted_market_execution {
            // A spot reservation or fill can change the quote collateral of a
            // perp on the same venue. The actor has already refreshed those
            // perp snapshots, so scan the whole room rather than only the
            // instrument that produced the user execution.
            self.advance_pending_liquidations(room_id, usize::MAX)?;
        }

        Ok(())
    }

    pub fn advance_pending_liquidations(
        &mut self,
        room_id: &str,
        max_attempts: usize,
    ) -> Result<Vec<ActorExecution>, RoomManagerError> {
        if self.simulation_room(room_id)?.status() == MarketStatus::Closed {
            self.pending_liquidations
                .entry(room_id.to_string())
                .or_default()
                .clear();
            return Ok(Vec::new());
        }
        self.refresh_pending_liquidations(room_id)?;
        let attempts = self
            .pending_liquidations
            .get(room_id)
            .map(VecDeque::len)
            .unwrap_or(0)
            .min(max_attempts);
        let mut executions = Vec::with_capacity(attempts);

        for _ in 0..attempts {
            let Some(trigger) = self
                .pending_liquidations
                .get_mut(room_id)
                .and_then(VecDeque::pop_front)
            else {
                break;
            };
            if !self.liquidation_trigger_is_active(room_id, &trigger)? {
                continue;
            }

            let cancellations = self.cancel_cross_margin_collateral_orders(
                room_id,
                &trigger.instrument_id,
                trigger.account_id,
            )?;
            executions.extend(cancellations);
            if !self.liquidation_trigger_is_active(room_id, &trigger)? {
                continue;
            }
            let order_id = self.next_system_liquidation_order_id(room_id)?;
            let liquidation = match self.simulation_room_mut(room_id)?.liquidate_account(
                &trigger.instrument_id,
                trigger.account_id,
                order_id,
            ) {
                Ok(execution) => execution,
                Err(error) => {
                    self.enqueue_liquidation(room_id, trigger);
                    return Err(RoomManagerError::Actor(error));
                }
            };
            self.executions
                .entry(room_id.to_string())
                .or_default()
                .push(liquidation.clone());
            executions.push(liquidation);

            if self.liquidation_trigger_is_active(room_id, &trigger)? {
                self.enqueue_liquidation(room_id, trigger);
            }
        }

        Ok(executions)
    }

    pub fn pending_liquidations(
        &self,
        room_id: &str,
    ) -> Result<Vec<PendingRoomLiquidation>, RoomManagerError> {
        self.room(room_id)?;
        Ok(self
            .pending_liquidations
            .get(room_id)
            .map(|queue| queue.iter().cloned().collect())
            .unwrap_or_default())
    }

    pub fn pending_liquidation_count(&self, room_id: &str) -> Result<usize, RoomManagerError> {
        self.room(room_id)?;
        Ok(self
            .pending_liquidations
            .get(room_id)
            .map(VecDeque::len)
            .unwrap_or(0))
    }

    fn refresh_pending_liquidations(&mut self, room_id: &str) -> Result<(), RoomManagerError> {
        let _timer = crate::performance::Timer::start(3);
        let instrument_ids = {
            let room = self.simulation_room(room_id)?;
            let mut instrument_ids = Vec::new();
            for venue_id in room.venue_ids() {
                let exchange = room
                    .exchange(venue_id)
                    .map_err(RoomManagerError::Simulation)?;
                instrument_ids.extend(exchange.instrument_ids().into_iter().map(str::to_string));
            }
            instrument_ids
        };

        for instrument_id in instrument_ids {
            for trigger in
                self.liquidation_candidates_for_instrument(room_id, &instrument_id, true)?
            {
                self.enqueue_liquidation(room_id, trigger);
            }
        }
        Ok(())
    }

    fn liquidation_candidates_for_instrument(
        &self,
        room_id: &str,
        instrument_id: &str,
        include_liquidatable_accounts: bool,
    ) -> Result<Vec<PendingRoomLiquidation>, RoomManagerError> {
        let room = self.simulation_room(room_id)?;
        let mut account_ids = room
            .pending_liquidation_accounts_for(instrument_id)
            .map_err(RoomManagerError::Actor)?
            .into_iter()
            .collect::<BTreeSet<_>>();
        if include_liquidatable_accounts {
            account_ids.extend(
                room.liquidatable_account_ids_for(instrument_id)
                    .map_err(RoomManagerError::Actor)?,
            );
        }

        Ok(account_ids
            .into_iter()
            .map(|account_id| PendingRoomLiquidation {
                instrument_id: instrument_id.to_string(),
                account_id,
            })
            .collect())
    }

    fn liquidation_trigger_is_active(
        &self,
        room_id: &str,
        trigger: &PendingRoomLiquidation,
    ) -> Result<bool, RoomManagerError> {
        Ok(self
            .liquidation_candidates_for_instrument(room_id, &trigger.instrument_id, true)?
            .contains(trigger))
    }

    fn enqueue_liquidation(&mut self, room_id: &str, trigger: PendingRoomLiquidation) {
        let queue = self
            .pending_liquidations
            .entry(room_id.to_string())
            .or_default();
        if !queue.contains(&trigger) {
            queue.push_back(trigger);
        }
    }

    fn next_system_liquidation_order_id(&self, room_id: &str) -> Result<OrderId, RoomManagerError> {
        // The room cursor is persisted with checkpoints, while execution
        // history may be stored and restored separately (or compacted away).
        // Deriving the system order id from history can therefore rewind and
        // collide with an already-journaled liquidation order after restore.
        let next_seq = self.simulation_room(room_id)?.next_command_seq();
        system_liquidation_order_id(next_seq, 0)
    }

    pub fn pause_room(&mut self, room_id: &str) -> Result<(), RoomManagerError> {
        self.simulation_room_mut(room_id).map(|room| room.pause())
    }

    pub fn resume_room(&mut self, room_id: &str) -> Result<(), RoomManagerError> {
        self.simulation_room_mut(room_id).map(|room| room.resume())
    }

    pub fn close_room(&mut self, room_id: &str) -> Result<(), RoomManagerError> {
        self.simulation_room_mut(room_id).map(|room| room.close())
    }

    pub fn restore_room_status(
        &mut self,
        room_id: &str,
        status: MarketStatus,
    ) -> Result<(), RoomManagerError> {
        self.simulation_room_mut(room_id)
            .map(|room| room.restore_status(status))
    }

    pub fn simulation_room(&self, room_id: &str) -> Result<&SimulationRoom, RoomManagerError> {
        self.rooms
            .get(room_id)
            .ok_or_else(|| RoomManagerError::RoomNotFound {
                room_id: room_id.to_string(),
            })
    }

    pub fn simulation_room_mut(
        &mut self,
        room_id: &str,
    ) -> Result<&mut SimulationRoom, RoomManagerError> {
        self.rooms
            .get_mut(room_id)
            .ok_or_else(|| RoomManagerError::RoomNotFound {
                room_id: room_id.to_string(),
            })
    }

    pub fn room(&self, room_id: &str) -> Result<&ExchangeActor, RoomManagerError> {
        self.simulation_room(room_id)
            .map(SimulationRoom::primary_exchange)
    }

    pub fn room_mut(&mut self, room_id: &str) -> Result<&mut ExchangeActor, RoomManagerError> {
        self.simulation_room_mut(room_id)
            .map(SimulationRoom::primary_exchange_mut)
    }

    pub fn room_ids(&self) -> Vec<&str> {
        self.rooms.keys().map(String::as_str).collect()
    }

    pub fn remove_room(&mut self, room_id: &str) -> bool {
        self.recovered_last_trades
            .retain(|(room, _), _| room != room_id);
        let removed = self.rooms.remove(room_id).is_some();
        self.executions.remove(room_id);
        self.pending_liquidations.remove(room_id);
        self.restored_bot_history.remove(room_id);
        removed
    }

    pub fn status(&self, room_id: &str) -> Result<MarketStatus, RoomManagerError> {
        self.simulation_room(room_id).map(SimulationRoom::status)
    }

    pub fn book_snapshot(&self, room_id: &str) -> Result<BookSnapshot, RoomManagerError> {
        self.simulation_room(room_id)
            .map(SimulationRoom::book_snapshot)
    }

    pub fn book_snapshot_for(
        &self,
        room_id: &str,
        instrument_id: &str,
    ) -> Result<BookSnapshot, RoomManagerError> {
        self.simulation_room(room_id)?
            .book_snapshot_for(instrument_id)
            .map_err(RoomManagerError::Actor)
    }

    pub fn account_snapshot(
        &self,
        room_id: &str,
        account_id: AccountId,
    ) -> Result<Option<AccountSnapshot>, RoomManagerError> {
        self.simulation_room(room_id)
            .map(|room| room.account_snapshot(account_id))
    }

    pub fn account_snapshot_for(
        &self,
        room_id: &str,
        instrument_id: &str,
        account_id: AccountId,
    ) -> Result<Option<AccountSnapshot>, RoomManagerError> {
        self.simulation_room(room_id)?
            .account_snapshot_for(instrument_id, account_id)
            .map_err(RoomManagerError::Actor)
    }

    pub fn account_snapshots(&self, room_id: &str) -> Result<AccountSnapshots, RoomManagerError> {
        self.simulation_room(room_id)
            .map(SimulationRoom::account_snapshots)
    }

    pub fn account_snapshots_for(
        &self,
        room_id: &str,
        instrument_id: &str,
    ) -> Result<AccountSnapshots, RoomManagerError> {
        self.simulation_room(room_id)?
            .account_snapshots_for(instrument_id)
            .map_err(RoomManagerError::Actor)
    }

    pub fn order_owner_for(
        &self,
        room_id: &str,
        instrument_id: &str,
        order_id: OrderId,
    ) -> Result<Option<AccountId>, RoomManagerError> {
        self.simulation_room(room_id)?
            .order_owner_for(instrument_id, order_id)
            .map_err(RoomManagerError::Actor)
    }

    pub fn resting_orders_for_account(
        &self,
        room_id: &str,
        instrument_id: &str,
        account_id: AccountId,
    ) -> Result<Vec<crate::model::Order>, RoomManagerError> {
        self.simulation_room(room_id)?
            .resting_orders_for_account(instrument_id, account_id)
            .map_err(RoomManagerError::Actor)
    }

    pub fn participant_observation(
        &self,
        room_id: &str,
        instrument_id: &str,
        account_id: AccountId,
    ) -> Result<crate::observation::ParticipantObservation, RoomManagerError> {
        use crate::model::Event;
        use crate::observation::{
            MAX_PUBLIC_TRADES_IN_OBSERVATION, PARTICIPANT_OBSERVATION_VERSION,
        };

        let room = self.simulation_room(room_id)?;
        if room.status() == crate::actor::MarketStatus::Closed {
            // Observation is still allowed; closed rooms remain inspectable.
        }
        let venue_id = room
            .venue_id_for_instrument(instrument_id)
            .ok_or_else(|| {
                RoomManagerError::Actor(ActorRejectReason::InstrumentNotFound {
                    instrument_id: instrument_id.to_string(),
                })
            })?
            .to_string();
        let clock = room.clock();
        let book = self.book_snapshot_for(room_id, instrument_id)?;
        let own_account = self
            .account_snapshot_for(room_id, instrument_id, account_id)?
            .filter(|snapshot| match snapshot {
                crate::actor::AccountSnapshot::Spot(account) => account.account_id == account_id,
                crate::actor::AccountSnapshot::Perp(account) => account.account_id == account_id,
            });
        let own_orders = self.resting_orders_for_account(room_id, instrument_id, account_id)?;
        let mut public_trades = Vec::new();
        if let Ok(history) = self.execution_history_from(room_id, 0) {
            for execution in history.rev() {
                if execution.instrument_id != instrument_id {
                    continue;
                }
                let trades = match &execution.result {
                    ActorExecutionResult::Accepted(MarketExecution::Spot(result)) => result
                        .events
                        .iter()
                        .filter_map(|record| match &record.event {
                            Event::TradePrinted(trade) => Some(trade.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                    ActorExecutionResult::Accepted(MarketExecution::Perp(result)) => result
                        .events
                        .iter()
                        .filter_map(|record| match &record.event {
                            Event::TradePrinted(trade) => Some(trade.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                    ActorExecutionResult::Rejected(_) => Vec::new(),
                };
                for trade in trades.into_iter().rev() {
                    public_trades.push(trade);
                    if public_trades.len() == MAX_PUBLIC_TRADES_IN_OBSERVATION {
                        break;
                    }
                }
                if public_trades.len() == MAX_PUBLIC_TRADES_IN_OBSERVATION {
                    break;
                }
            }
        }
        // A durable engine checkpoint can omit the command history. The restored
        // receipt projection still supplies actual recent trades; don't make a
        // restart look like a market with no trades, or duplicate replayed ones.
        if public_trades.len() < MAX_PUBLIC_TRADES_IN_OBSERVATION
            && let Some((receipts, _)) = self.restored_bot_history.get(room_id)
        {
            let mut seen = public_trades
                .iter()
                .map(|trade| trade.trade_id)
                .collect::<BTreeSet<_>>();
            for receipt in receipts
                .iter()
                .rev()
                .filter(|receipt| receipt.instrument_id == instrument_id)
            {
                if seen.insert(receipt.trade.trade_id) {
                    public_trades.push(receipt.trade.clone());
                    if public_trades.len() == MAX_PUBLIC_TRADES_IN_OBSERVATION {
                        break;
                    }
                }
            }
        }
        // Restored receipts may overlap the replay tail. Trade IDs are monotonic
        // within one instrument, so sorting gives the same chronological window.
        public_trades.sort_by_key(|trade| trade.trade_id);

        Ok(crate::observation::ParticipantObservation {
            version: PARTICIPANT_OBSERVATION_VERSION,
            room_id: room_id.to_string(),
            venue_id,
            instrument_id: instrument_id.to_string(),
            status: room.status(),
            step: clock.step(),
            market_time_ms: clock.market_time_ms(),
            book,
            public_trades,
            own_orders,
            own_account,
            related_markets: vec![],
            market_events: room.visible_market_events(instrument_id),
            bot_market_data: None,
            perp_price: room
                .perp_price_snapshot(instrument_id)
                .map_err(RoomManagerError::Actor)?,
        })
    }

    /// Public peer markets and this caller's account only, under one room lock.
    pub fn enrich_bot_observation(
        &self,
        observation: &mut crate::ParticipantObservation,
        instruments: &[String],
        account_id: AccountId,
    ) -> Result<(), RoomManagerError> {
        for instrument in instruments {
            if instrument != &observation.instrument_id {
                observation
                    .related_markets
                    .push(self.participant_observation(
                        &observation.room_id,
                        instrument,
                        account_id,
                    )?);
            }
        }
        Ok(())
    }

    /// Build history under the same room lock as the participant snapshot.
    pub fn bot_observation(
        &self,
        room_id: &str,
        instrument_id: &str,
        account_id: AccountId,
        request: Option<crate::bots::BotMarketDataRequest>,
    ) -> Result<crate::ParticipantObservation, RoomManagerError> {
        let mut observation = self.participant_observation(room_id, instrument_id, account_id)?;
        if let Some(request) = request {
            if request.validate().is_err() {
                return Err(RoomManagerError::Candle(
                    crate::CandleError::InvalidInterval,
                ));
            }
            let (receipts, complete) = self.bot_trade_history(room_id, instrument_id)?;
            let timed = receipts
                .iter()
                .map(|receipt| {
                    crate::TimedTrade::from_trade(receipt.market_time_ms, &receipt.trade)
                })
                .collect::<Vec<_>>();
            let mut candles =
                crate::aggregate_candles(&timed, request.interval_ms, observation.market_time_ms)
                    .map_err(RoomManagerError::Candle)?;
            candles.retain(|bar| bar.is_final);
            let mut truncated = !complete || candles.len() > request.max_bars;
            if candles.len() > request.max_bars {
                candles.drain(..candles.len() - request.max_bars);
            }
            let external_volume_qty = receipts
                .iter()
                .filter(|r| {
                    r.trade.maker_account_id != account_id && r.trade.taker_account_id != account_id
                })
                .fold(0u128, |n, r| n.saturating_add(u128::from(r.trade.qty)))
                .to_string();
            let mut fills = Vec::new();
            let mut details = Vec::new();
            for receipt in receipts {
                let trade = &receipt.trade;
                if trade.maker_account_id == account_id || trade.taker_account_id == account_id {
                    let buyer = if trade.taker_side == crate::Side::Buy {
                        trade.taker_account_id
                    } else {
                        trade.maker_account_id
                    };
                    details.push(crate::observation::BotFillDetail {
                        trade_id: trade.trade_id,
                        market_time_ms: receipt.market_time_ms,
                        fee_paid: if account_id == buyer {
                            receipt.buyer_fee
                        } else {
                            receipt.seller_fee
                        },
                    });
                    fills.push(receipt.trade);
                }
            }
            if fills.len() > 4096 {
                truncated = true;
                details.drain(..details.len() - 4096);
                fills.drain(..fills.len() - 4096);
            }
            observation.bot_market_data = Some(crate::BotMarketData {
                external_volume_qty,
                interval_ms: request.interval_ms,
                candles,
                own_fills: fills,
                fill_details: details,
                truncated,
            });
        }
        Ok(observation)
    }

    /// Restore public receipts independently of engine checkpoint replay.
    pub fn restore_bot_history(
        &mut self,
        room_id: &str,
        receipts: Vec<crate::observation::BotTradeReceipt>,
        complete: bool,
    ) {
        self.restored_bot_history.insert(
            room_id.to_string(),
            (std::sync::Arc::new(receipts), complete),
        );
    }

    fn bot_trade_history(
        &self,
        room_id: &str,
        instrument_id: &str,
    ) -> Result<(Vec<crate::observation::BotTradeReceipt>, bool), RoomManagerError> {
        use crate::{Event, PerpClearingEvent, SpotClearingEvent};
        let mut receipts = BTreeMap::new();
        let mut complete = true;
        if let Some((restored, restored_complete)) = self.restored_bot_history.get(room_id) {
            complete = *restored_complete;
            for receipt in restored
                .iter()
                .filter(|receipt| receipt.instrument_id == instrument_id)
            {
                receipts.insert(receipt.trade.trade_id, receipt.clone());
            }
        }
        for execution in self.execution_history_from(room_id, 0)? {
            if execution.instrument_id != instrument_id {
                continue;
            }
            let mut fees = BTreeMap::new();
            let events = match &execution.result {
                ActorExecutionResult::Accepted(MarketExecution::Spot(result)) => {
                    for event in &result.clearing_events {
                        let SpotClearingEvent::TradeSettled {
                            trade_id,
                            buyer_fee,
                            seller_fee,
                            ..
                        } = event;
                        fees.insert(*trade_id, (*buyer_fee, *seller_fee));
                    }
                    &result.events
                }
                ActorExecutionResult::Accepted(MarketExecution::Perp(result)) => {
                    for event in &result.clearing_events {
                        if let PerpClearingEvent::TradeSettled {
                            trade_id,
                            buyer_fee,
                            seller_fee,
                            ..
                        } = event
                        {
                            fees.insert(*trade_id, (*buyer_fee, *seller_fee));
                        }
                    }
                    &result.events
                }
                ActorExecutionResult::Rejected(_) => continue,
            };
            for record in events {
                if let Event::TradePrinted(trade) = &record.event {
                    let fee = fees.get(&trade.trade_id);
                    let receipt = crate::observation::BotTradeReceipt {
                        instrument_id: instrument_id.to_string(),
                        market_time_ms: execution.market_time_ms,
                        trade: trade.clone(),
                        buyer_fee: fee.map(|value| value.0),
                        seller_fee: fee.map(|value| value.1),
                    };
                    if let Some(previous) = receipts.insert(trade.trade_id, receipt.clone())
                        && previous != receipt
                    {
                        complete = false;
                    }
                }
            }
        }
        Ok((receipts.into_values().collect(), complete))
    }

    pub fn venue_account_snapshot(
        &self,
        room_id: &str,
        account_id: AccountId,
    ) -> Result<VenueAccountSnapshot, RoomManagerError> {
        self.simulation_room(room_id)
            .map(|room| room.venue_account_snapshot(account_id))
    }

    pub fn venue_account_snapshots(
        &self,
        room_id: &str,
    ) -> Result<Vec<VenueAccountSnapshot>, RoomManagerError> {
        self.simulation_room(room_id)
            .map(SimulationRoom::venue_account_snapshots)
    }

    pub fn venue_account_snapshots_by_venue(
        &self,
        room_id: &str,
    ) -> Result<Vec<VenueAccountVenueSnapshot>, RoomManagerError> {
        self.simulation_room(room_id)
            .map(SimulationRoom::venue_account_snapshots_by_venue)
    }

    pub fn portfolio_snapshot(
        &self,
        room_id: &str,
        account_id: AccountId,
    ) -> Result<PortfolioAccountSnapshot, RoomManagerError> {
        self.simulation_room(room_id)
            .map(|room| room.portfolio_snapshot(account_id))
    }

    pub fn portfolio_snapshots(
        &self,
        room_id: &str,
    ) -> Result<Vec<PortfolioAccountSnapshot>, RoomManagerError> {
        self.simulation_room(room_id)
            .map(SimulationRoom::portfolio_snapshots)
    }

    pub fn asset_ledger(&self, room_id: &str) -> Result<&[AssetLedgerEntry], RoomManagerError> {
        self.simulation_room(room_id)
            .map(SimulationRoom::asset_ledger)
    }

    pub fn net_worth_snapshot(
        &self,
        room_id: &str,
    ) -> Result<RoomNetWorthSnapshot, RoomManagerError> {
        self.simulation_room(room_id)
            .map(SimulationRoom::net_worth_snapshot)
    }

    pub fn clock(&self, room_id: &str) -> Result<SimulationClock, RoomManagerError> {
        self.simulation_room(room_id).map(SimulationRoom::clock)
    }

    pub fn ticker(
        &self,
        room_id: &str,
        instrument_id: &str,
    ) -> Result<crate::candles::Ticker, RoomManagerError> {
        let book = self.book_snapshot_for(room_id, instrument_id)?;
        let last = self
            .timed_trades(room_id, instrument_id)?
            .last()
            .map(|trade| trade.price_tick)
            .or_else(|| {
                self.recovered_last_trades
                    .get(&(room_id.to_string(), instrument_id.to_string()))
                    .copied()
            });
        Ok(crate::candles::Ticker::from_book_and_last(
            book.bids.first().map(|level| level.price_tick),
            book.asks.first().map(|level| level.price_tick),
            last,
        ))
    }

    /// Preserve last-trade ticker state when restoring only a journal suffix.
    pub fn restore_last_trade_price(
        &mut self,
        room_id: &str,
        instrument_id: &str,
        price_tick: i64,
    ) {
        self.recovered_last_trades
            .insert((room_id.to_string(), instrument_id.to_string()), price_tick);
    }

    pub fn timed_trades(
        &self,
        room_id: &str,
        instrument_id: &str,
    ) -> Result<Vec<crate::candles::TimedTrade>, RoomManagerError> {
        use crate::model::Event;
        let mut trades = Vec::new();
        for execution in self.execution_history_from(room_id, 0)? {
            if execution.instrument_id != instrument_id {
                continue;
            }
            let events = match &execution.result {
                ActorExecutionResult::Accepted(MarketExecution::Spot(result)) => {
                    result.events.as_slice()
                }
                ActorExecutionResult::Accepted(MarketExecution::Perp(result)) => {
                    result.events.as_slice()
                }
                ActorExecutionResult::Rejected(_) => continue,
            };
            for record in events {
                if let Event::TradePrinted(trade) = &record.event {
                    trades.push(crate::candles::TimedTrade::from_trade(
                        execution.market_time_ms,
                        trade,
                    ));
                }
            }
        }
        Ok(trades)
    }

    pub fn candles(
        &self,
        room_id: &str,
        instrument_id: &str,
        interval_ms: u64,
    ) -> Result<Vec<crate::candles::Candle>, RoomManagerError> {
        let now = self.clock(room_id)?.market_time_ms();
        let trades = self.timed_trades(room_id, instrument_id)?;
        crate::candles::aggregate_candles(&trades, interval_ms, now)
            .map_err(RoomManagerError::Candle)
    }

    pub fn advance_clock(
        &mut self,
        room_id: &str,
        steps: u64,
    ) -> Result<Vec<VenueTransfer>, RoomManagerError> {
        if self.status(room_id)? == crate::actor::MarketStatus::Closed {
            return Err(RoomManagerError::Simulation(SimulationRoomError::Closed));
        }
        if !self.simulation_room(room_id)?.has_funding() {
            let transfers = self
                .simulation_room_mut(room_id)?
                .advance_clock(steps)
                .map_err(RoomManagerError::Simulation)?;
            let clock_executions = self.simulation_room_mut(room_id)?.take_clock_executions();
            self.executions
                .entry(room_id.to_string())
                .or_default()
                .extend(clock_executions);
            self.advance_pending_liquidations(room_id, usize::MAX)?;
            return Ok(transfers);
        }
        self.clock(room_id)?
            .checked_time_after(steps)
            .map_err(|error| RoomManagerError::Simulation(SimulationRoomError::Clock(error)))?;
        if steps > 1 {
            let mut staged = self.clone();
            let transfers = staged.advance_clock_in_steps(room_id, steps)?;
            *self = staged;
            Ok(transfers)
        } else {
            self.advance_clock_in_steps(room_id, steps)
        }
    }

    fn advance_clock_in_steps(
        &mut self,
        room_id: &str,
        steps: u64,
    ) -> Result<Vec<VenueTransfer>, RoomManagerError> {
        let mut transfers = Vec::new();
        for _ in 0..steps {
            let room = self.simulation_room_mut(room_id)?;
            transfers.extend(
                room.advance_clock(1)
                    .map_err(RoomManagerError::Simulation)?,
            );
            let funding = room.take_clock_executions();
            self.executions
                .entry(room_id.to_string())
                .or_default()
                .extend(funding);
            self.advance_pending_liquidations(room_id, usize::MAX)?;
        }
        if steps == 0 {
            self.advance_pending_liquidations(room_id, usize::MAX)?;
        }
        Ok(transfers)
    }

    pub fn submit_deposit(
        &mut self,
        room_id: &str,
        venue_id: Option<&str>,
        account_id: AccountId,
        asset_id: impl Into<String>,
        amount: Money,
    ) -> Result<VenueTransfer, RoomManagerError> {
        self.simulation_room_mut(room_id)?
            .submit_deposit(venue_id, account_id, asset_id, amount)
            .map_err(RoomManagerError::Simulation)
    }

    pub fn submit_withdrawal(
        &mut self,
        room_id: &str,
        venue_id: Option<&str>,
        account_id: AccountId,
        asset_id: impl Into<String>,
        amount: Money,
    ) -> Result<VenueTransfer, RoomManagerError> {
        self.simulation_room_mut(room_id)?
            .submit_withdrawal(venue_id, account_id, asset_id, amount)
            .map_err(RoomManagerError::Simulation)
    }

    pub fn submit_venue_to_venue_transfer(
        &mut self,
        room_id: &str,
        from_venue_id: &str,
        to_venue_id: &str,
        account_id: AccountId,
        asset_id: impl Into<String>,
        amount: Money,
    ) -> Result<VenueToVenueTransfer, RoomManagerError> {
        self.simulation_room_mut(room_id)?
            .submit_venue_to_venue_transfer(
                from_venue_id,
                to_venue_id,
                account_id,
                asset_id,
                amount,
            )
            .map_err(RoomManagerError::Simulation)
    }

    pub fn transfers(&self, room_id: &str) -> Result<Vec<VenueTransfer>, RoomManagerError> {
        self.simulation_room(room_id).map(SimulationRoom::transfers)
    }

    pub fn execution_history(&self, room_id: &str) -> Result<&[ActorExecution], RoomManagerError> {
        self.room(room_id)?;
        Ok(self
            .executions
            .get(room_id)
            .map(|history| &**history)
            .unwrap_or_default())
    }

    /// Count records without materializing a contiguous export of the history.
    pub fn execution_history_len(&self, room_id: &str) -> Result<usize, RoomManagerError> {
        self.room(room_id)?;
        Ok(self.executions.get(room_id).map_or(0, History::len))
    }

    /// Ordered transaction suffix, sharing old chunks with candidate rooms.
    /// The legacy slice API remains available for explicit complete exports.
    pub fn execution_history_from(
        &self,
        room_id: &str,
        start: usize,
    ) -> Result<impl DoubleEndedIterator<Item = &ActorExecution>, RoomManagerError> {
        self.room(room_id)?;
        Ok(self
            .executions
            .get(room_id)
            .into_iter()
            .flat_map(move |history| history.iter_from(start)))
    }
}

pub struct RoomObservationBatch<'a> {
    rooms: &'a RoomManager,
    room_id: &'a str,
    public: BTreeMap<String, crate::ParticipantObservation>,
    history: BTreeMap<String, ObservationHistory>,
}

/// The immutable room borrow is the cache validity boundary. Keep extra
/// receipts/bars for this wave to avoid replaying history for every POV bot.
struct ObservationHistory {
    receipts: Vec<crate::observation::BotTradeReceipt>,
    complete: bool,
    candles: BTreeMap<u64, Vec<crate::Candle>>,
    volume: Option<u128>,
    accounts: BTreeMap<AccountId, ObservationAccountHistory>,
}

#[derive(Default)]
struct ObservationAccountHistory {
    volume: u128,
    receipt_indices: Vec<usize>,
}

impl ObservationHistory {
    fn new(receipts: Vec<crate::observation::BotTradeReceipt>, complete: bool) -> Self {
        let mut volume = Some(0u128);
        let mut accounts = BTreeMap::<AccountId, ObservationAccountHistory>::new();
        for (index, receipt) in receipts.iter().enumerate() {
            let trade = &receipt.trade;
            volume = volume.and_then(|value| value.checked_add(u128::from(trade.qty)));
            for (side, account) in [trade.maker_account_id, trade.taker_account_id]
                .into_iter()
                .enumerate()
            {
                if side == 1 && trade.maker_account_id == trade.taker_account_id {
                    continue;
                }
                let own = accounts.entry(account).or_default();
                own.volume = own.volume.saturating_add(u128::from(trade.qty));
                own.receipt_indices.push(index);
            }
        }
        Self {
            receipts,
            complete,
            candles: BTreeMap::new(),
            volume,
            accounts,
        }
    }
}

impl RoomObservationBatch<'_> {
    pub fn bot_observation(
        &mut self,
        instrument_id: &str,
        account_id: AccountId,
        request: Option<crate::bots::BotMarketDataRequest>,
        related: &[String],
    ) -> Result<crate::ParticipantObservation, RoomManagerError> {
        let mut view = self.participant(instrument_id, account_id)?;
        if let Some(request) = request {
            if request.validate().is_err() {
                return Err(RoomManagerError::Candle(
                    crate::CandleError::InvalidInterval,
                ));
            }
            if !self.history.contains_key(instrument_id) {
                let (receipts, complete) =
                    self.rooms.bot_trade_history(self.room_id, instrument_id)?;
                self.history.insert(
                    instrument_id.to_string(),
                    ObservationHistory::new(receipts, complete),
                );
            }
            let history = self
                .history
                .get_mut(instrument_id)
                .expect("inserted history");
            if !history.candles.contains_key(&request.interval_ms) {
                let timed = history
                    .receipts
                    .iter()
                    .map(|receipt| {
                        crate::TimedTrade::from_trade(receipt.market_time_ms, &receipt.trade)
                    })
                    .collect::<Vec<_>>();
                let mut candles =
                    crate::aggregate_candles(&timed, request.interval_ms, view.market_time_ms)
                        .map_err(RoomManagerError::Candle)?;
                candles.retain(|bar| bar.is_final);
                history.candles.insert(request.interval_ms, candles);
            }
            let candles = &history.candles[&request.interval_ms];
            let own = history.accounts.get(&account_id);
            let external_volume_qty = history
                .volume
                .map(|volume| volume - own.map_or(0, |own| own.volume))
                .unwrap_or_else(|| {
                    // Preserve the original saturating filtered sum even if
                    // the total quantity cannot be represented in u128.
                    history
                        .receipts
                        .iter()
                        .filter(|receipt| {
                            receipt.trade.maker_account_id != account_id
                                && receipt.trade.taker_account_id != account_id
                        })
                        .fold(0u128, |n, receipt| {
                            n.saturating_add(u128::from(receipt.trade.qty))
                        })
                })
                .to_string();
            let indices = own.map_or(&[][..], |own| own.receipt_indices.as_slice());
            let mut own_fills = Vec::new();
            let mut fill_details = Vec::new();
            for &index in &indices[indices.len().saturating_sub(4096)..] {
                let receipt = &history.receipts[index];
                let trade = &receipt.trade;
                let buyer = if trade.taker_side == crate::Side::Buy {
                    trade.taker_account_id
                } else {
                    trade.maker_account_id
                };
                fill_details.push(crate::observation::BotFillDetail {
                    trade_id: trade.trade_id,
                    market_time_ms: receipt.market_time_ms,
                    fee_paid: if account_id == buyer {
                        receipt.buyer_fee
                    } else {
                        receipt.seller_fee
                    },
                });
                own_fills.push(trade.clone());
            }
            let truncated =
                !history.complete || candles.len() > request.max_bars || indices.len() > 4096;
            view.bot_market_data = Some(crate::BotMarketData {
                interval_ms: request.interval_ms,
                external_volume_qty,
                candles: candles[candles.len().saturating_sub(request.max_bars)..].to_vec(),
                own_fills,
                fill_details,
                truncated,
            });
        }
        for instrument in related {
            if instrument != instrument_id {
                view.related_markets
                    .push(self.participant(instrument, account_id)?);
            }
        }
        Ok(view)
    }

    fn participant(
        &mut self,
        instrument: &str,
        account: AccountId,
    ) -> Result<crate::ParticipantObservation, RoomManagerError> {
        if let Some(public) = self.public.get(instrument) {
            let mut view = public.clone();
            view.own_account = self
                .rooms
                .account_snapshot_for(self.room_id, instrument, account)?
                .filter(|snapshot| match snapshot {
                    crate::AccountSnapshot::Spot(value) => value.account_id == account,
                    crate::AccountSnapshot::Perp(value) => value.account_id == account,
                });
            view.own_orders =
                self.rooms
                    .resting_orders_for_account(self.room_id, instrument, account)?;
            return Ok(view);
        }
        let view = self
            .rooms
            .participant_observation(self.room_id, instrument, account)?;
        let mut public = view.clone();
        public.own_account = None;
        public.own_orders.clear();
        self.public.insert(instrument.to_string(), public);
        Ok(view)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoomBootstrap {
    pub room_id: RoomId,
    pub seed_executions: Vec<ActorExecution>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RoomManagerError {
    RoomAlreadyExists {
        room_id: RoomId,
    },
    RoomNotFound {
        room_id: RoomId,
    },
    SystemOrderIdOverflow,
    OrderOwnershipMismatch {
        order_id: OrderId,
        account_id: AccountId,
        owner_account_id: AccountId,
    },
    MarketConfig(crate::market::MarketConfigError),
    Actor(ActorRejectReason),
    Scenario(ScenarioError),
    Simulation(SimulationRoomError),
    Candle(crate::candles::CandleError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingRoomLiquidation {
    pub instrument_id: String,
    pub account_id: AccountId,
}

fn system_liquidation_order_id(
    trigger_command_seq: u64,
    liquidation_index: u64,
) -> Result<OrderId, RoomManagerError> {
    let offset = trigger_command_seq
        .checked_mul(SYSTEM_LIQUIDATION_ORDER_ID_STRIDE)
        .and_then(|offset| offset.checked_add(liquidation_index))
        .ok_or(RoomManagerError::SystemOrderIdOverflow)?;
    SYSTEM_LIQUIDATION_ORDER_ID_BASE
        .checked_add(offset)
        .ok_or(RoomManagerError::SystemOrderIdOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AssetKind, AssetSelector,
        actor::{ActorExecutionResult, ActorRejectReason, MarketExecution},
        market::{
            ExchangeConfig, InstrumentConfig, MarketConfig, PerpMarketConfig, SpotMarketConfig,
        },
        model::{BookLevel, Event, NewOrder, OrderKind, SetMarkPrice, Side},
        perp::{PerpAccountSnapshot, PerpClearingConfig, PerpMarginStatus},
        risk::{PerpRiskConfig, SpotRiskConfig},
        scenario::{
            ScenarioAccount, ScenarioAllocation, ScenarioPortfolio, ScenarioVenueAllocation,
        },
        spot::SpotClearingConfig,
        transfer::{VenueTransferRejectReason, VenueTransferStatus},
    };

    include!("order_protection_tests.rs");

    #[test]
    fn moving_execution_owners_preserves_all_room_state_and_rejects_overlap() {
        let mut rooms = RoomManager::new();
        rooms
            .create_room(pending_perp_liquidation_scenario("move-perp"))
            .unwrap();
        rooms.create_room(spot_scenario("move-spot")).unwrap();
        rooms.restore_last_trade_price("move-perp", "V-PERP", 77);
        rooms.restore_last_trade_price("move-spot", "V-SPOT", 88);
        let before = rooms.clone();
        let moved = rooms.take_room("move-perp").unwrap();
        assert!(rooms.status("move-perp").is_err());
        assert!(rooms.status("move-spot").is_ok());
        assert_eq!(
            serde_json::to_value(moved.simulation_room("move-perp").unwrap()).unwrap(),
            serde_json::to_value(before.simulation_room("move-perp").unwrap()).unwrap()
        );
        rooms.join_disjoint(moved).unwrap();
        assert_eq!(
            serde_json::to_value(&rooms.rooms).unwrap(),
            serde_json::to_value(&before.rooms).unwrap()
        );
        assert_eq!(rooms.pending_liquidations, before.pending_liquidations);
        assert_eq!(rooms.recovered_last_trades, before.recovered_last_trades);
        assert_eq!(rooms.restored_bot_history, before.restored_bot_history);
        for id in ["move-perp", "move-spot"] {
            assert_eq!(
                rooms.execution_history(id).unwrap(),
                before.execution_history(id).unwrap()
            );
        }
        assert!(rooms.join_disjoint(rooms.clone()).is_err());
        assert_eq!(
            serde_json::to_value(&rooms.rooms).unwrap(),
            serde_json::to_value(&before.rooms).unwrap()
        );
        assert!(rooms.take_room("missing").is_none());
    }

    #[test]
    #[ignore = "isolated Release snapshot-versus-raw liquidation scan comparison"]
    fn liquidation_scan_fixed_work_benchmark() {
        for case in 0..3 {
            let mut scenario = if case == 0 {
                spot_scenario("scan-bench")
            } else {
                pending_perp_liquidation_scenario("scan-bench")
            };
            if case == 2
                && let MarketConfig::Perp(config) = &mut scenario.market
            {
                config.clearing.position_mode = crate::PositionMode::Hedge;
            }
            scenario.accounts = (1..=1000)
                .map(|account_id| ScenarioAccount::Basic {
                    account_id,
                    cash_balance: 1_000_000,
                })
                .collect();
            scenario.seed_orders.clear();
            if case > 0 {
                for n in 0..1000 {
                    let side = if n % 2 == 0 { Side::Sell } else { Side::Buy };
                    let mut command = limit(n + 1, n + 1, side, 100, 2);
                    if case == 2
                        && let Command::NewOrder(order) = &mut command
                    {
                        order.position_side = if side == Side::Buy {
                            crate::PositionSide::Long
                        } else {
                            crate::PositionSide::Short
                        };
                    }
                    scenario.seed_orders.push(command);
                }
            }
            let instrument = scenario.market.instrument_id().to_string();
            let mut manager = RoomManager::new();
            manager.create_room(scenario).unwrap();
            let before =
                serde_json::to_value(manager.simulation_room("scan-bench").unwrap()).unwrap();
            let expected = manager
                .liquidation_candidates_for_instrument("scan-bench", &instrument, true)
                .unwrap();
            assert!(expected.is_empty());
            for snapshot in [true, false, false, true, true, false, false, true] {
                crate::simulation::SNAPSHOT_LIQUIDATION_REFERENCE.set(snapshot);
                let start = std::time::Instant::now();
                for _ in 0..500 {
                    let actual = std::hint::black_box(&manager)
                        .liquidation_candidates_for_instrument("scan-bench", &instrument, true)
                        .unwrap();
                    assert_eq!(actual, expected);
                }
                println!(
                    "scan_case={case} snapshot={snapshot} seconds={:.6}",
                    start.elapsed().as_secs_f64()
                );
                crate::simulation::SNAPSHOT_LIQUIDATION_REFERENCE.set(false);
                assert_eq!(
                    serde_json::to_value(manager.simulation_room("scan-bench").unwrap()).unwrap(),
                    before
                );
            }
        }
    }

    fn spot_scenario(room_id: &str) -> ScenarioConfig {
        ScenarioConfig {
            room_id: room_id.to_string(),
            market_events: Vec::new(),
            venue_preset: None,
            venue_rules: crate::VenueRuleConfig::default(),
            venue_asset_policy: crate::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: MarketConfig::Spot(SpotMarketConfig {
                instrument: InstrumentConfig::new("V-BTC-SPOT", 1, 1).unwrap(),
                clearing: SpotClearingConfig::default(),
                risk: SpotRiskConfig::default(),
            }),
            extra_markets: Vec::new(),
            initial_portfolios: Vec::new(),
            initial_allocations: Vec::new(),
            routed_initial_allocations: Vec::new(),
            accounts: vec![
                ScenarioAccount::Spot {
                    account_id: 10,
                    cash_balance: 1_000,
                    position_qty: 10,
                },
                ScenarioAccount::Basic {
                    account_id: 20,
                    cash_balance: 1_000,
                },
            ],
            seed_orders: vec![limit(1, 10, Side::Sell, 100, 5)],
            routed_seed_orders: Vec::new(),
        }
    }

    fn pending_perp_liquidation_scenario(room_id: &str) -> ScenarioConfig {
        ScenarioConfig {
            room_id: room_id.to_string(),
            market_events: Vec::new(),
            venue_preset: None,
            venue_rules: crate::VenueRuleConfig::default(),
            venue_asset_policy: crate::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: MarketConfig::Perp(PerpMarketConfig {
                instrument: InstrumentConfig::new("V-BTC-PERP", 1, 1).unwrap(),
                clearing: PerpClearingConfig {
                    leverage: 10,
                    maintenance_margin_ppm: 50_000,
                    ..PerpClearingConfig::default()
                },
                risk: PerpRiskConfig::default(),
                initial_mark_price_tick: 100,
                price_link: None,
                funding: None,
            }),
            extra_markets: Vec::new(),
            initial_portfolios: Vec::new(),
            initial_allocations: Vec::new(),
            routed_initial_allocations: Vec::new(),
            accounts: vec![
                ScenarioAccount::Basic {
                    account_id: 10,
                    cash_balance: 10_000,
                },
                ScenarioAccount::Basic {
                    account_id: 20,
                    cash_balance: 200,
                },
                ScenarioAccount::Basic {
                    account_id: 30,
                    cash_balance: 10_000,
                },
            ],
            seed_orders: Vec::new(),
            routed_seed_orders: Vec::new(),
        }
    }

    fn pending_cross_margin_liquidation_scenario(room_id: &str) -> ScenarioConfig {
        let perp_market = |instrument_id: &str, base_asset: &str| {
            MarketConfig::Perp(PerpMarketConfig {
                instrument: InstrumentConfig::new_for_venue(
                    "virtual",
                    instrument_id,
                    base_asset,
                    "USDT",
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
        };
        ScenarioConfig {
            room_id: room_id.to_string(),
            market_events: Vec::new(),
            venue_preset: None,
            venue_rules: crate::VenueRuleConfig::default(),
            venue_asset_policy: crate::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: perp_market("V-BTC-PERP", "BTC"),
            extra_markets: vec![perp_market("V-ETH-PERP", "ETH")],
            initial_portfolios: Vec::new(),
            initial_allocations: Vec::new(),
            routed_initial_allocations: Vec::new(),
            accounts: vec![
                ScenarioAccount::Basic {
                    account_id: 10,
                    cash_balance: 10_000,
                },
                ScenarioAccount::Basic {
                    account_id: 20,
                    cash_balance: 200,
                },
            ],
            seed_orders: Vec::new(),
            routed_seed_orders: Vec::new(),
        }
    }

    fn distressed_perp_with_spot_exchange(
        room_id: &str,
        spot_base_asset: &str,
        spot_quote_asset: &str,
    ) -> ExchangeActor {
        let spot = MarketConfig::Spot(SpotMarketConfig {
            instrument: InstrumentConfig::new_for_venue(
                "virtual",
                "V-COLLATERAL-SPOT",
                spot_base_asset,
                spot_quote_asset,
                "Collateral Spot",
                1,
                1,
            )
            .unwrap(),
            clearing: SpotClearingConfig::default(),
            risk: SpotRiskConfig::default(),
        });
        let perp = MarketConfig::Perp(PerpMarketConfig {
            instrument: InstrumentConfig::new_for_venue(
                "virtual",
                "V-ETH-PERP",
                "ETH",
                "USDT",
                "ETH-USDT Perp",
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
        let config = ExchangeConfig::new("virtual", vec![spot, perp]).unwrap();
        let mut exchange = ExchangeActor::new(room_id, config).unwrap();
        for (account_id, cash_balance) in [(10, 10_000), (20, 200), (30, 10_000), (40, 10_000)] {
            exchange.create_account(account_id, cash_balance);
        }
        exchange
            .apply_venue_asset_delta(30, spot_base_asset, 1_000)
            .unwrap();
        exchange
            .apply_to_instrument("V-ETH-PERP", limit(1, 10, Side::Sell, 100, 10))
            .unwrap();
        exchange
            .apply_to_instrument(
                "V-ETH-PERP",
                Command::NewOrder(NewOrder {
                    position_side: crate::model::PositionSide::Both,
                    order_id: 2,
                    account_id: 20,
                    side: Side::Buy,
                    kind: OrderKind::Market,
                    qty: 10,
                    reduce_only: false,
                }),
            )
            .unwrap();
        exchange
            .apply_to_instrument(
                "V-ETH-PERP",
                Command::SetMarkPrice(SetMarkPrice { price_tick: 94 }),
            )
            .unwrap();
        assert!(matches!(
            exchange.account_snapshot_for("V-ETH-PERP", 20).unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                equity: 140,
                margin_status: PerpMarginStatus::Healthy,
                ..
            }))
        ));
        exchange
    }

    fn perp_transfer_sync_scenario(room_id: &str, delay_steps: u64) -> ScenarioConfig {
        ScenarioConfig {
            room_id: room_id.to_string(),
            market_events: Vec::new(),
            venue_preset: None,
            venue_rules: crate::VenueRuleConfig {
                transfers: crate::TransferPolicyConfig {
                    deposit_delay_steps: delay_steps,
                    withdrawal_delay_steps: delay_steps,
                },
                ..crate::VenueRuleConfig::default()
            },
            venue_asset_policy: crate::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: MarketConfig::Perp(PerpMarketConfig {
                instrument: InstrumentConfig::new_for_venue(
                    "virtual",
                    "V-BTC-PERP",
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
            }),
            extra_markets: Vec::new(),
            initial_portfolios: vec![ScenarioPortfolio {
                account_id: 20,
                balances: BTreeMap::from([("USDT".to_string(), 100)]),
            }],
            initial_allocations: Vec::new(),
            routed_initial_allocations: Vec::new(),
            accounts: vec![ScenarioAccount::Basic {
                account_id: 20,
                cash_balance: 200,
            }],
            seed_orders: Vec::new(),
            routed_seed_orders: Vec::new(),
        }
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

    fn venue_spot_market(venue_id: &str, instrument_id: &str) -> MarketConfig {
        MarketConfig::Spot(SpotMarketConfig {
            instrument: InstrumentConfig::new_for_venue(
                venue_id,
                instrument_id,
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

    fn cross_venue_transfer_scenario(
        room_id: &str,
        source_venue_id: &str,
        destination_venue_id: &str,
    ) -> ScenarioConfig {
        ScenarioConfig {
            room_id: room_id.to_string(),
            market_events: Vec::new(),
            venue_preset: None,
            venue_rules: crate::VenueRuleConfig {
                transfers: crate::TransferPolicyConfig {
                    deposit_delay_steps: 1,
                    withdrawal_delay_steps: 1,
                },
                ..crate::VenueRuleConfig::default()
            },
            venue_asset_policy: crate::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: venue_spot_market(source_venue_id, &format!("{source_venue_id}:spot")),
            extra_markets: vec![venue_spot_market(
                destination_venue_id,
                &format!("{destination_venue_id}:spot"),
            )],
            initial_portfolios: vec![ScenarioPortfolio {
                account_id: 20,
                balances: BTreeMap::from([("USDT".to_string(), 100)]),
            }],
            initial_allocations: Vec::new(),
            routed_initial_allocations: vec![ScenarioVenueAllocation {
                venue_id: source_venue_id.to_string(),
                account_id: 20,
                asset_id: "USDT".to_string(),
                amount: 100,
            }],
            accounts: vec![ScenarioAccount::Basic {
                account_id: 20,
                cash_balance: 0,
            }],
            seed_orders: Vec::new(),
            routed_seed_orders: Vec::new(),
        }
    }

    fn venue_asset_total(
        manager: &RoomManager,
        room_id: &str,
        venue_id: &str,
        asset_id: &str,
    ) -> Money {
        manager
            .simulation_room(room_id)
            .unwrap()
            .exchange(venue_id)
            .unwrap()
            .venue_balance_snapshot(20, asset_id)
            .map(|balance| balance.total)
            .unwrap_or_default()
    }

    #[test]
    fn creates_room_from_scenario_and_routes_commands() {
        let mut manager = RoomManager::new();
        let bootstrap = manager
            .create_room(spot_scenario("room-1"))
            .expect("room should create");

        assert_eq!(bootstrap.room_id, "room-1");
        assert_eq!(bootstrap.seed_executions.len(), 1);
        assert_eq!(
            manager.book_snapshot("room-1").unwrap().asks,
            vec![BookLevel {
                price_tick: 100,
                qty: 5,
            }]
        );

        let execution = manager
            .apply("room-1", limit(2, 20, Side::Buy, 100, 2))
            .expect("command should route");

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
        assert_eq!(manager.execution_history("room-1").unwrap().len(), 2);
    }

    #[test]
    fn bot_history_is_closed_complete_private_to_account_and_bounded() {
        let mut scenario = spot_scenario("history");
        scenario.seed_orders.clear();
        scenario.accounts.push(ScenarioAccount::Basic {
            account_id: 30,
            cash_balance: 1000,
        });
        let mut manager = RoomManager::new();
        manager.create_room(scenario).unwrap();
        for index in 0..40u64 {
            let (seller, buyer) = if index % 2 == 0 { (10, 20) } else { (20, 10) };
            manager
                .apply("history", limit(index * 2 + 1, seller, Side::Sell, 100, 1))
                .unwrap();
            manager
                .apply("history", limit(index * 2 + 2, buyer, Side::Buy, 100, 1))
                .unwrap();
            manager.advance_clock("history", 1).unwrap();
        }
        let ordinary = manager
            .participant_observation("history", "V-BTC-SPOT", 20)
            .unwrap();
        assert_eq!(ordinary.public_trades.len(), 32);
        assert!(
            serde_json::to_value(&ordinary)
                .unwrap()
                .get("bot_market_data")
                .is_none()
        );
        let request = crate::bots::BotMarketDataRequest {
            interval_ms: 1000,
            max_bars: 64,
        };
        {
            let mut batch = manager.observation_batch("history");
            for account in [20, 30, 10, 999, 20] {
                for history in [
                    None,
                    Some(request),
                    Some(crate::bots::BotMarketDataRequest {
                        interval_ms: 2000,
                        max_bars: 2,
                    }),
                    Some(crate::bots::BotMarketDataRequest {
                        interval_ms: 1,
                        max_bars: 1,
                    }),
                ] {
                    assert_eq!(
                        batch
                            .bot_observation("V-BTC-SPOT", account, history, &[])
                            .unwrap(),
                        manager
                            .bot_observation("history", "V-BTC-SPOT", account, history)
                            .unwrap(),
                        "shared public snapshots must not leak accounts or fills"
                    );
                }
            }
            assert_eq!(batch.history.len(), 1);
            assert_eq!(batch.history["V-BTC-SPOT"].candles.len(), 3);
        }
        let view = manager
            .bot_observation("history", "V-BTC-SPOT", 20, Some(request))
            .unwrap();
        let data = view.bot_market_data.unwrap();
        assert_eq!(data.candles.len(), 40);
        assert_eq!(data.own_fills.len(), 40);
        assert!(!data.truncated);
        assert!(
            data.candles
                .iter()
                .all(|bar| bar.is_final && bar.close_time_ms <= view.market_time_ms)
        );
        let empty = manager
            .bot_observation("history", "V-BTC-SPOT", 30, Some(request))
            .unwrap();
        assert!(empty.bot_market_data.unwrap().own_fills.is_empty());
        let bounded = manager
            .bot_observation(
                "history",
                "V-BTC-SPOT",
                20,
                Some(crate::bots::BotMarketDataRequest {
                    max_bars: 2,
                    ..request
                }),
            )
            .unwrap();
        let bounded = bounded.bot_market_data.unwrap();
        assert!(bounded.truncated);
        assert_eq!(bounded.candles.len(), 2);
        assert_eq!(bounded.candles[0].open_time_ms, 38000);
        manager
            .apply("history", limit(81, 10, Side::Sell, 100, 1))
            .unwrap();
        manager
            .apply("history", limit(82, 20, Side::Buy, 100, 1))
            .unwrap();
        let forming = manager
            .bot_observation("history", "V-BTC-SPOT", 20, Some(request))
            .unwrap();
        assert_eq!(forming.bot_market_data.unwrap().candles.len(), 40);
        assert!(
            manager
                .bot_observation(
                    "history",
                    "V-BTC-SPOT",
                    20,
                    Some(crate::bots::BotMarketDataRequest {
                        max_bars: 4097,
                        ..request
                    })
                )
                .is_err()
        );
        let recovered: crate::ParticipantObservation =
            serde_json::from_value(serde_json::to_value(ordinary).unwrap()).unwrap();
        assert!(recovered.bot_market_data.is_none());
    }

    fn restored_history_room() -> RoomManager {
        let mut manager = RoomManager::new();
        manager
            .create_room(spot_scenario("cached-history"))
            .unwrap();
        manager
            .apply("cached-history", limit(2, 20, Side::Buy, 100, 1))
            .unwrap();
        let (receipts, _) = manager
            .bot_trade_history("cached-history", "V-BTC-SPOT")
            .unwrap();
        let template = &receipts[0];
        let receipts = (0..4200)
            .map(|index| {
                let mut receipt = template.clone();
                receipt.trade.trade_id = index + 100;
                receipt.market_time_ms = index * 1000;
                receipt.buyer_fee = Some(2);
                receipt.seller_fee = Some(1);
                receipt
            })
            .collect();
        manager.restore_bot_history("cached-history", receipts, false);
        manager.advance_clock("cached-history", 4200).unwrap();
        manager
    }

    #[test]
    fn wave_history_preserves_bounded_fills_conflicts_and_new_wave_freshness() {
        let mut manager = restored_history_room();
        let request = crate::bots::BotMarketDataRequest {
            interval_ms: 1000,
            max_bars: 2,
        };
        {
            let mut batch = manager.observation_batch("cached-history");
            for account in [10, 20, 30, 999, 10] {
                let view = batch
                    .bot_observation("V-BTC-SPOT", account, Some(request), &[])
                    .unwrap();
                assert_eq!(
                    view,
                    manager
                        .bot_observation("cached-history", "V-BTC-SPOT", account, Some(request))
                        .unwrap()
                );
                let data = view.bot_market_data.unwrap();
                assert!(data.truncated);
                if account == 10 || account == 20 {
                    assert_eq!(data.own_fills.len(), 4096);
                    assert_eq!(data.own_fills[0].trade_id, 204);
                    assert_eq!(data.fill_details.len(), 4096);
                } else {
                    assert!(data.own_fills.is_empty());
                }
            }
        }
        manager
            .apply("cached-history", limit(3, 20, Side::Buy, 100, 1))
            .unwrap();
        // A new wave observes the append immediately, including the forming bar.
        let mut batch = manager.observation_batch("cached-history");
        let view = batch
            .bot_observation("V-BTC-SPOT", 20, Some(request), &[])
            .unwrap();
        assert_eq!(
            view,
            manager
                .bot_observation("cached-history", "V-BTC-SPOT", 20, Some(request))
                .unwrap()
        );
        assert_eq!(batch.history["V-BTC-SPOT"].receipts.len(), 4202);
        // The fallback retains the exact filtered sum if the cached total is unavailable.
        batch.history.get_mut("V-BTC-SPOT").unwrap().volume = None;
        assert_eq!(
            batch
                .bot_observation("V-BTC-SPOT", 20, Some(request), &[])
                .unwrap(),
            view
        );
        let mut conflict = batch.history["V-BTC-SPOT"]
            .receipts
            .first()
            .unwrap()
            .clone();
        conflict.trade.price_tick += 1;
        drop(batch);
        manager.restore_bot_history("cached-history", vec![conflict], true);
        let mut batch = manager.observation_batch("cached-history");
        assert_eq!(
            batch
                .bot_observation("V-BTC-SPOT", 20, Some(request), &[])
                .unwrap(),
            manager
                .bot_observation("cached-history", "V-BTC-SPOT", 20, Some(request))
                .unwrap()
        );
        assert!(!batch.history["V-BTC-SPOT"].complete);
    }

    #[test]
    #[ignore = "fixed-work release comparison; run explicitly"]
    fn wave_history_fixed_work_benchmark() {
        let manager = restored_history_room();
        let request = crate::bots::BotMarketDataRequest {
            interval_ms: 1000,
            max_bars: 64,
        };
        let mut reference = None;
        for cached in [false, true, true, false, false, true, true, false] {
            let start = std::time::Instant::now();
            let mut batch = manager.observation_batch("cached-history");
            let mut views = Vec::new();
            for n in 0..100 {
                let account = [10, 20, 30, 999][n % 4];
                views.push(if cached {
                    batch
                        .bot_observation("V-BTC-SPOT", account, Some(request), &[])
                        .unwrap()
                } else {
                    manager
                        .bot_observation("cached-history", "V-BTC-SPOT", account, Some(request))
                        .unwrap()
                });
            }
            println!(
                "cached={cached} seconds={:.6}",
                start.elapsed().as_secs_f64()
            );
            if let Some(prior) = &reference {
                assert_eq!(&views, prior);
            } else {
                reference = Some(views);
            }
        }
    }

    #[test]
    fn exposes_execution_history_for_room_timeline() {
        let mut manager = RoomManager::new();
        manager.create_room(spot_scenario("room-1")).unwrap();

        assert_eq!(manager.execution_history("room-1").unwrap().len(), 1);

        manager
            .apply("room-1", limit(2, 20, Side::Buy, 100, 2))
            .unwrap();

        let history = manager.execution_history("room-1").unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].command_seq, 0);
        assert_eq!(history[1].command_seq, 1);
    }

    #[test]
    fn candidate_history_suffix_and_observations_preserve_original_and_recovery() {
        let room = "history-fork";
        let mut original = RoomManager::new();
        original.create_room(spot_scenario(room)).unwrap();
        // Cross multiple history chunks with real commands, including refusals.
        for order in 2..620 {
            original
                .apply(room, limit(order, 20, Side::Buy, 1, 1))
                .unwrap();
        }
        let previous = original.execution_history_len(room).unwrap();
        let before = original
            .participant_observation(room, "V-BTC-SPOT", 20)
            .unwrap();
        let mut candidate = original.clone();
        candidate
            .apply(room, limit(620, 20, Side::Buy, 100, 1))
            .unwrap();
        candidate
            .apply(room, limit(621, 20, Side::Buy, 100, 1))
            .unwrap();
        let suffix = candidate
            .execution_history_from(room, previous)
            .unwrap()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(suffix.len(), 2);
        assert_eq!(original.execution_history_len(room).unwrap(), previous);
        assert_eq!(
            original
                .participant_observation(room, "V-BTC-SPOT", 20)
                .unwrap(),
            before
        );
        let exported = candidate.execution_history(room).unwrap().to_vec();
        assert_eq!(suffix, exported[previous..]);
        let mut restored = RoomManager::new();
        restored
            .restore_simulation_room(candidate.simulation_room(room).unwrap().clone(), exported)
            .unwrap();
        assert_eq!(
            restored
                .execution_history_from(room, previous)
                .unwrap()
                .collect::<Vec<_>>(),
            candidate
                .execution_history_from(room, previous)
                .unwrap()
                .collect::<Vec<_>>()
        );
        assert_eq!(
            restored
                .participant_observation(room, "V-BTC-SPOT", 20)
                .unwrap(),
            candidate
                .participant_observation(room, "V-BTC-SPOT", 20)
                .unwrap()
        );
    }

    #[test]
    fn removing_room_clears_room_scoped_runtime_state() {
        let mut manager = RoomManager::new();
        manager.create_room(spot_scenario("room-1")).unwrap();
        manager
            .apply("room-1", limit(2, 20, Side::Buy, 100, 2))
            .unwrap();

        assert!(manager.remove_room("room-1"));
        assert!(!manager.remove_room("room-1"));
        assert!(matches!(
            manager.execution_history("room-1"),
            Err(RoomManagerError::RoomNotFound { .. })
        ));
    }

    #[test]
    fn restore_rebuilds_legacy_market_reservations_before_withdrawal() {
        let config =
            ExchangeConfig::new("binance", vec![venue_spot_market("binance", "btc-usdt")]).unwrap();
        let mut exchange = ExchangeActor::new("legacy-room", config).unwrap();
        exchange.create_account(20, 1_000);
        let execution = exchange
            .apply_to_instrument("btc-usdt", limit(1, 20, Side::Buy, 100, 6))
            .unwrap();
        assert!(matches!(
            execution.result,
            ActorExecutionResult::Accepted(MarketExecution::Spot(_))
        ));
        assert_eq!(
            exchange
                .venue_balance_snapshot(20, "USDT")
                .unwrap()
                .reserved,
            600
        );

        let room = SimulationRoom::from_exchange(exchange);
        let mut legacy_json = serde_json::to_value(room).unwrap();
        legacy_json
            .pointer_mut("/exchanges/binance")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("market_reservations");
        *legacy_json
            .pointer_mut("/exchanges/binance/venue_accounts/balances/20/USDT/reserved")
            .unwrap() = serde_json::json!(0);
        let legacy_room: SimulationRoom = serde_json::from_value(legacy_json).unwrap();
        assert_eq!(
            legacy_room
                .venue_account_snapshot(20)
                .balances
                .into_iter()
                .find(|balance| balance.asset_id == "USDT")
                .unwrap()
                .reserved,
            0
        );

        let mut manager = RoomManager::new();
        manager
            .restore_simulation_room(legacy_room, Vec::new())
            .unwrap();
        let restored_balance = manager
            .venue_account_snapshot("legacy-room", 20)
            .unwrap()
            .balances
            .into_iter()
            .find(|balance| balance.asset_id == "USDT")
            .unwrap();
        assert_eq!(restored_balance.total, 1_000);
        assert_eq!(restored_balance.reserved, 600);
        assert_eq!(restored_balance.available, 400);

        let withdrawal = manager
            .submit_withdrawal("legacy-room", None, 20, "USDT", 1_000)
            .unwrap();
        assert_eq!(withdrawal.status, VenueTransferStatus::Rejected);
        assert_eq!(
            withdrawal.reject_reason,
            Some(VenueTransferRejectReason::InsufficientAvailableBalance)
        );
        let balance_after = manager
            .venue_account_snapshot("legacy-room", 20)
            .unwrap()
            .balances
            .into_iter()
            .find(|balance| balance.asset_id == "USDT")
            .unwrap();
        assert_eq!(balance_after.total, 1_000);
        assert_eq!(balance_after.reserved, 600);
    }

    #[test]
    fn restore_market_actor_rebuilds_venue_balances_and_reservations() {
        let mut actor = MarketActor::new(
            "legacy-market-room",
            venue_spot_market("binance", "btc-usdt"),
        )
        .unwrap();
        actor.create_account(20, 1_000);
        let execution = actor.apply(limit(1, 20, Side::Buy, 100, 6));
        assert!(matches!(
            execution.result,
            ActorExecutionResult::Accepted(MarketExecution::Spot(_))
        ));
        assert!(matches!(
            actor.account_snapshot(20),
            Some(AccountSnapshot::Spot(crate::SpotAccountSnapshot {
                cash_balance: 1_000,
                reserved_cash: 600,
                ..
            }))
        ));

        let mut manager = RoomManager::new();
        manager.restore_room(actor, vec![execution]).unwrap();
        let restored_balance = manager
            .venue_account_snapshot("legacy-market-room", 20)
            .unwrap()
            .balances
            .into_iter()
            .find(|balance| balance.asset_id == "USDT")
            .unwrap();
        assert_eq!(restored_balance.total, 1_000);
        assert_eq!(restored_balance.reserved, 600);
        assert_eq!(restored_balance.available, 400);

        let withdrawal = manager
            .submit_withdrawal("legacy-market-room", None, 20, "USDT", 1_000)
            .unwrap();
        assert_eq!(withdrawal.status, VenueTransferStatus::Rejected);
        assert_eq!(
            withdrawal.reject_reason,
            Some(VenueTransferRejectReason::InsufficientAvailableBalance)
        );
        assert_eq!(
            manager.book_snapshot("legacy-market-room").unwrap().bids,
            vec![BookLevel {
                price_tick: 100,
                qty: 6,
            }]
        );
    }

    #[test]
    fn room_manager_records_perp_liquidation_execution() {
        let scenario = ScenarioConfig {
            room_id: "perp-liquidation-room".to_string(),
            market_events: Vec::new(),
            venue_preset: None,
            venue_rules: crate::VenueRuleConfig::default(),
            venue_asset_policy: crate::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: MarketConfig::Perp(PerpMarketConfig {
                instrument: InstrumentConfig::new("V-BTC-PERP", 1, 1).unwrap(),
                clearing: PerpClearingConfig {
                    leverage: 10,
                    maintenance_margin_ppm: 50_000,
                    ..PerpClearingConfig::default()
                },
                risk: PerpRiskConfig::default(),
                initial_mark_price_tick: 80,
                price_link: None,
                funding: None,
            }),
            extra_markets: Vec::new(),
            initial_portfolios: Vec::new(),
            initial_allocations: Vec::new(),
            routed_initial_allocations: Vec::new(),
            accounts: vec![
                ScenarioAccount::Basic {
                    account_id: 10,
                    cash_balance: 10_000,
                },
                ScenarioAccount::Basic {
                    account_id: 20,
                    cash_balance: 200,
                },
                ScenarioAccount::Basic {
                    account_id: 30,
                    cash_balance: 10_000,
                },
            ],
            seed_orders: Vec::new(),
            routed_seed_orders: Vec::new(),
        };
        let mut manager = RoomManager::new();
        manager.create_room(scenario).unwrap();

        manager
            .apply("perp-liquidation-room", limit(1, 10, Side::Sell, 100, 10))
            .unwrap();
        manager
            .apply("perp-liquidation-room", limit(3, 30, Side::Buy, 80, 10))
            .unwrap();
        manager
            .apply(
                "perp-liquidation-room",
                Command::NewOrder(NewOrder {
                    position_side: crate::model::PositionSide::Both,
                    order_id: 2,
                    account_id: 20,
                    side: Side::Buy,
                    kind: OrderKind::Market,
                    qty: 10,
                    reduce_only: false,
                }),
            )
            .unwrap();

        let history = manager.execution_history("perp-liquidation-room").unwrap();
        assert_eq!(history.len(), 4);
        let ActorExecutionResult::Accepted(MarketExecution::Perp(result)) =
            history.last().unwrap().result.clone()
        else {
            panic!("expected accepted perp liquidation");
        };
        assert_eq!(result.clearing_events.len(), 2);
        assert!(matches!(
            manager
                .account_snapshot("perp-liquidation-room", 20)
                .unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                position_qty: 0,
                margin_status: PerpMarginStatus::Flat,
                ..
            }))
        ));
    }

    #[test]
    fn room_manager_retains_and_explicitly_advances_pending_liquidation() {
        let scenario = pending_perp_liquidation_scenario("perp-retry-room");
        let mut manager = RoomManager::new();
        manager.create_room(scenario).unwrap();
        manager
            .apply("perp-retry-room", limit(1, 10, Side::Sell, 100, 10))
            .unwrap();
        manager
            .apply(
                "perp-retry-room",
                Command::NewOrder(NewOrder {
                    position_side: crate::model::PositionSide::Both,
                    order_id: 2,
                    account_id: 20,
                    side: Side::Buy,
                    kind: OrderKind::Market,
                    qty: 10,
                    reduce_only: false,
                }),
            )
            .unwrap();
        manager
            .apply(
                "perp-retry-room",
                Command::SetMarkPrice(SetMarkPrice { price_tick: 80 }),
            )
            .unwrap();

        let history = manager.execution_history("perp-retry-room").unwrap();
        let ActorExecutionResult::Accepted(MarketExecution::Perp(empty_attempt)) =
            &history.last().unwrap().result
        else {
            panic!("expected accepted no-depth liquidation attempt");
        };
        assert!(empty_attempt.events.iter().any(|event| matches!(
            event.event,
            Event::OrderExpired {
                unfilled_qty: 10,
                ..
            }
        )));
        assert_eq!(manager.pending_liquidation_count("perp-retry-room"), Ok(1));
        assert!(matches!(
            manager.account_snapshot("perp-retry-room", 20).unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                position_qty: 10,
                margin_status: PerpMarginStatus::Liquidatable,
                ..
            }))
        ));

        let before_explicit_retry = history.len();
        let retries = manager
            .advance_pending_liquidations("perp-retry-room", 1)
            .unwrap();
        assert_eq!(retries.len(), 1);
        assert_eq!(
            manager.execution_history("perp-retry-room").unwrap().len(),
            before_explicit_retry + 1
        );
        assert_eq!(manager.pending_liquidation_count("perp-retry-room"), Ok(1));

        let before_clock_retry = manager.execution_history("perp-retry-room").unwrap().len();
        manager.advance_clock("perp-retry-room", 1).unwrap();
        assert_eq!(
            manager.execution_history("perp-retry-room").unwrap().len(),
            before_clock_retry + 1
        );
        assert_eq!(manager.pending_liquidation_count("perp-retry-room"), Ok(1));

        manager
            .apply("perp-retry-room", limit(3, 30, Side::Buy, 80, 10))
            .unwrap();

        let history = manager.execution_history("perp-retry-room").unwrap();
        let ActorExecutionResult::Accepted(MarketExecution::Perp(liquidation)) =
            &history.last().unwrap().result
        else {
            panic!("expected successful retried liquidation");
        };
        assert!(
            liquidation
                .events
                .iter()
                .any(|event| matches!(event.event, Event::TradePrinted(_)))
        );
        assert!(matches!(
            manager.account_snapshot("perp-retry-room", 20).unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                position_qty: 0,
                margin_status: PerpMarginStatus::Flat,
                ..
            }))
        ));
        assert_eq!(manager.pending_liquidation_count("perp-retry-room"), Ok(0));
    }

    #[test]
    fn closed_room_discards_pending_liquidations_without_consuming_sequence() {
        let room_id = "closed-perp-pending-room";
        let mut manager = RoomManager::new();
        manager
            .create_room(pending_perp_liquidation_scenario(room_id))
            .unwrap();
        manager
            .apply(room_id, limit(1, 10, Side::Sell, 100, 10))
            .unwrap();
        manager
            .apply(
                room_id,
                Command::NewOrder(NewOrder {
                    position_side: crate::model::PositionSide::Both,
                    order_id: 2,
                    account_id: 20,
                    side: Side::Buy,
                    kind: OrderKind::Market,
                    qty: 10,
                    reduce_only: false,
                }),
            )
            .unwrap();
        manager
            .apply(
                room_id,
                Command::SetMarkPrice(SetMarkPrice { price_tick: 80 }),
            )
            .unwrap();
        assert_eq!(manager.pending_liquidation_count(room_id), Ok(1));

        manager.close_room(room_id).unwrap();
        let cursor_before = manager.simulation_room(room_id).unwrap().next_command_seq();
        let history_len_before = manager.execution_history(room_id).unwrap().len();

        assert!(
            manager
                .advance_pending_liquidations(room_id, usize::MAX)
                .unwrap()
                .is_empty()
        );
        assert_eq!(manager.pending_liquidation_count(room_id), Ok(0));
        assert_eq!(
            manager.simulation_room(room_id).unwrap().next_command_seq(),
            cursor_before
        );
        assert_eq!(
            manager.execution_history(room_id).unwrap().len(),
            history_len_before
        );
    }

    #[test]
    fn restored_room_rebuilds_pending_liquidation_queue_and_can_resume() {
        let room_id = "perp-restore-pending-room";
        let mut manager = RoomManager::new();
        manager
            .create_room(pending_perp_liquidation_scenario(room_id))
            .unwrap();
        manager
            .apply(room_id, limit(1, 10, Side::Sell, 100, 10))
            .unwrap();
        manager
            .apply(
                room_id,
                Command::NewOrder(NewOrder {
                    position_side: crate::model::PositionSide::Both,
                    order_id: 2,
                    account_id: 20,
                    side: Side::Buy,
                    kind: OrderKind::Market,
                    qty: 10,
                    reduce_only: false,
                }),
            )
            .unwrap();
        manager
            .apply(
                room_id,
                Command::SetMarkPrice(SetMarkPrice { price_tick: 80 }),
            )
            .unwrap();
        assert_eq!(manager.pending_liquidation_count(room_id), Ok(1));

        let snapshot_json =
            serde_json::to_string(manager.simulation_room(room_id).unwrap()).unwrap();
        let restored_room: SimulationRoom = serde_json::from_str(&snapshot_json).unwrap();
        let restored_cursor = restored_room.next_command_seq();
        let mut restored = RoomManager::new();
        restored
            // A durable checkpoint can be restored without replay history.
            // System order ids must still continue from the persisted cursor.
            .restore_simulation_room(restored_room, Vec::new())
            .unwrap();
        assert_eq!(
            restored.pending_liquidations(room_id).unwrap(),
            vec![PendingRoomLiquidation {
                instrument_id: "V-BTC-PERP".to_string(),
                account_id: 20,
            }]
        );

        let no_depth_retry = restored.advance_pending_liquidations(room_id, 1).unwrap();
        assert_eq!(no_depth_retry.len(), 1);
        assert_eq!(no_depth_retry[0].command_seq, restored_cursor);
        let ActorExecutionResult::Accepted(MarketExecution::Perp(retry)) =
            &no_depth_retry[0].result
        else {
            panic!("expected accepted no-depth liquidation retry");
        };
        let Command::NewOrder(retry_order) = &retry.command.command else {
            panic!("liquidation retry must be recorded as a new order");
        };
        assert_eq!(
            retry_order.order_id,
            system_liquidation_order_id(restored_cursor, 0).unwrap()
        );
        assert_eq!(restored.pending_liquidation_count(room_id), Ok(1));

        let liquidity =
            restored
                .simulation_room_mut(room_id)
                .unwrap()
                .apply(limit(3, 30, Side::Buy, 80, 10));
        assert!(matches!(
            liquidity.result,
            ActorExecutionResult::Accepted(MarketExecution::Perp(_))
        ));
        let completed = restored.advance_pending_liquidations(room_id, 1).unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(restored.pending_liquidation_count(room_id), Ok(0));
        assert!(matches!(
            restored.account_snapshot(room_id, 20).unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                position_qty: 0,
                margin_status: PerpMarginStatus::Flat,
                ..
            }))
        ));
    }

    #[test]
    fn cross_margin_liquidation_journals_peer_cancel_and_skips_flat_peer_queue() {
        let room_id = "cross-margin-pending-room";
        let mut manager = RoomManager::new();
        manager
            .create_room(pending_cross_margin_liquidation_scenario(room_id))
            .unwrap();
        manager
            .apply_to_instrument(room_id, "V-BTC-PERP", limit(1, 10, Side::Sell, 100, 10))
            .unwrap();
        manager
            .apply_to_instrument(
                room_id,
                "V-BTC-PERP",
                Command::NewOrder(NewOrder {
                    position_side: crate::model::PositionSide::Both,
                    order_id: 2,
                    account_id: 20,
                    side: Side::Buy,
                    kind: OrderKind::Market,
                    qty: 10,
                    reduce_only: false,
                }),
            )
            .unwrap();
        manager
            .apply_to_instrument(room_id, "V-ETH-PERP", limit(3, 20, Side::Buy, 100, 1))
            .unwrap();
        assert_eq!(
            manager
                .book_snapshot_for(room_id, "V-ETH-PERP")
                .unwrap()
                .bids[0]
                .qty,
            1
        );

        let mark = manager
            .apply_to_instrument(
                room_id,
                "V-BTC-PERP",
                Command::SetMarkPrice(SetMarkPrice { price_tick: 80 }),
            )
            .unwrap();
        assert_eq!(mark.command_seq, 3);
        assert!(
            manager
                .book_snapshot_for(room_id, "V-ETH-PERP")
                .unwrap()
                .bids
                .is_empty()
        );
        assert_eq!(
            manager.pending_liquidations(room_id).unwrap(),
            vec![PendingRoomLiquidation {
                instrument_id: "V-BTC-PERP".to_string(),
                account_id: 20,
            }]
        );

        let history = manager.execution_history(room_id).unwrap();
        assert_eq!(history.len(), 6);
        assert_eq!(history[4].instrument_id, "V-ETH-PERP");
        let ActorExecutionResult::Accepted(MarketExecution::Perp(peer_cancel)) = &history[4].result
        else {
            panic!("peer cancellation must be a journal-visible perp execution");
        };
        assert!(matches!(
            peer_cancel.command.command,
            Command::CancelOrder(CancelOrder { order_id: 3 })
        ));
        assert!(
            peer_cancel
                .events
                .iter()
                .any(|event| matches!(event.event, Event::OrderCanceled { order_id: 3, .. }))
        );
        assert_eq!(history[5].instrument_id, "V-BTC-PERP");

        let blocked = manager
            .apply_to_instrument(room_id, "V-ETH-PERP", limit(4, 20, Side::Buy, 90, 1))
            .unwrap();
        let ActorExecutionResult::Accepted(MarketExecution::Perp(blocked)) = blocked.result else {
            panic!("cross-market pending freeze must be an auditable risk rejection");
        };
        assert!(blocked.events.iter().any(|event| matches!(
            event.event,
            Event::RiskRejected {
                order_id: 4,
                reason: crate::model::RiskRejectReason::InsufficientMargin,
            }
        )));

        let history = manager.execution_history(room_id).unwrap();
        assert_eq!(
            history
                .iter()
                .map(|execution| execution.command_seq)
                .collect::<Vec<_>>(),
            (0..u64::try_from(history.len()).unwrap()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn collateral_spot_orders_are_canceled_before_liquidation_on_both_asset_sides() {
        let cases = [
            (
                "spot-quote-collateral-cure",
                "BTC",
                "USDT",
                Side::Buy,
                100,
                1,
            ),
            (
                "spot-base-collateral-cure",
                "USDT",
                "BTC",
                Side::Sell,
                1,
                100,
            ),
        ];

        for (room_id, spot_base, spot_quote, side, price_tick, qty) in cases {
            let exchange = distressed_perp_with_spot_exchange(room_id, spot_base, spot_quote);
            let mut manager = RoomManager::new();
            manager.restore_exchange_room(exchange, Vec::new()).unwrap();
            let cursor = manager.simulation_room(room_id).unwrap().next_command_seq();

            let user_execution = manager
                .apply_to_instrument(
                    room_id,
                    "V-COLLATERAL-SPOT",
                    limit(100, 20, side, price_tick, qty),
                )
                .unwrap();
            assert_eq!(user_execution.command_seq, cursor);

            let history = manager.execution_history(room_id).unwrap();
            assert_eq!(history.len(), 2);
            assert_eq!(history[0].command_seq, cursor);
            assert_eq!(history[1].command_seq, cursor + 1);
            assert_eq!(history[1].instrument_id, "V-COLLATERAL-SPOT");
            let ActorExecutionResult::Accepted(MarketExecution::Spot(cancel)) = &history[1].result
            else {
                panic!("collateral release must be a top-level spot cancellation");
            };
            assert!(matches!(
                cancel.command.command,
                Command::CancelOrder(CancelOrder { order_id: 100 })
            ));
            assert!(
                cancel
                    .events
                    .iter()
                    .any(|event| matches!(event.event, Event::OrderCanceled { order_id: 100, .. }))
            );
            assert!(
                manager
                    .book_snapshot_for(room_id, "V-COLLATERAL-SPOT")
                    .unwrap()
                    .bids
                    .is_empty()
            );
            assert!(
                manager
                    .book_snapshot_for(room_id, "V-COLLATERAL-SPOT")
                    .unwrap()
                    .asks
                    .is_empty()
            );
            assert!(matches!(
                manager
                    .account_snapshot_for(room_id, "V-ETH-PERP", 20)
                    .unwrap(),
                Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                    cash_balance: 200,
                    equity: 140,
                    margin_status: PerpMarginStatus::Healthy,
                    ..
                }))
            ));
            assert_eq!(manager.pending_liquidation_count(room_id), Ok(0));
        }
    }

    #[test]
    fn completed_spot_fill_immediately_triggers_journaled_perp_liquidation() {
        let room_id = "spot-fill-perp-liquidation";
        let exchange = distressed_perp_with_spot_exchange(room_id, "BTC", "USDT");
        let mut manager = RoomManager::new();
        manager.restore_exchange_room(exchange, Vec::new()).unwrap();

        manager
            .apply_to_instrument(
                room_id,
                "V-COLLATERAL-SPOT",
                limit(100, 30, Side::Sell, 100, 1),
            )
            .unwrap();
        manager
            .apply_to_instrument(room_id, "V-ETH-PERP", limit(101, 40, Side::Buy, 94, 10))
            .unwrap();
        let history_len_before = manager.execution_history(room_id).unwrap().len();

        let user_execution = manager
            .apply_to_instrument(
                room_id,
                "V-COLLATERAL-SPOT",
                limit(102, 20, Side::Buy, 100, 1),
            )
            .unwrap();

        let history = manager.execution_history(room_id).unwrap();
        let appended = &history[history_len_before..];
        assert_eq!(appended.len(), 2);
        assert_eq!(appended[0], user_execution);
        assert_eq!(appended[1].command_seq, user_execution.command_seq + 1);
        assert_eq!(appended[1].instrument_id, "V-ETH-PERP");
        let ActorExecutionResult::Accepted(MarketExecution::Spot(spot_fill)) = &appended[0].result
        else {
            panic!("expected accepted collateral-consuming spot fill");
        };
        assert!(
            spot_fill
                .events
                .iter()
                .any(|event| matches!(event.event, Event::TradePrinted(_)))
        );
        let ActorExecutionResult::Accepted(MarketExecution::Perp(liquidation)) =
            &appended[1].result
        else {
            panic!("expected accepted automatic perp liquidation");
        };
        assert!(
            liquidation
                .events
                .iter()
                .any(|event| matches!(event.event, Event::TradePrinted(_)))
        );
        assert!(matches!(
            manager
                .account_snapshot_for(room_id, "V-ETH-PERP", 20)
                .unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                position_qty: 0,
                margin_status: PerpMarginStatus::Flat,
                ..
            }))
        ));
        assert_eq!(manager.pending_liquidation_count(room_id), Ok(0));
    }

    #[test]
    fn rejects_duplicate_room_ids() {
        let mut manager = RoomManager::new();
        manager.create_room(spot_scenario("room-1")).unwrap();

        assert_eq!(
            manager.create_room(spot_scenario("room-1")).map(|_| ()),
            Err(RoomManagerError::RoomAlreadyExists {
                room_id: "room-1".to_string(),
            })
        );
    }

    #[test]
    fn returns_not_found_for_unknown_room() {
        let mut manager = RoomManager::new();

        assert_eq!(
            manager
                .apply("missing", limit(1, 20, Side::Buy, 100, 1))
                .map(|_| ()),
            Err(RoomManagerError::RoomNotFound {
                room_id: "missing".to_string(),
            })
        );
    }

    #[test]
    fn controls_room_lifecycle() {
        let mut manager = RoomManager::new();
        manager.create_room(spot_scenario("room-1")).unwrap();

        manager.pause_room("room-1").unwrap();
        assert_eq!(manager.status("room-1"), Ok(MarketStatus::Paused));
        let paused = manager
            .apply("room-1", limit(2, 20, Side::Buy, 100, 1))
            .unwrap();
        assert_eq!(
            paused.result,
            ActorExecutionResult::Rejected(ActorRejectReason::MarketPaused)
        );

        manager.resume_room("room-1").unwrap();
        assert_eq!(manager.status("room-1"), Ok(MarketStatus::Running));

        manager.close_room("room-1").unwrap();
        assert_eq!(manager.status("room-1"), Ok(MarketStatus::Closed));
        let closed = manager
            .apply("room-1", limit(3, 20, Side::Buy, 100, 1))
            .unwrap();
        assert_eq!(
            closed.result,
            ActorExecutionResult::Rejected(ActorRejectReason::MarketClosed)
        );
    }

    #[test]
    fn lists_room_ids_in_stable_order() {
        let mut manager = RoomManager::new();
        manager.create_room(spot_scenario("room-b")).unwrap();
        manager.create_room(spot_scenario("room-a")).unwrap();

        assert_eq!(manager.room_ids(), vec!["room-a", "room-b"]);
    }

    #[test]
    fn room_routes_instruments_across_venues_and_tracks_global_assets() {
        let mut balances = BTreeMap::new();
        balances.insert("USDT".to_string(), 1_000);
        let scenario = ScenarioConfig {
            room_id: "multi-venue-room".to_string(),
            market_events: Vec::new(),
            venue_preset: None,
            venue_rules: crate::VenueRuleConfig::default(),
            venue_asset_policy: crate::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: venue_spot_market("binance", "binance:btc-usdt:spot"),
            extra_markets: vec![venue_spot_market("okx", "okx:btc-usdt:spot")],
            initial_portfolios: vec![ScenarioPortfolio {
                account_id: 20,
                balances,
            }],
            initial_allocations: Vec::new(),
            routed_initial_allocations: vec![
                ScenarioVenueAllocation {
                    venue_id: "binance".to_string(),
                    account_id: 20,
                    asset_id: "USDT".to_string(),
                    amount: 400,
                },
                ScenarioVenueAllocation {
                    venue_id: "okx".to_string(),
                    account_id: 20,
                    asset_id: "USDT".to_string(),
                    amount: 300,
                },
            ],
            accounts: vec![ScenarioAccount::Basic {
                account_id: 20,
                cash_balance: 0,
            }],
            seed_orders: Vec::new(),
            routed_seed_orders: Vec::new(),
        };
        let mut manager = RoomManager::new();
        manager.create_room(scenario).unwrap();

        let venues = manager
            .venue_account_snapshots_by_venue("multi-venue-room")
            .unwrap();
        let venue_balance = |venue_id: &str| {
            venues
                .iter()
                .find(|snapshot| snapshot.venue_id == venue_id)
                .and_then(|snapshot| {
                    snapshot
                        .account
                        .balances
                        .iter()
                        .find(|balance| balance.account_id == 20 && balance.asset_id == "USDT")
                })
                .map(|balance| balance.total)
                .unwrap()
        };
        assert_eq!(venue_balance("binance"), 400);
        assert_eq!(venue_balance("okx"), 300);

        let wallet = manager.portfolio_snapshot("multi-venue-room", 20).unwrap();
        assert_eq!(wallet.balances[0].total, 300);

        let execution = manager
            .apply_to_instrument(
                "multi-venue-room",
                "okx:btc-usdt:spot",
                limit(1, 20, Side::Buy, 100, 1),
            )
            .unwrap();
        assert_eq!(execution.instrument_id, "okx:btc-usdt:spot");
        assert_eq!(execution.command_seq, 0);

        let primary_execution = manager
            .apply_to_instrument(
                "multi-venue-room",
                "binance:btc-usdt:spot",
                limit(2, 20, Side::Buy, 90, 1),
            )
            .unwrap();
        assert_eq!(primary_execution.command_seq, 1);

        let net_worth = manager.net_worth_snapshot("multi-venue-room").unwrap();
        let usdt = net_worth.accounts[0]
            .assets
            .iter()
            .find(|asset| asset.asset_id == "USDT")
            .unwrap();
        assert_eq!(usdt.portfolio_total, 300);
        assert_eq!(usdt.venue_total, 700);
        assert_eq!(usdt.total, 1_000);

        // Legacy room snapshots had only per-exchange cursors. Removing the
        // room-global cursor exercises checked recovery across both venues.
        let mut serialized = serde_json::to_value(
            manager
                .simulation_room("multi-venue-room")
                .expect("room should exist"),
        )
        .unwrap();
        serialized
            .as_object_mut()
            .unwrap()
            .remove("next_command_seq");
        let restored_room: SimulationRoom = serde_json::from_value(serialized).unwrap();
        let history = manager
            .execution_history("multi-venue-room")
            .unwrap()
            .to_vec();
        let mut restored = RoomManager::new();
        restored
            .restore_simulation_room(restored_room, history)
            .unwrap();
        let continued = restored
            .apply_to_instrument(
                "multi-venue-room",
                "okx:btc-usdt:spot",
                limit(3, 20, Side::Buy, 80, 1),
            )
            .unwrap();
        assert_eq!(continued.command_seq, 2);
    }

    #[test]
    fn completed_transfers_immediately_refresh_perp_equity_and_restored_state() {
        let delayed_room_id = "delayed-perp-transfer-room";
        let mut manager = RoomManager::new();
        manager
            .create_room(perp_transfer_sync_scenario(delayed_room_id, 1))
            .unwrap();

        let pending_deposit = manager
            .submit_deposit(delayed_room_id, None, 20, "USDT", 50)
            .unwrap();
        assert_eq!(pending_deposit.status, crate::VenueTransferStatus::Pending);
        assert!(matches!(
            manager.account_snapshot(delayed_room_id, 20).unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                equity: 200,
                ..
            }))
        ));
        manager.advance_clock(delayed_room_id, 1).unwrap();
        assert!(matches!(
            manager.account_snapshot(delayed_room_id, 20).unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                cash_balance: 250,
                equity: 250,
                ..
            }))
        ));

        let pending_withdrawal = manager
            .submit_withdrawal(delayed_room_id, None, 20, "USDT", 50)
            .unwrap();
        assert_eq!(
            pending_withdrawal.status,
            crate::VenueTransferStatus::Pending
        );
        assert!(matches!(
            manager.account_snapshot(delayed_room_id, 20).unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                cash_balance: 200,
                equity: 200,
                available_cash: 200,
                ..
            }))
        ));
        let blocked_during_pending_withdrawal = manager
            .apply_to_instrument(
                delayed_room_id,
                "V-BTC-PERP",
                limit(99, 20, Side::Buy, 100, 21),
            )
            .unwrap();
        let ActorExecutionResult::Accepted(MarketExecution::Perp(blocked)) =
            blocked_during_pending_withdrawal.result
        else {
            panic!("pending withdrawal should reach perp risk as reduced collateral");
        };
        assert!(blocked.events.iter().any(|event| matches!(
            event.event,
            Event::RiskRejected {
                order_id: 99,
                reason: crate::model::RiskRejectReason::InsufficientMargin,
            }
        )));
        manager.advance_clock(delayed_room_id, 1).unwrap();
        assert!(matches!(
            manager.account_snapshot(delayed_room_id, 20).unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                cash_balance: 200,
                equity: 200,
                ..
            }))
        ));

        let snapshot_json = serde_json::to_string(
            manager
                .simulation_room(delayed_room_id)
                .expect("room should exist"),
        )
        .unwrap();
        let restored_room: SimulationRoom = serde_json::from_str(&snapshot_json).unwrap();
        let mut restored = RoomManager::new();
        restored
            .restore_simulation_room(restored_room, Vec::new())
            .unwrap();
        assert!(matches!(
            restored.account_snapshot(delayed_room_id, 20).unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                cash_balance: 200,
                equity: 200,
                ..
            }))
        ));

        let immediate_room_id = "immediate-perp-transfer-room";
        let mut immediate = RoomManager::new();
        immediate
            .create_room(perp_transfer_sync_scenario(immediate_room_id, 0))
            .unwrap();
        assert_eq!(
            immediate
                .submit_deposit(immediate_room_id, None, 20, "USDT", 50)
                .unwrap()
                .status,
            crate::VenueTransferStatus::Completed
        );
        assert!(matches!(
            immediate.account_snapshot(immediate_room_id, 20).unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                equity: 250,
                ..
            }))
        ));
        assert_eq!(
            immediate
                .submit_withdrawal(immediate_room_id, None, 20, "USDT", 50)
                .unwrap()
                .status,
            crate::VenueTransferStatus::Completed
        );
        assert!(matches!(
            immediate.account_snapshot(immediate_room_id, 20).unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                equity: 200,
                ..
            }))
        ));
    }

    #[test]
    fn cross_venue_transfer_timing_is_independent_of_venue_sort_order() {
        for (room_id, source_venue_id, destination_venue_id) in [
            ("source-sorts-first", "a-source", "z-destination"),
            ("source-sorts-last", "z-source", "a-destination"),
        ] {
            let mut manager = RoomManager::new();
            manager
                .create_room(cross_venue_transfer_scenario(
                    room_id,
                    source_venue_id,
                    destination_venue_id,
                ))
                .unwrap();
            let submitted = manager
                .submit_venue_to_venue_transfer(
                    room_id,
                    source_venue_id,
                    destination_venue_id,
                    20,
                    "USDT",
                    100,
                )
                .unwrap();
            assert_eq!(submitted.withdrawal.status, VenueTransferStatus::Pending);

            let first_step = manager.advance_clock(room_id, 1).unwrap();
            assert!(first_step.iter().any(|transfer| {
                transfer.kind == crate::VenueTransferKind::Withdrawal
                    && transfer.status == VenueTransferStatus::Completed
            }));
            assert!(first_step.iter().any(|transfer| {
                transfer.kind == crate::VenueTransferKind::Deposit
                    && transfer.status == VenueTransferStatus::Pending
            }));
            assert_eq!(
                venue_asset_total(&manager, room_id, destination_venue_id, "USDT"),
                0
            );

            let second_step = manager.advance_clock(room_id, 1).unwrap();
            assert!(second_step.iter().any(|transfer| {
                transfer.kind == crate::VenueTransferKind::Deposit
                    && transfer.status == VenueTransferStatus::Completed
                    && transfer.completed_at_step == Some(2)
            }));
            assert_eq!(
                venue_asset_total(&manager, room_id, destination_venue_id, "USDT"),
                100
            );
        }
    }

    #[test]
    fn batched_clock_advance_matches_repeated_single_steps() {
        let room_id = "batched-cross-venue-clock";
        let source_venue_id = "z-source";
        let destination_venue_id = "a-destination";
        let mut initial = RoomManager::new();
        initial
            .create_room(cross_venue_transfer_scenario(
                room_id,
                source_venue_id,
                destination_venue_id,
            ))
            .unwrap();
        initial
            .submit_venue_to_venue_transfer(
                room_id,
                source_venue_id,
                destination_venue_id,
                20,
                "USDT",
                100,
            )
            .unwrap();

        let mut batched = initial.clone();
        let batched_transfers = batched.advance_clock(room_id, 2).unwrap();
        let mut repeated = initial;
        let mut repeated_transfers = repeated.advance_clock(room_id, 1).unwrap();
        repeated_transfers.extend(repeated.advance_clock(room_id, 1).unwrap());

        assert_eq!(batched_transfers, repeated_transfers);
        assert_eq!(
            serde_json::to_value(batched.simulation_room(room_id).unwrap()).unwrap(),
            serde_json::to_value(repeated.simulation_room(room_id).unwrap()).unwrap()
        );
    }

    #[test]
    fn room_asset_policy_accepts_virtual_assets_by_tags_and_listing_rules() {
        let mut balances = BTreeMap::new();
        balances.insert("PENGUIN".to_string(), 1_000);
        let scenario = ScenarioConfig {
            room_id: "penguin-room".to_string(),
            market_events: Vec::new(),
            venue_preset: None,
            venue_rules: crate::VenueRuleConfig::default(),
            venue_asset_policy: crate::VenueAssetPolicyConfig {
                deposit_rules: vec![AssetSelector::TagsAll {
                    tags: ["penguin-fiat"].into_iter().map(str::to_string).collect(),
                }],
                withdrawal_rules: vec![AssetSelector::TagsAll {
                    tags: ["penguin-fiat"].into_iter().map(str::to_string).collect(),
                }],
                settlement_rules: vec![AssetSelector::AssetIds {
                    asset_ids: ["PENGUIN"].into_iter().map(str::to_string).collect(),
                }],
                margin_rules: vec![AssetSelector::ListedOnVenue {
                    venue_id: "penguin-exchange".to_string(),
                }],
                ..crate::VenueAssetPolicyConfig::default()
            },
            assets: vec![
                crate::AssetConfig {
                    asset_id: "PENGUIN".to_string(),
                    kind: AssetKind::Fiat,
                    tags: ["virtual", "penguin-fiat"]
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                    issuer: Some("penguin-admin".to_string()),
                    native_venue: Some("penguin-exchange".to_string()),
                    listed_venues: ["penguin-exchange"]
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                },
                crate::AssetConfig {
                    asset_id: "PENG".to_string(),
                    kind: AssetKind::Equity,
                    tags: ["penguin-equity"].into_iter().map(str::to_string).collect(),
                    issuer: Some("penguin-inc".to_string()),
                    native_venue: None,
                    listed_venues: ["penguin-exchange"]
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                },
            ],
            market: MarketConfig::Spot(SpotMarketConfig {
                instrument: InstrumentConfig::new_for_venue(
                    "penguin-exchange",
                    "penguin:peng-penguin:spot",
                    "PENG",
                    "PENGUIN",
                    "PENG-PENGUIN Spot",
                    1,
                    1,
                )
                .unwrap(),
                clearing: SpotClearingConfig::default(),
                risk: SpotRiskConfig::default(),
            }),
            extra_markets: Vec::new(),
            initial_portfolios: vec![ScenarioPortfolio {
                account_id: 20,
                balances,
            }],
            initial_allocations: vec![ScenarioAllocation {
                account_id: 20,
                asset_id: "PENGUIN".to_string(),
                amount: 400,
            }],
            routed_initial_allocations: Vec::new(),
            accounts: vec![ScenarioAccount::Basic {
                account_id: 20,
                cash_balance: 0,
            }],
            seed_orders: Vec::new(),
            routed_seed_orders: Vec::new(),
        };

        let mut manager = RoomManager::new();
        manager.create_room(scenario).unwrap();
        assert_eq!(
            manager
                .venue_account_snapshot("penguin-room", 20)
                .unwrap()
                .balances
                .iter()
                .find(|balance| balance.asset_id == "PENGUIN")
                .unwrap()
                .total,
            400
        );
    }
}
