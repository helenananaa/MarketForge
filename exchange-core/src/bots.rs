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
pub const MAX_BOT_MARKET_DATA_BARS: usize = 4096;

/// A committed consumer cursor. The host must authenticate its digest against
/// the complete authoritative history before reducing an observation window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BotHistoryCursor {
    pub instrument_id: String,
    pub interval_ms: u64,
    pub count: u64,
    pub digest: [u8; 32],
}

/// Optional, bounded public history requested by an installed factory.
#[derive(Clone, Copy, Debug)]
pub struct BotMarketDataRequest {
    pub interval_ms: u64,
    pub max_bars: usize,
}

impl BotMarketDataRequest {
    pub fn validate(&self) -> Result<(), BotError> {
        if self.interval_ms == 0 || !(1..=MAX_BOT_MARKET_DATA_BARS).contains(&self.max_bars) {
            return Err(BotError("invalid bot market data bounds".into()));
        }
        Ok(())
    }
}

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
    fn decide_shared(
        &mut self,
        observation: &crate::observation::SharedParticipantObservation,
    ) -> Result<Vec<OrderAction>, BotError> {
        self.decide(&observation.to_owned_observation())
    }
    fn snapshot(&self) -> PersistedAgentKindState;
    /// Restore state released by `take_snapshot` before a cached instance runs.
    /// The scheduler supplies the exact committed state after validating reuse.
    fn prepare_reused_call(&mut self, _state: &PersistedAgentKindState) -> Result<(), BotError> {
        Ok(())
    }
    /// Transfer a decision's state to the scheduler. Native bots retain their
    /// normal snapshot behavior; transport adapters may release large payloads.
    fn take_snapshot(&mut self) -> PersistedAgentKindState {
        self.snapshot()
    }
    fn execution_feedback(&mut self, _action: &OrderAction, _execution: &crate::GatewayExecution) {}
}

/// Resolve metadata without permitting a plugin to change room or account.
pub fn resolve_bot_order<'a>(
    primary: &'a str,
    action: &'a OrderAction,
) -> Result<(&'a str, &'a OrderAction), BotError> {
    match action {
        OrderAction::BotOrder {
            instrument_id,
            key,
            action,
        } => {
            if instrument_id.is_empty()
                || instrument_id.len() > 256
                || key.is_empty()
                || key.len() > 256
                || matches!(**action, OrderAction::BotOrder { .. })
            {
                return Err(BotError("invalid or nested bot order envelope".into()));
            }
            Ok((instrument_id, action))
        }
        _ => Ok((primary, action)),
    }
}

/// Validate the entire decision before any action is applied.
pub fn validate_bot_orders(
    primary: &str,
    related: &[String],
    actions: &[OrderAction],
    state: &PersistedAgentKindState,
) -> Result<(), BotError> {
    for action in actions {
        let (instrument, _) = resolve_bot_order(primary, action)?;
        if instrument != primary && !related.iter().any(|i| i == instrument) {
            return Err(BotError(
                "bot order targets an undeclared instrument".into(),
            ));
        }
        if matches!(action, OrderAction::BotOrder { .. })
            && !matches!(state, PersistedAgentKindState::Plugin { data, .. } if data.is_object())
        {
            return Err(BotError("tracked bot orders require object state".into()));
        }
    }
    Ok(())
}

/// Receipts live in the same journal transaction as actions and plugin state.
/// Plugins consume _host_receipts on their next decision. No guessed order IDs.
pub fn record_bot_execution(
    state: &mut PersistedAgentKindState,
    action: &OrderAction,
    execution: &crate::GatewayExecution,
) {
    let OrderAction::BotOrder { key, .. } = action else {
        return;
    };
    let PersistedAgentKindState::Plugin { data, .. } = state else {
        return;
    };
    use crate::{ActorExecutionResult, Command, Event, MarketExecution};
    let order_id = match &execution.command {
        Command::NewOrder(order) => Some(order.order_id),
        Command::NewOrderWithProtection { order, .. } => Some(order.order_id),
        Command::CancelOrder(order) => Some(order.order_id),
        Command::AmendOrder(order) => Some(order.order_id),
        _ => None,
    };
    let (events, mut error) = match &execution.execution.result {
        ActorExecutionResult::Accepted(MarketExecution::Spot(r)) => (r.events.as_slice(), None),
        ActorExecutionResult::Accepted(MarketExecution::Perp(r)) => (r.events.as_slice(), None),
        ActorExecutionResult::Rejected(reason) => (&[][..], Some(format!("{reason:?}"))),
    };
    for event in events {
        match &event.event {
            Event::OrderRejected { .. }
            | Event::RiskRejected { .. }
            | Event::CancelRejected { .. }
            | Event::AmendRejected { .. } => error = Some(format!("{:?}", event.event)),
            _ => {}
        }
    }
    let receipt = serde_json::json!({"key": key, "instrument_id": execution.instrument_id,
        "command_seq": execution.execution.command_seq, "market_time_ms": execution.execution.market_time_ms,
        "order_id": order_id, "action": execution.action, "accepted": error.is_none(), "error": error});
    if let Some(object) = Arc::make_mut(data).as_object_mut() {
        let receipts = object
            .entry("_host_receipts")
            .or_insert_with(|| serde_json::json!([]));
        if let Some(receipts) = receipts.as_array_mut() {
            receipts.push(receipt);
            if receipts.len() > MAX_BOT_ACTIONS {
                receipts.drain(..receipts.len() - MAX_BOT_ACTIONS);
            }
        }
    }
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

/// An opt-in conservative prefilter. Book-driven and per-tick bots bypass it.
#[derive(Clone, Copy, Debug)]
pub struct BotEventFilter {
    pub closed_bars: bool,
    pub account: bool,
    pub interval_ms: Option<u64>,
}

pub trait BotFactory: Send + Sync {
    fn descriptor(&self) -> &BotDescriptor;
    fn event_filter(&self) -> Option<BotEventFilter> {
        None
    }
    /// Pure host scheduling policy. Defaults preserve legacy per-tick execution.
    fn decision_is_due(
        &self,
        _template: &AgentTemplate,
        _state: &PersistedAgentKindState,
        _observation: &ParticipantObservation,
    ) -> Result<bool, BotError> {
        Ok(true)
    }
    fn supports_plugin_version(&self, version: &str) -> bool {
        self.descriptor().version == version
    }
    /// Opt in only when snapshot/restore captures every stateful decision input.
    /// Reusing an instance must be equivalent to restoring it before each call;
    /// observation metadata must depend only on the template. Plugins default off.
    fn supports_instance_reuse(&self) -> bool {
        false
    }
    /// Only short, trusted native decisions opt in. External/process factories
    /// retain their own task so a slow plugin cannot hold a native batch.
    fn supports_native_batching(&self) -> bool {
        false
    }
    /// Metadata only: must not execute plugin code or perform IO.
    fn market_data_request(
        &self,
        _template: &AgentTemplate,
    ) -> Result<Option<BotMarketDataRequest>, BotError> {
        Ok(None)
    }
    /// Optional committed cursors for a runtime that supports authenticated
    /// suffixes. Unlike static observation metadata, this is read every turn.
    fn checkpoint_history_cursors(
        &self,
        _template: &AgentTemplate,
        _state: &PersistedAgentKindState,
    ) -> Result<Option<Vec<BotHistoryCursor>>, BotError> {
        Ok(None)
    }
    fn related_instruments(&self, _template: &AgentTemplate) -> Result<Vec<String>, BotError> {
        Ok(vec![])
    }
    fn additional_intervals(&self, _template: &AgentTemplate) -> Result<Vec<u64>, BotError> {
        Ok(vec![])
    }
    fn account_events(&self) -> bool {
        false
    }
    /// False when decisions never read `own_fills`/`fill_details`; a host may
    /// then leave them empty instead of copying the account's fill history.
    fn reads_own_fills(&self) -> bool {
        true
    }
    /// How many decisions the factory can run at once, such as its worker
    /// processes; a host may size its dispatch threads to it. Metadata only.
    fn parallel_calls(&self) -> Option<usize> {
        None
    }
    fn create(
        &self,
        template: &AgentTemplate,
        state: &PersistedAgentKindState,
    ) -> Result<Box<dyn ScheduledBot>, BotError>;
}

#[derive(Clone, Default)]
pub struct BotRegistry {
    factories: Arc<BTreeMap<String, Arc<dyn BotFactory>>>,
}
impl BotRegistry {
    pub fn event_filter(&self, template: &AgentTemplate) -> Option<BotEventFilter> {
        self.factories
            .get(template.bot_id())
            .and_then(|f| f.event_filter())
    }
    pub fn account_events(&self, template: &AgentTemplate) -> bool {
        self.factories
            .get(template.bot_id())
            .is_some_and(|f| f.account_events())
    }
    pub fn reads_own_fills(&self, template: &AgentTemplate) -> bool {
        self.factories
            .get(template.bot_id())
            .is_none_or(|f| f.reads_own_fills())
    }
    pub fn parallel_calls(&self, template: &AgentTemplate) -> Option<usize> {
        self.factories
            .get(template.bot_id())
            .and_then(|f| f.parallel_calls())
    }
    pub fn additional_intervals(&self, template: &AgentTemplate) -> Result<Vec<u64>, BotError> {
        let factory = self
            .factories
            .get(template.bot_id())
            .ok_or_else(|| BotError("unknown bot".into()))?;
        let intervals = factory.additional_intervals(template)?;
        if intervals.len() > 8
            || intervals.contains(&0)
            || intervals
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != intervals.len()
        {
            return Err(BotError("invalid additional intervals".into()));
        }
        Ok(intervals)
    }
    pub fn with_builtins() -> Self {
        let mut registry = Self::default();
        crate::agents::register_builtin_bots(&mut registry);
        crate::market_bots::register_market_bots(&mut registry);
        crate::native_pine::register_native_pine_bots(&mut registry);
        crate::technical_trader::register(&mut registry);
        crate::adaptive_liquidity::register(&mut registry);
        crate::endogenous_value::register(&mut registry);
        crate::style_switching::register(&mut registry);
        crate::basis_link::register(&mut registry);
        crate::reactive_flow::register(&mut registry);
        registry
    }
    pub fn register(&mut self, factory: impl BotFactory + 'static) -> Result<(), BotError> {
        let descriptor = factory.descriptor();
        descriptor.validate()?;
        if self.factories.contains_key(&descriptor.id) {
            return Err(BotError(format!("duplicate bot id: {}", descriptor.id)));
        }
        Arc::make_mut(&mut self.factories).insert(descriptor.id.clone(), Arc::new(factory));
        Ok(())
    }
    pub fn descriptors(&self) -> Vec<BotDescriptor> {
        self.factories
            .values()
            .map(|factory| factory.descriptor().clone())
            .collect()
    }
    pub fn supports_instance_reuse(&self, template: &AgentTemplate) -> bool {
        self.factories
            .get(template.bot_id())
            .is_some_and(|factory| factory.supports_instance_reuse())
    }

    pub fn supports_native_batching(&self, template: &AgentTemplate) -> bool {
        self.factories
            .get(template.bot_id())
            .is_some_and(|factory| factory.supports_native_batching())
    }

    pub fn decision_is_due(
        &self,
        template: &AgentTemplate,
        state: &PersistedAgentKindState,
        observation: &ParticipantObservation,
    ) -> Result<bool, BotError> {
        self.factories
            .get(template.bot_id())
            .ok_or_else(|| BotError(format!("unknown bot: {}", template.bot_id())))?
            .decision_is_due(template, state, observation)
    }

    /// Identity check only; never calls factory/plugin code in the writer lane.
    pub fn same_factory(&self, template: &AgentTemplate, previous: &Self) -> bool {
        self.factories
            .get(template.bot_id())
            .zip(previous.factories.get(template.bot_id()))
            .is_some_and(|(current, old)| Arc::ptr_eq(current, old))
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
            if !factory.supports_plugin_version(&config.plugin_version)
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
    pub fn related_instruments(&self, template: &AgentTemplate) -> Result<Vec<String>, BotError> {
        let factory = self
            .factories
            .get(template.bot_id())
            .ok_or_else(|| BotError("unknown bot".into()))?;
        let instruments = factory.related_instruments(template)?;
        if instruments.len() > 8
            || instruments.iter().any(|i| i.is_empty() || i.len() > 256)
            || instruments
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != instruments.len()
        {
            return Err(BotError("invalid related instruments".into()));
        }
        Ok(instruments)
    }

    pub fn validate_template(&self, template: &AgentTemplate) -> Result<(), BotError> {
        let intervals = self.additional_intervals(template)?;
        let related = self.related_instruments(template)?;
        if (intervals.len() + 1) * (related.len() + 1) > 16 {
            return Err(BotError("too many bot data streams".into()));
        }
        self.related_instruments(template)?;
        self.market_data_request(template)?;
        self.create(template, &template.initial_state()).map(|_| ())
    }

    pub fn market_data_request(
        &self,
        template: &AgentTemplate,
    ) -> Result<Option<BotMarketDataRequest>, BotError> {
        let factory = self
            .factories
            .get(template.bot_id())
            .ok_or_else(|| BotError(format!("unknown bot: {}", template.bot_id())))?;
        let request = factory.market_data_request(template)?;
        if let Some(request) = request {
            request.validate()?;
        }
        Ok(request)
    }

    pub fn checkpoint_history_cursors(
        &self,
        template: &AgentTemplate,
        state: &PersistedAgentKindState,
    ) -> Result<Option<Vec<BotHistoryCursor>>, BotError> {
        let factory = self
            .factories
            .get(template.bot_id())
            .ok_or_else(|| BotError(format!("unknown bot: {}", template.bot_id())))?;
        let Some(cursors) = factory.checkpoint_history_cursors(template, state)? else {
            return Ok(None);
        };
        let Some(history) = self.market_data_request(template)? else {
            return Err(BotError("checkpoint cursors require market data".into()));
        };
        let primary = template
            .config()
            .instrument_id
            .as_deref()
            .ok_or_else(|| BotError("checkpoint cursors require an instrument".into()))?;
        let instruments = std::iter::once(primary.to_string())
            .chain(self.related_instruments(template)?)
            .collect::<std::collections::BTreeSet<_>>();
        let intervals = std::iter::once(history.interval_ms)
            .chain(self.additional_intervals(template)?)
            .collect::<std::collections::BTreeSet<_>>();
        let expected = instruments
            .iter()
            .flat_map(|instrument| {
                intervals
                    .iter()
                    .map(move |interval| (instrument.clone(), *interval))
            })
            .collect::<std::collections::BTreeSet<_>>();
        let actual = cursors
            .iter()
            .map(|cursor| (cursor.instrument_id.clone(), cursor.interval_ms))
            .collect::<std::collections::BTreeSet<_>>();
        if cursors.is_empty()
            || cursors.len() > 16
            || actual.len() != cursors.len()
            || actual != expected
        {
            return Err(BotError(
                "checkpoint cursors must cover every requested stream exactly once".into(),
            ));
        }
        Ok(Some(cursors))
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
    use crate::{NoiseTraderConfig, ParticipantKind, TradingApi};
    #[test]
    fn bot_envelopes_reject_nested_and_undeclared_targets_before_submission() {
        let action = OrderAction::BotOrder {
            instrument_id: "peer".into(),
            key: "leg".into(),
            action: Box::new(OrderAction::PlaceLimit {
                side: crate::Side::Buy,
                price_tick: 100,
                qty: 1,
            }),
        };
        let state = PersistedAgentKindState::Plugin {
            plugin_id: "test".into(),
            plugin_version: "1".into(),
            state_version: 1,
            data: serde_json::json!({}).into(),
        };
        assert!(
            validate_bot_orders("primary", &[], std::slice::from_ref(&action), &state).is_err()
        );
        assert!(
            validate_bot_orders(
                "primary",
                &["peer".into()],
                std::slice::from_ref(&action),
                &state
            )
            .is_ok()
        );
        let nested = OrderAction::BotOrder {
            instrument_id: "peer".into(),
            key: "outer".into(),
            action: Box::new(action),
        };
        assert!(resolve_bot_order("primary", &nested).is_err());
    }
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
    fn reusable_builtins_match_snapshot_restore_on_every_observation() {
        let registry = BotRegistry::with_builtins();
        let spec: crate::population::BackgroundMarket = serde_json::from_str(include_str!(
            "../../scripts/fixtures/microstructure_market.json"
        ))
        .unwrap();
        let room = spec.scenario.room_id.clone();
        let mut actions_seen = 0;
        let mut next_order_id = 1_000_000;
        for descriptor in registry.descriptors() {
            // Keep each factory's input account/history independent. Earlier
            // factories can leave resting orders and self trades; the Pine
            // broker domain correctly refuses those invalid own-fill receipts.
            let mut rooms = crate::RoomManager::new();
            rooms.create_room(spec.scenario.clone()).unwrap();
            let instrument = if matches!(
                descriptor.id.as_str(),
                "FundingRateTrader" | "LeveragedTrendTrader"
            ) {
                "V-BTC-PERP"
            } else {
                "V-BTC-SPOT"
            };
            let template = AgentTemplate::Plugin(BotConfig {
                participant: ParticipantConfig {
                    room_id: room.clone(),
                    instrument_id: Some(instrument.into()),
                    ..participant()
                },
                plugin_id: descriptor.id.clone(),
                plugin_version: descriptor.version,
                state_version: 1,
                config_version: 1,
                seed: 7,
                config: if descriptor.id.starts_with("NativePine")
                    || descriptor.id == crate::technical_trader::ID
                {
                    serde_json::json!({"initial_avg_price_tick":100})
                } else {
                    serde_json::json!({})
                },
            });
            assert!(registry.supports_instance_reuse(&template));
            let mut saved = template.initial_state();
            let mut hot = registry.create(&template, &saved).unwrap();
            let mut shared_bot = registry.create(&template, &saved).unwrap();
            for _ in 0..32 {
                rooms.advance_clock(&room, 1).unwrap();
                let mut observation = rooms
                    .bot_observation(
                        &room,
                        instrument,
                        20,
                        registry.market_data_request(&template).unwrap(),
                    )
                    .unwrap();
                rooms
                    .enrich_bot_observation(
                        &mut observation,
                        &registry.related_instruments(&template).unwrap(),
                        20,
                    )
                    .unwrap();
                let mut restored = registry.create(&template, &saved).unwrap();
                let actions = hot.decide(&observation).unwrap();
                let frozen = rooms.clone().into_native_observation_wave(room.clone());
                let mut batch = frozen.batch();
                let shared_view = batch
                    .shared_bot_observation(
                        instrument,
                        20,
                        registry.market_data_request(&template).unwrap(),
                        &registry.related_instruments(&template).unwrap(),
                    )
                    .unwrap();
                assert_eq!(
                    shared_view.to_owned_observation(),
                    observation,
                    "{} delivery",
                    descriptor.id
                );
                let other = batch
                    .shared_bot_observation(instrument, 21, None, &[])
                    .unwrap();
                assert!(std::sync::Arc::ptr_eq(&shared_view.public, &other.public));
                assert!(shared_view.public.own_account.is_none());
                assert!(shared_view.public.own_orders.is_empty());
                assert!(shared_view.public.risk.is_none());
                assert!(shared_view.public.position_protections.is_empty());
                assert_eq!(
                    shared_bot.decide_shared(&shared_view).unwrap(),
                    actions,
                    "{} shared actions",
                    descriptor.id
                );
                assert_eq!(
                    actions,
                    restored.decide(&observation).unwrap(),
                    "{}",
                    descriptor.id
                );
                saved = restored.snapshot();
                assert_eq!(hot.snapshot(), saved, "{}", descriptor.id);
                assert_eq!(
                    shared_bot.snapshot(),
                    saved,
                    "{} shared state",
                    descriptor.id
                );
                actions_seen += actions.len();
                let mut gateway = crate::OrderGateway::new(&mut rooms, next_order_id);
                for action in actions {
                    gateway
                        .submit_action(crate::GatewayRequest {
                            participant_id: template.participant_id().to_string(),
                            room_id: room.clone(),
                            instrument_id: Some(instrument.into()),
                            account_id: 20,
                            action,
                        })
                        .unwrap();
                }
                next_order_id = gateway.next_order_id();
            }
        }
        assert!(actions_seen > 0);
    }

    #[test]
    fn all_builtins_accept_unified_config_and_legacy_state_still_serializes() {
        let registry = BotRegistry::with_builtins();
        assert_eq!(registry.descriptors().len(), 37);
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
