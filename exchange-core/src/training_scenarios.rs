//! Versioned training scenarios for F5. Reports describe injected facts, not
//! real-market manipulation evidence.

use crate::{
    agents::{
        AgentTemplate, CancelAtStepConfig, ContinuousMmConfig, DcaTraderConfig, GridTraderConfig,
    },
    market::{InstrumentConfig, MarketConfig, SpotMarketConfig},
    model::{AccountId, Command, NewOrder, OrderKind, Qty, Side},
    participant::{ParticipantConfig, ParticipantKind},
    risk::SpotRiskConfig,
    scenario::{ScenarioAccount, ScenarioConfig},
    spot::SpotClearingConfig,
};

pub const SCENARIO_SPEC_VERSION: u16 = 1;
pub const BASIC_EXECUTION_ID: &str = "basic_execution";
pub const LIQUIDITY_WITHDRAWAL_ID: &str = "liquidity_withdrawal";
pub const INVENTORY_STRESS_ID: &str = "inventory_stress";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrainingScenario {
    pub id: &'static str,
    pub version: u16,
    pub seed: u64,
    pub scoring_version: u16,
    pub scenario: ScenarioConfig,
    pub agents: Vec<AgentTemplate>,
    pub expected: &'static str,
}

fn base_scenario(room_id: &str, sell_qty: Qty) -> ScenarioConfig {
    ScenarioConfig {
        room_id: room_id.to_string(),
        venue_preset: None,
        venue_rules: Default::default(),
        venue_asset_policy: Default::default(),
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
                cash_balance: 1_000_000,
                position_qty: 1_000,
            },
            ScenarioAccount::Spot {
                account_id: 20,
                cash_balance: 1_000_000,
                position_qty: 0,
            },
        ],
        seed_orders: vec![
            Command::NewOrder(NewOrder {
                order_id: 1,
                account_id: 10,
                side: Side::Buy,
                kind: OrderKind::Limit { price_tick: 99 },
                qty: sell_qty,
                reduce_only: false,
            }),
            Command::NewOrder(NewOrder {
                order_id: 2,
                account_id: 10,
                side: Side::Sell,
                kind: OrderKind::Limit { price_tick: 101 },
                qty: sell_qty,
                reduce_only: false,
            }),
        ],
        routed_seed_orders: Vec::new(),
    }
}

fn participant(room_id: &str, id: &str, account_id: AccountId) -> ParticipantConfig {
    ParticipantConfig {
        participant_id: id.to_string(),
        kind: ParticipantKind::RuleAgent,
        room_id: room_id.to_string(),
        account_id,
        instrument_id: Some("V-BTC-SPOT".to_string()),
    }
}

pub fn child_seed(scenario_seed: u64, name: &str) -> u64 {
    let mut h = scenario_seed.max(1);
    for byte in name.as_bytes() {
        h = h
            .wrapping_mul(0x1000_0000_01b3)
            .wrapping_add(u64::from(*byte));
    }
    h.max(1)
}

pub fn basic_execution(room_id: &str, seed: u64) -> TrainingScenario {
    let mm_seed = child_seed(seed, "mm");
    TrainingScenario {
        id: BASIC_EXECUTION_ID,
        version: SCENARIO_SPEC_VERSION,
        seed,
        scoring_version: 1,
        scenario: base_scenario(room_id, 20),
        agents: vec![AgentTemplate::ContinuousMarketMaker(ContinuousMmConfig {
            participant: participant(room_id, "mm", 10),
            version: 1,
            seed: mm_seed,
            half_spread_ticks: 2,
            size_per_level: 2,
            inventory_target: 0,
            inventory_cap: 50,
            requote_threshold_ticks: 3,
            max_resting_orders: 4,
            replenish_steps: 1,
            fallback_price_tick: 100,
        })],
        expected: "stable two-sided quotes; timed buy can complete against displayed size",
    }
}

pub fn liquidity_withdrawal(room_id: &str, seed: u64) -> TrainingScenario {
    TrainingScenario {
        id: LIQUIDITY_WITHDRAWAL_ID,
        version: SCENARIO_SPEC_VERSION,
        seed,
        scoring_version: 1,
        scenario: base_scenario(room_id, 20),
        agents: vec![
            AgentTemplate::GridTrader(GridTraderConfig {
                participant: participant(room_id, "liquidity", 10),
                center_price_tick: 100,
                grid_spacing_ticks: 1,
                levels: 1,
                qty_per_level: 5,
            }),
            AgentTemplate::CancelAtStep(CancelAtStepConfig {
                participant: participant(room_id, "liquidity-cancel", 10),
                cancel_at_step: 2,
            }),
        ],
        expected: "named liquidity account cancels at sim step 2; no direct price rewrite",
    }
}

pub fn inventory_stress(room_id: &str, seed: u64) -> TrainingScenario {
    let mut scenario = base_scenario(room_id, 40);
    scenario.seed_orders.clear();
    scenario.accounts = vec![
        ScenarioAccount::Spot {
            account_id: 10,
            cash_balance: 1_000_000,
            position_qty: 8,
        },
        ScenarioAccount::Spot {
            account_id: 20,
            cash_balance: 1_000_000,
            position_qty: 0,
        },
    ];
    TrainingScenario {
        id: INVENTORY_STRESS_ID,
        version: SCENARIO_SPEC_VERSION,
        seed,
        scoring_version: 1,
        scenario,
        agents: vec![
            AgentTemplate::ContinuousMarketMaker(ContinuousMmConfig {
                participant: participant(room_id, "mm", 10),
                version: 1,
                seed: child_seed(seed, "mm"),
                half_spread_ticks: 1,
                size_per_level: 4,
                inventory_target: 0,
                inventory_cap: 8,
                requote_threshold_ticks: 1,
                max_resting_orders: 4,
                replenish_steps: 1,
                fallback_price_tick: 100,
            }),
            AgentTemplate::DcaTrader(DcaTraderConfig {
                participant: participant(room_id, "taker", 20),
                interval_steps: 1,
                order_qty: 4,
                use_market_order: false,
                limit_offset_ticks: 0,
                fallback_price_tick: 101,
                side: Side::Buy,
            }),
        ],
        expected: "background size stresses MM inventory cap and trader cost",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        room::RoomManager,
        scheduler::{CrashPoint, SchedulerMode, SchedulerState, run_scheduler_step},
    };

    #[test]
    fn child_seed_depends_on_name_and_parent() {
        assert_ne!(child_seed(7, "mm"), child_seed(7, "flow"));
        assert_ne!(child_seed(7, "mm"), child_seed(8, "mm"));
        assert_eq!(child_seed(7, "mm"), child_seed(7, "mm"));
    }

    #[test]
    fn three_scenarios_are_versioned_and_distinct() {
        let basic = basic_execution("s1", 1);
        let withdraw = liquidity_withdrawal("s2", 1);
        let stress = inventory_stress("s3", 1);
        assert_eq!(basic.id, BASIC_EXECUTION_ID);
        assert_eq!(withdraw.id, LIQUIDITY_WITHDRAWAL_ID);
        assert_eq!(stress.id, INVENTORY_STRESS_ID);
        assert!(matches!(
            withdraw.agents[1],
            AgentTemplate::CancelAtStep(CancelAtStepConfig {
                cancel_at_step: 2,
                ..
            })
        ));
        assert!(
            stress
                .agents
                .iter()
                .any(|agent| { matches!(agent, AgentTemplate::DcaTrader(_)) })
        );
        assert_eq!(basic.version, 1);
        assert_ne!(basic.expected, withdraw.expected);
    }

    fn run_steps(scenario: TrainingScenario, steps: u64) -> RoomManager {
        let room_id = scenario.scenario.room_id.clone();
        let mut rooms = RoomManager::new();
        rooms.create_room(scenario.scenario.clone()).unwrap();
        let mut next_order_id = 100;
        let mut state = SchedulerState::new(room_id, scenario.agents, SchedulerMode::Manual);
        for _ in 0..steps {
            let outcome =
                run_scheduler_step(&mut rooms, &mut next_order_id, state, CrashPoint::None)
                    .unwrap();
            state = outcome.state;
        }
        rooms
    }

    fn resting_count(rooms: &RoomManager, room_id: &str, account_id: AccountId) -> usize {
        rooms
            .resting_orders_for_account(room_id, "V-BTC-SPOT", account_id)
            .unwrap()
            .len()
    }

    fn position(rooms: &RoomManager, room_id: &str, account_id: AccountId) -> i128 {
        match rooms.account_snapshots_for(room_id, "V-BTC-SPOT").unwrap() {
            crate::AccountSnapshots::Spot(items) => items
                .into_iter()
                .find(|item| item.account_id == account_id)
                .map(|item| item.position_qty)
                .unwrap_or(0),
            crate::AccountSnapshots::Perp(items) => items
                .into_iter()
                .find(|item| item.account_id == account_id)
                .map(|item| item.position_qty)
                .unwrap_or(0),
        }
    }

    #[test]
    fn basic_execution_sample_quotes_two_sided_and_stays_bounded() {
        for seed in [1_u64, 2, 7] {
            let room = format!("basic-{seed}");
            let rooms = run_steps(basic_execution(&room, seed), 4);
            let book = rooms.book_snapshot_for(&room, "V-BTC-SPOT").unwrap();
            assert!(
                !book.bids.is_empty() && !book.asks.is_empty(),
                "seed {seed} book={book:?}"
            );
            assert!(resting_count(&rooms, &room, 10) <= 6);
            let qty = position(&rooms, &room, 10);
            assert!(
                (qty - 1_000).abs() <= 50,
                "seed {seed} inventory moved from 1000 to {qty}"
            );
        }
    }

    #[test]
    fn liquidity_withdrawal_named_account_cancels_at_sim_step() {
        let before = run_steps(liquidity_withdrawal("lw-before", 1), 1);
        let after = run_steps(liquidity_withdrawal("lw-after", 1), 3);
        let before_resting = resting_count(&before, "lw-before", 10);
        let after_resting = resting_count(&after, "lw-after", 10);
        assert!(
            before_resting > 0,
            "grid should rest before cancel step, got {before_resting}"
        );
        assert!(
            after_resting < before_resting,
            "named liquidity account should cancel at step 2: before={before_resting} after={after_resting}"
        );
        for seed in [1_u64, 3, 9] {
            let room = format!("lw-{seed}");
            let rooms = run_steps(liquidity_withdrawal(&room, seed), 4);
            assert!(
                resting_count(&rooms, &room, 10) < before_resting,
                "seed {seed} still quoting after cancel"
            );
        }
    }

    fn fill_qty(rooms: &RoomManager, room_id: &str) -> u64 {
        rooms
            .execution_history(room_id)
            .unwrap()
            .iter()
            .map(|execution| match &execution.result {
                crate::ActorExecutionResult::Accepted(crate::MarketExecution::Spot(result)) => {
                    result
                        .events
                        .iter()
                        .filter_map(|record| match &record.event {
                            crate::Event::TradePrinted(trade) => Some(trade.qty),
                            _ => None,
                        })
                        .sum::<u64>()
                }
                _ => 0,
            })
            .sum()
    }

    #[test]
    fn inventory_stress_background_flow_keeps_mm_within_cap() {
        let stress = inventory_stress("stress-cfg", 1);
        assert!(stress.scenario.seed_orders.is_empty());
        match &stress.scenario.accounts[0] {
            ScenarioAccount::Spot { position_qty, .. } => assert_eq!(*position_qty, 8),
            other => panic!("expected MM spot inventory, got {other:?}"),
        }
        for seed in [1_u64, 4, 8] {
            let room = format!("stress-{seed}");
            let rooms = run_steps(inventory_stress(&room, seed), 6);
            let filled = fill_qty(&rooms, &room);
            assert!(filled > 0, "seed {seed} background taker produced no fills");
            assert!(resting_count(&rooms, &room, 10) <= 4);
            let qty = position(&rooms, &room, 10);
            assert!(qty.abs() <= 8, "seed {seed} inventory {qty} exceeded cap 8");
            assert!(
                qty < 8,
                "seed {seed} inventory {qty} did not decline from 8"
            );
        }
    }
}
