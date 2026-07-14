use std::collections::BTreeMap;

use crate::{
    account::{Money, VenueAccountSnapshot},
    actor::{
        AccountSnapshot, AccountSnapshots, ActorExecution, ActorExecutionResult, ActorRejectReason,
        ExchangeActor, MarketActor, MarketExecution, MarketStatus, RoomId,
    },
    clock::SimulationClock,
    model::{AccountId, BookSnapshot, Command, OrderId},
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
        self.executions.insert(room_id, executions);
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
        self.executions.insert(room_id, executions);
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
        self.executions.insert(room_id, executions);
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
        let execution = self
            .simulation_room_mut(room_id)?
            .liquidate_account(instrument_id, account_id, order_id)
            .map_err(RoomManagerError::Actor)?;
        self.record_execution_and_auto_liquidate(room_id, execution.clone(), false)?;
        Ok(execution)
    }

    fn record_execution_and_auto_liquidate(
        &mut self,
        room_id: &str,
        execution: ActorExecution,
        scan_liquidatable_accounts: bool,
    ) -> Result<(), RoomManagerError> {
        let mut pending = vec![(execution, scan_liquidatable_accounts)];
        let mut liquidation_index = 0;

        while let Some((execution, should_scan)) = pending.pop() {
            let triggers = if should_scan {
                self.liquidatable_accounts_for_execution(room_id, &execution)?
            } else {
                Vec::new()
            };
            self.executions
                .entry(room_id.to_string())
                .or_default()
                .push(execution.clone());

            for trigger in triggers {
                let order_id =
                    system_liquidation_order_id(execution.command_seq, liquidation_index)?;
                liquidation_index += 1;
                let liquidation = self
                    .simulation_room_mut(room_id)?
                    .liquidate_account(&trigger.instrument_id, trigger.account_id, order_id)
                    .map_err(RoomManagerError::Actor)?;
                pending.push((liquidation, false));
            }
        }

        Ok(())
    }

    fn liquidatable_accounts_for_execution(
        &self,
        room_id: &str,
        execution: &ActorExecution,
    ) -> Result<Vec<LiquidationTrigger>, RoomManagerError> {
        if !matches!(
            execution.result,
            ActorExecutionResult::Accepted(MarketExecution::Perp(_))
        ) {
            return Ok(Vec::new());
        }
        let accounts = self
            .simulation_room(room_id)?
            .account_snapshots_for(&execution.instrument_id)
            .map_err(RoomManagerError::Actor)?;
        let AccountSnapshots::Perp(accounts) = accounts else {
            return Ok(Vec::new());
        };
        Ok(accounts
            .into_iter()
            .filter(|account| account.margin_status == crate::PerpMarginStatus::Liquidatable)
            .map(|account| LiquidationTrigger {
                instrument_id: execution.instrument_id.clone(),
                account_id: account.account_id,
            })
            .collect())
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
        self.simulation_room_mut(room_id)
            .map(|room| room.advance_clock(steps))
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

struct LiquidationTrigger {
    instrument_id: String,
    account_id: AccountId,
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
    fn room_manager_retries_failed_liquidation_once_on_later_external_command() {
        let scenario = ScenarioConfig {
            room_id: "perp-retry-room".to_string(),
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
        };
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
        assert!(matches!(
            history.last().unwrap().result,
            ActorExecutionResult::Rejected(ActorRejectReason::Clearing(
                crate::ClearingError::LiquidationUnfilled
            ))
        ));
        assert!(matches!(
            manager.account_snapshot("perp-retry-room", 20).unwrap(),
            Some(AccountSnapshot::Perp(PerpAccountSnapshot {
                position_qty: 10,
                margin_status: PerpMarginStatus::Liquidatable,
                ..
            }))
        ));

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

        let net_worth = manager.net_worth_snapshot("multi-venue-room").unwrap();
        let usdt = net_worth.accounts[0]
            .assets
            .iter()
            .find(|asset| asset.asset_id == "USDT")
            .unwrap();
        assert_eq!(usdt.portfolio_total, 300);
        assert_eq!(usdt.venue_total, 700);
        assert_eq!(usdt.total, 1_000);
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
