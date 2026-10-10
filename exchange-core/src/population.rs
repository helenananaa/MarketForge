//! Finite-capital spot population for demos and reproducible experiments.
use crate::{
    AgentTemplate, BotConfig, Command, InstrumentConfig, MarketConfig, NewOrder, OrderKind,
    ParticipantConfig, ParticipantKind, ScenarioAccount, ScenarioConfig, Side, SpotClearingConfig,
    SpotMarketConfig, SpotRiskConfig,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BackgroundMarket {
    pub scenario: ScenarioConfig,
    pub agents: Vec<AgentTemplate>,
    pub agent_interval_ms: u64,
    pub autostart_agents: bool,
}

pub fn background_market(room_id: &str, seed: u64) -> BackgroundMarket {
    let instrument_id = "V-BTC-SPOT";
    let mut scenario = ScenarioConfig {
        room_id: room_id.into(),
        market_events: Vec::new(),
        venue_preset: None,
        venue_rules: Default::default(),
        venue_asset_policy: Default::default(),
        assets: vec![],
        market: MarketConfig::Spot(SpotMarketConfig {
            instrument: InstrumentConfig::new(instrument_id, 1, 1).unwrap(),
            clearing: SpotClearingConfig {
                maker_fee_ppm: 100,
                taker_fee_ppm: 300,
            },
            risk: SpotRiskConfig {
                allow_short: false,
                ..Default::default()
            },
        }),
        extra_markets: vec![],
        initial_portfolios: vec![],
        initial_allocations: vec![],
        routed_initial_allocations: vec![],
        routed_seed_orders: vec![],
        accounts: vec![
            ScenarioAccount::Spot {
                account_id: 10,
                cash_balance: 100_000,
                position_qty: 200,
            },
            ScenarioAccount::Spot {
                account_id: 20,
                cash_balance: 10_000,
                position_qty: 20,
            },
            ScenarioAccount::Spot {
                account_id: 30,
                cash_balance: 10_000,
                position_qty: 20,
            },
        ],
        seed_orders: [Side::Buy, Side::Sell]
            .into_iter()
            .enumerate()
            .map(|(i, side)| {
                Command::NewOrder(NewOrder {
                    position_side: crate::model::PositionSide::Both,
                    order_id: 10_000 + i as u64,
                    account_id: 10,
                    side,
                    kind: OrderKind::Limit {
                        price_tick: if side == Side::Buy { 98 } else { 102 },
                    },
                    qty: 20,
                    reduce_only: false,
                })
            })
            .collect(),
    };
    let mut agents = vec![];
    let mut add = |id: &str, name: String, position: i128, config: serde_json::Value| {
        let account_id = 100 + agents.len() as u64;
        scenario.accounts.push(ScenarioAccount::Spot {
            account_id,
            cash_balance: 20_000 + (account_id % 5) as i128 * 5_000,
            position_qty: position,
        });
        agents.push(AgentTemplate::Plugin(BotConfig {
            participant: ParticipantConfig {
                participant_id: name.clone(),
                kind: if id == "DynamicMarketMaker" {
                    ParticipantKind::MarketMaker
                } else {
                    ParticipantKind::RuleAgent
                },
                room_id: room_id.into(),
                account_id,
                instrument_id: Some(instrument_id.into()),
            },
            plugin_id: id.into(),
            plugin_version: "1".into(),
            state_version: 1,
            config_version: 1,
            // JSON is also consumed by the browser. Keep derived seeds within
            // JavaScript's exact integer range so every host runs the same recipe.
            seed: (crate::child_seed(seed, &name) & ((1_u64 << 53) - 1)).max(1),
            config,
        }));
    };
    for i in 0..3 {
        add(
            "DynamicMarketMaker",
            format!("maker-{i}"),
            40,
            json!({
                "inventory_target":40,"inventory_cap":100,"max_qty":3+i,
                "half_spread_ticks":1+i,"levels":2,"decision_interval_ms":1000+i*500,"jitter_ms":500,
            }),
        );
    }
    for i in 0..4 {
        add(
            "ValueTrader",
            format!("value-{i}"),
            30,
            json!({
                "fair_price_tick":98+i*2,"value_shift_ticks":12,"value_shift_at_ms":60000,
                "information_delay_ms":i*4000,"decision_interval_ms":2000+i*500,"jitter_ms":1500,"max_qty":1+i%3,
            }),
        );
    }
    for i in 0..3 {
        add(
            "TrendTrader",
            format!("trend-{i}"),
            25,
            json!({
                "inventory_target":25,"position_size":15,"inventory_cap":70,"lookback":4+i*4,
                "signal_threshold_ticks":1+i,"decision_interval_ms":1500+i*500,"jitter_ms":1000,
            }),
        );
    }
    for i in 0..8 {
        add(
            "AdaptiveNoiseTrader",
            format!("noise-{i}"),
            30,
            json!({
                "inventory_cap":60,"max_qty":1+i%3,"activity_ppm":250000+i*50000,
                "side_persistence_ppm":500000+i*40000,"market_order_ratio_ppm":300000+i*50000,
                "decision_interval_ms":1000+i*250,"jitter_ms":2500,"order_ttl_ms":4000+i*1000,
            }),
        );
    }
    for (side, position, start) in [("Buy", 10, 20_000), ("Sell", 80, 35_000)] {
        add(
            "ExecutionTrader",
            format!("execution-{}", side.to_lowercase()),
            position,
            json!({
                "side":side,"target_qty":40,"horizon_ms":120000,"start_after_ms":start,
                "decision_interval_ms":2000,"jitter_ms":500,"max_qty":3,"max_slippage_ticks":3,
            }),
        );
    }
    BackgroundMarket {
        scenario,
        agents,
        agent_interval_ms: 500,
        autostart_agents: true,
    }
}

#[cfg(test)]
#[path = "population_tests.rs"]
mod tests;
