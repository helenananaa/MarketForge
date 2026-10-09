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

pub const MARKET_BOT_IDS: [&str; 10] = [
    "AdaptiveNoiseTrader",
    "DynamicMarketMaker",
    "ValueTrader",
    "TrendTrader",
    "ExecutionTrader",
    "PovExecutionTrader",
    "MarketEventTrader",
    "BasisArbitrageTrader",
    "FundingRateTrader",
    "LeveragedTrendTrader",
];
const PPM: i128 = 1_000_000;

// The catalog exposes only parameters relevant to each strategy; the shared
// representation keeps scheduling, capital checks and recovery identical.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    decision_interval_ms: u64,
    jitter_ms: u64,
    arrival_mode: String,
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
    book_pressure_ticks: i64,
    book_pressure_levels: usize,
    index_basis_ticks: i64,
    toxic_flow_threshold_ppm: u32,
    toxic_flow_min_qty: u64,
    toxic_cooldown_ms: u64,
    recovery_ramp_ms: u64,
    requote_threshold_ticks: i64,
    size_volatility_ticks: u64,
    replenish_depth_ppm: u32,
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
    participation_ppm: u32,
    deadline_urgency_ms: u64,
    confidence_ppm: u32,
    crowd_strength_ppm: u32,
    hedge_instrument_id: String,
    leg: String,
    entry_basis_ticks: i64,
    exit_basis_ticks: i64,
    hedge_timeout_ms: u64,
    target_leverage: u32,
    funding_entry_rate_ppm: u32,
    funding_exit_rate_ppm: u32,
    funding_entry_window_ms: u64,
    risk: crate::bot_risk::BotRiskConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            decision_interval_ms: 1_000,
            jitter_ms: 1_000,
            arrival_mode: "Periodic".into(),
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
            book_pressure_ticks: 0,
            book_pressure_levels: 3,
            index_basis_ticks: 0,
            toxic_flow_threshold_ppm: 0,
            toxic_flow_min_qty: 1,
            toxic_cooldown_ms: 5000,
            recovery_ramp_ms: 5000,
            requote_threshold_ticks: 0,
            size_volatility_ticks: 1,
            replenish_depth_ppm: 0,
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
            participation_ppm: 100_000,
            deadline_urgency_ms: 0,
            confidence_ppm: 1_000_000,
            crowd_strength_ppm: 0,
            hedge_instrument_id: "V-BTC-PERP".into(),
            leg: "Spot".into(),
            entry_basis_ticks: 4,
            exit_basis_ticks: 0,
            hedge_timeout_ms: 5_000,
            target_leverage: 1,
            funding_entry_rate_ppm: 200,
            funding_exit_rate_ppm: 50,
            funding_entry_window_ms: 60000,
            risk: Default::default(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
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
    risk: crate::bot_risk::BotRiskState,
    volume_baseline: Option<String>,
    external_volume_qty: String,
    received_event_ids: Vec<String>,
    event_fair_price: Option<i64>,
    unhedged_since_ms: Option<u64>,
    arbitrage_exiting: bool,
    arbitrage_cycles: u64,
    last_flow_trade_id: Option<u64>,
    toxic_until_ms: u64,
    maker_last_buy_qty: Option<u64>,
    maker_last_sell_qty: Option<u64>,
    funding_cycle_time_ms: Option<u64>,
    funding_exiting: bool,
    last_funding_rate_ppm: Option<i32>,
    target_position: Option<String>,
    deleverage_decisions: u64,
}

struct Factory(BotDescriptor);

pub(crate) fn register_market_bots(registry: &mut BotRegistry) {
    for id in MARKET_BOT_IDS {
        let mut defaults = json!({
            "decision_interval_ms": 1000, "jitter_ms": 1000, "order_ttl_ms": 6000,
            "inventory_cap": 100, "max_qty": 2,
            "fee_buffer_ppm": 5000, "fallback_price_tick": 100,
            "risk": {},
        });
        let (name, specific) = match id {
            "AdaptiveNoiseTrader" => (
                "异质噪声交易者",
                json!({"activity_ppm":400000,"side_persistence_ppm":650000,"market_order_ratio_ppm":400000,"price_radius_ticks":3,"arrival_mode":"Periodic"}),
            ),
            "DynamicMarketMaker" => (
                "动态库存做市商",
                json!({"inventory_target":0,"half_spread_ticks":1,"inventory_skew_ticks":4,"volatility_spread_multiplier":2,"withdraw_volatility_ticks":20,"levels":2,"level_spacing_ticks":1,
                    "book_pressure_ticks":0,"book_pressure_levels":3,"index_basis_ticks":0,"toxic_flow_threshold_ppm":0,"toxic_flow_min_qty":1,"toxic_cooldown_ms":5000,"recovery_ramp_ms":5000,"requote_threshold_ticks":0,"size_volatility_ticks":1,"replenish_depth_ppm":0}),
            ),
            "ValueTrader" => (
                "价值与信息交易者",
                json!({"fair_price_tick":100,"edge_ticks":1,"value_shift_at_ms":60000,"value_shift_ticks":0,"information_delay_ms":0}),
            ),
            "TrendTrader" => (
                "趋势与退出交易者",
                json!({"inventory_target":0,"lookback":8,"signal_threshold_ticks":2,"position_size":10}),
            ),
            "MarketEventTrader" => (
                "消息与群体交易者",
                json!({"fair_price_tick":100,"information_delay_ms":0,"confidence_ppm":1000000,"crowd_strength_ppm":0,"inventory_target":0,"position_size":10,"edge_ticks":1,"lookback":8}),
            ),
            "BasisArbitrageTrader" => (
                "现货永续基差套利腿",
                json!({"hedge_instrument_id":"V-BTC-PERP","leg":"Spot","entry_basis_ticks":4,"exit_basis_ticks":0,"position_size":10,"hedge_timeout_ms":5000,"max_slippage_ticks":2}),
            ),
            "PovExecutionTrader" => (
                "成交量参与率执行者",
                json!({"side":"Buy","target_qty":20,"horizon_ms":120000,"start_after_ms":0,"max_slippage_ticks":3,"participation_ppm":100000,"deadline_urgency_ms":0}),
            ),
            "FundingRateTrader" => (
                "资金费率持仓交易者",
                json!({"target_leverage":1,"position_size":10,"funding_entry_rate_ppm":200,"funding_exit_rate_ppm":50,"funding_entry_window_ms":60000,"max_slippage_ticks":3}),
            ),
            "LeveragedTrendTrader" => (
                "杠杆趋势与主动减仓交易者",
                json!({"target_leverage":2,"lookback":8,"signal_threshold_ticks":2,"position_size":100,"max_slippage_ticks":3}),
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
        if id == "BasisArbitrageTrader" {
            defaults.as_object_mut().unwrap().remove("risk");
        }
        let parameters = defaults
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| {
                let (minimum, maximum) = match key.as_str() {
                    "inventory_target" | "value_shift_ticks" | "exit_basis_ticks"
                    | "index_basis_ticks" => (-1_000_000, 1_000_000),
                    "lookback" => (2, 128),
                    "levels" => (1, 8),
                    "book_pressure_levels" => (1, 8),
                    "target_leverage" => (1, 20),
                    "toxic_flow_min_qty" => (1, 1_000_000),
                    "size_volatility_ticks" => (1, 1_000_000_000),
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
                        kind: if value.is_object() {
                            ParameterType::Object
                        } else if value.is_string() {
                            ParameterType::String
                        } else {
                            ParameterType::Integer
                        },
                        required: false,
                        default: Some(value.clone()),
                        minimum: value.is_number().then_some(minimum),
                        maximum: value.is_number().then_some(maximum),
                        choices: if key == "side" {
                            vec![json!("Buy"), json!("Sell")]
                        } else if key == "arrival_mode" {
                            vec![json!("Periodic"), json!("Poisson")]
                        } else if key == "leg" {
                            vec![json!("Spot"), json!("Perp")]
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

    fn related_instruments(&self, template: &AgentTemplate) -> Result<Vec<String>, BotError> {
        if self.0.id != "BasisArbitrageTrader" {
            return Ok(vec![]);
        }
        let AgentTemplate::Plugin(plugin) = template else {
            return Err(BotError("plugin required".into()));
        };
        let config: Config =
            serde_json::from_value(self.0.validate_config(&plugin.config)?).map_err(json_error)?;
        if Some(&config.hedge_instrument_id) == plugin.participant.instrument_id.as_ref() {
            return Err(BotError(
                "arbitrage requires two different instruments".into(),
            ));
        }
        Ok(vec![config.hedge_instrument_id])
    }

    fn market_data_request(
        &self,
        _template: &AgentTemplate,
    ) -> Result<Option<crate::bots::BotMarketDataRequest>, BotError> {
        Ok(
            (self.0.id == "PovExecutionTrader").then_some(crate::bots::BotMarketDataRequest {
                interval_ms: 1000,
                max_bars: 4096,
            }),
        )
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
        if !config.risk.validate()
            || config.deadline_urgency_ms > config.horizon_ms
            || (self.0.id == "BasisArbitrageTrader"
                && (config.exit_basis_ticks >= config.entry_basis_ticks
                    || config.position_size > config.inventory_cap
                    || config.hedge_timeout_ms == 0))
        {
            return Err(BotError("invalid risk, urgency or arbitrage config".into()));
        }
        if self.0.id == "FundingRateTrader"
            && (config.funding_exit_rate_ppm >= config.funding_entry_rate_ppm
                || config.funding_entry_window_ms == 0)
        {
            return Err(BotError(
                "funding exit threshold must be below entry and entry window positive".into(),
            ));
        }
        if matches!(
            self.0.id.as_str(),
            "FundingRateTrader" | "LeveragedTrendTrader"
        ) && config.position_size > config.inventory_cap
        {
            return Err(BotError(
                "position_size must be within inventory_cap".into(),
            ));
        }
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
            if !state.risk.valid()
                || state
                    .volume_baseline
                    .as_ref()
                    .is_some_and(|v| v.parse::<u128>().is_err())
                || (!state.external_volume_qty.is_empty()
                    && state.external_volume_qty.parse::<u128>().is_err())
                || state.received_event_ids.len() > 256
                || state.rng == 0
                || state.prices.len() > config.lookback
                || state.order_seen_ms.len() > 64
                || state.volatility_ticks < 0
                || state.prices.iter().any(|price| *price <= 0)
                || state.last_mid.is_some_and(|price| price <= 0)
                || state
                    .target_position
                    .as_ref()
                    .is_some_and(|v| v.parse::<i128>().is_err())
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
// defaults to full notional; the two perpetual motive traders explicitly choose
// a target leverage. Reductions remain possible without
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
    leverage: u32,
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
            leverage: config.target_leverage,
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
        let cost = if self.spot_available.is_some() {
            (i128::from(price) * (PPM + i128::from(self.fee_ppm)) + PPM - 1) / PPM
        } else {
            (i128::from(price) + i128::from(self.leverage) - 1) / i128::from(self.leverage)
                + (i128::from(price) * i128::from(self.fee_ppm) + PPM - 1) / PPM
        };
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
    fn protected_taker(
        &self,
        view: &ParticipantObservation,
        budget: &mut Budget,
        side: Side,
        mid: i64,
        qty: u64,
    ) -> Vec<OrderAction> {
        let limit = match side {
            Side::Buy => mid.saturating_add(self.config.max_slippage_ticks),
            Side::Sell => mid.saturating_sub(self.config.max_slippage_ticks).max(1),
        };
        let crossing: Vec<_> = view
            .own_orders
            .iter()
            .filter(|o| {
                o.side != side
                    && match side {
                        Side::Buy => limit >= o.price_tick,
                        Side::Sell => limit <= o.price_tick,
                    }
            })
            .take(64)
            .map(|o| OrderAction::Cancel {
                order_id: o.order_id,
            })
            .collect();
        if !crossing.is_empty() {
            return crossing;
        }
        let Some(top) = (match side {
            Side::Buy => view.book.asks.first(),
            Side::Sell => view.book.bids.first(),
        }) else {
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
        vec![OrderAction::PlaceProtected {
            position_side: Default::default(),
            side,
            qty,
            price_tick: limit,
            order_type: crate::model::ProtectedOrderType::ImmediateOrCancel,
            reduce_only: budget.reduce_only,
            valid_until_market_time_ms: Some(
                view.market_time_ms.saturating_add(self.config.order_ttl_ms),
            ),
            expires_at_market_time_ms: None,
        }]
    }

    fn event_fair(&mut self, view: &ParticipantObservation, mid: i64) -> i64 {
        let mut impact = 0i128;
        for event in view.market_events.iter().take(256) {
            if view.market_time_ms
                < event
                    .published_at_ms
                    .saturating_add(self.config.information_delay_ms)
            {
                continue;
            }
            if !self.state.received_event_ids.contains(&event.id) {
                self.state.received_event_ids.push(event.id.clone());
            }
            if view.market_time_ms < event.expires_at_ms {
                impact = impact.saturating_add(event.impact_ticks.into());
            }
        }
        let crowd = self
            .state
            .prices
            .first()
            .map_or(0, |p| i128::from(mid) - i128::from(*p));
        let fair = (i128::from(self.config.fair_price_tick)
            + impact * i128::from(self.config.confidence_ppm) / PPM
            + crowd * i128::from(self.config.crowd_strength_ppm) / PPM)
            .clamp(1, i128::from(i64::MAX)) as i64;
        self.state.event_fair_price = Some(fair);
        fair
    }

    fn event(
        &mut self,
        view: &ParticipantObservation,
        budget: &mut Budget,
        mid: i64,
    ) -> Vec<OrderAction> {
        let fair = self.event_fair(view, mid);
        let offset = if fair > mid.saturating_add(self.config.edge_ticks) {
            self.config.position_size
        } else if fair < mid.saturating_sub(self.config.edge_ticks) {
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
        let qty = delta.unsigned_abs().min(self.config.max_qty.into()) as u64;
        self.protected_taker(view, budget, side, mid, qty)
    }

    /// Two independently scheduled legs coordinate using actual inventory.
    /// Each owns one market of the same exclusive account. No synthetic fills
    /// or atomic two-leg execution: the spot leg waits for the hedge and unwinds
    /// on timeout; the perp leg always reconciles to the acquired spot quantity.
    fn arbitrage(
        &mut self,
        view: &ParticipantObservation,
        budget: &mut Budget,
    ) -> Result<Vec<OrderAction>, BotError> {
        let peer = view
            .related_markets
            .iter()
            .find(|p| p.instrument_id == self.config.hedge_instrument_id)
            .ok_or_else(|| BotError("arbitrage requires a peer-market observation".into()))?;
        if peer.market_time_ms != view.market_time_ms || peer.venue_id != view.venue_id {
            return Err(BotError(
                "arbitrage requires coherent same-venue observations".into(),
            ));
        }
        let (spot, perp) = if self.config.leg == "Spot" {
            (view, peer)
        } else {
            (peer, view)
        };
        let (Some(AccountSnapshot::Spot(sa)), Some(AccountSnapshot::Perp(pa))) =
            (&spot.own_account, &perp.own_account)
        else {
            return Err(BotError(
                "arbitrage requires a funded spot/perp account pair".into(),
            ));
        };
        let Some(price) = &perp.perp_price else {
            return Err(BotError(
                "arbitrage requires explicitly linked perpetual".into(),
            ));
        };
        if price.spot_instrument_id != spot.instrument_id
            || sa.account_id != pa.account_id
            || sa.account_id != self.plugin.participant.account_id
            || sa.position_qty < 0
            || pa.position_qty > 0
        {
            return Err(BotError(
                "invalid cash-and-carry account pair or positions".into(),
            ));
        }
        if !view.own_orders.is_empty() {
            return Ok(self.cancel_orders(view));
        }
        let residual = sa.position_qty.saturating_add(pa.position_qty);
        let healthy = matches!(
            pa.margin_status,
            PerpMarginStatus::Flat | PerpMarginStatus::Healthy
        ) && price.status == crate::PriceLinkStatus::Live;
        let mid = |v: &ParticipantObservation| match (v.book.bids.first(), v.book.asks.first()) {
            (Some(b), Some(a)) => b.price_tick + (a.price_tick - b.price_tick) / 2,
            (Some(b), None) => b.price_tick,
            (None, Some(a)) => a.price_tick,
            _ => self.config.fallback_price_tick,
        };
        if self.config.leg == "Perp" {
            if residual < 0 {
                budget.reduce_only = true;
                return Ok(self.protected_taker(
                    view,
                    budget,
                    Side::Buy,
                    mid(view),
                    residual.unsigned_abs().min(self.config.max_qty.into()) as u64,
                ));
            }
            if residual > 0 && healthy {
                return Ok(self.protected_taker(
                    view,
                    budget,
                    Side::Sell,
                    mid(view),
                    residual.unsigned_abs().min(self.config.max_qty.into()) as u64,
                ));
            }
            return Ok(vec![]);
        }
        if residual > 0 {
            let since = *self
                .state
                .unhedged_since_ms
                .get_or_insert(view.market_time_ms);
            if view.market_time_ms.saturating_sub(since) >= self.config.hedge_timeout_ms {
                self.state.arbitrage_exiting = true;
            }
        } else {
            self.state.unhedged_since_ms = None;
        }
        if sa.position_qty == 0 && pa.position_qty == 0 && self.state.arbitrage_exiting {
            self.state.arbitrage_exiting = false;
            self.state.arbitrage_cycles = self.state.arbitrage_cycles.saturating_add(1);
            self.state.next_decision_ms = view
                .market_time_ms
                .saturating_add(self.config.hedge_timeout_ms);
        }
        let exit_spread = spot
            .book
            .bids
            .first()
            .zip(perp.book.asks.first())
            .map(|(s, p)| p.price_tick.saturating_sub(s.price_tick));
        if sa.position_qty > 0
            && (!healthy || exit_spread.is_some_and(|b| b <= self.config.exit_basis_ticks))
        {
            self.state.arbitrage_exiting = true;
        }
        if residual < 0 {
            return Ok(vec![]);
        }
        if self.state.arbitrage_exiting {
            return Ok(self.protected_taker(
                view,
                budget,
                Side::Sell,
                mid(view),
                sa.position_qty
                    .unsigned_abs()
                    .min(self.config.max_qty.into()) as u64,
            ));
        }
        if residual != 0 || !healthy || view.market_time_ms < self.state.next_decision_ms {
            return Ok(vec![]);
        }
        let Some((ask, bid)) = spot.book.asks.first().zip(perp.book.bids.first()) else {
            return Ok(vec![]);
        };
        // Conservative round-trip fee budget for both legs; spread is executable.
        let costs = ((i128::from(ask.price_tick) + i128::from(bid.price_tick))
            * i128::from(self.config.fee_buffer_ppm)
            * 2
            + PPM
            - 1)
            / PPM;
        if i128::from(bid.price_tick) - i128::from(ask.price_tick) - costs
            < i128::from(self.config.entry_basis_ticks)
        {
            return Ok(vec![]);
        }
        let requested = (i128::from(self.config.position_size) - sa.position_qty)
            .max(0)
            .min(self.config.max_qty.into()) as u64;
        let mut hedge_budget = Budget::new(perp, &self.config)
            .ok_or_else(|| BotError("missing hedge budget".into()))?;
        let qty = hedge_budget
            .allocate(Side::Sell, bid.price_tick, requested)
            .min(ask.qty)
            .min(bid.qty);
        self.state.next_decision_ms = view
            .market_time_ms
            .saturating_add(self.config.decision_interval_ms);
        Ok(self.protected_taker(view, budget, Side::Buy, mid(view), qty))
    }

    fn arrival_delay(&mut self) -> u64 {
        if self.config.arrival_mode == "Poisson" {
            // A private, persisted RNG provides an independent exponential
            // waiting time. Actions still occur on the simulation clock grid.
            let uniform = (self.random(1_000_000) + 1) as f64 / 1_000_001.0;
            (-uniform.ln() * self.config.decision_interval_ms as f64)
                .ceil()
                .max(1.0) as u64
        } else {
            self.config
                .decision_interval_ms
                .saturating_add(self.random(self.config.jitter_ms + 1))
        }
    }

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
            || view.market_time_ms < self.state.toxic_until_ms
            || self.state.volatility_ticks >= self.config.withdraw_volatility_ticks
        {
            return self.cancel_orders(view);
        }
        let deviation = budget.position - i128::from(self.config.inventory_target);
        let skew = deviation * i128::from(self.config.inventory_skew_ticks)
            / i128::from(self.config.inventory_cap);
        let depth = |side: Side| -> i128 {
            let levels = if side == Side::Buy {
                &view.book.bids
            } else {
                &view.book.asks
            };
            levels
                .iter()
                .take(self.config.book_pressure_levels)
                .map(|level| {
                    let own: i128 = view
                        .own_orders
                        .iter()
                        .filter(|order| order.side == side && order.price_tick == level.price_tick)
                        .map(|order| i128::from(order.remaining_qty))
                        .sum();
                    (i128::from(level.qty) - own).max(0)
                })
                .sum()
        };
        let (bid_depth, ask_depth) = (depth(Side::Buy), depth(Side::Sell));
        let imbalance = (bid_depth - ask_depth) * PPM / (bid_depth + ask_depth).max(1);
        let pressure = imbalance * i128::from(self.config.book_pressure_ticks) / PPM;
        let basis = if view.perp_price.is_some() {
            self.config.index_basis_ticks
        } else {
            0
        };
        let center = (i128::from(mid) + i128::from(basis) + pressure - skew)
            .clamp(1, i128::from(i64::MAX)) as i64;
        let spread = self.config.half_spread_ticks.saturating_add(
            self.state
                .volatility_ticks
                .saturating_mul(self.config.volatility_spread_multiplier),
        );
        let mut size = (self.config.max_qty
            / (1 + self.state.volatility_ticks as u64 / self.config.size_volatility_ticks))
            .max(1);
        if self.config.toxic_flow_threshold_ppm > 0
            && self.state.toxic_until_ms > 0
            && self.config.recovery_ramp_ms > 0
        {
            let ramp = view
                .market_time_ms
                .saturating_sub(self.state.toxic_until_ms)
                .min(self.config.recovery_ramp_ms);
            size = (u128::from(size) * u128::from(ramp) / u128::from(self.config.recovery_ramp_ms))
                as u64;
        }
        let side_size = |side: Side| -> u64 {
            if self.config.book_pressure_ticks == 0 {
                return size;
            }
            let weight = if side == Side::Buy {
                PPM + imbalance
            } else {
                PPM - imbalance
            };
            (i128::from(size) * weight / PPM).clamp(0, i128::from(u64::MAX)) as u64
        };
        let (buy_size, sell_size) = (side_size(Side::Buy), side_size(Side::Sell));
        let threshold = self.config.requote_threshold_ticks.max(1);
        let moved = |previous: Option<i64>, next: i64| {
            previous.is_none_or(|p| p.abs_diff(next) >= threshold as u64)
        };
        let changed = moved(self.state.last_quote_center, center)
            || moved(self.state.last_quote_spread, spread)
            || (self.config.book_pressure_ticks > 0 || self.config.toxic_flow_threshold_ppm > 0)
                && (self.state.maker_last_buy_qty != Some(buy_size)
                    || self.state.maker_last_sell_qty != Some(sell_size));
        // Cancellation is acknowledged through a subsequent observation. Never
        // spend cash that might remain reserved if a cancellation failed.
        if changed && !view.own_orders.is_empty() {
            return self.cancel_orders(view);
        }
        let mut actions = vec![];
        for level in 0..self.config.levels {
            let distance =
                spread.saturating_add(i64::from(level) * self.config.level_spacing_ticks);
            for side in [Side::Buy, Side::Sell] {
                let mut price = match side {
                    Side::Buy => center.saturating_sub(distance).max(1),
                    Side::Sell => center.saturating_add(distance),
                };
                if self.config.book_pressure_ticks > 0 {
                    match side {
                        Side::Buy => {
                            if let Some(ask) = view.book.asks.first() {
                                price = price.min(ask.price_tick.saturating_sub(1));
                            }
                        }
                        Side::Sell => {
                            if let Some(bid) = view.book.bids.first() {
                                price = price.max(bid.price_tick.saturating_add(1));
                            }
                        }
                    }
                    if price <= 0 {
                        continue;
                    }
                }
                let existing = view
                    .own_orders
                    .iter()
                    .filter(|o| o.side == side && o.price_tick == price)
                    .fold(0u64, |sum, o| sum.saturating_add(o.remaining_qty));
                let target = if side == Side::Buy {
                    buy_size
                } else {
                    sell_size
                };
                if existing > 0
                    && (self.config.replenish_depth_ppm == 0
                        || u128::from(existing) * PPM as u128
                            >= u128::from(target) * u128::from(self.config.replenish_depth_ppm))
                {
                    continue;
                }
                let qty = budget.allocate(side, price, target.saturating_sub(existing));
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
        self.state.maker_last_buy_qty = Some(buy_size);
        self.state.maker_last_sell_qty = Some(sell_size);
        actions
    }

    fn update_maker_flow(&mut self, view: &ParticipantObservation) {
        if self.config.toxic_flow_threshold_ppm == 0 {
            return;
        }
        let mut buys = 0u128;
        let mut sells = 0u128;
        for trade in &view.public_trades {
            if self
                .state
                .last_flow_trade_id
                .is_some_and(|id| trade.trade_id <= id)
                || trade.maker_account_id != self.plugin.participant.account_id
                || trade.taker_account_id == trade.maker_account_id
            {
                continue;
            }
            match trade.taker_side {
                Side::Buy => buys += u128::from(trade.qty),
                Side::Sell => sells += u128::from(trade.qty),
            }
        }
        if let Some(id) = view.public_trades.iter().map(|t| t.trade_id).max() {
            self.state.last_flow_trade_id =
                Some(self.state.last_flow_trade_id.unwrap_or(id).max(id));
        }
        let total = buys + sells;
        if total >= u128::from(self.config.toxic_flow_min_qty)
            && buys.max(sells) * PPM as u128
                >= total * u128::from(self.config.toxic_flow_threshold_ppm)
        {
            self.state.toxic_until_ms = view
                .market_time_ms
                .saturating_add(self.config.toxic_cooldown_ms);
        }
    }

    fn perp_target(
        &mut self,
        view: &ParticipantObservation,
        budget: &mut Budget,
        mid: i64,
        wanted: i128,
    ) -> Vec<OrderAction> {
        let Some(AccountSnapshot::Perp(account)) = &view.own_account else {
            return vec![];
        };
        let mark = view
            .perp_price
            .as_ref()
            .map_or(mid, |price| price.mark_price_tick)
            .max(1);
        let limit = account
            .equity
            .max(0)
            .saturating_mul(i128::from(self.config.target_leverage))
            / i128::from(mark);
        let target = wanted.clamp(-limit, limit).clamp(-budget.cap, budget.cap);
        self.state.target_position = Some(target.to_string());
        // Flatten first on reversal. Progress and the next opening leg depend on
        // actual account positions, never a presumed IOC fill.
        let effective =
            if budget.position != 0 && target != 0 && budget.position.signum() != target.signum() {
                0
            } else {
                target
            };
        let delta = effective.saturating_sub(budget.position);
        if delta == 0 {
            return vec![];
        }
        budget.reduce_only = budget.position != 0
            && (effective == 0
                || effective.signum() == budget.position.signum()
                    && effective.unsigned_abs() < budget.position.unsigned_abs());
        if budget.reduce_only {
            self.state.deleverage_decisions = self.state.deleverage_decisions.saturating_add(1);
        }
        self.protected_taker(
            view,
            budget,
            if delta > 0 { Side::Buy } else { Side::Sell },
            mid,
            delta.unsigned_abs().min(self.config.max_qty.into()) as u64,
        )
    }

    fn funding(
        &mut self,
        view: &ParticipantObservation,
        budget: &mut Budget,
        mid: i64,
    ) -> Vec<OrderAction> {
        let funding = view
            .perp_price
            .as_ref()
            .and_then(|price| price.funding.as_ref());
        let rate = funding.and_then(|funding| funding.estimated_rate_ppm);
        self.state.last_funding_rate_ppm = rate;
        if budget.position == 0 && self.state.funding_exiting {
            self.state.funding_cycle_time_ms = None;
            self.state.funding_exiting = false;
        }
        if budget.position != 0
            && (rate.is_none_or(|r| {
                r.unsigned_abs() <= self.config.funding_exit_rate_ppm
                    || (r > 0 && budget.position > 0)
                    || (r < 0 && budget.position < 0)
            }) || funding.is_none_or(|f| {
                self.state
                    .funding_cycle_time_ms
                    .is_some_and(|time| f.next_funding_time_ms != time)
            }))
        {
            self.state.funding_exiting = true;
        }
        if self.state.funding_exiting {
            return self.perp_target(view, budget, mid, 0);
        }
        let Some(funding) = funding else {
            return vec![];
        };
        let Some(rate) = rate else {
            return vec![];
        };
        if rate.unsigned_abs() < self.config.funding_entry_rate_ppm
            || funding.next_funding_time_ms <= view.market_time_ms
            || funding
                .next_funding_time_ms
                .saturating_sub(view.market_time_ms)
                > self.config.funding_entry_window_ms
        {
            return vec![];
        }
        self.state.funding_cycle_time_ms = Some(funding.next_funding_time_ms);
        self.perp_target(
            view,
            budget,
            mid,
            if rate > 0 {
                -i128::from(self.config.position_size)
            } else {
                self.config.position_size.into()
            },
        )
    }

    fn leveraged_trend(
        &mut self,
        view: &ParticipantObservation,
        budget: &mut Budget,
        mid: i64,
    ) -> Vec<OrderAction> {
        let change = self
            .state
            .prices
            .first()
            .map_or(0, |first| mid.saturating_sub(*first));
        let target = if change.unsigned_abs() >= self.config.signal_threshold_ticks as u64 {
            i128::from(change.signum()) * i128::from(self.config.position_size)
        } else {
            0
        };
        self.perp_target(view, budget, mid, target)
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
        let mut due = (u128::from(self.config.target_qty)
            * u128::from(
                elapsed
                    .saturating_add(self.config.decision_interval_ms)
                    .min(self.config.horizon_ms),
            ))
        .div_ceil(u128::from(self.config.horizon_ms)) as u64;
        if self.plugin.plugin_id == "PovExecutionTrader" {
            let Some(data) = &view.bot_market_data else {
                return vec![];
            };
            if data.truncated {
                return vec![];
            }
            let Ok(volume) = data.external_volume_qty.parse::<u128>() else {
                return vec![];
            };
            let baseline = self
                .state
                .volume_baseline
                .get_or_insert_with(|| volume.to_string())
                .parse::<u128>()
                .expect("validated volume baseline");
            if volume < baseline {
                return vec![];
            }
            let external = volume - baseline;
            self.state.external_volume_qty = external.to_string();
            due = external
                .saturating_mul(self.config.participation_ppm.into())
                .checked_div(PPM as u128)
                .unwrap_or(0)
                .min(self.config.target_qty.into()) as u64;
            // Optional final urgency explicitly relaxes participation, never the price limit.
            if self.config.deadline_urgency_ms > 0
                && self.config.horizon_ms - elapsed <= self.config.deadline_urgency_ms
            {
                due = self.config.target_qty;
            }
        }
        let qty = due
            .saturating_sub(self.state.completed_qty)
            .min(self.config.target_qty - self.state.completed_qty)
            .min(self.config.max_qty);
        let limit = match self.config.side {
            Side::Buy => mid.saturating_add(self.config.max_slippage_ticks),
            Side::Sell => mid.saturating_sub(self.config.max_slippage_ticks).max(1),
        };
        if self.plugin.plugin_id == "PovExecutionTrader" {
            let mut actions = self.protected_taker(view, budget, self.config.side, mid, qty);
            for action in &mut actions {
                if let OrderAction::PlaceProtected {
                    valid_until_market_time_ms,
                    ..
                } = action
                {
                    *valid_until_market_time_ms = Some(
                        valid_until_market_time_ms
                            .unwrap_or(u64::MAX)
                            .min(start.saturating_add(self.config.horizon_ms)),
                    );
                }
            }
            actions
        } else {
            self.taker(view, budget, self.config.side, limit, qty)
        }
    }
}

impl ScheduledBot for MarketBot {
    fn decide(&mut self, view: &ParticipantObservation) -> Result<Vec<OrderAction>, BotError> {
        if view.status != MarketStatus::Running
            || self.state.last_observation == Some((view.step, view.market_time_ms))
        {
            return Ok(vec![]);
        }
        if matches!(&view.own_account, Some(AccountSnapshot::Perp(a)) if a.hedge_positions.is_some())
        {
            return Err(BotError(
                "native market bots require one-way perpetual positions".into(),
            ));
        }
        if matches!(
            self.plugin.plugin_id.as_str(),
            "FundingRateTrader" | "LeveragedTrendTrader"
        ) && !matches!(view.own_account, Some(AccountSnapshot::Perp(_)))
        {
            return Err(BotError(
                "funding and leveraged traders require a perpetual account".into(),
            ));
        }
        if self.state.last_observation.is_none() && self.config.arrival_mode == "Poisson" {
            self.state.next_decision_ms = view.market_time_ms.saturating_add(self.arrival_delay());
        }
        self.state.last_observation = Some((view.step, view.market_time_ms));
        let book_mid = match (view.book.bids.first(), view.book.asks.first()) {
            (Some(b), Some(a)) => b.price_tick + (a.price_tick - b.price_tick) / 2,
            (Some(b), None) => b.price_tick,
            (None, Some(a)) => a.price_tick,
            _ => view
                .public_trades
                .last()
                .map_or(self.config.fallback_price_tick, |t| t.price_tick),
        }
        .max(1);
        let mid = if self.plugin.plugin_id == "DynamicMarketMaker" {
            view.perp_price
                .as_ref()
                .and_then(|price| price.index_price_tick)
                .unwrap_or(book_mid)
        } else {
            book_mid
        };
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
        if self.plugin.plugin_id == "MarketEventTrader" {
            self.event_fair(view, book_mid);
        }
        if self.plugin.plugin_id == "DynamicMarketMaker" {
            self.update_maker_flow(view);
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
        if self.plugin.plugin_id == "BasisArbitrageTrader" {
            return self.arbitrage(view, &mut budget);
        }
        if self.state.risk.evaluate(
            &self.config.risk,
            view,
            book_mid,
            self.state.volatility_ticks,
        ) {
            if !view.own_orders.is_empty() {
                return Ok(self.cancel_orders(view));
            }
            if budget.position == 0 {
                return Ok(vec![]);
            }
            budget.reduce_only = matches!(view.own_account, Some(AccountSnapshot::Perp(_)));
            let side = if budget.position > 0 {
                Side::Sell
            } else {
                Side::Buy
            };
            let qty = budget
                .position
                .unsigned_abs()
                .min(self.config.max_qty.into()) as u64;
            return Ok(self.protected_taker(view, &mut budget, side, book_mid, qty));
        }
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
            && view
                .perp_price
                .as_ref()
                .is_some_and(|price| price.status != crate::PriceLinkStatus::Live)
        {
            return Ok(self.cancel_orders(view));
        }
        if self.plugin.plugin_id == "DynamicMarketMaker"
            && view.market_time_ms < self.state.toxic_until_ms
        {
            return Ok(self.cancel_orders(view));
        }
        if matches!(
            self.plugin.plugin_id.as_str(),
            "FundingRateTrader" | "LeveragedTrendTrader"
        ) {
            let Some(AccountSnapshot::Perp(account)) = &view.own_account else {
                unreachable!()
            };
            let live = view
                .perp_price
                .as_ref()
                .is_some_and(|p| p.status == crate::PriceLinkStatus::Live);
            let mark = view
                .perp_price
                .as_ref()
                .map_or(book_mid, |p| p.mark_price_tick)
                .max(1);
            let limit = account
                .equity
                .max(0)
                .saturating_mul(i128::from(self.config.target_leverage))
                / i128::from(mark);
            if !live || budget.position.unsigned_abs() > limit as u128 {
                if !view.own_orders.is_empty() {
                    return Ok(self.cancel_orders(view));
                }
                let wanted = if live { budget.position } else { 0 };
                return Ok(self.perp_target(view, &mut budget, book_mid, wanted));
            }
        }
        if self.plugin.plugin_id == "DynamicMarketMaker"
            && self.state.volatility_ticks >= self.config.withdraw_volatility_ticks
        {
            return Ok(self.cancel_orders(view));
        }
        if view.market_time_ms < self.state.next_decision_ms {
            return Ok(vec![]);
        }
        self.state.next_decision_ms = view.market_time_ms.saturating_add(self.arrival_delay());
        let actions = match self.plugin.plugin_id.as_str() {
            "AdaptiveNoiseTrader" => self.noise(view, &mut budget, mid),
            "DynamicMarketMaker" => self.maker(view, &mut budget, mid),
            "ValueTrader" => self.value(view, &mut budget),
            "TrendTrader" => self.trend(view, &mut budget, mid),
            "MarketEventTrader" => self.event(view, &mut budget, book_mid),
            "FundingRateTrader" => self.funding(view, &mut budget, book_mid),
            "LeveragedTrendTrader" => self.leveraged_trend(view, &mut budget, book_mid),
            "ExecutionTrader" | "PovExecutionTrader" => self.execution(view, &mut budget, mid),
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
                        OrderAction::PlaceReduceOnlyImmediateOrCancel {
                            side,
                            price_tick: Some(price),
                            ..
                        }
                        | OrderAction::PlaceProtected {
                            side,
                            price_tick: price,
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
