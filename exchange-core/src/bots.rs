//! Host-neutral bot factories and versioned plugin protocol.
use std::{collections::BTreeMap, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AgentTemplate, OrderAction, ParticipantConfig, ParticipantObservation, PersistedAgentKindState,
};

pub const BOT_PROTOCOL_VERSION: &str = "bot.v1";
pub const BOT_CONFIG_VERSION: u16 = 1;
pub const MAX_BOT_ACTIONS: usize = 64;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BotConfig {
    pub participant: ParticipantConfig,
    pub plugin_id: String,
    pub plugin_version: String,
    #[serde(default = "version_one")]
    pub state_version: u16,
    #[serde(default = "version_one")]
    pub config_version: u16,
    #[serde(default = "default_seed")]
    pub seed: u64,
    #[serde(default = "empty_object")]
    pub config: Value,
}

fn version_one() -> u16 {
    1
}
fn default_seed() -> u64 {
    1
}
fn empty_object() -> Value {
    serde_json::json!({})
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ParameterType {
    Integer,
    Boolean,
    String,
    Object,
    Array,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BotParameter {
    #[serde(rename = "type")]
    pub kind: ParameterType,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default)]
    pub minimum: Option<i64>,
    #[serde(default)]
    pub maximum: Option<i64>,
    #[serde(default)]
    pub choices: Vec<Value>,
}

impl BotParameter {
    fn validate(&self, name: &str, value: &Value) -> Result<(), BotError> {
        let typed = match self.kind {
            ParameterType::Integer => value.is_i64() || value.is_u64(),
            ParameterType::Boolean => value.is_boolean(),
            ParameterType::String => value.is_string(),
            ParameterType::Object => value.is_object(),
            ParameterType::Array => value.is_array(),
        };
        if !typed {
            return Err(BotError(format!("parameter {name}: wrong type")));
        }
        if matches!(self.kind, ParameterType::Integer) {
            let number = value
                .as_i64()
                .map(i128::from)
                .or_else(|| value.as_u64().map(i128::from))
                .unwrap();
            if self.minimum.is_some_and(|min| number < i128::from(min))
                || self.maximum.is_some_and(|max| number > i128::from(max))
            {
                return Err(BotError(format!("parameter {name}: outside allowed range")));
            }
        }
        if !self.choices.is_empty() && !self.choices.contains(value) {
            return Err(BotError(format!("parameter {name}: unsupported value")));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BotDescriptor {
    pub id: String,
    pub name: String,
    pub version: String,
    pub protocol_version: String,
    pub state_version: u16,
    pub runtime: String,
    #[serde(default)]
    pub parameters: BTreeMap<String, BotParameter>,
}

impl BotDescriptor {
    pub fn validate(&self) -> Result<(), BotError> {
        if self.id.is_empty()
            || self.version.is_empty()
            || self.state_version == 0
            || self.protocol_version != BOT_PROTOCOL_VERSION
        {
            return Err(BotError(format!("invalid bot descriptor: {}", self.id)));
        }
        for (name, parameter) in &self.parameters {
            if parameter
                .minimum
                .zip(parameter.maximum)
                .is_some_and(|(min, max)| min > max)
                || (!matches!(parameter.kind, ParameterType::Integer)
                    && (parameter.minimum.is_some() || parameter.maximum.is_some()))
            {
                return Err(BotError(format!("invalid bounds for parameter {name}")));
            }
            for choice in &parameter.choices {
                parameter.validate(name, choice)?;
            }
            if let Some(default) = &parameter.default {
                parameter.validate(name, default)?;
            }
        }
        Ok(())
    }

    pub fn validate_config(&self, config: &Value) -> Result<Value, BotError> {
        let mut values = config
            .as_object()
            .cloned()
            .ok_or_else(|| BotError("bot config must be an object".into()))?;
        for name in values.keys() {
            if !self.parameters.contains_key(name) {
                return Err(BotError(format!("unknown parameter: {name}")));
            }
        }
        for (name, parameter) in &self.parameters {
            if !values.contains_key(name) {
                if let Some(default) = &parameter.default {
                    values.insert(name.clone(), default.clone());
                } else if parameter.required {
                    return Err(BotError(format!("missing parameter: {name}")));
                }
            }
            if let Some(value) = values.get(name) {
                parameter.validate(name, value)?;
            }
        }
        Ok(Value::Object(values))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BotError(pub String);
impl std::fmt::Display for BotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for BotError {}

/// Decisions must depend only on observation, config, seed and saved state.
/// The host journals returned actions; bots never submit orders themselves.
pub trait ScheduledBot: Send {
    fn decide(
        &mut self,
        observation: &ParticipantObservation,
    ) -> Result<Vec<OrderAction>, BotError>;
    fn snapshot(&self) -> PersistedAgentKindState;
}

/// The host can apply domain policies before submission and update projections
/// after each execution. Validation also runs for recovered unfinished actions.
pub trait BotExecutionPolicy {
    fn before_action(
        &self,
        request: &crate::GatewayRequest,
        observation: &ParticipantObservation,
    ) -> Result<(), BotError>;
    fn after_action(
        &mut self,
        execution: &crate::GatewayExecution,
        book_before: &crate::BookSnapshot,
    );
}

struct UnrestrictedBotPolicy;
impl BotExecutionPolicy for UnrestrictedBotPolicy {
    fn before_action(
        &self,
        _: &crate::GatewayRequest,
        _: &ParticipantObservation,
    ) -> Result<(), BotError> {
        Ok(())
    }
    fn after_action(&mut self, _: &crate::GatewayExecution, _: &crate::BookSnapshot) {}
}

pub(crate) fn unrestricted_policy() -> impl BotExecutionPolicy {
    UnrestrictedBotPolicy
}

pub trait BotFactory: Send + Sync {
    fn descriptor(&self) -> &BotDescriptor;
    fn create(
        &self,
        template: &AgentTemplate,
        state: &PersistedAgentKindState,
    ) -> Result<Box<dyn ScheduledBot>, BotError>;
}

#[derive(Clone, Default)]
pub struct BotRegistry {
    factories: BTreeMap<String, Arc<dyn BotFactory>>,
}
impl BotRegistry {
    pub fn with_builtins() -> Self {
        let mut registry = Self::default();
        crate::agents::register_builtin_bots(&mut registry);
        registry
    }
    pub fn register(&mut self, factory: impl BotFactory + 'static) -> Result<(), BotError> {
        let descriptor = factory.descriptor();
        descriptor.validate()?;
        if self.factories.contains_key(&descriptor.id) {
            return Err(BotError(format!("duplicate bot id: {}", descriptor.id)));
        }
        self.factories
            .insert(descriptor.id.clone(), Arc::new(factory));
        Ok(())
    }
    pub fn descriptors(&self) -> Vec<BotDescriptor> {
        self.factories
            .values()
            .map(|factory| factory.descriptor().clone())
            .collect()
    }
    pub fn create(
        &self,
        template: &AgentTemplate,
        state: &PersistedAgentKindState,
    ) -> Result<Box<dyn ScheduledBot>, BotError> {
        let factory = self
            .factories
            .get(template.bot_id())
            .ok_or_else(|| BotError(format!("unknown bot: {}", template.bot_id())))?;
        if let AgentTemplate::Plugin(config) = template {
            let descriptor = factory.descriptor();
            if config.plugin_version != descriptor.version
                || config.state_version != descriptor.state_version
                || config.config_version != BOT_CONFIG_VERSION
            {
                return Err(BotError(format!(
                    "bot {} version mismatch",
                    config.plugin_id
                )));
            }
            match state {
                PersistedAgentKindState::Plugin {
                    plugin_id,
                    plugin_version,
                    state_version,
                    ..
                } if plugin_id == &config.plugin_id
                    && plugin_version == &config.plugin_version
                    && state_version == &config.state_version => {}
                _ => {
                    return Err(BotError(format!(
                        "bot {} state identity mismatch",
                        config.plugin_id
                    )));
                }
            }
        }
        let mut normalized = template.clone();
        if let AgentTemplate::Plugin(config) = &mut normalized {
            config.config = factory.descriptor().validate_config(&config.config)?;
        }
        factory.create(&normalized, state)
    }
    pub fn validate_template(&self, template: &AgentTemplate) -> Result<(), BotError> {
        self.create(template, &template.initial_state()).map(|_| ())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BotDecisionRequest {
    pub protocol_version: String,
    pub plugin_id: String,
    pub plugin_version: String,
    pub state_version: u16,
    pub participant: ParticipantConfig,
    pub seed: u64,
    pub config: Value,
    pub state: Value,
    pub observation: ParticipantObservation,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BotDecisionResponse {
    pub protocol_version: String,
    pub plugin_id: String,
    pub plugin_version: String,
    pub state_version: u16,
    pub actions: Vec<OrderAction>,
    pub state: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NoiseTraderConfig, ParticipantKind};
    fn participant() -> ParticipantConfig {
        ParticipantConfig {
            participant_id: "bot-1".into(),
            kind: ParticipantKind::RuleAgent,
            room_id: "room".into(),
            account_id: 20,
            instrument_id: Some("V-BTC-SPOT".into()),
        }
    }
    #[test]
    fn all_builtins_accept_unified_config_and_legacy_state_still_serializes() {
        let registry = BotRegistry::with_builtins();
        assert_eq!(registry.descriptors().len(), 5);
        for descriptor in registry.descriptors() {
            let template = AgentTemplate::Plugin(BotConfig {
                participant: participant(),
                plugin_id: descriptor.id.clone(),
                plugin_version: descriptor.version,
                state_version: 1,
                config_version: 1,
                seed: 7,
                config: serde_json::json!({}),
            });
            let bot = registry
                .create(&template, &template.initial_state())
                .unwrap();
            let state = bot.snapshot();
            let decoded: PersistedAgentKindState =
                serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
            registry.create(&template, &decoded).unwrap();
        }
        let legacy = AgentTemplate::NoiseTrader(NoiseTraderConfig {
            participant: participant(),
            seed: 7,
            reference_price_tick: 100,
            price_radius_ticks: 3,
            max_qty: 2,
            market_order_ratio_ppm: 0,
        });
        let value = serde_json::to_value(&legacy).unwrap();
        assert!(value.get("NoiseTrader").is_some());
        assert_eq!(
            legacy.initial_state(),
            PersistedAgentKindState::Noise { rng_state: 7 }
        );
        registry.validate_template(&legacy).unwrap();
    }
    #[test]
    fn config_defaults_bounds_choices_and_unknown_fields_are_enforced() {
        let descriptor: BotDescriptor = serde_json::from_value(serde_json::json!({
            "id":"test", "name":"Test", "version":"1", "protocol_version":"bot.v1", "state_version":1, "runtime":"process",
            "parameters": {"qty":{"type":"integer","required":true,"minimum":1,"maximum":10},
                "side":{"type":"string","default":"Buy","choices":["Buy","Sell"]}}
        })).unwrap();
        descriptor.validate().unwrap();
        assert_eq!(
            descriptor
                .validate_config(&serde_json::json!({"qty":2}))
                .unwrap(),
            serde_json::json!({"qty":2,"side":"Buy"})
        );
        for value in [
            serde_json::json!({}),
            serde_json::json!({"qty":0}),
            serde_json::json!({"qty":11}),
            serde_json::json!({"qty":true}),
            serde_json::json!({"qty":2,"side":"Other"}),
            serde_json::json!({"qty":2,"command":"sh"}),
        ] {
            assert!(descriptor.validate_config(&value).is_err(), "{value}");
        }
        let mut invalid = descriptor;
        invalid.parameters.get_mut("qty").unwrap().default = Some(0.into());
        assert!(invalid.validate().is_err());
    }
    #[test]
    fn unknown_bots_versions_and_state_identity_fail_before_decision() {
        let registry = BotRegistry::with_builtins();
        let mut config = BotConfig {
            participant: participant(),
            plugin_id: "missing".into(),
            plugin_version: "1".into(),
            state_version: 1,
            config_version: 1,
            seed: 1,
            config: serde_json::json!({}),
        };
        assert!(
            registry
                .validate_template(&AgentTemplate::Plugin(config.clone()))
                .is_err()
        );
        config.plugin_id = "DcaTrader".into();
        config.plugin_version = "2".into();
        assert!(
            registry
                .validate_template(&AgentTemplate::Plugin(config.clone()))
                .is_err()
        );
        config.plugin_version = "1".into();
        let template = AgentTemplate::Plugin(config);
        assert!(
            registry
                .create(
                    &template,
                    &PersistedAgentKindState::Grid {
                        has_seeded_grid: false
                    }
                )
                .is_err()
        );
        registry.validate_template(&template).unwrap();
    }
}
