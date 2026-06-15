use serde::{Deserialize, Serialize};

use crate::{
    account::{Money, PositionQty},
    actor::{ActorExecution, ActorRejectReason, MarketActor, RoomId},
    market::{MarketConfig, MarketConfigError, MarketKind},
    model::{AccountId, Command},
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScenarioConfig {
    pub room_id: RoomId,
    pub market: MarketConfig,
    pub accounts: Vec<ScenarioAccount>,
    pub seed_orders: Vec<Command>,
}

impl ScenarioConfig {
    pub fn bootstrap(self) -> Result<ScenarioBootstrap, ScenarioError> {
        let mut actor = MarketActor::new(self.room_id.clone(), self.market.clone())
            .map_err(ScenarioError::MarketConfig)?;

        for account in &self.accounts {
            account.apply_to_actor(&mut actor)?;
        }

        let mut seed_executions = Vec::with_capacity(self.seed_orders.len());
        for command in self.seed_orders {
            seed_executions.push(actor.apply(command));
        }

        Ok(ScenarioBootstrap {
            actor,
            seed_executions,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ScenarioAccount {
    Basic {
        account_id: AccountId,
        cash_balance: Money,
    },
    Spot {
        account_id: AccountId,
        cash_balance: Money,
        position_qty: PositionQty,
    },
}

impl ScenarioAccount {
    fn apply_to_actor(&self, actor: &mut MarketActor) -> Result<(), ScenarioError> {
        match self {
            Self::Basic {
                account_id,
                cash_balance,
            } => {
                actor.create_account(*account_id, *cash_balance);
                Ok(())
            }
            Self::Spot {
                account_id,
                cash_balance,
                position_qty,
            } => {
                if actor.kind() != MarketKind::Spot {
                    return Err(ScenarioError::WrongAccountForMarket);
                }
                actor
                    .create_spot_account_with_position(*account_id, *cash_balance, *position_qty)
                    .map_err(ScenarioError::Actor)?;
                Ok(())
            }
        }
    }
}

#[derive(Debug)]
pub struct ScenarioBootstrap {
    pub actor: MarketActor,
    pub seed_executions: Vec<ActorExecution>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScenarioError {
    MarketConfig(MarketConfigError),
    Actor(ActorRejectReason),
    WrongAccountForMarket,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        actor::{AccountSnapshot, ActorExecutionResult, MarketExecution},
        market::{InstrumentConfig, PerpMarketConfig, SpotMarketConfig},
        model::{BookLevel, Event, NewOrder, OrderKind, Side},
        perp::PerpClearingConfig,
        risk::{PerpRiskConfig, SpotRiskConfig},
        spot::{SpotAccountSnapshot, SpotClearingConfig},
    };

    fn limit(order_id: u64, account_id: u64, side: Side, price_tick: i64, qty: u64) -> Command {
        Command::NewOrder(NewOrder {
            order_id,
            account_id,
            side,
            kind: OrderKind::Limit { price_tick },
            qty,
        })
    }

    fn spot_market() -> MarketConfig {
        MarketConfig::Spot(SpotMarketConfig {
            instrument: InstrumentConfig::new("V-BTC-SPOT", 1, 1).unwrap(),
            clearing: SpotClearingConfig::default(),
            risk: SpotRiskConfig::default(),
        })
    }

    fn perp_market() -> MarketConfig {
        MarketConfig::Perp(PerpMarketConfig {
            instrument: InstrumentConfig::new("V-BTC-PERP", 1, 1).unwrap(),
            clearing: PerpClearingConfig {
                leverage: 10,
                ..PerpClearingConfig::default()
            },
            risk: PerpRiskConfig::default(),
            initial_mark_price_tick: 100,
        })
    }

    #[test]
    fn bootstraps_spot_room_with_accounts_and_seed_orders() {
        let scenario = ScenarioConfig {
            room_id: "spot-room".to_string(),
            market: spot_market(),
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
        };

        let bootstrap = scenario.bootstrap().expect("scenario should bootstrap");

        assert_eq!(bootstrap.actor.room_id(), "spot-room");
        assert_eq!(
            bootstrap.actor.book_snapshot().asks,
            vec![BookLevel {
                price_tick: 100,
                qty: 5,
            }]
        );
        assert_eq!(
            bootstrap.actor.account_snapshot(10),
            Some(AccountSnapshot::Spot(SpotAccountSnapshot {
                account_id: 10,
                cash_balance: 1_000,
                position_qty: 10,
                fees_paid: 0,
            }))
        );
        assert_eq!(bootstrap.seed_executions.len(), 1);
        let ActorExecutionResult::Accepted(MarketExecution::Spot(execution)) =
            &bootstrap.seed_executions[0].result
        else {
            panic!("expected accepted spot seed order");
        };
        assert!(
            execution
                .events
                .iter()
                .any(|record| matches!(record.event, Event::OrderRested { order_id: 1, .. }))
        );
    }

    #[test]
    fn bootstraps_perp_room_with_seed_orders() {
        let scenario = ScenarioConfig {
            room_id: "perp-room".to_string(),
            market: perp_market(),
            accounts: vec![
                ScenarioAccount::Basic {
                    account_id: 10,
                    cash_balance: 1_000,
                },
                ScenarioAccount::Basic {
                    account_id: 20,
                    cash_balance: 1_000,
                },
            ],
            seed_orders: vec![limit(1, 10, Side::Sell, 100, 5)],
        };

        let bootstrap = scenario.bootstrap().expect("scenario should bootstrap");

        assert_eq!(bootstrap.actor.kind(), MarketKind::Perp);
        assert_eq!(
            bootstrap.actor.book_snapshot().asks,
            vec![BookLevel {
                price_tick: 100,
                qty: 5,
            }]
        );
    }

    #[test]
    fn rejects_spot_account_on_perp_market() {
        let scenario = ScenarioConfig {
            room_id: "bad-room".to_string(),
            market: perp_market(),
            accounts: vec![ScenarioAccount::Spot {
                account_id: 10,
                cash_balance: 1_000,
                position_qty: 10,
            }],
            seed_orders: vec![],
        };

        assert_eq!(
            scenario.bootstrap().map(|_| ()),
            Err(ScenarioError::WrongAccountForMarket)
        );
    }
}
