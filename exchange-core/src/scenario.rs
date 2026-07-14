use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    account::{Money, PositionQty},
    actor::{ActorExecution, ActorRejectReason, ExchangeActor, RoomId},
    market::{
        AssetConfig, AssetId, ExchangeConfig, InstrumentId, MarketConfig, MarketConfigError,
        MarketKind, VenueAssetPolicyConfig, VenueId,
    },
    model::{AccountId, Command},
    transfer::VenueTransferRejectReason,
    venue_rules::{VenuePreset, VenueRuleConfig},
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScenarioConfig {
    pub room_id: RoomId,
    #[serde(default)]
    pub venue_preset: Option<VenuePreset>,
    #[serde(default)]
    pub venue_rules: VenueRuleConfig,
    #[serde(default)]
    pub venue_asset_policy: VenueAssetPolicyConfig,
    #[serde(default)]
    pub assets: Vec<AssetConfig>,
    pub market: MarketConfig,
    #[serde(default)]
    pub extra_markets: Vec<MarketConfig>,
    #[serde(default)]
    pub initial_portfolios: Vec<ScenarioPortfolio>,
    #[serde(default)]
    pub initial_allocations: Vec<ScenarioAllocation>,
    #[serde(default)]
    pub routed_initial_allocations: Vec<ScenarioVenueAllocation>,
    pub accounts: Vec<ScenarioAccount>,
    pub seed_orders: Vec<Command>,
    #[serde(default)]
    pub routed_seed_orders: Vec<ScenarioSeedOrder>,
}

impl ScenarioConfig {
    pub fn seed_commands(&self) -> Vec<Command> {
        self.seed_orders
            .iter()
            .cloned()
            .chain(
                self.routed_seed_orders
                    .iter()
                    .map(|seed_order| seed_order.command.clone()),
            )
            .collect()
    }

    pub fn seed_order_count(&self) -> usize {
        self.seed_orders.len() + self.routed_seed_orders.len()
    }

    pub fn bootstrap(self) -> Result<ScenarioBootstrap, ScenarioError> {
        let mut markets = Vec::with_capacity(1 + self.extra_markets.len());
        markets.push(self.market.clone());
        markets.extend(self.extra_markets.clone());
        let mut exchange_config = ExchangeConfig::new(self.market.venue_id().to_string(), markets)
            .map_err(ScenarioError::MarketConfig)?;
        exchange_config.merge_asset_metadata(&self.assets);
        exchange_config.venue_rules = match &self.venue_preset {
            Some(preset) => preset
                .rules_for_markets(&exchange_config.markets)
                .map_err(|error| ScenarioError::MarketConfig(MarketConfigError::VenueRule(error)))?
                .merge_overrides(self.venue_rules.clone()),
            None => self.venue_rules.clone(),
        };
        exchange_config.asset_policy = match &self.venue_preset {
            Some(preset) => preset
                .asset_policy_for_markets(&exchange_config.markets)
                .merge_overrides(self.venue_asset_policy.clone()),
            None => self.venue_asset_policy.clone(),
        };
        exchange_config
            .validate()
            .map_err(ScenarioError::MarketConfig)?;
        let mut exchange = ExchangeActor::new(self.room_id.clone(), exchange_config)
            .map_err(ScenarioError::MarketConfig)?;

        for account in &self.accounts {
            account.apply_to_exchange(&mut exchange)?;
        }
        for portfolio in &self.initial_portfolios {
            portfolio.apply_to_exchange(&mut exchange);
        }
        for allocation in &self.initial_allocations {
            allocation.apply_to_exchange(&mut exchange)?;
        }

        let mut seed_executions = Vec::with_capacity(self.seed_orders.len());
        for command in self.seed_orders {
            seed_executions.push(exchange.apply(command));
        }
        for seed_order in self.routed_seed_orders {
            let execution = match seed_order.instrument_id {
                Some(instrument_id) => exchange
                    .apply_to_instrument(&instrument_id, seed_order.command)
                    .map_err(ScenarioError::Actor)?,
                None => exchange.apply(seed_order.command),
            };
            seed_executions.push(execution);
        }

        Ok(ScenarioBootstrap {
            exchange,
            seed_executions,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScenarioSeedOrder {
    pub instrument_id: Option<InstrumentId>,
    pub command: Command,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScenarioPortfolio {
    pub account_id: AccountId,
    pub balances: BTreeMap<AssetId, Money>,
}

impl ScenarioPortfolio {
    pub(crate) fn apply_to_exchange(&self, exchange: &mut ExchangeActor) {
        for (asset_id, total) in &self.balances {
            exchange.set_portfolio_balance(self.account_id, asset_id.clone(), *total);
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScenarioAllocation {
    pub account_id: AccountId,
    pub asset_id: AssetId,
    pub amount: Money,
}

impl ScenarioAllocation {
    pub(crate) fn apply_to_exchange(
        &self,
        exchange: &mut ExchangeActor,
    ) -> Result<(), ScenarioError> {
        exchange
            .allocate_from_portfolio(self.account_id, self.asset_id.clone(), self.amount)
            .map_err(ScenarioError::Allocation)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScenarioVenueAllocation {
    pub venue_id: VenueId,
    pub account_id: AccountId,
    pub asset_id: AssetId,
    pub amount: Money,
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
    pub(crate) fn apply_to_exchange(
        &self,
        exchange: &mut ExchangeActor,
    ) -> Result<(), ScenarioError> {
        match self {
            Self::Basic {
                account_id,
                cash_balance,
            } => {
                exchange.create_account(*account_id, *cash_balance);
                Ok(())
            }
            Self::Spot {
                account_id,
                cash_balance,
                position_qty,
            } => {
                if exchange.primary_market().kind() != MarketKind::Spot {
                    return Err(ScenarioError::WrongAccountForMarket);
                }
                exchange
                    .create_spot_account_with_position(*account_id, *cash_balance, *position_qty)
                    .map_err(ScenarioError::Actor)?;
                Ok(())
            }
        }
    }
}

#[derive(Debug)]
pub struct ScenarioBootstrap {
    pub exchange: ExchangeActor,
    pub seed_executions: Vec<ActorExecution>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScenarioError {
    MarketConfig(MarketConfigError),
    Actor(ActorRejectReason),
    Allocation(VenueTransferRejectReason),
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
        venue_rules::VenuePreset,
    };

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
            venue_preset: None,
            venue_rules: VenueRuleConfig::default(),
            venue_asset_policy: crate::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: spot_market(),
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
        };

        let bootstrap = scenario.bootstrap().expect("scenario should bootstrap");

        assert_eq!(bootstrap.exchange.room_id(), "spot-room");
        assert_eq!(
            bootstrap.exchange.book_snapshot().asks,
            vec![BookLevel {
                price_tick: 100,
                qty: 5,
            }]
        );
        assert_eq!(
            bootstrap.exchange.account_snapshot(10),
            Some(AccountSnapshot::Spot(SpotAccountSnapshot {
                account_id: 10,
                cash_balance: 1_000,
                position_qty: 10,
                reserved_cash: 0,
                reserved_position: 5,
                available_cash: 1_000,
                available_position: 5,
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
            venue_preset: None,
            venue_rules: VenueRuleConfig::default(),
            venue_asset_policy: crate::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: perp_market(),
            extra_markets: Vec::new(),
            initial_portfolios: Vec::new(),
            initial_allocations: Vec::new(),
            routed_initial_allocations: Vec::new(),
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
            routed_seed_orders: Vec::new(),
        };

        let bootstrap = scenario.bootstrap().expect("scenario should bootstrap");

        assert_eq!(bootstrap.exchange.primary_market().kind(), MarketKind::Perp);
        assert_eq!(
            bootstrap.exchange.book_snapshot().asks,
            vec![BookLevel {
                price_tick: 100,
                qty: 5,
            }]
        );
    }

    #[test]
    fn bootstraps_room_with_extra_market_and_routed_seed_order() {
        let scenario = ScenarioConfig {
            room_id: "multi-market-room".to_string(),
            venue_preset: None,
            venue_rules: VenueRuleConfig::default(),
            venue_asset_policy: crate::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: spot_market(),
            extra_markets: vec![perp_market()],
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
            seed_orders: Vec::new(),
            routed_seed_orders: vec![ScenarioSeedOrder {
                instrument_id: Some("V-BTC-PERP".to_string()),
                command: limit(1, 20, Side::Sell, 100, 5),
            }],
        };

        let bootstrap = scenario.bootstrap().expect("scenario should bootstrap");

        assert_eq!(
            bootstrap.exchange.instrument_ids(),
            vec!["V-BTC-PERP", "V-BTC-SPOT"]
        );
        assert!(bootstrap.exchange.book_snapshot().asks.is_empty());
        assert_eq!(
            bootstrap
                .exchange
                .book_snapshot_for("V-BTC-PERP")
                .unwrap()
                .asks,
            vec![BookLevel {
                price_tick: 100,
                qty: 5,
            }]
        );
        assert_eq!(bootstrap.seed_executions[0].command_seq, 0);
        assert_eq!(bootstrap.seed_executions[0].instrument_id, "V-BTC-PERP");
    }

    #[test]
    fn bootstraps_initial_portfolios_and_allocations_into_venue_accounts() {
        let mut balances = BTreeMap::new();
        balances.insert("BTC".to_string(), 10_000);
        let scenario = ScenarioConfig {
            room_id: "allocation-room".to_string(),
            venue_preset: None,
            venue_rules: VenueRuleConfig::default(),
            venue_asset_policy: crate::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: spot_market(),
            extra_markets: Vec::new(),
            initial_portfolios: vec![ScenarioPortfolio {
                account_id: 20,
                balances,
            }],
            initial_allocations: vec![ScenarioAllocation {
                account_id: 20,
                asset_id: "BTC".to_string(),
                amount: 4_000,
            }],
            routed_initial_allocations: Vec::new(),
            accounts: vec![ScenarioAccount::Basic {
                account_id: 20,
                cash_balance: 1_000,
            }],
            seed_orders: Vec::new(),
            routed_seed_orders: Vec::new(),
        };

        let bootstrap = scenario.bootstrap().expect("scenario should bootstrap");
        let wallet = bootstrap.exchange.portfolio_snapshot(20);
        let wallet_btc = wallet
            .balances
            .iter()
            .find(|balance| balance.asset_id == "BTC")
            .unwrap();
        assert_eq!(wallet_btc.total, 6_000);
        assert_eq!(
            bootstrap
                .exchange
                .venue_balance_snapshot(20, "BTC")
                .unwrap()
                .total,
            5_000
        );
    }

    #[test]
    fn rejects_initial_allocation_when_portfolio_is_insufficient() {
        let mut balances = BTreeMap::new();
        balances.insert("BTC".to_string(), 100);
        let scenario = ScenarioConfig {
            room_id: "bad-allocation-room".to_string(),
            venue_preset: None,
            venue_rules: VenueRuleConfig::default(),
            venue_asset_policy: crate::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: spot_market(),
            extra_markets: Vec::new(),
            initial_portfolios: vec![ScenarioPortfolio {
                account_id: 20,
                balances,
            }],
            initial_allocations: vec![ScenarioAllocation {
                account_id: 20,
                asset_id: "BTC".to_string(),
                amount: 101,
            }],
            routed_initial_allocations: Vec::new(),
            accounts: vec![ScenarioAccount::Basic {
                account_id: 20,
                cash_balance: 1_000,
            }],
            seed_orders: Vec::new(),
            routed_seed_orders: Vec::new(),
        };

        assert_eq!(
            scenario.bootstrap().map(|_| ()),
            Err(ScenarioError::Allocation(
                VenueTransferRejectReason::InsufficientPortfolioBalance
            ))
        );
    }

    #[test]
    fn rejects_spot_account_on_perp_market() {
        let scenario = ScenarioConfig {
            room_id: "bad-room".to_string(),
            venue_preset: None,
            venue_rules: VenueRuleConfig::default(),
            venue_asset_policy: crate::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: perp_market(),
            extra_markets: Vec::new(),
            initial_portfolios: Vec::new(),
            initial_allocations: Vec::new(),
            routed_initial_allocations: Vec::new(),
            accounts: vec![ScenarioAccount::Spot {
                account_id: 10,
                cash_balance: 1_000,
                position_qty: 10,
            }],
            seed_orders: vec![],
            routed_seed_orders: Vec::new(),
        };

        assert_eq!(
            scenario.bootstrap().map(|_| ()),
            Err(ScenarioError::WrongAccountForMarket)
        );
    }

    #[test]
    fn venue_preset_builds_exchange_rules_and_allows_explicit_overrides() {
        let mut reference_price_ticks = std::collections::BTreeMap::new();
        reference_price_ticks.insert("V-BTC-SPOT".to_string(), 100);
        let scenario = ScenarioConfig {
            room_id: "sse-room".to_string(),
            venue_preset: Some(VenuePreset::SseLike {
                reference_price_ticks,
            }),
            venue_rules: VenueRuleConfig {
                settlement: crate::SettlementRuleConfig {
                    spot_sell_delay_steps: 2,
                },
                ..VenueRuleConfig::default()
            },
            venue_asset_policy: VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: spot_market(),
            extra_markets: Vec::new(),
            initial_portfolios: Vec::new(),
            initial_allocations: Vec::new(),
            routed_initial_allocations: Vec::new(),
            accounts: vec![ScenarioAccount::Basic {
                account_id: 20,
                cash_balance: 1_000,
            }],
            seed_orders: Vec::new(),
            routed_seed_orders: Vec::new(),
        };

        let mut bootstrap = scenario.bootstrap().unwrap();
        let rejected = bootstrap.exchange.apply(limit(1, 20, Side::Buy, 111, 1));

        assert!(matches!(
            rejected.result,
            ActorExecutionResult::Rejected(crate::ActorRejectReason::VenueRule(_))
        ));
    }
}
