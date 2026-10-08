use super::*;
use crate::{
    AccountSnapshots, ActorExecutionResult, CrashPoint, Event, MarketExecution, RoomManager,
    SchedulerMode, SchedulerState, run_scheduler_step,
};
use std::collections::{BTreeMap, BTreeSet};

// Matching serializes its duplicate-detection HashSet in arbitrary order.
// Canonicalize this set alone; price queues, commands and events stay ordered.
fn canonical_snapshot(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, child) in fields {
                if key == "seen_order_ids" {
                    child
                        .as_array_mut()
                        .unwrap()
                        .sort_by_key(|id| id.as_u64().unwrap());
                } else {
                    canonical_snapshot(child);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                canonical_snapshot(item);
            }
        }
        _ => {}
    }
}

fn setup(seed: u64) -> (RoomManager, SchedulerState, u64) {
    let recipe = background_market("population-test", seed);
    let mut rooms = RoomManager::new();
    rooms.create_room(recipe.scenario).unwrap();
    (
        rooms,
        SchedulerState::new("population-test", recipe.agents, SchedulerMode::Manual),
        100_000,
    )
}

fn totals(rooms: &RoomManager) -> (i128, i128) {
    let AccountSnapshots::Spot(accounts) = rooms.account_snapshots("population-test").unwrap()
    else {
        panic!()
    };
    (
        accounts.iter().map(|a| a.position_qty).sum(),
        accounts.iter().map(|a| a.cash_balance + a.fees_paid).sum(),
    )
}

#[test]
fn exported_recipe_matches_builder_and_uses_exclusive_funded_accounts() {
    let exported: serde_json::Value = serde_json::from_str(include_str!(
        "../../scripts/fixtures/background_market.json"
    ))
    .unwrap();
    assert_eq!(
        exported,
        serde_json::to_value(background_market("background-market", 7)).unwrap()
    );
    let recipe = background_market("population-test", 7);
    assert_eq!(recipe.agents.len(), 20);
    let accounts: BTreeSet<_> = recipe
        .agents
        .iter()
        .map(|a| a.config().account_id)
        .collect();
    assert_eq!(accounts.len(), 20);
    let registry = crate::BotRegistry::with_builtins();
    for template in recipe.agents {
        registry.validate_template(&template).unwrap();
    }
}

#[test]
fn multiple_seeds_produce_all_five_types_of_flow_without_minting_or_self_trades() {
    for seed in [7, 19, 41] {
        let (mut rooms, mut scheduler, mut order_id) = setup(seed);
        let initial = totals(&rooms);
        let owners: BTreeMap<_, _> = scheduler
            .agents
            .iter()
            .map(|a| {
                (
                    a.template.config().account_id,
                    a.template.bot_id().to_string(),
                )
            })
            .collect();
        for _ in 0..240 {
            scheduler = run_scheduler_step(&mut rooms, &mut order_id, scheduler, CrashPoint::None)
                .unwrap()
                .state;
        }
        assert_eq!(initial, totals(&rooms));
        let AccountSnapshots::Spot(accounts) = rooms.account_snapshots("population-test").unwrap()
        else {
            panic!()
        };
        for a in &accounts {
            assert!(
                a.available_cash >= 0 && a.available_position >= 0,
                "seed {seed}: {a:?}"
            );
            if let Some(template) = scheduler
                .agents
                .iter()
                .find(|agent| agent.account_id() == a.account_id)
            {
                let AgentTemplate::Plugin(config) = &template.template else {
                    panic!()
                };
                let cap = config
                    .config
                    .get("inventory_cap")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(100);
                assert!(a.position_qty <= i128::from(cap));
            }
        }
        let mut active = BTreeSet::new();
        let mut trades = 0;
        for execution in rooms.execution_history("population-test").unwrap() {
            let ActorExecutionResult::Accepted(MarketExecution::Spot(market)) = &execution.result
            else {
                continue;
            };
            for event in &market.events {
                if let Event::TradePrinted(t) = &event.event {
                    assert_ne!(
                        t.maker_account_id, t.taker_account_id,
                        "seed {seed}: self trade"
                    );
                    trades += 1;
                    for account in [t.maker_account_id, t.taker_account_id] {
                        if let Some(kind) = owners.get(&account) {
                            active.insert(kind.clone());
                        }
                    }
                }
            }
        }
        assert!(trades > 100, "seed {seed}: {trades}");
        assert_eq!(active.len(), 5, "seed {seed}: {active:?}");
        for agent in &scheduler.agents {
            if agent.template.bot_id() == "ExecutionTrader" {
                let crate::PersistedAgentKindState::Plugin { data, .. } = &agent.kind_state else {
                    panic!()
                };
                let initial = data["initial_position"]
                    .as_str()
                    .unwrap()
                    .parse::<i128>()
                    .unwrap();
                let account = accounts
                    .iter()
                    .find(|a| a.account_id == agent.account_id())
                    .unwrap();
                let acquired = (account.position_qty - initial).abs();
                assert!(acquired > 0 && acquired <= 40);
                assert_eq!(data["completed_qty"].as_u64().unwrap(), acquired as u64);
                assert_eq!(data["deadline_reached"], true);
            }
        }
    }
}

#[test]
fn complete_population_recovers_exactly_across_saved_decision_and_partial_submissions() {
    let (mut baseline, mut baseline_state, mut baseline_id) = setup(7);
    for _ in 0..75 {
        baseline_state = run_scheduler_step(
            &mut baseline,
            &mut baseline_id,
            baseline_state,
            CrashPoint::None,
        )
        .unwrap()
        .state;
    }
    let baseline_prefix = baseline.clone();
    let prefix_state = baseline_state.clone();
    let prefix_id = baseline_id;
    for _ in 75..160 {
        baseline_state = run_scheduler_step(
            &mut baseline,
            &mut baseline_id,
            baseline_state,
            CrashPoint::None,
        )
        .unwrap()
        .state;
    }
    for crash in [
        CrashPoint::AfterDecisionPersist {
            step: 76,
            participant_index: 0,
        },
        CrashPoint::BeforeActionSubmit {
            step: 76,
            participant_index: 0,
            action_index: 0,
        },
        CrashPoint::AfterActionSubmit {
            step: 76,
            participant_index: 0,
            action_index: 0,
        },
    ] {
        let mut rooms = baseline_prefix.clone();
        let mut order_id = prefix_id;
        // Pick a participant with an action at this step, so submission crash
        // points are actually reached (rather than silently testing no crash).
        let mut probe = rooms.clone();
        let mut probe_id = order_id;
        let probe_result = run_scheduler_step(
            &mut probe,
            &mut probe_id,
            prefix_state.clone(),
            CrashPoint::AfterDecisionPersist {
                step: 76,
                participant_index: 0,
            },
        )
        .unwrap();
        let index = if probe_result.state.agents[0].unfinished_actions.is_empty() {
            prefix_state
                .agents
                .iter()
                .position(|a| a.template.bot_id() == "DynamicMarketMaker")
                .unwrap()
        } else {
            0
        };
        let crash = match crash {
            CrashPoint::AfterDecisionPersist { step, .. } => CrashPoint::AfterDecisionPersist {
                step,
                participant_index: index,
            },
            CrashPoint::BeforeActionSubmit {
                step, action_index, ..
            } => CrashPoint::BeforeActionSubmit {
                step,
                participant_index: index,
                action_index,
            },
            CrashPoint::AfterActionSubmit {
                step, action_index, ..
            } => CrashPoint::AfterActionSubmit {
                step,
                participant_index: index,
                action_index,
            },
            _ => unreachable!(),
        };
        let outcome =
            run_scheduler_step(&mut rooms, &mut order_id, prefix_state.clone(), crash).unwrap();
        assert!(outcome.crashed, "{crash:?}");
        let saved_room =
            serde_json::to_vec(rooms.simulation_room("population-test").unwrap()).unwrap();
        let saved_state = serde_json::to_vec(&outcome.state).unwrap();
        let history = rooms.execution_history("population-test").unwrap().to_vec();
        let mut restored = RoomManager::new();
        restored
            .restore_simulation_room(serde_json::from_slice(&saved_room).unwrap(), history)
            .unwrap();
        let mut state: SchedulerState = serde_json::from_slice(&saved_state).unwrap();
        state = run_scheduler_step(&mut restored, &mut order_id, state, CrashPoint::None)
            .unwrap()
            .state;
        for _ in 76..160 {
            state = run_scheduler_step(&mut restored, &mut order_id, state, CrashPoint::None)
                .unwrap()
                .state;
        }
        assert_eq!(order_id, baseline_id);
        assert_eq!(state, baseline_state);
        assert_eq!(
            restored.execution_history("population-test").unwrap(),
            baseline.execution_history("population-test").unwrap()
        );
        let mut actual =
            serde_json::to_value(restored.simulation_room("population-test").unwrap()).unwrap();
        let mut expected =
            serde_json::to_value(baseline.simulation_room("population-test").unwrap()).unwrap();
        canonical_snapshot(&mut actual);
        canonical_snapshot(&mut expected);
        if actual != expected {
            let receipts = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.local");
            std::fs::create_dir_all(&receipts).unwrap();
            std::fs::write(
                receipts.join("population-recovery-actual.json"),
                serde_json::to_vec_pretty(&actual).unwrap(),
            )
            .unwrap();
            std::fs::write(
                receipts.join("population-recovery-expected.json"),
                serde_json::to_vec_pretty(&expected).unwrap(),
            )
            .unwrap();
            panic!(
                "room snapshot mismatch after {crash:?}; JSON receipts in .local/population-recovery-*.json"
            );
        }
    }
}
