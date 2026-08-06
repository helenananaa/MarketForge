use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::{
    account::{Money, VenueAccountSnapshot},
    actor::{
        AccountSnapshot, AccountSnapshots, ActorExecution, ActorExecutionResult, ActorRejectReason,
        ExchangeActor, MarketActor, MarketExecution, MarketStatus, RoomId,
    },
    clock::SimulationClock,
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
    executions: BTreeMap<RoomId, Vec<ActorExecution>>,
    pending_liquidations: BTreeMap<RoomId, VecDeque<PendingRoomLiquidation>>,
}

impl RoomManager {
    pub fn new() -> Self {
        Self::default()
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
            .insert(room_id.clone(), seed_executions.clone());
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
        room.normalize_after_restore()
            .map_err(RoomManagerError::Actor)?;
        self.rooms.insert(room_id.clone(), room);
        self.executions.insert(room_id.clone(), executions);
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
        room.normalize_after_restore()
            .map_err(RoomManagerError::Actor)?;
        self.rooms.insert(room_id.clone(), room);
        self.executions.insert(room_id.clone(), executions);
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

        room.normalize_after_restore()
            .map_err(RoomManagerError::Actor)?;
        self.rooms.insert(room_id.clone(), room);
        self.executions.insert(room_id.clone(), executions);
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
        let execution = self
            .simulation_room_mut(room_id)?
            .apply_to_instrument(instrument_id, command)
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
            let accounts = room
                .account_snapshots_for(instrument_id)
                .map_err(RoomManagerError::Actor)?;
            if let AccountSnapshots::Perp(accounts) = accounts {
                account_ids.extend(
                    accounts
                        .into_iter()
                        .filter(|account| {
                            account.position_qty != 0
                                && account.margin_status == crate::PerpMarginStatus::Liquidatable
                        })
                        .map(|account| account.account_id),
                );
            }
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
        let removed = self.rooms.remove(room_id).is_some();
        self.executions.remove(room_id);
        self.pending_liquidations.remove(room_id);
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

    pub fn advance_clock(
        &mut self,
        room_id: &str,
        steps: u64,
    ) -> Result<Vec<VenueTransfer>, RoomManagerError> {
        let transfers = self
            .simulation_room_mut(room_id)
            .map(|room| room.advance_clock(steps))?
            .map_err(RoomManagerError::Simulation)?;
        self.advance_pending_liquidations(room_id, usize::MAX)?;
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
            .map(Vec::as_slice)
            .unwrap_or_default())
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

    fn spot_scenario(room_id: &str) -> ScenarioConfig {
        ScenarioConfig {
            room_id: room_id.to_string(),
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
            })
        };
        ScenarioConfig {
            room_id: room_id.to_string(),
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
