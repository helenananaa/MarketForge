use serde::{Deserialize, Serialize};

use crate::{
    clock::SimulationClock,
    gateway::{GatewayError, GatewayExecution, OrderAction, TradingApi},
    model::{PriceTick, Qty, Side},
    observation::ParticipantObservation,
    participant::{Participant, ParticipantConfig, run_participant_once},
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum AgentTemplate {
    NoiseTrader(NoiseTraderConfig),
    DcaTrader(DcaTraderConfig),
    GridTrader(GridTraderConfig),
    ContinuousMarketMaker(ContinuousMmConfig),
    CancelAtStep(CancelAtStepConfig),
    Plugin(crate::bots::BotConfig),
}

#[derive(Default)]
pub struct AgentRuntime {
    participants: Vec<Box<dyn Participant>>,
    clock: SimulationClock,
}

impl AgentRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn new_with_clock(clock: SimulationClock) -> Self {
        Self {
            participants: Vec::new(),
            clock,
        }
    }

    pub fn add_participant<P>(&mut self, participant: P)
    where
        P: Participant + 'static,
    {
        self.participants.push(Box::new(participant));
    }

    pub fn participant_count(&self) -> usize {
        self.participants.len()
    }

    pub fn step(&self) -> u64 {
        self.clock.step()
    }

    pub fn market_time_ms(&self) -> u64 {
        self.clock.market_time_ms()
    }

    pub fn clock(&self) -> SimulationClock {
        self.clock
    }

    pub fn run_step<T: TradingApi>(&mut self, api: &mut T) -> AgentStep {
        self.clock.advance_step();
        let mut participant_results = Vec::with_capacity(self.participants.len());

        for participant in &mut self.participants {
            let participant_id = participant.config().participant_id.clone();
            let result = run_participant_once(api, participant.as_mut());
            participant_results.push(AgentParticipantStep {
                participant_id,
                result,
            });
        }

        AgentStep {
            step: self.clock.step(),
            market_time_ms: self.clock.market_time_ms(),
            participant_results,
        }
    }
}

impl AgentTemplate {
    pub fn config(&self) -> &ParticipantConfig {
        match self {
            Self::NoiseTrader(config) => &config.participant,
            Self::DcaTrader(config) => &config.participant,
            Self::GridTrader(config) => &config.participant,
            Self::ContinuousMarketMaker(config) => &config.participant,
            Self::CancelAtStep(config) => &config.participant,
            Self::Plugin(config) => &config.participant,
        }
    }

    pub fn participant_id(&self) -> &str {
        &self.config().participant_id
    }

    pub fn bot_id(&self) -> &str {
        match self {
            Self::NoiseTrader(_) => "NoiseTrader",
            Self::DcaTrader(_) => "DcaTrader",
            Self::GridTrader(_) => "GridTrader",
            Self::ContinuousMarketMaker(_) => "ContinuousMarketMaker",
            Self::CancelAtStep(_) => "CancelAtStep",
            Self::Plugin(config) => &config.plugin_id,
        }
    }

    pub fn initial_state(&self) -> PersistedAgentKindState {
        match self {
            Self::NoiseTrader(config) => PersistedAgentKindState::Noise {
                rng_state: config.seed.max(1),
            },
            Self::DcaTrader(_) => PersistedAgentKindState::Dca { observed_steps: 0 },
            Self::GridTrader(_) => PersistedAgentKindState::Grid {
                has_seeded_grid: false,
            },
            Self::ContinuousMarketMaker(config) => PersistedAgentKindState::ContinuousMm {
                rng_state: config.seed.max(1),
                last_mid: None,
                steps_since_quote: 0,
                inventory: 0,
            },
            Self::CancelAtStep(_) => PersistedAgentKindState::CancelAtStep {
                canceled: false,
                observed_steps: 0,
            },
            Self::Plugin(config) => PersistedAgentKindState::Plugin {
                plugin_id: config.plugin_id.clone(),
                plugin_version: config.plugin_version.clone(),
                state_version: config.state_version,
                data: serde_json::Value::Null,
            },
        }
    }

    /// For standalone participants. Scheduled plugins require a host BotRegistry.
    pub fn into_participant(self) -> Result<Box<dyn Participant>, crate::bots::BotError> {
        Ok(match self {
            Self::NoiseTrader(config) => Box::new(NoiseTrader::new(config)),
            Self::DcaTrader(config) => Box::new(DcaTrader::new(config)),
            Self::GridTrader(config) => Box::new(GridTrader::new(config)),
            Self::ContinuousMarketMaker(config) => Box::new(ContinuousMarketMaker::new(config)),
            Self::CancelAtStep(config) => Box::new(CancelAtStepTrader::new(config)),
            Self::Plugin(_) => {
                return Err(crate::bots::BotError(
                    "plugins require a host BotRegistry".into(),
                ));
            }
        })
    }
}

macro_rules! builtin_bot {
    ($factory:ident, $bot:ty, $variant:ident, $seeded:expr) => {
        struct $factory(crate::bots::BotDescriptor);
        impl crate::bots::ScheduledBot for $bot {
            fn decide(
                &mut self,
                observation: &ParticipantObservation,
            ) -> Result<Vec<OrderAction>, crate::bots::BotError> {
                Participant::observe(self, observation);
                Ok(Participant::decide(self))
            }
            fn snapshot(&self) -> PersistedAgentKindState {
                self.persist_kind_state()
            }
        }
        impl crate::bots::BotFactory for $factory {
            fn supports_instance_reuse(&self) -> bool {
                true
            }
            fn descriptor(&self) -> &crate::bots::BotDescriptor {
                &self.0
            }
            fn create(
                &self,
                template: &AgentTemplate,
                state: &PersistedAgentKindState,
            ) -> Result<Box<dyn crate::bots::ScheduledBot>, crate::bots::BotError> {
                let config = match template {
                    AgentTemplate::$variant(config) => config.clone(),
                    AgentTemplate::Plugin(config) => {
                        let mut value = self.0.validate_config(&config.config)?;
                        if $seeded {
                            value["seed"] = config.seed.into();
                        }
                        value["participant"] = serde_json::to_value(&config.participant)
                            .map_err(|error| crate::bots::BotError(error.to_string()))?;
                        serde_json::from_value(value)
                            .map_err(|error| crate::bots::BotError(error.to_string()))?
                    }
                    _ => return Err(crate::bots::BotError("incorrect builtin template".into())),
                };
                let initial_state = AgentTemplate::$variant(config.clone()).initial_state();
                let mut bot = <$bot>::new(config);
                let owned_state;
                let state = if let PersistedAgentKindState::Plugin { data, .. } = state {
                    owned_state = if data.is_null() {
                        initial_state
                    } else {
                        serde_json::from_value(data.clone())
                            .map_err(|error| crate::bots::BotError(error.to_string()))?
                    };
                    &owned_state
                } else {
                    state
                };
                if !bot.restore_kind_state(state) {
                    return Err(crate::bots::BotError("incorrect builtin state".into()));
                }
                if let AgentTemplate::Plugin(config) = template {
                    Ok(Box::new(BuiltinPluginAdapter {
                        bot: Box::new(bot),
                        config: config.clone(),
                    }))
                } else {
                    Ok(Box::new(bot))
                }
            }
        }
    };
}

struct BuiltinPluginAdapter {
    bot: Box<dyn crate::bots::ScheduledBot>,
    config: crate::bots::BotConfig,
}
impl crate::bots::ScheduledBot for BuiltinPluginAdapter {
    fn decide(
        &mut self,
        observation: &ParticipantObservation,
    ) -> Result<Vec<OrderAction>, crate::bots::BotError> {
        self.bot.decide(observation)
    }
    fn snapshot(&self) -> PersistedAgentKindState {
        PersistedAgentKindState::Plugin {
            plugin_id: self.config.plugin_id.clone(),
            plugin_version: self.config.plugin_version.clone(),
            state_version: self.config.state_version,
            data: serde_json::to_value(self.bot.snapshot()).expect("builtin state serializes"),
        }
    }
}

builtin_bot!(NoiseFactory, NoiseTrader, NoiseTrader, true);
builtin_bot!(DcaFactory, DcaTrader, DcaTrader, false);
builtin_bot!(GridFactory, GridTrader, GridTrader, false);
builtin_bot!(
    MmFactory,
    ContinuousMarketMaker,
    ContinuousMarketMaker,
    true
);
builtin_bot!(CancelFactory, CancelAtStepTrader, CancelAtStep, false);

pub(crate) fn register_builtin_bots(registry: &mut crate::bots::BotRegistry) {
    use crate::bots::{BOT_PROTOCOL_VERSION, BotDescriptor, BotParameter, ParameterType};
    fn descriptor(id: &str, name: &str, defaults: serde_json::Value) -> BotDescriptor {
        let parameters = defaults
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| {
                let kind = if value.is_boolean() {
                    ParameterType::Boolean
                } else if value.is_string() {
                    ParameterType::String
                } else {
                    ParameterType::Integer
                };
                (
                    key.clone(),
                    BotParameter {
                        kind,
                        required: false,
                        default: Some(value.clone()),
                        minimum: None,
                        maximum: None,
                        choices: Vec::new(),
                    },
                )
            })
            .collect();
        BotDescriptor {
            id: id.into(),
            name: name.into(),
            version: "1".into(),
            protocol_version: BOT_PROTOCOL_VERSION.into(),
            state_version: 1,
            runtime: "builtin".into(),
            parameters,
        }
    }
    registry.register(NoiseFactory(descriptor("NoiseTrader", "Noise trader", serde_json::json!({"reference_price_tick":100,"price_radius_ticks":3,"max_qty":2,"market_order_ratio_ppm":0})))).unwrap();
    registry.register(DcaFactory(descriptor("DcaTrader", "DCA trader", serde_json::json!({"interval_steps":1,"order_qty":1,"use_market_order":false,"limit_offset_ticks":0,"fallback_price_tick":100,"side":"Buy"})))).unwrap();
    registry.register(GridFactory(descriptor("GridTrader", "Grid trader", serde_json::json!({"center_price_tick":100,"grid_spacing_ticks":5,"levels":1,"qty_per_level":2})))).unwrap();
    registry.register(MmFactory(descriptor("ContinuousMarketMaker", "Continuous market maker", serde_json::json!({"version":1,"half_spread_ticks":2,"size_per_level":1,"inventory_target":0,"inventory_cap":10,"requote_threshold_ticks":1,"max_resting_orders":2,"replenish_steps":1,"fallback_price_tick":100})))).unwrap();
    registry
        .register(CancelFactory(descriptor(
            "CancelAtStep",
            "Cancel at step",
            serde_json::json!({"cancel_at_step":1}),
        )))
        .unwrap();
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStep {
    pub step: u64,
    pub market_time_ms: u64,
    pub participant_results: Vec<AgentParticipantStep>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentParticipantStep {
    pub participant_id: String,
    pub result: Result<Vec<GatewayExecution>, GatewayError>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NoiseTraderConfig {
    pub participant: ParticipantConfig,
    pub seed: u64,
    pub reference_price_tick: PriceTick,
    pub price_radius_ticks: PriceTick,
    pub max_qty: Qty,
    pub market_order_ratio_ppm: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DcaTraderConfig {
    pub participant: ParticipantConfig,
    pub interval_steps: u64,
    pub order_qty: Qty,
    pub use_market_order: bool,
    pub limit_offset_ticks: PriceTick,
    pub fallback_price_tick: PriceTick,
    pub side: Side,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GridTraderConfig {
    pub participant: ParticipantConfig,
    pub center_price_tick: PriceTick,
    pub grid_spacing_ticks: PriceTick,
    pub levels: u32,
    pub qty_per_level: Qty,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContinuousMmConfig {
    pub participant: ParticipantConfig,
    pub version: u16,
    pub seed: u64,
    pub half_spread_ticks: PriceTick,
    pub size_per_level: Qty,
    pub inventory_target: i64,
    pub inventory_cap: i64,
    pub requote_threshold_ticks: PriceTick,
    pub max_resting_orders: u32,
    pub replenish_steps: u64,
    pub fallback_price_tick: PriceTick,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CancelAtStepConfig {
    pub participant: ParticipantConfig,
    pub cancel_at_step: u64,
}

pub const AGENT_STATE_VERSION: u16 = 1;
pub const AGENT_CONFIG_VERSION: u16 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PersistedAgentKindState {
    Plugin {
        plugin_id: String,
        plugin_version: String,
        state_version: u16,
        data: serde_json::Value,
    },
    Noise {
        rng_state: u64,
    },
    Dca {
        observed_steps: u64,
    },
    Grid {
        has_seeded_grid: bool,
    },
    ContinuousMm {
        rng_state: u64,
        last_mid: Option<PriceTick>,
        steps_since_quote: u64,
        inventory: i64,
    },
    CancelAtStep {
        canceled: bool,
        observed_steps: u64,
    },
}

pub struct NoiseTrader {
    config: NoiseTraderConfig,
    rng: DeterministicRng,
    last_view: Option<ParticipantObservation>,
}

impl NoiseTrader {
    pub fn new(config: NoiseTraderConfig) -> Self {
        Self {
            rng: DeterministicRng::new(config.seed),
            config,
            last_view: None,
        }
    }

    pub fn persist_kind_state(&self) -> PersistedAgentKindState {
        PersistedAgentKindState::Noise {
            rng_state: self.rng.state(),
        }
    }

    pub fn restore_kind_state(&mut self, state: &PersistedAgentKindState) -> bool {
        match state {
            PersistedAgentKindState::Noise { rng_state } => {
                self.rng.set_state(*rng_state);
                true
            }
            _ => false,
        }
    }

    fn reference_price(&self) -> PriceTick {
        self.last_view
            .as_ref()
            .and_then(
                |view| match (view.book.bids.first(), view.book.asks.first()) {
                    (Some(bid), Some(ask)) => Some((bid.price_tick + ask.price_tick) / 2),
                    (Some(bid), None) => Some(bid.price_tick),
                    (None, Some(ask)) => Some(ask.price_tick),
                    (None, None) => None,
                },
            )
            .unwrap_or(self.config.reference_price_tick)
    }
}

impl Participant for NoiseTrader {
    fn config(&self) -> &ParticipantConfig {
        &self.config.participant
    }

    fn observe(&mut self, view: &ParticipantObservation) {
        self.last_view = Some(view.clone());
    }

    fn decide(&mut self) -> Vec<OrderAction> {
        if self.config.max_qty == 0 {
            return Vec::new();
        }

        let side = if self.rng.next_bool() {
            Side::Buy
        } else {
            Side::Sell
        };
        let qty = self.rng.next_inclusive(self.config.max_qty).max(1);
        let use_market =
            self.rng.next_mod(1_000_000) < u64::from(self.config.market_order_ratio_ppm);

        if use_market {
            return vec![OrderAction::PlaceMarket { side, qty }];
        }

        let reference = self.reference_price();
        let radius = self.config.price_radius_ticks.max(0);
        let offset = if radius == 0 {
            0
        } else {
            self.rng.next_mod((radius * 2 + 1) as u64) as PriceTick - radius
        };

        vec![OrderAction::PlaceLimit {
            side,
            price_tick: (reference + offset).max(1),
            qty,
        }]
    }
}

pub struct DcaTrader {
    config: DcaTraderConfig,
    observed_steps: u64,
    last_view: Option<ParticipantObservation>,
}

impl DcaTrader {
    pub fn new(config: DcaTraderConfig) -> Self {
        Self {
            config,
            observed_steps: 0,
            last_view: None,
        }
    }

    pub fn persist_kind_state(&self) -> PersistedAgentKindState {
        PersistedAgentKindState::Dca {
            observed_steps: self.observed_steps,
        }
    }

    pub fn restore_kind_state(&mut self, state: &PersistedAgentKindState) -> bool {
        match state {
            PersistedAgentKindState::Dca { observed_steps } => {
                self.observed_steps = *observed_steps;
                true
            }
            _ => false,
        }
    }

    fn limit_price(&self) -> PriceTick {
        let fallback = self.config.fallback_price_tick.max(1);
        let book_price = self
            .last_view
            .as_ref()
            .and_then(|view| match self.config.side {
                Side::Buy => view.book.asks.first().map(|level| level.price_tick),
                Side::Sell => view.book.bids.first().map(|level| level.price_tick),
            });
        let base = book_price.unwrap_or(fallback);

        match self.config.side {
            Side::Buy => (base + self.config.limit_offset_ticks).max(1),
            Side::Sell => (base - self.config.limit_offset_ticks).max(1),
        }
    }
}

impl Participant for DcaTrader {
    fn config(&self) -> &ParticipantConfig {
        &self.config.participant
    }

    fn observe(&mut self, view: &ParticipantObservation) {
        self.observed_steps += 1;
        self.last_view = Some(view.clone());
    }

    fn decide(&mut self) -> Vec<OrderAction> {
        if self.config.order_qty == 0 || self.config.interval_steps == 0 {
            return Vec::new();
        }
        if !self
            .observed_steps
            .is_multiple_of(self.config.interval_steps)
        {
            return Vec::new();
        }

        if self.config.use_market_order {
            vec![OrderAction::PlaceMarket {
                side: self.config.side,
                qty: self.config.order_qty,
            }]
        } else {
            vec![OrderAction::PlaceLimit {
                side: self.config.side,
                price_tick: self.limit_price(),
                qty: self.config.order_qty,
            }]
        }
    }
}

pub struct GridTrader {
    config: GridTraderConfig,
    has_seeded_grid: bool,
}

impl GridTrader {
    pub fn new(config: GridTraderConfig) -> Self {
        Self {
            config,
            has_seeded_grid: false,
        }
    }

    pub fn persist_kind_state(&self) -> PersistedAgentKindState {
        PersistedAgentKindState::Grid {
            has_seeded_grid: self.has_seeded_grid,
        }
    }

    pub fn restore_kind_state(&mut self, state: &PersistedAgentKindState) -> bool {
        match state {
            PersistedAgentKindState::Grid { has_seeded_grid } => {
                self.has_seeded_grid = *has_seeded_grid;
                true
            }
            _ => false,
        }
    }
}

impl Participant for GridTrader {
    fn config(&self) -> &ParticipantConfig {
        &self.config.participant
    }

    fn observe(&mut self, _view: &ParticipantObservation) {}

    fn decide(&mut self) -> Vec<OrderAction> {
        if self.has_seeded_grid
            || self.config.levels == 0
            || self.config.qty_per_level == 0
            || self.config.grid_spacing_ticks <= 0
        {
            return Vec::new();
        }
        self.has_seeded_grid = true;

        let mut actions = Vec::with_capacity((self.config.levels * 2) as usize);
        for level in 1..=self.config.levels {
            let distance = self.config.grid_spacing_ticks * PriceTick::from(level);
            let bid_price = (self.config.center_price_tick - distance).max(1);
            let ask_price = self.config.center_price_tick + distance;

            actions.push(OrderAction::PlaceLimit {
                side: Side::Buy,
                price_tick: bid_price,
                qty: self.config.qty_per_level,
            });
            actions.push(OrderAction::PlaceLimit {
                side: Side::Sell,
                price_tick: ask_price,
                qty: self.config.qty_per_level,
            });
        }

        actions
    }
}

pub struct ContinuousMarketMaker {
    config: ContinuousMmConfig,
    rng: DeterministicRng,
    last_view: Option<ParticipantObservation>,
    last_mid: Option<PriceTick>,
    steps_since_quote: u64,
    inventory: i64,
}

impl ContinuousMarketMaker {
    pub fn new(config: ContinuousMmConfig) -> Self {
        let seed = config.seed;
        Self {
            rng: DeterministicRng::new(seed),
            config,
            last_view: None,
            last_mid: None,
            steps_since_quote: 0,
            inventory: 0,
        }
    }

    pub fn persist_kind_state(&self) -> PersistedAgentKindState {
        PersistedAgentKindState::ContinuousMm {
            rng_state: self.rng.state(),
            last_mid: self.last_mid,
            steps_since_quote: self.steps_since_quote,
            inventory: self.inventory,
        }
    }

    pub fn restore_kind_state(&mut self, state: &PersistedAgentKindState) -> bool {
        match state {
            PersistedAgentKindState::ContinuousMm {
                rng_state,
                last_mid,
                steps_since_quote,
                inventory,
            } => {
                self.rng.set_state(*rng_state);
                self.last_mid = *last_mid;
                self.steps_since_quote = *steps_since_quote;
                self.inventory = *inventory;
                true
            }
            _ => false,
        }
    }

    fn mid(&self) -> Option<PriceTick> {
        let view = self.last_view.as_ref()?;
        match (view.book.bids.first(), view.book.asks.first()) {
            (Some(bid), Some(ask)) => Some((bid.price_tick + ask.price_tick) / 2),
            (Some(bid), None) => Some(bid.price_tick),
            (None, Some(ask)) => Some(ask.price_tick),
            (None, None) => view
                .public_trades
                .last()
                .map(|trade| trade.price_tick)
                .or(Some(self.config.fallback_price_tick.max(1))),
        }
    }
}

impl Participant for ContinuousMarketMaker {
    fn config(&self) -> &ParticipantConfig {
        &self.config.participant
    }

    fn observe(&mut self, view: &ParticipantObservation) {
        self.last_view = Some(view.clone());
        self.inventory = match &view.own_account {
            Some(crate::AccountSnapshot::Spot(account)) => {
                i64::try_from(account.position_qty).unwrap_or(i64::MAX)
            }
            Some(crate::AccountSnapshot::Perp(account)) => {
                i64::try_from(account.position_qty).unwrap_or(i64::MAX)
            }
            None => self.inventory,
        };
        self.steps_since_quote = self.steps_since_quote.saturating_add(1);
    }

    fn decide(&mut self) -> Vec<OrderAction> {
        let Some(mid) = self.mid() else {
            return Vec::new();
        };
        let spread = self.config.half_spread_ticks.max(1);
        let size = self.config.size_per_level.max(1);
        let resting = self
            .last_view
            .as_ref()
            .map(|view| view.own_orders.len() as u32)
            .unwrap_or(0);
        let moved = self
            .last_mid
            .is_some_and(|prev| (mid - prev).abs() >= self.config.requote_threshold_ticks.max(1));
        let due = self.config.replenish_steps == 0
            || self.steps_since_quote >= self.config.replenish_steps
            || self.last_mid.is_none();
        if !due && !moved {
            return Vec::new();
        }
        if resting >= self.config.max_resting_orders.max(1) && !moved {
            return Vec::new();
        }
        let mut actions = Vec::new();
        if moved && let Some(view) = &self.last_view {
            for order in &view.own_orders {
                actions.push(OrderAction::Cancel {
                    order_id: order.order_id,
                });
            }
        }
        let cap = self.config.inventory_cap.abs().max(1);
        if self.inventory < cap {
            actions.push(OrderAction::PlaceLimit {
                side: Side::Buy,
                price_tick: (mid - spread).max(1),
                qty: size,
            });
        }
        if self.inventory > -cap {
            actions.push(OrderAction::PlaceLimit {
                side: Side::Sell,
                price_tick: (mid + spread).max(1),
                qty: size,
            });
        }
        self.last_mid = Some(mid);
        self.steps_since_quote = 0;
        let _ = self.rng.next();
        actions
    }
}

pub struct CancelAtStepTrader {
    config: CancelAtStepConfig,
    observed_steps: u64,
    canceled: bool,
    last_view: Option<ParticipantObservation>,
}

impl CancelAtStepTrader {
    pub fn new(config: CancelAtStepConfig) -> Self {
        Self {
            config,
            observed_steps: 0,
            canceled: false,
            last_view: None,
        }
    }

    pub fn persist_kind_state(&self) -> PersistedAgentKindState {
        PersistedAgentKindState::CancelAtStep {
            canceled: self.canceled,
            observed_steps: self.observed_steps,
        }
    }

    pub fn restore_kind_state(&mut self, state: &PersistedAgentKindState) -> bool {
        match state {
            PersistedAgentKindState::CancelAtStep {
                canceled,
                observed_steps,
            } => {
                self.canceled = *canceled;
                self.observed_steps = *observed_steps;
                true
            }
            _ => false,
        }
    }
}

impl Participant for CancelAtStepTrader {
    fn config(&self) -> &ParticipantConfig {
        &self.config.participant
    }

    fn observe(&mut self, view: &ParticipantObservation) {
        self.observed_steps = self.observed_steps.saturating_add(1);
        self.last_view = Some(view.clone());
    }

    fn decide(&mut self) -> Vec<OrderAction> {
        if self.canceled || self.observed_steps < self.config.cancel_at_step {
            return Vec::new();
        }
        self.canceled = true;
        self.last_view
            .as_ref()
            .map(|view| {
                view.own_orders
                    .iter()
                    .map(|order| OrderAction::Cancel {
                        order_id: order.order_id,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug)]
struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }

    fn state(&self) -> u64 {
        self.state
    }

    fn set_state(&mut self, state: u64) {
        self.state = state.max(1);
    }

    fn next(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        self.state
    }

    fn next_bool(&mut self) -> bool {
        self.next() & 1 == 0
    }

    fn next_mod(&mut self, modulo: u64) -> u64 {
        if modulo == 0 {
            return 0;
        }
        self.next() % modulo
    }

    fn next_inclusive(&mut self, max: u64) -> u64 {
        self.next_mod(max) + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SpotRiskConfig,
        actor::{AccountSnapshot, ActorExecutionResult, MarketExecution, MarketStatus},
        gateway::{GatewayRequest, OrderGateway, TradingApi},
        market::{InstrumentConfig, MarketConfig, SpotMarketConfig},
        model::BookSnapshot,
        observation::{PARTICIPANT_OBSERVATION_VERSION, ParticipantObservation},
        participant::{ParticipantKind, run_participant_once},
        room::RoomManager,
        scenario::{ScenarioAccount, ScenarioConfig},
        spot::SpotClearingConfig,
    };

    fn participant_config(id: &str, account_id: u64) -> ParticipantConfig {
        ParticipantConfig {
            participant_id: id.to_string(),
            kind: ParticipantKind::RuleAgent,
            room_id: "room-1".to_string(),
            account_id,
            instrument_id: Some("V-BTC-SPOT".to_string()),
        }
    }

    fn spot_scenario() -> ScenarioConfig {
        ScenarioConfig {
            room_id: "room-1".to_string(),
            market_events: Vec::new(),
            venue_preset: None,
            venue_rules: crate::VenueRuleConfig::default(),
            venue_asset_policy: crate::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: MarketConfig::Spot(SpotMarketConfig {
                instrument: InstrumentConfig::new("V-BTC-SPOT", 1, 1).unwrap(),
                clearing: SpotClearingConfig::default(),
                risk: SpotRiskConfig {
                    allow_short: true,
                    ..SpotRiskConfig::default()
                },
            }),
            extra_markets: Vec::new(),
            initial_portfolios: Vec::new(),
            initial_allocations: Vec::new(),
            routed_initial_allocations: Vec::new(),
            accounts: vec![
                ScenarioAccount::Basic {
                    account_id: 20,
                    cash_balance: 10_000,
                },
                ScenarioAccount::Spot {
                    account_id: 30,
                    cash_balance: 10_000,
                    position_qty: 100,
                },
            ],
            seed_orders: vec![],
            routed_seed_orders: Vec::new(),
        }
    }

    #[test]
    fn dca_trader_submits_on_configured_interval_through_gateway() {
        let mut rooms = RoomManager::new();
        rooms.create_room(spot_scenario()).unwrap();
        let mut gateway = OrderGateway::new(&mut rooms, 1);
        let mut trader = DcaTrader::new(DcaTraderConfig {
            participant: participant_config("dca-1", 20),
            interval_steps: 2,
            order_qty: 3,
            use_market_order: false,
            limit_offset_ticks: 0,
            fallback_price_tick: 100,
            side: Side::Buy,
        });

        assert!(
            run_participant_once(&mut gateway, &mut trader)
                .unwrap()
                .is_empty()
        );
        let executions = run_participant_once(&mut gateway, &mut trader).unwrap();

        assert_eq!(executions.len(), 1);
        let ActorExecutionResult::Accepted(MarketExecution::Spot(result)) =
            &executions[0].execution.result
        else {
            panic!("expected accepted spot execution");
        };
        assert!(result.clearing_events.is_empty());
    }

    #[test]
    fn grid_trader_seeds_buy_and_sell_levels_once() {
        let mut rooms = RoomManager::new();
        rooms.create_room(spot_scenario()).unwrap();
        let mut gateway = OrderGateway::new(&mut rooms, 1);
        let mut trader = GridTrader::new(GridTraderConfig {
            participant: participant_config("grid-1", 30),
            center_price_tick: 100,
            grid_spacing_ticks: 5,
            levels: 2,
            qty_per_level: 1,
        });

        let executions = run_participant_once(&mut gateway, &mut trader).unwrap();
        let second = run_participant_once(&mut gateway, &mut trader).unwrap();

        assert_eq!(executions.len(), 4);
        assert!(second.is_empty());
        let view = gateway.market_view("room-1").unwrap();
        assert_eq!(view.book.bids.len(), 2);
        assert_eq!(view.book.asks.len(), 2);
    }

    #[test]
    fn continuous_mm_quotes_two_sided_and_restores_without_double_seed() {
        let mut rooms = RoomManager::new();
        rooms.create_room(spot_scenario()).unwrap();
        let mut gateway = OrderGateway::new(&mut rooms, 1);
        let config = ContinuousMmConfig {
            participant: participant_config("cmm-1", 30),
            version: 1,
            seed: 9,
            half_spread_ticks: 2,
            size_per_level: 1,
            inventory_target: 0,
            inventory_cap: 10,
            requote_threshold_ticks: 5,
            max_resting_orders: 4,
            replenish_steps: 10,
            fallback_price_tick: 100,
        };
        let mut trader = ContinuousMarketMaker::new(config.clone());
        let first = run_participant_once(&mut gateway, &mut trader).unwrap();
        assert!(!first.is_empty());
        assert!(first.len() <= 2);
        let persisted = trader.persist_kind_state();
        let mut restored = ContinuousMarketMaker::new(config.clone());
        assert!(restored.restore_kind_state(&persisted));
        let second = run_participant_once(&mut gateway, &mut restored).unwrap();
        assert!(
            second.is_empty(),
            "restore must not double-quote, got {second:?}"
        );
        let view = gateway.market_view("room-1").unwrap();
        assert!(view.book.bids.len() + view.book.asks.len() <= 4);
    }

    #[test]
    fn continuous_mm_replenishes_after_fill_and_restart_matches_live() {
        let config = ContinuousMmConfig {
            participant: participant_config("cmm-1", 30),
            version: 1,
            seed: 9,
            half_spread_ticks: 2,
            size_per_level: 1,
            inventory_target: 0,
            inventory_cap: 10,
            requote_threshold_ticks: 5,
            max_resting_orders: 4,
            replenish_steps: 1,
            fallback_price_tick: 100,
        };

        let mut live_rooms = RoomManager::new();
        live_rooms.create_room(spot_scenario()).unwrap();
        let mut live_gateway = OrderGateway::new(&mut live_rooms, 1);
        let mut live = ContinuousMarketMaker::new(config.clone());
        let quoted = run_participant_once(&mut live_gateway, &mut live).unwrap();
        assert!(!quoted.is_empty());
        let ask = live_gateway
            .market_view("room-1")
            .unwrap()
            .book
            .asks
            .first()
            .expect("ask")
            .price_tick;
        live_gateway
            .submit_action(GatewayRequest {
                participant_id: "taker".to_string(),
                room_id: "room-1".to_string(),
                instrument_id: Some("V-BTC-SPOT".to_string()),
                account_id: 20,
                action: OrderAction::PlaceLimit {
                    side: Side::Buy,
                    price_tick: ask,
                    qty: 1,
                },
            })
            .unwrap();
        let replenished = run_participant_once(&mut live_gateway, &mut live).unwrap();
        assert!(!replenished.is_empty(), "MM should replenish after a fill");
        let view = live_gateway.market_view("room-1").unwrap();
        assert!(view.book.bids.len() + view.book.asks.len() <= 4);

        let mut restart_rooms = RoomManager::new();
        restart_rooms.create_room(spot_scenario()).unwrap();
        let mut restart_gateway = OrderGateway::new(&mut restart_rooms, 1);
        let mut first_leg = ContinuousMarketMaker::new(config.clone());
        let _ = run_participant_once(&mut restart_gateway, &mut first_leg).unwrap();
        let persisted = first_leg.persist_kind_state();
        let mut restored = ContinuousMarketMaker::new(config.clone());
        assert!(restored.restore_kind_state(&persisted));
        let restarted = run_participant_once(&mut restart_gateway, &mut restored).unwrap();

        let mut twin_rooms = RoomManager::new();
        twin_rooms.create_room(spot_scenario()).unwrap();
        let mut twin_gateway = OrderGateway::new(&mut twin_rooms, 1);
        let mut twin = ContinuousMarketMaker::new(config);
        let _ = run_participant_once(&mut twin_gateway, &mut twin).unwrap();
        let continued = run_participant_once(&mut twin_gateway, &mut twin).unwrap();
        assert_eq!(
            restarted.len(),
            continued.len(),
            "restart decisions must match a live twin"
        );
    }

    #[test]
    fn noise_trader_is_deterministic_for_same_seed() {
        let config = NoiseTraderConfig {
            participant: participant_config("noise-1", 20),
            seed: 42,
            reference_price_tick: 100,
            price_radius_ticks: 3,
            max_qty: 5,
            market_order_ratio_ppm: 0,
        };
        let mut first = NoiseTrader::new(config.clone());
        let mut second = NoiseTrader::new(config);
        let empty_view = ParticipantObservation {
            version: PARTICIPANT_OBSERVATION_VERSION,
            room_id: "room-1".to_string(),
            venue_id: "default-venue".to_string(),
            instrument_id: "V-BTC-SPOT".to_string(),
            status: MarketStatus::Running,
            step: 0,
            market_time_ms: 0,
            book: BookSnapshot {
                bids: Vec::new(),
                asks: Vec::new(),
            },
            public_trades: Vec::new(),
            own_orders: Vec::new(),
            own_account: None::<AccountSnapshot>,
            related_markets: vec![],
            market_events: vec![],
            bot_market_data: None,
            perp_price: None,
        };

        first.observe(&empty_view);
        second.observe(&empty_view);

        assert_eq!(first.decide(), second.decide());
    }

    #[test]
    fn agent_runtime_runs_multiple_participants_through_gateway() {
        let mut rooms = RoomManager::new();
        rooms.create_room(spot_scenario()).unwrap();
        let mut gateway = OrderGateway::new(&mut rooms, 1);
        let mut runtime = AgentRuntime::new();

        runtime.add_participant(GridTrader::new(GridTraderConfig {
            participant: participant_config("grid-1", 30),
            center_price_tick: 100,
            grid_spacing_ticks: 5,
            levels: 1,
            qty_per_level: 2,
        }));
        runtime.add_participant(DcaTrader::new(DcaTraderConfig {
            participant: participant_config("dca-1", 20),
            interval_steps: 1,
            order_qty: 1,
            use_market_order: false,
            limit_offset_ticks: 10,
            fallback_price_tick: 100,
            side: Side::Buy,
        }));

        let step = runtime.run_step(&mut gateway);

        assert_eq!(step.step, 1);
        assert_eq!(step.market_time_ms, 1_000);
        assert_eq!(runtime.step(), 1);
        assert_eq!(runtime.market_time_ms(), 1_000);
        assert_eq!(runtime.participant_count(), 2);
        assert_eq!(step.participant_results.len(), 2);
        assert_eq!(step.participant_results[0].participant_id, "grid-1");
        assert_eq!(step.participant_results[1].participant_id, "dca-1");
        assert_eq!(
            step.participant_results[0]
                .result
                .as_ref()
                .expect("grid should submit")
                .len(),
            2
        );
        assert_eq!(
            step.participant_results[1]
                .result
                .as_ref()
                .expect("dca should submit")
                .len(),
            1
        );
        assert_eq!(gateway.next_order_id(), 4);
    }
}
