//! Fixed-input comparison: restore every decision versus native instance reuse.
//! This isolates runtime construction; it is not a market throughput benchmark.
use exchange_core::{BotRegistry, ParticipantObservation, RoomManager, ScheduledBot};
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let count: usize = std::env::args().nth(1).unwrap_or("1000".into()).parse()?;
    let rounds: u64 = std::env::args().nth(2).unwrap_or("64".into()).parse()?;
    assert!(count > 0 && rounds > 0);
    let spec: exchange_core::population::BackgroundMarket = serde_json::from_str(include_str!(
        "../../scripts/fixtures/microstructure_market.json"
    ))?;
    let room = spec.scenario.room_id.clone();
    let mut rooms = RoomManager::new();
    rooms
        .create_room(spec.scenario)
        .map_err(|e| format!("{e:?}"))?;
    let registry = BotRegistry::with_builtins();
    let templates: Vec<_> = (0..count)
        .map(|i| spec.agents[i % spec.agents.len()].clone())
        .collect();
    let observations: Vec<ParticipantObservation> = templates
        .iter()
        .map(|template| {
            assert!(registry.supports_instance_reuse(template));
            rooms
                .observation_batch(&room)
                .bot_observation(
                    template.config().instrument_id.as_deref().unwrap(),
                    template.config().account_id,
                    registry.market_data_request(template).unwrap(),
                    &registry.related_instruments(template).unwrap(),
                )
                .unwrap()
        })
        .collect();
    let mut oracle = None;
    let mut reports = Vec::new();
    // Interleaving reduces first-run and thermal bias; every run gets fresh state.
    for reuse in [false, true, true, false] {
        let mut states: Vec<_> = templates.iter().map(|t| t.initial_state()).collect();
        let mut instances: Vec<Option<Box<dyn ScheduledBot>>> = (0..count).map(|_| None).collect();
        let mut outputs = Vec::with_capacity(count * rounds as usize);
        let started = Instant::now();
        for step in 1..=rounds {
            for (i, template) in templates.iter().enumerate() {
                let mut observation = observations[i].clone();
                observation.step = step;
                observation.market_time_ms = step * 1000;
                for related in &mut observation.related_markets {
                    related.step = step;
                    related.market_time_ms = step * 1000;
                }
                if !reuse || instances[i].is_none() {
                    instances[i] = Some(registry.create(template, &states[i])?);
                }
                let bot = instances[i].as_mut().unwrap();
                outputs.push(bot.decide(&observation)?);
                states[i] = bot.snapshot();
            }
        }
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let actions: usize = outputs.iter().map(Vec::len).sum();
        let receipt = (outputs, states);
        if let Some(oracle) = &oracle {
            assert_eq!(
                &receipt, oracle,
                "actions and final persisted state must be identical"
            );
        } else {
            oracle = Some(receipt);
        }
        reports.push(
            serde_json::json!({"reuse": reuse, "elapsed_ms": elapsed_ms, "actions": actions}),
        );
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "bots": count, "rounds": rounds, "decisions_per_run": count as u64 * rounds,
            "exact_actions_and_final_states_equal": true, "runs": reports,
            "scope": "fixed observations; includes snapshot and observation clone; excludes order execution and journaling"
        }))?
    );
    Ok(())
}
