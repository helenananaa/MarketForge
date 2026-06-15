use std::collections::BTreeMap;

use crate::{
    actor::{AccountSnapshot, AccountSnapshots, ActorExecution, MarketActor, MarketStatus, RoomId},
    model::{AccountId, BookSnapshot, Command},
    scenario::{ScenarioBootstrap, ScenarioConfig, ScenarioError},
};

#[derive(Debug, Default)]
pub struct RoomManager {
    rooms: BTreeMap<RoomId, MarketActor>,
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

        let ScenarioBootstrap {
            actor,
            seed_executions,
        } = scenario.bootstrap().map_err(RoomManagerError::Scenario)?;
        let room_id = actor.room_id().to_string();
        self.rooms.insert(room_id.clone(), actor);
        self.executions
            .insert(room_id.clone(), seed_executions.clone());

        Ok(RoomBootstrap {
            room_id,
            seed_executions,
        })
    }

    pub fn apply(
        &mut self,
        room_id: &str,
        command: Command,
    ) -> Result<ActorExecution, RoomManagerError> {
        let execution = self.room_mut(room_id)?.apply(command);
        self.executions
            .entry(room_id.to_string())
            .or_default()
            .push(execution.clone());
        Ok(execution)
    }

    pub fn pause_room(&mut self, room_id: &str) -> Result<(), RoomManagerError> {
        self.room_mut(room_id).map(|room| room.pause())
    }

    pub fn resume_room(&mut self, room_id: &str) -> Result<(), RoomManagerError> {
        self.room_mut(room_id).map(|room| room.resume())
    }

    pub fn close_room(&mut self, room_id: &str) -> Result<(), RoomManagerError> {
        self.room_mut(room_id).map(|room| room.close())
    }

    pub fn room(&self, room_id: &str) -> Result<&MarketActor, RoomManagerError> {
        self.rooms
            .get(room_id)
            .ok_or_else(|| RoomManagerError::RoomNotFound {
                room_id: room_id.to_string(),
            })
    }

    pub fn room_mut(&mut self, room_id: &str) -> Result<&mut MarketActor, RoomManagerError> {
        self.rooms
            .get_mut(room_id)
            .ok_or_else(|| RoomManagerError::RoomNotFound {
                room_id: room_id.to_string(),
            })
    }

    pub fn room_ids(&self) -> Vec<&str> {
        self.rooms.keys().map(String::as_str).collect()
    }

    pub fn status(&self, room_id: &str) -> Result<MarketStatus, RoomManagerError> {
        self.room(room_id).map(MarketActor::status)
    }

    pub fn book_snapshot(&self, room_id: &str) -> Result<BookSnapshot, RoomManagerError> {
        self.room(room_id).map(MarketActor::book_snapshot)
    }

    pub fn account_snapshot(
        &self,
        room_id: &str,
        account_id: AccountId,
    ) -> Result<Option<AccountSnapshot>, RoomManagerError> {
        self.room(room_id)
            .map(|room| room.account_snapshot(account_id))
    }

    pub fn account_snapshots(&self, room_id: &str) -> Result<AccountSnapshots, RoomManagerError> {
        self.room(room_id).map(MarketActor::account_snapshots)
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
    Scenario(ScenarioError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        actor::{ActorExecutionResult, ActorRejectReason, MarketExecution},
        market::{InstrumentConfig, MarketConfig, SpotMarketConfig},
        model::{BookLevel, Event, NewOrder, OrderKind, Side},
        risk::SpotRiskConfig,
        scenario::ScenarioAccount,
        spot::SpotClearingConfig,
    };

    fn spot_scenario(room_id: &str) -> ScenarioConfig {
        ScenarioConfig {
            room_id: room_id.to_string(),
            market: MarketConfig::Spot(SpotMarketConfig {
                instrument: InstrumentConfig::new("V-BTC-SPOT", 1, 1).unwrap(),
                clearing: SpotClearingConfig::default(),
                risk: SpotRiskConfig::default(),
            }),
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
}
