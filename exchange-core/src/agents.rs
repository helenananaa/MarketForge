use serde::{Deserialize, Serialize};

use crate::{
    clock::SimulationClock,
    gateway::{GatewayError, GatewayExecution, MarketView, OrderAction, TradingApi},
    model::{PriceTick, Qty, Side},
    participant::{Participant, ParticipantConfig, run_participant_once},
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum AgentTemplate {
    NoiseTrader(NoiseTraderConfig),
    DcaTrader(DcaTraderConfig),
    GridTrader(GridTraderConfig),
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

pub struct NoiseTrader {
    config: NoiseTraderConfig,
    rng: DeterministicRng,
    last_view: Option<MarketView>,
}

impl NoiseTrader {
    pub fn new(config: NoiseTraderConfig) -> Self {
        Self {
            rng: DeterministicRng::new(config.seed),
            config,
            last_view: None,
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

    fn observe(&mut self, view: &MarketView) {
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
    last_view: Option<MarketView>,
}

impl DcaTrader {
    pub fn new(config: DcaTraderConfig) -> Self {
        Self {
            config,
            observed_steps: 0,
            last_view: None,
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

    fn observe(&mut self, view: &MarketView) {
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
}

impl Participant for GridTrader {
    fn config(&self) -> &ParticipantConfig {
        &self.config.participant
    }

    fn observe(&mut self, _view: &MarketView) {}

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

#[derive(Clone, Copy, Debug)]
struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
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
        actor::{AccountSnapshots, ActorExecutionResult, MarketExecution, MarketStatus},
        gateway::{OrderGateway, TradingApi},
        market::{InstrumentConfig, MarketConfig, SpotMarketConfig},
        model::BookSnapshot,
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
        }
    }

    fn spot_scenario() -> ScenarioConfig {
        ScenarioConfig {
            room_id: "room-1".to_string(),
            market: MarketConfig::Spot(SpotMarketConfig {
                instrument: InstrumentConfig::new("V-BTC-SPOT", 1, 1).unwrap(),
                clearing: SpotClearingConfig::default(),
                risk: SpotRiskConfig {
                    allow_short: true,
                    ..SpotRiskConfig::default()
                },
            }),
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
        let empty_view = MarketView {
            room_id: "room-1".to_string(),
            status: MarketStatus::Running,
            book: BookSnapshot {
                bids: Vec::new(),
                asks: Vec::new(),
            },
            accounts: AccountSnapshots::Spot(Vec::new()),
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
