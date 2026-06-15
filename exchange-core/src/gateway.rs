use serde::{Deserialize, Serialize};

use crate::{
    actor::{AccountSnapshots, ActorExecution, MarketStatus, RoomId},
    model::{AccountId, BookSnapshot, Command, NewOrder, OrderId, OrderKind, PriceTick, Qty, Side},
    room::{RoomManager, RoomManagerError},
};

pub type ParticipantId = String;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum OrderAction {
    PlaceLimit {
        side: Side,
        price_tick: PriceTick,
        qty: Qty,
    },
    PlaceMarket {
        side: Side,
        qty: Qty,
    },
    Cancel {
        order_id: OrderId,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GatewayRequest {
    pub participant_id: ParticipantId,
    pub room_id: RoomId,
    pub account_id: AccountId,
    pub action: OrderAction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GatewayExecution {
    pub participant_id: ParticipantId,
    pub room_id: RoomId,
    pub account_id: AccountId,
    pub action: OrderAction,
    pub command: Command,
    pub execution: ActorExecution,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketView {
    pub room_id: RoomId,
    pub status: MarketStatus,
    pub book: BookSnapshot,
    pub accounts: AccountSnapshots,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GatewayError {
    Room(RoomManagerError),
}

pub trait TradingApi {
    fn submit_action(&mut self, request: GatewayRequest) -> Result<GatewayExecution, GatewayError>;
    fn market_view(&self, room_id: &str) -> Result<MarketView, GatewayError>;
}

pub struct OrderGateway<'a> {
    rooms: &'a mut RoomManager,
    next_order_id: OrderId,
}

impl<'a> OrderGateway<'a> {
    pub fn new(rooms: &'a mut RoomManager, first_order_id: OrderId) -> Self {
        Self {
            rooms,
            next_order_id: first_order_id,
        }
    }

    pub fn next_order_id(&self) -> OrderId {
        self.next_order_id
    }

    fn action_to_command(&mut self, account_id: AccountId, action: &OrderAction) -> Command {
        match action {
            OrderAction::PlaceLimit {
                side,
                price_tick,
                qty,
            } => Command::NewOrder(NewOrder {
                order_id: self.take_order_id(),
                account_id,
                side: *side,
                kind: OrderKind::Limit {
                    price_tick: *price_tick,
                },
                qty: *qty,
            }),
            OrderAction::PlaceMarket { side, qty } => Command::NewOrder(NewOrder {
                order_id: self.take_order_id(),
                account_id,
                side: *side,
                kind: OrderKind::Market,
                qty: *qty,
            }),
            OrderAction::Cancel { order_id } => Command::CancelOrder(crate::model::CancelOrder {
                order_id: *order_id,
            }),
        }
    }

    fn take_order_id(&mut self) -> OrderId {
        let order_id = self.next_order_id;
        self.next_order_id += 1;
        order_id
    }
}

impl TradingApi for OrderGateway<'_> {
    fn submit_action(&mut self, request: GatewayRequest) -> Result<GatewayExecution, GatewayError> {
        let command = self.action_to_command(request.account_id, &request.action);
        let execution = self
            .rooms
            .apply(&request.room_id, command.clone())
            .map_err(GatewayError::Room)?;

        Ok(GatewayExecution {
            participant_id: request.participant_id,
            room_id: request.room_id,
            account_id: request.account_id,
            action: request.action,
            command,
            execution,
        })
    }

    fn market_view(&self, room_id: &str) -> Result<MarketView, GatewayError> {
        Ok(MarketView {
            room_id: room_id.to_string(),
            status: self.rooms.status(room_id).map_err(GatewayError::Room)?,
            book: self
                .rooms
                .book_snapshot(room_id)
                .map_err(GatewayError::Room)?,
            accounts: self
                .rooms
                .account_snapshots(room_id)
                .map_err(GatewayError::Room)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SpotRiskConfig,
        actor::{ActorExecutionResult, MarketExecution},
        market::{InstrumentConfig, MarketConfig, SpotMarketConfig},
        scenario::{ScenarioAccount, ScenarioConfig},
        spot::SpotClearingConfig,
    };

    fn spot_scenario() -> ScenarioConfig {
        ScenarioConfig {
            room_id: "room-1".to_string(),
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
            seed_orders: vec![],
        }
    }

    #[test]
    fn gateway_submits_human_shaped_action_to_room_manager() {
        let mut rooms = RoomManager::new();
        rooms.create_room(spot_scenario()).unwrap();
        let mut gateway = OrderGateway::new(&mut rooms, 1);

        let execution = gateway
            .submit_action(GatewayRequest {
                participant_id: "human-1".to_string(),
                room_id: "room-1".to_string(),
                account_id: 20,
                action: OrderAction::PlaceLimit {
                    side: Side::Buy,
                    price_tick: 100,
                    qty: 2,
                },
            })
            .expect("gateway should route action");

        assert_eq!(execution.participant_id, "human-1");
        assert_eq!(gateway.next_order_id(), 2);
        let ActorExecutionResult::Accepted(MarketExecution::Spot(result)) =
            execution.execution.result
        else {
            panic!("expected accepted spot execution");
        };
        assert!(result.clearing_events.is_empty());
    }

    #[test]
    fn gateway_can_provide_market_view() {
        let mut rooms = RoomManager::new();
        rooms.create_room(spot_scenario()).unwrap();
        let gateway = OrderGateway::new(&mut rooms, 1);

        let view = gateway.market_view("room-1").unwrap();

        assert_eq!(view.room_id, "room-1");
        assert_eq!(view.status, MarketStatus::Running);
        assert!(view.book.bids.is_empty());
    }
}
