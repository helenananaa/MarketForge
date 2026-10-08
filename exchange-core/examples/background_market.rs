//! Export the shared demo recipe or run an isolated deterministic experiment.
use exchange_core::{
    ActorExecutionResult, CrashPoint, Event, MarketExecution, RoomManager, SchedulerMode,
    SchedulerState, run_scheduler_step,
};
use std::collections::BTreeMap;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let seed = if args.first().is_some_and(|a| a == "--export") {
        7
    } else {
        args.get(1).map_or(Ok(7), |s| s.parse::<u64>())?
    };
    let recipe = exchange_core::population::background_market("background-market", seed);
    if let [flag, path] = args.as_slice()
        && flag == "--export"
    {
        std::fs::write(path, serde_json::to_string_pretty(&recipe)? + "\n")?;
        return Ok(());
    }
    let steps = args.first().map_or(Ok(300), |s| s.parse::<u64>())?;
    let owners: BTreeMap<_, _> = recipe
        .agents
        .iter()
        .map(|a| (a.config().account_id, a.participant_id().to_string()))
        .collect();
    let mut rooms = RoomManager::new();
    rooms
        .create_room(recipe.scenario)
        .map_err(|e| format!("{e:?}"))?;
    let mut scheduler =
        SchedulerState::new("background-market", recipe.agents, SchedulerMode::Manual);
    let mut order_id = 100_000;
    let mut activity = BTreeMap::<String, u64>::new();
    let mut taker_activity = BTreeMap::<String, u64>::new();
    let mut rejection_reasons = BTreeMap::<String, u64>::new();
    let mut prices = vec![];
    let mut rejects = 0;
    let mut cancels = 0;
    for _ in 0..steps {
        let result = run_scheduler_step(&mut rooms, &mut order_id, scheduler, CrashPoint::None)
            .map_err(|e| format!("{e:?}"))?;
        scheduler = result.state;
    }
    for execution in rooms
        .execution_history("background-market")
        .map_err(|e| format!("{e:?}"))?
    {
        let ActorExecutionResult::Accepted(market) = &execution.result else {
            rejects += 1;
            *rejection_reasons
                .entry(format!("{:?}", execution.result))
                .or_default() += 1;
            continue;
        };
        let events = match market {
            MarketExecution::Spot(e) => &e.events,
            MarketExecution::Perp(e) => &e.events,
        };
        for record in events {
            match &record.event {
                Event::TradePrinted(trade) => {
                    prices.push(trade.price_tick);
                    for account in [trade.maker_account_id, trade.taker_account_id] {
                        if let Some(name) = owners.get(&account) {
                            *activity.entry(name.clone()).or_default() += trade.qty;
                        }
                    }
                    if let Some(name) = owners.get(&trade.taker_account_id) {
                        *taker_activity.entry(name.clone()).or_default() += trade.qty;
                    }
                }
                Event::OrderCanceled { .. } => cancels += 1,
                Event::OrderRejected { .. }
                | Event::RiskRejected { .. }
                | Event::CancelRejected { .. } => {
                    rejects += 1;
                    *rejection_reasons
                        .entry(format!("{:?}", record.event))
                        .or_default() += 1;
                }
                _ => {}
            }
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "seed":seed,"steps":steps,"market_time_ms":rooms.clock("background-market").map_err(|e| format!("{e:?}"))?.market_time_ms(),
            "agents":scheduler.agents.len(),"trades":prices.len(),"cancellations":cancels,"rejections":rejects,
            "min_price":prices.iter().min(),"max_price":prices.iter().max(),"last_price":prices.last(),
            "fill_qty_by_bot":activity,"taker_qty_by_bot":taker_activity,"rejection_reasons":rejection_reasons,
            "states":scheduler.agents.iter().map(|a| serde_json::json!({"id":a.template.participant_id(),"state":a.kind_state})).collect::<Vec<_>>(),
            "qualification":"synthetic deterministic experiment; not calibrated to a real market",
        }))?
    );
    Ok(())
}
