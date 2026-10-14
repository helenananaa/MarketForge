//! JSON-lines differential probe for fixed Pine ports. All execution stays in
//! the caller's authoritative observation; this example never submits orders.
use exchange_core::{
    AgentTemplate, BotConfig, BotDecisionRequest, BotDecisionResponse, BotRegistry,
    PersistedAgentKindState, ScheduledBot,
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    io::{self, BufRead},
};

#[derive(Deserialize)]
struct Input {
    request: BotDecisionRequest,
    #[serde(default)]
    reset: bool,
    #[serde(default)]
    restore: bool,
}

struct Session {
    template: AgentTemplate,
    bot: Box<dyn ScheduledBot>,
}

fn decide(
    registry: &BotRegistry,
    sessions: &mut BTreeMap<String, Session>,
    input: Input,
) -> Result<BotDecisionResponse, Box<dyn std::error::Error>> {
    if input.reset {
        sessions.clear();
    }
    let request = input.request;
    let template = AgentTemplate::Plugin(BotConfig {
        participant: request.participant.clone(),
        plugin_id: request.plugin_id.clone(),
        plugin_version: request.plugin_version.clone(),
        state_version: request.state_version,
        config_version: 1,
        seed: request.seed,
        config: request.config,
    });
    let state = PersistedAgentKindState::Plugin {
        plugin_id: request.plugin_id.clone(),
        plugin_version: request.plugin_version.clone(),
        state_version: request.state_version,
        data: request.state.into(),
    };
    let key = request.participant.participant_id;
    let mut session = sessions.remove(&key);
    // Speculative calls are reusable only when the caller supplied the same
    // committed state. A retry with older state always creates a fresh bot.
    let reusable = !input.restore
        && session
            .as_ref()
            .is_some_and(|session| session.template == template && session.bot.snapshot() == state);
    if !reusable {
        session = Some(Session {
            bot: registry.create(&template, &state)?,
            template,
        });
    } else {
        session.as_mut().unwrap().bot.prepare_reused_call(&state)?;
    }
    let mut session = session.unwrap();
    let actions = session.bot.decide(&request.observation)?;
    let PersistedAgentKindState::Plugin { data, .. } = session.bot.snapshot() else {
        return Err("native port returned a non-plugin snapshot".into());
    };
    sessions.insert(key, session);
    Ok(BotDecisionResponse {
        protocol_version: request.protocol_version,
        plugin_id: request.plugin_id,
        plugin_version: request.plugin_version,
        state_version: request.state_version,
        actions,
        state: (*data).clone(),
    })
}

fn main() {
    let registry = BotRegistry::with_builtins();
    let mut sessions = BTreeMap::new();
    for line in io::stdin().lock().lines() {
        let result = (|| {
            let input: Input = serde_json::from_str(&line?)?;
            decide(&registry, &mut sessions, input)
        })();
        let response = match result {
            Ok(response) => serde_json::json!({"response": response}),
            Err(error) => serde_json::json!({"error": error.to_string()}),
        };
        println!("{response}");
    }
}
