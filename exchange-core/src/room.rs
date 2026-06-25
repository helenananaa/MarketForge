use std::collections::BTreeMap;

use crate::{
    account::{Money, VenueAccountSnapshot},
    actor::{
        AccountSnapshot, AccountSnapshots, ActorExecution, ActorRejectReason, ExchangeActor,
        MarketActor, MarketStatus, RoomId,
    },
    clock::SimulationClock,
    model::{AccountId, BookSnapshot, Command},
    portfolio::PortfolioAccountSnapshot,
    scenario::{ScenarioConfig, ScenarioError},
    simulation::{
        AssetLedgerEntry, RoomNetWorthSnapshot, SimulationBootstrap, SimulationRoom,
        SimulationRoomError, VenueAccountVenueSnapshot, VenueToVenueTransfer,
    },
    transfer::VenueTransfer,
};

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
        self.rooms
            .insert(room_id.clone(), SimulationRoom::from_exchange(exchange));
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

        self.rooms
            .insert(room_id.clone(), SimulationRoom::from_exchange(exchange));
        self.executions.insert(room_id, executions);
        Ok(())
    }

    pub fn restore_simulation_room(
        &mut self,
        room: SimulationRoom,
        executions: Vec<ActorExecution>,
    ) -> Result<(), RoomManagerError> {
        let room_id = room.room_id().to_string();
        if self.rooms.contains_key(&room_id) {
            return Err(RoomManagerError::RoomAlreadyExists { room_id });
        }

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
        self.executions
            .entry(room_id.to_string())
            .or_default()
            .push(execution.clone());
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
        self.executions
            .entry(room_id.to_string())
            .or_default()
            .push(execution.clone());
        Ok(execution)
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
    RoomAlreadyExists { room_id: RoomId },
    RoomNotFound { room_id: RoomId },
    MarketConfig(crate::market::MarketConfigError),
    Actor(ActorRejectReason),
    Scenario(ScenarioError),
    Simulation(SimulationRoomError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AssetKind, AssetSelector,
        actor::{ActorExecutionResult, ActorRejectReason, MarketExecution},
        market::{InstrumentConfig, MarketConfig, SpotMarketConfig},
        model::{BookLevel, Event, NewOrder, OrderKind, Side},
        risk::SpotRiskConfig,
        scenario::{
            ScenarioAccount, ScenarioAllocation, ScenarioPortfolio, ScenarioVenueAllocation,
        },
        spot::SpotClearingConfig,
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
