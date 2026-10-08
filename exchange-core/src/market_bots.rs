//! Version-pinned background participants. Legacy bots retain their behavior.
//! Prices are produced by matching; valuation shocks change beliefs only.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    AccountSnapshot, AgentTemplate, BotDescriptor, BotError, BotFactory, BotParameter, BotRegistry,
    MarketStatus, OrderAction, ParameterType, ParticipantObservation, PerpMarginStatus,
    PersistedAgentKindState, ScheduledBot, Side,
};

pub const MARKET_BOT_IDS: [&str; 5] = [
    "AdaptiveNoiseTrader",
    "DynamicMarketMaker",
    "ValueTrader",
    "TrendTrader",
    "ExecutionTrader",
];
const PPM: i128 = 1_000_000;

// The catalog exposes only parameters relevant to each strategy; the shared
// representation keeps scheduling, capital checks and recovery identical.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    decision_interval_ms: u64,
    jitter_ms: u64,
    order_ttl_ms: u64,
    inventory_cap: i64,
    inventory_target: i64,
    max_qty: u64,
    fee_buffer_ppm: u32,
    fallback_price_tick: i64,
    activity_ppm: u32,
    side_persistence_ppm: u32,
    market_order_ratio_ppm: u32,
    price_radius_ticks: i64,
    half_spread_ticks: i64,
    inventory_skew_ticks: i64,
    volatility_spread_multiplier: i64,
    withdraw_volatility_ticks: i64,
    levels: u32,
    level_spacing_ticks: i64,
    fair_price_tick: i64,
    edge_ticks: i64,
    value_shift_at_ms: u64,
    value_shift_ticks: i64,
    information_delay_ms: u64,
    lookback: usize,
    signal_threshold_ticks: i64,
    position_size: i64,
    side: Side,
    target_qty: u64,
    horizon_ms: u64,
    start_after_ms: u64,
    max_slippage_ticks: i64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            decision_interval_ms: 1_000,
            jitter_ms: 1_000,
            order_ttl_ms: 6_000,
            inventory_cap: 100,
            inventory_target: 0,
            max_qty: 2,
            fee_buffer_ppm: 5_000,
            fallback_price_tick: 100,
            activity_ppm: 400_000,
            side_persistence_ppm: 650_000,
            market_order_ratio_ppm: 400_000,
            price_radius_ticks: 3,
            half_spread_ticks: 1,
            inventory_skew_ticks: 4,
            volatility_spread_multiplier: 2,
            withdraw_volatility_ticks: 20,
            levels: 2,
            level_spacing_ticks: 1,
            fair_price_tick: 100,
            edge_ticks: 1,
            value_shift_at_ms: 60_000,
            value_shift_ticks: 0,
            information_delay_ms: 0,
            lookback: 8,
            signal_threshold_ticks: 2,
            position_size: 10,
            side: Side::Buy,
            target_qty: 20,
            horizon_ms: 120_000,
            start_after_ms: 0,
            max_slippage_ticks: 3,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    rng: u64,
    next_decision_ms: u64,
    last_observation: Option<(u64, u64)>,
    last_mid: Option<i64>,
    volatility_ticks: i64,
    prices: Vec<i64>,
    last_side: Option<Side>,
    order_seen_ms: BTreeMap<u64, u64>,
    last_quote_center: Option<i64>,
    last_quote_spread: Option<i64>,
    initial_position: Option<String>,
    execution_start_ms: Option<u64>,
    completed_qty: u64,
    deadline_reached: bool,
}

struct Factory(BotDescriptor);

pub(crate) fn register_market_bots(registry: &mut BotRegistry) {
    for id in MARKET_BOT_IDS {
        let mut defaults = json!({
            "decision_interval_ms": 1000, "jitter_ms": 1000, "order_ttl_ms": 6000,
            "inventory_cap": 100, "max_qty": 2,
            "fee_buffer_ppm": 5000, "fallback_price_tick": 100,
        });
        let (name, specific) = match id {
            "AdaptiveNoiseTrader" => (
                "异质噪声交易者",
                json!({"activity_ppm":400000,"side_persistence_ppm":650000,"market_order_ratio_ppm":400000,"price_radius_ticks":3}),
            ),
            "DynamicMarketMaker" => (
                "动态库存做市商",
                json!({"inventory_target":0,"half_spread_ticks":1,"inventory_skew_ticks":4,"volatility_spread_multiplier":2,"withdraw_volatility_ticks":20,"levels":2,"level_spacing_ticks":1}),
            ),
            "ValueTrader" => (
                "价值与信息交易者",
                json!({"fair_price_tick":100,"edge_ticks":1,"value_shift_at_ms":60000,"value_shift_ticks":0,"information_delay_ms":0}),
            ),
            "TrendTrader" => (
                "趋势与退出交易者",
                json!({"inventory_target":0,"lookback":8,"signal_threshold_ticks":2,"position_size":10}),
            ),
            _ => (
                "目标拆单执行者",
                json!({"side":"Buy","target_qty":20,"horizon_ms":120000,"start_after_ms":0,"max_slippage_ticks":3}),
            ),
        };
        defaults
            .as_object_mut()
            .unwrap()
            .extend(specific.as_object().unwrap().clone());
        let parameters = defaults
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| {
                let (minimum, maximum) = match key.as_str() {
                    "inventory_target" | "value_shift_ticks" => (-1_000_000, 1_000_000),
                    "lookback" => (2, 128),
                    "levels" => (1, 8),
                    "fee_buffer_ppm" => (0, 100_000),
                    key if key.ends_with("_ppm") => (0, 1_000_000),
                    "inventory_cap" | "max_qty" | "target_qty" | "position_size" => (1, 1_000_000),
                    "fallback_price_tick" | "fair_price_tick" => (1, 1_000_000_000),
                    "decision_interval_ms" | "order_ttl_ms" | "horizon_ms" => (1, 86_400_000),
                    "half_spread_ticks" | "level_spacing_ticks" | "withdraw_volatility_ticks" => {
                        (1, 1_000_000)
                    }
                    _ => (0, 86_400_000),
                };
                (
                    key.clone(),
                    BotParameter {
                        kind: if value.is_string() {
                            ParameterType::String
                        } else {
                            ParameterType::Integer
                        },
                        required: false,
                        default: Some(value.clone()),
                        minimum: (!value.is_string()).then_some(minimum),
                        maximum: (!value.is_string()).then_some(maximum),
                        choices: if value.is_string() {
                            vec![json!("Buy"), json!("Sell")]
                        } else {
                            vec![]
                        },
                    },
                )
            })
            .collect();
        registry
            .register(Factory(BotDescriptor {
                id: id.into(),
                name: name.into(),
                version: "1".into(),
                protocol_version: crate::BOT_PROTOCOL_VERSION.into(),
                state_version: 1,
                runtime: "builtin".into(),
                parameters,
            }))
            .expect("valid market bot descriptor");
    }
}

impl BotFactory for Factory {
    fn descriptor(&self) -> &BotDescriptor {
        &self.0
    }

    fn create(
        &self,
        template: &AgentTemplate,
        saved: &PersistedAgentKindState,
    ) -> Result<Box<dyn ScheduledBot>, BotError> {
        let AgentTemplate::Plugin(plugin) = template else {
            return Err(BotError("market bots require Plugin config".into()));
        };
        let config: Config =
            serde_json::from_value(self.0.validate_config(&plugin.config)?).map_err(json_error)?;
        if config.inventory_target.abs() > config.inventory_cap {
            return Err(BotError(
                "inventory_target must be within inventory_cap".into(),
            ));
        }
        if self.0.id == "TrendTrader" && config.position_size > config.inventory_cap {
            return Err(BotError(
                "position_size must be within inventory_cap".into(),
            ));
        }
        let PersistedAgentKindState::Plugin { data, .. } = saved else {
            return Err(BotError("incorrect market bot state".into()));
        };
        let state = if data.is_null() {
            State {
                rng: plugin.seed.max(1),
                ..State::default()
            }
        } else {
            let state: State = serde_json::from_value(data.clone()).map_err(json_error)?;
            if state.rng == 0
                || state.prices.len() > config.lookback
                || state.order_seen_ms.len() > 64
                || state.volatility_ticks < 0
                || state.prices.iter().any(|price| *price <= 0)
                || state.last_mid.is_some_and(|price| price <= 0)
                || state
                    .initial_position
                    .as_ref()
                    .is_some_and(|position| position.parse::<i128>().is_err())
            {
                return Err(BotError("invalid market bot state".into()));
            }
            state
        };
        Ok(Box::new(MarketBot {
            plugin: plugin.clone(),
            config,
            state,
        }))
    }
}

fn json_error(error: serde_json::Error) -> BotError {
    BotError(error.to_string())
}

struct MarketBot {
    plugin: crate::BotConfig,
    config: Config,
    state: State,
}

// Conservative preflight, not a replacement for venue risk. Perpetual exposure
// uses full notional (no assumed leverage); reductions remain possible without
// available opening capital. Resting orders count against directional limits.
struct Budget {
    position: i128,
    buy_open: i128,
    sell_open: i128,
    cash: i128,
    spot_available: Option<i128>,
    reduce_only: bool,
    cap: i128,
    fee_ppm: u32,
}

impl Budget {
    fn new(view: &ParticipantObservation, config: &Config) -> Option<Self> {
        let (position, cash, spot_available, reduce_only) = match view.own_account.as_ref()? {
            AccountSnapshot::Spot(a) => (
                a.position_qty,
                a.available_cash.max(0),
                Some(a.available_position.max(0)),
                false,
            ),
            AccountSnapshot::Perp(a) => (
                a.position_qty,
                a.equity
                    .saturating_sub(a.portfolio_initial_margin.max(a.initial_margin))
                    .saturating_sub(a.reserved_margin)
                    .max(0),
                None,
                !matches!(
                    a.margin_status,
                    PerpMarginStatus::Flat | PerpMarginStatus::Healthy
                ),
            ),
        };
        Some(Self {
            position,
            cash,
            spot_available,
            reduce_only,
            cap: config.inventory_cap.into(),
            fee_ppm: config.fee_buffer_ppm,
            buy_open: view
                .own_orders
                .iter()
                .filter(|o| o.side == Side::Buy)
                .map(|o| i128::from(o.remaining_qty))
                .sum(),
            sell_open: view
                .own_orders
                .iter()
                .filter(|o| o.side == Side::Sell)
                .map(|o| i128::from(o.remaining_qty))
                .sum(),
        })
    }

    fn allocate(&mut self, side: Side, price: i64, requested: u64) -> u64 {
        let (capacity, reducing) = match side {
            Side::Buy => (
                self.cap
                    .saturating_sub(self.position)
                    .saturating_sub(self.buy_open),
                self.position
                    .saturating_neg()
                    .saturating_sub(self.buy_open)
                    .max(0),
            ),
            Side::Sell => (
                self.cap
                    .saturating_add(self.position)
                    .saturating_sub(self.sell_open),
                self.position.saturating_sub(self.sell_open).max(0),
            ),
        };
        let cost = (i128::from(price) * (PPM + i128::from(self.fee_ppm)) + PPM - 1) / PPM;
        let affordable = self.cash / cost.max(1);
        let limit = if let Some(available) = self.spot_available {
            if side == Side::Sell {
                available
            } else {
                affordable
            }
        } else if self.reduce_only {
            reducing
        } else {
            affordable.saturating_add(reducing)
        };
        let qty = i128::from(requested).min(capacity).min(limit).max(0) as u64;
        if side == Side::Buy {
            self.buy_open += i128::from(qty);
        } else {
            self.sell_open += i128::from(qty);
        }
        if let Some(available) = &mut self.spot_available {
            if side == Side::Sell {
                *available -= i128::from(qty);
            } else {
                self.cash -= i128::from(qty) * cost;
            }
        } else {
            self.cash -= (i128::from(qty) - reducing).max(0) * cost;
        }
        qty
    }
}

impl MarketBot {
    fn random(&mut self, modulo: u64) -> u64 {
        let mut x = self.state.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state.rng = x;
        x % modulo.max(1)
    }

    fn cancel_orders(&self, view: &ParticipantObservation) -> Vec<OrderAction> {
        view.own_orders
            .iter()
            .take(64)
            .map(|o| OrderAction::Cancel {
                order_id: o.order_id,
            })
            .collect()
    }

    fn taker(
        &self,
        view: &ParticipantObservation,
        budget: &mut Budget,
        side: Side,
        limit: i64,
        qty: u64,
    ) -> Vec<OrderAction> {
        let top = match side {
            Side::Buy => view.book.asks.first(),
            Side::Sell => view.book.bids.first(),
        };
        let Some(top) = top else {
            return vec![];
        };
        if (side == Side::Buy && top.price_tick > limit)
            || (side == Side::Sell && top.price_tick < limit)
        {
            return vec![];
        }
        let qty = budget.allocate(side, limit, qty);
        if qty == 0 {
            return vec![];
        }
        if budget.reduce_only {
            vec![OrderAction::PlaceReduceOnlyImmediateOrCancel {
                side,
                price_tick: Some(limit),
                qty,
            }]
        } else {
            vec![OrderAction::PlaceImmediateOrCancel {
                side,
                price_tick: Some(limit),
                qty,
            }]
        }
    }

    fn noise(
        &mut self,
        view: &ParticipantObservation,
        budget: &mut Budget,
        mid: i64,
    ) -> Vec<OrderAction> {
        if view.own_orders.len() >= 8
            || self.random(1_000_000) >= u64::from(self.config.activity_ppm)
        {
            return vec![];
        }
        let side = if let Some(previous) = self.state.last_side
            && self.random(1_000_000) < u64::from(self.config.side_persistence_ppm)
        {
            previous
        } else if self.random(2) == 0 {
            Side::Buy
        } else {
            Side::Sell
        };
        self.state.last_side = Some(side);
        let qty = 1 + self.random(self.config.max_qty);
        let aggressive = self.random(1_000_000) < u64::from(self.config.market_order_ratio_ppm)
            || budget.reduce_only;
        if aggressive {
            let limit = match side {
                Side::Buy => mid.saturating_add(self.config.price_radius_ticks),
                Side::Sell => mid.saturating_sub(self.config.price_radius_ticks).max(1),
            };
            return self.taker(view, budget, side, limit, qty);
        }
        let radius = self.config.price_radius_ticks;
        let price = mid
            .saturating_add(self.random((radius * 2 + 1) as u64) as i64 - radius)
            .max(1);
        let qty = budget.allocate(side, price, qty);
        if qty == 0 {
            vec![]
        } else {
            vec![OrderAction::PlaceLimit {
                side,
                price_tick: price,
                qty,
            }]
        }
    }

    fn maker(
        &mut self,
        view: &ParticipantObservation,
        budget: &mut Budget,
        mid: i64,
    ) -> Vec<OrderAction> {
        if budget.reduce_only
            || self.state.volatility_ticks >= self.config.withdraw_volatility_ticks
        {
            return self.cancel_orders(view);
        }
        let deviation = budget.position - i128::from(self.config.inventory_target);
        let skew = deviation * i128::from(self.config.inventory_skew_ticks)
            / i128::from(self.config.inventory_cap);
        let center = (i128::from(mid) - skew).clamp(1, i128::from(i64::MAX)) as i64;
        let spread = self.config.half_spread_ticks.saturating_add(
            self.state
                .volatility_ticks
                .saturating_mul(self.config.volatility_spread_multiplier),
        );
        let changed = self.state.last_quote_center != Some(center)
            || self.state.last_quote_spread != Some(spread);
        // Cancellation is acknowledged through a subsequent observation. Never
        // spend cash that might remain reserved if a cancellation failed.
        if changed && !view.own_orders.is_empty() {
            return self.cancel_orders(view);
        }
        let size = (self.config.max_qty / (1 + self.state.volatility_ticks as u64)).max(1);
        let mut actions = vec![];
        for level in 0..self.config.levels {
            let distance =
                spread.saturating_add(i64::from(level) * self.config.level_spacing_ticks);
            for side in [Side::Buy, Side::Sell] {
                let price = match side {
                    Side::Buy => center.saturating_sub(distance).max(1),
                    Side::Sell => center.saturating_add(distance),
                };
                if view
                    .own_orders
                    .iter()
                    .any(|o| o.side == side && o.price_tick == price)
                {
                    continue;
                }
                let qty = budget.allocate(side, price, size);
                if qty > 0 {
                    actions.push(OrderAction::PlacePostOnly {
                        side,
                        price_tick: price,
                        qty,
                    });
                }
            }
        }
        self.state.last_quote_center = Some(center);
        self.state.last_quote_spread = Some(spread);
        actions
    }

    fn value(&self, view: &ParticipantObservation, budget: &mut Budget) -> Vec<OrderAction> {
        let shift = if view.market_time_ms
            >= self
                .config
                .value_shift_at_ms
                .saturating_add(self.config.information_delay_ms)
        {
            self.config.value_shift_ticks
        } else {
            0
        };
        let fair = self.config.fair_price_tick.saturating_add(shift).max(1);
        let bid = fair.saturating_sub(self.config.edge_ticks).max(1);
        let ask = fair.saturating_add(self.config.edge_ticks);
        if view
            .book
            .asks
            .first()
            .is_some_and(|top| top.price_tick < bid)
        {
            self.taker(view, budget, Side::Buy, bid, self.config.max_qty)
        } else if view
            .book
            .bids
            .first()
            .is_some_and(|top| top.price_tick > ask)
        {
            self.taker(view, budget, Side::Sell, ask, self.config.max_qty)
        } else {
            vec![]
        }
    }

    fn trend(
        &self,
        view: &ParticipantObservation,
        budget: &mut Budget,
        mid: i64,
    ) -> Vec<OrderAction> {
        if self.state.prices.len() < self.config.lookback {
            return vec![];
        }
        let change = mid.saturating_sub(self.state.prices[0]);
        let offset = if change > self.config.signal_threshold_ticks {
            self.config.position_size
        } else if change < -self.config.signal_threshold_ticks {
            -self.config.position_size
        } else {
            0
        };
        let target = self
            .config
            .inventory_target
            .saturating_add(offset)
            .clamp(-self.config.inventory_cap, self.config.inventory_cap);
        let delta = i128::from(target) - budget.position;
        let side = if delta > 0 { Side::Buy } else { Side::Sell };
        let qty = delta.unsigned_abs().min(u128::from(self.config.max_qty)) as u64;
        let top = match side {
            Side::Buy => view.book.asks.first(),
            Side::Sell => view.book.bids.first(),
        };
        top.map_or_else(Vec::new, |top| {
            self.taker(view, budget, side, top.price_tick, qty)
        })
    }

    fn execution(
        &mut self,
        view: &ParticipantObservation,
        budget: &mut Budget,
        mid: i64,
    ) -> Vec<OrderAction> {
        if view.market_time_ms < self.config.start_after_ms {
            return vec![];
        }
        let start = *self
            .state
            .execution_start_ms
            .get_or_insert(view.market_time_ms);
        let initial = self
            .state
            .initial_position
            .get_or_insert_with(|| budget.position.to_string())
            .parse::<i128>()
            .expect("validated initial position");
        let filled = match self.config.side {
            Side::Buy => budget.position.saturating_sub(initial),
            Side::Sell => initial.saturating_sub(budget.position),
        }
        .max(0);
        self.state.completed_qty = filled.min(i128::from(self.config.target_qty)) as u64;
        let elapsed = view.market_time_ms.saturating_sub(start);
        self.state.deadline_reached = elapsed >= self.config.horizon_ms;
        if self.state.deadline_reached || self.state.completed_qty >= self.config.target_qty {
            return vec![];
        }
        // Cumulative TWAP entitlement. Failed/partial executions leave debt for
        // later slices; the final deadline is a hard stop, not an infinite chase.
        let due = (u128::from(self.config.target_qty)
            * u128::from(
                elapsed
                    .saturating_add(self.config.decision_interval_ms)
                    .min(self.config.horizon_ms),
            ))
        .div_ceil(u128::from(self.config.horizon_ms)) as u64;
        let qty = due
            .saturating_sub(self.state.completed_qty)
            .min(self.config.target_qty - self.state.completed_qty)
            .min(self.config.max_qty);
        let limit = match self.config.side {
            Side::Buy => mid.saturating_add(self.config.max_slippage_ticks),
            Side::Sell => mid.saturating_sub(self.config.max_slippage_ticks).max(1),
        };
        self.taker(view, budget, self.config.side, limit, qty)
    }
}

impl ScheduledBot for MarketBot {
    fn decide(&mut self, view: &ParticipantObservation) -> Result<Vec<OrderAction>, BotError> {
        if view.status != MarketStatus::Running
            || self.state.last_observation == Some((view.step, view.market_time_ms))
        {
            return Ok(vec![]);
        }
        self.state.last_observation = Some((view.step, view.market_time_ms));
        let mid = match (view.book.bids.first(), view.book.asks.first()) {
            (Some(b), Some(a)) => b.price_tick + (a.price_tick - b.price_tick) / 2,
            (Some(b), None) => b.price_tick,
            (None, Some(a)) => a.price_tick,
            _ => view
                .public_trades
                .last()
                .map_or(self.config.fallback_price_tick, |t| t.price_tick),
        }
        .max(1);
        if let Some(previous) = self.state.last_mid {
            self.state.volatility_ticks = ((i128::from(self.state.volatility_ticks) * 3
                + (i128::from(mid) - i128::from(previous)).abs())
                / 4) as i64;
        }
        self.state.last_mid = Some(mid);
        self.state.prices.push(mid);
        if self.state.prices.len() > self.config.lookback {
            self.state.prices.remove(0);
        }
        self.state
            .order_seen_ms
            .retain(|id, _| view.own_orders.iter().any(|o| o.order_id == *id));
        for order in view.own_orders.iter().take(64) {
            self.state
                .order_seen_ms
                .entry(order.order_id)
                .or_insert(view.market_time_ms);
        }
        let expired: Vec<_> = view
            .own_orders
            .iter()
            .take(64)
            .filter(|o| {
                self.state
                    .order_seen_ms
                    .get(&o.order_id)
                    .is_some_and(|seen| {
                        view.market_time_ms.saturating_sub(*seen) >= self.config.order_ttl_ms
                    })
            })
            .map(|o| OrderAction::Cancel {
                order_id: o.order_id,
            })
            .collect();
        if !expired.is_empty() {
            return Ok(expired);
        }
        if view.own_orders.len() > 32 {
            return Ok(self.cancel_orders(view));
        }
        let Some(mut budget) = Budget::new(view, &self.config) else {
            return Ok(vec![]);
        };
        if budget.reduce_only {
            if !view.own_orders.is_empty() {
                return Ok(self.cancel_orders(view));
            }
            let side = if budget.position > 0 {
                Side::Sell
            } else {
                Side::Buy
            };
            let price = match side {
                Side::Buy => view.book.asks.first(),
                Side::Sell => view.book.bids.first(),
            };
            return Ok(price.map_or_else(Vec::new, |top| {
                self.taker(view, &mut budget, side, top.price_tick, self.config.max_qty)
            }));
        }
        if self.plugin.plugin_id == "DynamicMarketMaker"
            && self.state.volatility_ticks >= self.config.withdraw_volatility_ticks
        {
            return Ok(self.cancel_orders(view));
        }
        if view.market_time_ms < self.state.next_decision_ms {
            return Ok(vec![]);
        }
        self.state.next_decision_ms = view
            .market_time_ms
            .saturating_add(self.config.decision_interval_ms)
            .saturating_add(self.random(self.config.jitter_ms + 1));
        let actions = match self.plugin.plugin_id.as_str() {
            "AdaptiveNoiseTrader" => self.noise(view, &mut budget, mid),
            "DynamicMarketMaker" => self.maker(view, &mut budget, mid),
            "ValueTrader" => self.value(view, &mut budget),
            "TrendTrader" => self.trend(view, &mut budget, mid),
            "ExecutionTrader" => self.execution(view, &mut budget, mid),
            _ => unreachable!("factory identity checked by registry"),
        };
        // Cancel our opposite resting orders before a marketable action. The
        // book exposes aggregated levels; otherwise noise/IOC flow could print
        // artificial volume against its own account. Exclusive accounts are
        // required to distinguish execution progress and order ownership.
        let crossing: Vec<_> = view
            .own_orders
            .iter()
            .filter(|order| {
                actions.iter().any(|action| {
                    let (side, price) = match action {
                        OrderAction::PlaceLimit {
                            side, price_tick, ..
                        }
                        | OrderAction::PlacePostOnly {
                            side, price_tick, ..
                        } => (*side, *price_tick),
                        OrderAction::PlaceImmediateOrCancel {
                            side,
                            price_tick: Some(price),
                            ..
                        } => (*side, *price),
                        _ => return false,
                    };
                    order.side != side
                        && match side {
                            Side::Buy => price >= order.price_tick,
                            Side::Sell => price <= order.price_tick,
                        }
                })
            })
            .map(|order| OrderAction::Cancel {
                order_id: order.order_id,
            })
            .collect();
        Ok(if crossing.is_empty() {
            actions
        } else {
            crossing
        })
    }

    fn snapshot(&self) -> PersistedAgentKindState {
        PersistedAgentKindState::Plugin {
            plugin_id: self.plugin.plugin_id.clone(),
            plugin_version: self.plugin.plugin_version.clone(),
            state_version: 1,
            data: serde_json::to_value(&self.state).expect("bounded market bot state serializes"),
        }
    }
}

#[cfg(test)]
#[path = "market_bots_tests.rs"]
mod tests;
