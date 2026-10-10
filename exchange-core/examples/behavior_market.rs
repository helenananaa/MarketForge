//! Run the exact web/HTTP recipe with a deterministic clock and retain receipts.
use exchange_core::*;
use std::collections::BTreeMap;

// Research runner: keep decision objects alive and retain every actual gateway
// execution, without materializing a crash checkpoint after each action. This
// does not substitute for the server's durable scheduler/recovery validation.
fn continuous_steps(
    rooms: &mut RoomManager,
    order_id: &mut u64,
    state: &mut SchedulerState,
    steps: u64,
) -> Result<(), String> {
    let registry = BotRegistry::with_builtins();
    let mut runtime = state
        .agents
        .iter()
        .map(|agent| registry.create(&agent.template, &agent.kind_state))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let metadata = state
        .agents
        .iter()
        .map(|agent| {
            Ok((
                registry.market_data_request(&agent.template)?,
                registry.related_instruments(&agent.template)?,
            ))
        })
        .collect::<Result<Vec<_>, BotError>>()
        .map_err(|e| e.to_string())?;
    for _ in 0..steps {
        rooms
            .advance_clock(&state.room_id, 1)
            .map_err(|e| format!("{e:?}"))?;
        for (index, agent) in runtime.iter_mut().enumerate() {
            let template = &state.agents[index].template;
            let config = template.config();
            let instrument = config.instrument_id.as_ref().ok_or("instrument required")?;
            let mut observation = rooms
                .bot_observation(
                    &state.room_id,
                    instrument,
                    config.account_id,
                    metadata[index].0,
                )
                .map_err(|e| format!("{e:?}"))?;
            rooms
                .enrich_bot_observation(&mut observation, &metadata[index].1, config.account_id)
                .map_err(|e| format!("{e:?}"))?;
            let actions = agent.decide(&observation).map_err(|e| e.to_string())?;
            if actions.len() > 64 {
                return Err("bot exceeded action limit".into());
            }
            for action in actions {
                let mut gateway = OrderGateway::new_scheduler(rooms, *order_id);
                gateway
                    .submit_action(GatewayRequest {
                        participant_id: template.participant_id().into(),
                        room_id: state.room_id.clone(),
                        instrument_id: Some(instrument.clone()),
                        account_id: config.account_id,
                        action,
                    })
                    .map_err(|e| format!("{e:?}"))?;
                *order_id = gateway.next_order_id();
            }
        }
    }
    for (agent, runtime) in state.agents.iter_mut().zip(runtime) {
        agent.kind_state = runtime.snapshot();
    }
    state.phase = SchedulerPhase::StepComplete {
        step: rooms
            .clock(&state.room_id)
            .map_err(|e| format!("{e:?}"))?
            .step(),
    };
    Ok(())
}

fn main() -> Result<(), String> {
    let seed = std::env::args()
        .nth(1)
        .map_or(Ok(7), |s| s.parse::<u64>())
        .map_err(|e| e.to_string())?;
    let source = std::env::args()
        .nth(2)
        .filter(|s| s != "-")
        .map(std::fs::read_to_string)
        .transpose()
        .map_err(|e| e.to_string())?;
    let steps = std::env::args()
        .nth(3)
        .map_or(Ok(300), |s| s.parse::<u64>())
        .map_err(|e| e.to_string())?;
    if !(1..=5000).contains(&steps) {
        return Err("steps must be within 1..5000".into());
    }
    let mut spec: population::BackgroundMarket = serde_json::from_str(
        source
            .as_deref()
            .unwrap_or(include_str!("../../scripts/fixtures/behavior_market.json")),
    )
    .map_err(|e| e.to_string())?;
    for template in &mut spec.agents {
        let AgentTemplate::Plugin(bot) = template else {
            unreachable!()
        };
        bot.seed = (child_seed(seed, &bot.participant.participant_id) & ((1u64 << 53) - 1)).max(1);
    }
    let owners: BTreeMap<_, _> = spec
        .agents
        .iter()
        .map(|a| {
            (
                (
                    a.config().account_id,
                    a.config().instrument_id.clone().unwrap(),
                ),
                a.participant_id().to_string(),
            )
        })
        .collect();
    let room_id = spec.scenario.room_id.clone();
    let mut rooms = RoomManager::new();
    rooms
        .create_room(spec.scenario)
        .map_err(|e| format!("{e:?}"))?;
    let before = rooms
        .net_worth_snapshot(&room_id)
        .map_err(|e| format!("{e:?}"))?;
    let mut scheduler = SchedulerState::new(&room_id, spec.agents, SchedulerMode::Manual);
    let mut order_id = 100000;
    let mode = std::env::args().nth(4).unwrap_or_else(|| "durable".into());
    if mode == "continuous" {
        continuous_steps(&mut rooms, &mut order_id, &mut scheduler, steps)?;
    } else if mode == "durable" {
        for _ in 0..steps {
            scheduler = run_scheduler_step(&mut rooms, &mut order_id, scheduler, CrashPoint::None)
                .map_err(|e| format!("{e:?}"))?
                .state;
        }
    } else {
        return Err("mode must be durable or continuous".into());
    }
    let mut fills = BTreeMap::<String, u64>::new();
    let mut rejections = BTreeMap::<String, u64>::new();
    let mut trades = BTreeMap::<String, u64>::new();
    let mut trade_receipts = Vec::new();
    for execution in rooms
        .execution_history(&room_id)
        .map_err(|e| format!("{e:?}"))?
    {
        let ActorExecutionResult::Accepted(market) = &execution.result else {
            *rejections
                .entry(format!("{:?}", execution.result))
                .or_default() += 1;
            continue;
        };
        let events = match market {
            MarketExecution::Spot(m) => &m.events,
            MarketExecution::Perp(m) => &m.events,
        };
        for e in events {
            match &e.event {
                Event::TradePrinted(t) => {
                    if t.maker_account_id == t.taker_account_id {
                        return Err("self trade".into());
                    }
                    *trades.entry(execution.instrument_id.clone()).or_default() += 1;
                    trade_receipts.push(
                        serde_json::json!({"instrument_id": execution.instrument_id,
                        "time_ms":execution.market_time_ms,"price_tick":t.price_tick,"qty":t.qty,
                        "taker_side":t.taker_side,"trade_id":t.trade_id,
                        "taker_order_id":t.taker_order_id}),
                    );
                    for account in [t.maker_account_id, t.taker_account_id] {
                        if let Some(name) = owners.get(&(account, execution.instrument_id.clone()))
                        {
                            *fills.entry(name.clone()).or_default() += t.qty;
                        }
                    }
                }
                Event::RiskRejected { .. }
                | Event::OrderRejected { .. }
                | Event::CancelRejected { .. } => {
                    *rejections.entry(format!("{:?}", e.event)).or_default() += 1;
                }
                _ => {}
            }
        }
    }
    println!("{}",serde_json::to_string_pretty(&serde_json::json!({
        "seed":seed,"steps":steps,"runner":mode,"simulation_time_ms":rooms.simulation_room(&room_id).map_err(|e|format!("{e:?}"))?.clock().market_time_ms(),"bots":scheduler.agents.len(),"trades":trades,"fills":fills,"trade_receipts":trade_receipts,"rejections":rejections,
        "assets_before":before,"assets_after":rooms.net_worth_snapshot(&room_id).map_err(|e|format!("{e:?}"))?,
        "states":scheduler.agents.iter().map(|a|serde_json::json!({"id":a.template.participant_id(),"state":a.kind_state})).collect::<Vec<_>>(),
        "qualification":"synthetic deterministic experiment; not real-market calibration",
    })).map_err(|e|e.to_string())?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn research_runner_matches_durable_scheduler_orders_funding_accounts_and_states() {
        for source in [
            include_str!("../../scripts/fixtures/behavior_market.json"),
            include_str!("../../scripts/fixtures/microstructure_market.json"),
        ] {
            let spec: population::BackgroundMarket = serde_json::from_str(source).unwrap();
            let room = spec.scenario.room_id.clone();
            let mut durable = RoomManager::new();
            durable.create_room(spec.scenario).unwrap();
            let mut continuous = durable.clone();
            let mut state = SchedulerState::new(&room, spec.agents, SchedulerMode::Manual);
            let mut research = state.clone();
            let mut durable_id = 100000;
            let mut research_id = 100000;
            for _ in 0..72 {
                state = run_scheduler_step(&mut durable, &mut durable_id, state, CrashPoint::None)
                    .unwrap()
                    .state;
            }
            continuous_steps(&mut continuous, &mut research_id, &mut research, 72).unwrap();
            assert_eq!(durable_id, research_id);
            assert_eq!(
                durable.execution_history(&room).unwrap(),
                continuous.execution_history(&room).unwrap()
            );
            assert_eq!(state, research);
            assert_eq!(
                durable.net_worth_snapshot(&room).unwrap(),
                continuous.net_worth_snapshot(&room).unwrap()
            );
        }
    }
}
