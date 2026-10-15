//! Parameterized strategy populations on spot and one-way perpetual accounts.
//! Signals consume real closed candles. Execution follows authoritative account
//! positions; rejected/partial orders never create an assumed position.
use crate::bot_risk::{BotRiskConfig, BotRiskState};
use crate::bots::{BotEventFilter, BotMarketDataRequest};
use crate::market_bots::Budget;
use crate::observation::{ParticipantObservationView, SharedParticipantObservation};
use crate::{
    AccountSnapshot, AgentTemplate, BotConfig, BotDescriptor, BotError, BotFactory, BotParameter,
    BotRegistry, Candle, MarketStatus, OrderAction, ParameterType, ParticipantObservation,
    PersistedAgentKindState, ScheduledBot, Side,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::VecDeque, sync::Arc};
#[path = "technical_signals.rs"]
pub mod signals;
pub const ID: &str = "TechnicalStrategyTrader";
pub const VERSION: &str = "1.0.0";
const MAX_PRICE: i64 = (1_i64 << 41) - 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Config {
    family: String,
    parameters: Value,
    bar_interval_ms: u64,
    history_limit: usize,
    position_mode: String,
    position_size: u64,
    inventory_cap: i64,
    capital_fraction_ppm: u32,
    target_leverage: u32,
    max_qty: u64,
    slippage_ticks: i64,
    fee_buffer_ppm: u32,
    initial_avg_price_tick: i64,
    stop_loss_ppm: u32,
    take_profit_ppm: u32,
    max_holding_ms: u64,
    cooldown_ms: u64,
    signal_delay_ms: u64,
    risk: BotRiskConfig,
}

fn param(
    kind: ParameterType,
    default: Value,
    min: Option<i64>,
    max: Option<i64>,
    choices: Vec<Value>,
) -> BotParameter {
    BotParameter {
        kind,
        required: false,
        default: Some(default),
        minimum: min,
        maximum: max,
        choices,
    }
}
fn number(default: i64, min: i64, max: i64) -> BotParameter {
    param(
        ParameterType::Integer,
        json!(default),
        Some(min),
        Some(max),
        vec![],
    )
}
pub(crate) fn register(registry: &mut BotRegistry) {
    use std::collections::BTreeMap;
    let parameters = BTreeMap::from([
        (
            "family".into(),
            param(
                ParameterType::String,
                json!("EmaCross"),
                None,
                None,
                signals::FAMILIES.iter().map(|s| json!(s)).collect(),
            ),
        ),
        (
            "parameters".into(),
            param(ParameterType::Object, json!({}), None, None, vec![]),
        ),
        (
            "position_mode".into(),
            param(
                ParameterType::String,
                json!("LongShort"),
                None,
                None,
                vec![json!("LongShort"), json!("LongOnly"), json!("ShortOnly")],
            ),
        ),
        ("bar_interval_ms".into(), number(10000, 1000, 86400000)),
        ("history_limit".into(), number(1024, 16, 4095)),
        ("position_size".into(), number(20, 1, 1000000)),
        ("inventory_cap".into(), number(200, 1, 1000000)),
        ("capital_fraction_ppm".into(), number(200000, 1, 1000000)),
        ("target_leverage".into(), number(1, 1, 20)),
        ("max_qty".into(), number(4, 1, 1000000)),
        ("slippage_ticks".into(), number(2, 0, 1000000)),
        ("fee_buffer_ppm".into(), number(1000, 0, 1000000)),
        ("initial_avg_price_tick".into(), number(0, 0, MAX_PRICE)),
        ("stop_loss_ppm".into(), number(0, 0, 1000000)),
        ("take_profit_ppm".into(), number(0, 0, 1000000)),
        ("max_holding_ms".into(), number(0, 0, 864000000)),
        ("cooldown_ms".into(), number(15000, 0, 86400000)),
        ("signal_delay_ms".into(), number(0, 0, 86400000)),
        (
            "risk".into(),
            param(ParameterType::Object, json!({}), None, None, vec![]),
        ),
    ]);
    registry
        .register(Factory(BotDescriptor {
            id: ID.into(),
            name: "可配置多周期现货永续策略".into(),
            version: VERSION.into(),
            protocol_version: crate::bots::BOT_PROTOCOL_VERSION.into(),
            state_version: 1,
            runtime: "builtin".into(),
            parameters,
        }))
        .expect("valid technical catalog");
}

struct Factory(BotDescriptor);
impl Factory {
    fn config(
        &self,
        template: &AgentTemplate,
    ) -> Result<(BotConfig, Config, signals::Params, String), BotError> {
        let AgentTemplate::Plugin(original) = template else {
            return fail("technical strategy requires plugin template");
        };
        if original.plugin_id != ID
            || original.plugin_version != VERSION
            || original.state_version != 1
        {
            return fail("technical strategy identity mismatch");
        }
        let mut plugin = original.clone();
        plugin.config = self.0.validate_config(&plugin.config)?;
        let c: Config = serde_json::from_value(plugin.config.clone()).map_err(err)?;
        let mut params = serde_json::to_value(signals::Params::defaults(&c.family)).map_err(err)?;
        let Some(overrides) = c.parameters.as_object() else {
            return fail("strategy parameters must be an object");
        };
        for (key, value) in overrides {
            if !signals::parameter_keys(&c.family).contains(&key.as_str()) {
                return fail("indicator parameter is unknown or does not apply to this family");
            }
            params
                .as_object_mut()
                .unwrap()
                .insert(key.clone(), value.clone());
        }
        let p: signals::Params = serde_json::from_value(params).map_err(err)?;
        if !c.bar_interval_ms.is_multiple_of(1000)
            || !p.valid()
            || !c.risk.validate()
            || c.position_size > c.inventory_cap as u64
            || c.initial_avg_price_tick > MAX_PRICE
        {
            return fail("invalid strategy periods, thresholds or risk configuration");
        }
        // Keep enough bars for delayed/cloud/long-window cold initialization.
        let needed = match c.family.as_str() {
            "Ichimoku" => p.length + p.displacement,
            "Turtle" | "Donchian" => p.length.max(p.exit_length) + 1,
            "Squeeze" => p.length * 2,
            "StochRsi" => p.length * 2 + p.smooth + p.signal,
            "Adx" => p.length + p.signal + 1,
            "HullCross" => p.slow + (p.slow as f64).sqrt().round() as usize,
            "SmaCross" | "EmaCross" | "Macd" | "Obv" | "AlmaCross" | "WaveTrend" => {
                p.slow + p.signal
            }
            _ => p.length + 1,
        };
        if c.history_limit < needed {
            return fail("history_limit is insufficient for the configured warmup");
        }
        let identity = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&plugin).map_err(err)?)
        );
        Ok((plugin, c, p, identity))
    }
}

impl BotFactory for Factory {
    fn descriptor(&self) -> &BotDescriptor {
        &self.0
    }
    fn supports_native_batching(&self) -> bool {
        true
    }
    fn supports_instance_reuse(&self) -> bool {
        true
    }
    fn event_filter(&self) -> Option<BotEventFilter> {
        // Stops, pending reductions and delayed signals must still observe time,
        // book and margin changes between closed bars.
        None
    }
    fn market_data_request(
        &self,
        t: &AgentTemplate,
    ) -> Result<Option<BotMarketDataRequest>, BotError> {
        let (_, c, _, _) = self.config(t)?;
        Ok(Some(BotMarketDataRequest {
            interval_ms: c.bar_interval_ms,
            max_bars: c.history_limit + 1,
        }))
    }
    fn create(
        &self,
        t: &AgentTemplate,
        saved: &PersistedAgentKindState,
    ) -> Result<Box<dyn ScheduledBot>, BotError> {
        let (plugin, config, params, identity) = self.config(t)?;
        let PersistedAgentKindState::Plugin {
            plugin_id,
            plugin_version,
            state_version,
            data,
        } = saved
        else {
            return fail("technical strategy state required");
        };
        if plugin_id != ID || plugin_version != VERSION || *state_version != 1 {
            return fail("technical strategy state version mismatch");
        }
        let state = if data.is_null() {
            State {
                identity: identity.clone(),
                ..State::default()
            }
        } else {
            let s: State = serde_json::from_value(data.as_ref().clone()).map_err(err)?;
            if s.identity != identity || !s.valid(&config) {
                return fail("technical strategy saved state or config mismatch");
            }
            s
        };
        Ok(Box::new(TechnicalBot {
            plugin,
            config,
            params,
            state,
        }))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    identity: String,
    signals: signals::Signals,
    frame: signals::Frame,
    bars: u64,
    cursor: Option<u64>,
    audit: VecDeque<Candle>,
    last_observation_ms: Option<u64>,
    last_action_ms: Option<u64>,
    target: i64,
    pending_direction: Option<i32>,
    pending_due_ms: u64,
    last_position: String,
    spot_cost: String,
    last_fill_id: Option<u64>,
    holding_since_ms: Option<u64>,
    exiting: bool,
    cooldown_until_ms: u64,
    exit_reason: Option<String>,
    risk: BotRiskState,
    initialized: bool,
    emitted_orders: u64,
    precision: Option<(i64, u64)>,
    last_mid: Option<i64>,
    volatility_ticks: i64,
}
impl State {
    fn valid(&self, c: &Config) -> bool {
        self.signals.valid()
            && self.risk.valid()
            && self.audit.len() <= 64
            && self.target.unsigned_abs() <= c.inventory_cap as u64
            && (self.last_position.is_empty() || self.last_position.parse::<i128>().is_ok())
            && (self.spot_cost.is_empty() || self.spot_cost.parse::<i128>().is_ok_and(|v| v >= 0))
            && self
                .frame
                .plots
                .values()
                .all(|v| v.is_none_or(f64::is_finite))
            && self.pending_direction.is_none_or(|v| (-1..=1).contains(&v))
            && self.cursor == self.audit.back().map(|b| b.open_time_ms)
            && self.audit.iter().all(valid_bar)
            && self
                .audit
                .iter()
                .zip(self.audit.iter().skip(1))
                .all(|(a, b)| a.open_time_ms < b.open_time_ms)
            && self.precision.is_none_or(|(tick, lot)| tick > 0 && lot > 0)
            && self.last_mid.is_none_or(|mid| mid > 0 && mid <= MAX_PRICE)
            && self.volatility_ticks >= 0
    }
}
fn err(e: serde_json::Error) -> BotError {
    BotError(e.to_string())
}
fn fail<T>(message: &str) -> Result<T, BotError> {
    Err(BotError(message.into()))
}
fn valid_bar(b: &Candle) -> bool {
    b.is_final
        && b.close_time_ms > b.open_time_ms
        && b.low_tick > 0
        && b.high_tick <= MAX_PRICE
        && b.low_tick <= b.open_tick
        && b.open_tick <= b.high_tick
        && b.low_tick <= b.close_tick
        && b.close_tick <= b.high_tick
}

struct TechnicalBot {
    plugin: BotConfig,
    config: Config,
    params: signals::Params,
    state: State,
}
impl TechnicalBot {
    fn candles(&mut self, data: &crate::BotMarketData, now: u64) -> Result<bool, BotError> {
        if data.interval_ms != self.config.bar_interval_ms {
            return fail("strategy candle interval mismatch");
        }
        let mut last = None;
        for bar in &data.candles {
            if !valid_bar(bar)
                || bar.close_time_ms > now
                || bar.close_time_ms - bar.open_time_ms != data.interval_ms
                || !bar.open_time_ms.is_multiple_of(data.interval_ms)
                || last.is_some_and(|t| t >= bar.open_time_ms)
            {
                return fail("invalid, unordered or future candle");
            }
            last = Some(bar.open_time_ms);
            if let Some(old) = self
                .state
                .audit
                .iter()
                .find(|b| b.open_time_ms == bar.open_time_ms)
                && old != bar
            {
                return fail("committed strategy candle changed");
            }
        }
        if let Some(cursor) = self.state.cursor
            && !data.candles.iter().any(|b| b.open_time_ms == cursor)
        {
            return fail("strategy candle cursor missing; replay required");
        }
        let mut advanced = false;
        for bar in &data.candles {
            if self.state.cursor.is_some_and(|t| bar.open_time_ms <= t) {
                continue;
            }
            self.state.frame = self
                .state
                .signals
                .update(&self.config.family, &self.params, bar);
            self.state.bars = self.state.bars.saturating_add(1);
            self.state.cursor = Some(bar.open_time_ms);
            self.state.audit.push_back(bar.clone());
            if self.state.audit.len() > 64 {
                self.state.audit.pop_front();
            }
            advanced = true;
        }
        Ok(advanced)
    }
    fn account(
        &mut self,
        v: &ParticipantObservationView<'_>,
    ) -> Result<(i128, i128, i64, bool), BotError> {
        let Some(account) = v.own_account.as_ref() else {
            return fail("strategy account is missing");
        };
        let (id, pos, equity, entry, spot) = match account {
            AccountSnapshot::Spot(a) => {
                let data = v
                    .bot_market_data
                    .as_ref()
                    .ok_or_else(|| BotError("spot strategy requires own fill data".into()))?;
                if !self.state.initialized {
                    if a.position_qty != 0 && self.config.initial_avg_price_tick == 0 {
                        return fail("pre-existing spot position requires initial_avg_price_tick");
                    }
                    self.state.last_position = a.position_qty.to_string();
                    self.state.spot_cost = (a.position_qty
                        * i128::from(self.config.initial_avg_price_tick))
                    .to_string();
                    self.state.last_fill_id = data.own_fills.last().map(|t| t.trade_id);
                } else {
                    let mut pos = self
                        .state
                        .last_position
                        .parse::<i128>()
                        .map_err(|_| BotError("invalid spot ledger cursor".into()))?;
                    let mut cost = self
                        .state
                        .spot_cost
                        .parse::<i128>()
                        .map_err(|_| BotError("invalid spot cost".into()))?;
                    if let Some(id) = self.state.last_fill_id
                        && !data.own_fills.iter().any(|t| t.trade_id == id)
                    {
                        return fail("spot own fill cursor missing; replay required");
                    }
                    let mut previous = None;
                    for t in &data.own_fills {
                        if previous.is_some_and(|id| id >= t.trade_id) {
                            return fail("unordered spot fill stream");
                        }
                        previous = Some(t.trade_id);
                        if self.state.last_fill_id.is_some_and(|id| t.trade_id <= id) {
                            continue;
                        }
                        let maker = t.maker_account_id == a.account_id;
                        let taker = t.taker_account_id == a.account_id;
                        if maker == taker {
                            return fail("invalid own fill or self trade");
                        }
                        let buy = if taker {
                            t.taker_side == Side::Buy
                        } else {
                            t.taker_side == Side::Sell
                        };
                        if buy {
                            cost =
                                cost.saturating_add(i128::from(t.price_tick) * i128::from(t.qty));
                            pos += i128::from(t.qty);
                        } else {
                            if i128::from(t.qty) > pos {
                                return fail("spot fill would create a short position");
                            }
                            cost = if pos == i128::from(t.qty) {
                                0
                            } else {
                                cost - cost * i128::from(t.qty) / pos
                            };
                            pos -= i128::from(t.qty);
                        }
                        self.state.last_fill_id = Some(t.trade_id);
                    }
                    if pos != a.position_qty {
                        return fail("exclusive spot position and fill stream disagree");
                    }
                    self.state.spot_cost = cost.to_string();
                }
                let cost = self.state.spot_cost.parse::<i128>().unwrap_or(0);
                let mark = v.book.bids.first().map_or_else(
                    || self.state.audit.back().map_or(1, |b| b.close_tick),
                    |b| b.price_tick,
                );
                (
                    a.account_id,
                    a.position_qty,
                    a.cash_balance
                        .saturating_add(a.position_qty.saturating_mul(mark.into())),
                    if a.position_qty > 0 {
                        (cost / a.position_qty).min(i128::from(MAX_PRICE)) as i64
                    } else {
                        0
                    },
                    true,
                )
            }
            AccountSnapshot::Perp(a) => {
                if a.hedge_positions.is_some() {
                    return fail("technical strategy requires a one-way perpetual account");
                }
                (
                    a.account_id,
                    a.position_qty,
                    a.equity,
                    a.avg_entry_price_tick,
                    false,
                )
            }
        };
        if id != self.plugin.participant.account_id {
            return fail("strategy account identity mismatch");
        }
        let previous = self.state.last_position.parse::<i128>().unwrap_or(0);
        if pos.signum() != previous.signum() || (!self.state.initialized && pos != 0) {
            self.state.holding_since_ms = (pos != 0).then_some(v.market_time_ms);
        }
        if pos == 0 {
            self.state.holding_since_ms = None;
        }
        if !self.state.initialized {
            self.state.target = pos.clamp(
                -i128::from(self.config.inventory_cap),
                i128::from(self.config.inventory_cap),
            ) as i64;
        }
        self.state.last_position = pos.to_string();
        self.state.initialized = true;
        Ok((pos, equity, entry, spot))
    }
    fn decide_view(
        &mut self,
        v: &ParticipantObservationView<'_>,
        precision: (i64, u64),
    ) -> Result<Vec<OrderAction>, BotError> {
        if self.plugin.participant.instrument_id.as_deref() != Some(v.instrument_id) {
            return fail("strategy instrument mismatch");
        }
        if self
            .state
            .last_observation_ms
            .is_some_and(|t| v.market_time_ms < t)
        {
            return fail("strategy observation clock moved backwards");
        }
        self.state.last_observation_ms = Some(v.market_time_ms);
        if precision.0 <= 0
            || precision.1 == 0
            || self.state.precision.is_some_and(|p| p != precision)
        {
            return fail("strategy instrument precision changed or invalid");
        }
        self.state.precision = Some(precision);
        if !matches!(v.status, MarketStatus::Running | MarketStatus::Paused) {
            return Ok(vec![]);
        }
        let Some(data) = v.bot_market_data.as_ref() else {
            return Ok(vec![]);
        };
        let advanced = self.candles(data, v.market_time_ms)?;
        let (position, equity, entry, spot) = self.account(v)?;
        let mid = match (v.book.bids.first(), v.book.asks.first()) {
            (Some(b), Some(a)) => {
                ((i128::from(b.price_tick) + i128::from(a.price_tick)) / 2) as i64
            }
            (Some(b), None) => b.price_tick,
            (None, Some(a)) => a.price_tick,
            _ => self.state.audit.back().map_or(1, |b| b.close_tick),
        };
        if mid <= 0 || mid > MAX_PRICE {
            return fail("strategy mark outside supported range");
        }
        if let Some(previous) = self.state.last_mid {
            self.state.volatility_ticks = ((i128::from(self.state.volatility_ticks) * 3
                + (i128::from(mid) - i128::from(previous)).abs())
                / 4) as i64;
        }
        self.state.last_mid = Some(mid);
        let volatility = self
            .state
            .frame
            .plots
            .get("atr")
            .copied()
            .flatten()
            .unwrap_or(0.)
            .min(i64::MAX as f64) as i64;
        let volatility = volatility.max(self.state.volatility_ticks);
        let risk = self
            .state
            .risk
            .evaluate_view(&self.config.risk, v, mid, volatility);
        let mut budget = Budget::for_limits(
            v,
            self.config.inventory_cap,
            self.config.fee_buffer_ppm,
            if spot { 1 } else { self.config.target_leverage },
        )
        .ok_or_else(|| BotError("strategy account budget missing".into()))?;
        let stale = v
            .perp_price
            .as_ref()
            .is_some_and(|p| p.status != crate::PriceLinkStatus::Live);
        if self.state.exiting && position == 0 {
            self.state.exiting = false;
            self.state.target = 0;
            self.state.pending_direction = None;
            self.state.cooldown_until_ms = v.market_time_ms.saturating_add(self.config.cooldown_ms);
        }
        let adverse = if position < 0 {
            mid.saturating_sub(entry)
        } else {
            entry.saturating_sub(mid)
        };
        let profit = -adverse;
        let stopped = position != 0
            && entry > 0
            && self.config.stop_loss_ppm > 0
            && i128::from(adverse) * 1000000
                >= i128::from(entry) * i128::from(self.config.stop_loss_ppm);
        let takeprofit = position != 0
            && entry > 0
            && self.config.take_profit_ppm > 0
            && i128::from(profit) * 1000000
                >= i128::from(entry) * i128::from(self.config.take_profit_ppm);
        let timeout = position != 0
            && self.config.max_holding_ms > 0
            && self
                .state
                .holding_since_ms
                .is_some_and(|t| v.market_time_ms.saturating_sub(t) >= self.config.max_holding_ms);
        if position != 0
            && (risk || budget.reduce_only || stale || stopped || takeprofit || timeout)
        {
            self.state.exiting = true;
            self.state.target = 0;
            self.state.pending_direction = None;
            self.state.exit_reason = Some(
                if stopped {
                    "stop_loss"
                } else if takeprofit {
                    "take_profit"
                } else if timeout {
                    "holding_time"
                } else if stale {
                    "stale_index"
                } else {
                    "risk"
                }
                .into(),
            );
        }
        if advanced
            && self.state.frame.ready
            && !self.state.exiting
            && !risk
            && v.market_time_ms >= self.state.cooldown_until_ms
        {
            let f = &self.state.frame;
            let direction = if f.long_entry && self.config.position_mode != "ShortOnly" {
                Some(1)
            } else if f.short_entry && !spot && self.config.position_mode != "LongOnly" {
                Some(-1)
            } else if (position > 0 && f.long_exit) || (position < 0 && f.short_exit) {
                Some(0)
            } else {
                None
            };
            if let Some(d) = direction
                && self.state.pending_direction != Some(d)
            {
                self.state.pending_direction = Some(d);
                self.state.pending_due_ms = data
                    .candles
                    .last()
                    .map_or(v.market_time_ms, |b| b.close_time_ms)
                    .saturating_add(self.config.signal_delay_ms);
            }
        }
        if !self.state.exiting
            && self.state.pending_direction.is_some()
            && v.market_time_ms >= self.state.pending_due_ms
        {
            let d = self.state.pending_direction.take().unwrap();
            let leverage = if spot { 1 } else { self.config.target_leverage };
            let sized = equity
                .max(0)
                .saturating_mul(i128::from(self.config.capital_fraction_ppm))
                .saturating_mul(i128::from(leverage))
                / 1000000
                / i128::from(mid);
            let size = sized
                .min(i128::from(self.config.position_size))
                .min(self.config.inventory_cap.into())
                .max(0) as i64;
            self.state.target = i64::from(d) * (size / precision.1 as i64 * precision.1 as i64);
        }
        if risk && position == 0
            || stale && position == 0
            || v.market_time_ms < self.state.cooldown_until_ms
        {
            self.state.target = 0;
        }
        if self.state.last_action_ms == Some(v.market_time_ms) {
            return Ok(vec![]);
        }
        if !v.own_orders.is_empty() {
            self.state.last_action_ms = Some(v.market_time_ms);
            return Ok(v
                .own_orders
                .iter()
                .take(64)
                .map(|o| OrderAction::Cancel {
                    order_id: o.order_id,
                })
                .collect());
        }
        // Close the old side before opening the other side. Partial closes remain
        // reduce-only until the authoritative account is actually flat.
        let reversal = position != 0 && i128::from(self.state.target).signum() != position.signum();
        let desired = if reversal {
            0
        } else {
            i128::from(self.state.target)
        };
        let delta = desired - position;
        if delta == 0 {
            return Ok(vec![]);
        }
        let side = if delta > 0 { Side::Buy } else { Side::Sell };
        let top = match side {
            Side::Buy => v.book.asks.first(),
            Side::Sell => v.book.bids.first(),
        };
        let Some(top) = top else {
            return Ok(vec![]);
        };
        let slippage = self.config.slippage_ticks.saturating_mul(precision.0);
        let raw = match side {
            Side::Buy => mid.saturating_add(slippage),
            Side::Sell => mid.saturating_sub(slippage).max(precision.0),
        };
        let raw = match side {
            Side::Buy => raw.saturating_add(precision.0 - 1) / precision.0 * precision.0,
            Side::Sell => raw / precision.0 * precision.0,
        };
        let reducing = (position > 0 && side == Side::Sell) || (position < 0 && side == Side::Buy);
        let request = delta.unsigned_abs().min(u128::from(self.config.max_qty)) as u64;
        let qty = budget.allocate(side, raw, request) / precision.1 * precision.1;
        if qty == 0
            || (side == Side::Buy && top.price_tick > raw)
            || (side == Side::Sell && top.price_tick < raw)
        {
            return Ok(vec![]);
        }
        self.state.last_action_ms = Some(v.market_time_ms);
        self.state.emitted_orders = self.state.emitted_orders.saturating_add(1);
        Ok(vec![if !spot && reducing {
            OrderAction::PlaceReduceOnlyImmediateOrCancel {
                side,
                price_tick: Some(raw),
                qty,
            }
        } else {
            OrderAction::PlaceImmediateOrCancel {
                side,
                price_tick: Some(raw),
                qty,
            }
        }])
    }
}
impl ScheduledBot for TechnicalBot {
    fn decide(&mut self, v: &ParticipantObservation) -> Result<Vec<OrderAction>, BotError> {
        if v.room_id != self.plugin.participant.room_id {
            return fail("strategy room mismatch");
        }
        let precision = v
            .instrument
            .as_ref()
            .map_or((1, 1), |i| (i.tick_size, i.lot_size));
        let old = self.state.clone();
        let result = self.decide_view(&ParticipantObservationView::from(v), precision);
        if result.is_err() {
            self.state = old;
        }
        result
    }
    fn decide_shared(
        &mut self,
        v: &SharedParticipantObservation,
    ) -> Result<Vec<OrderAction>, BotError> {
        if v.public.room_id != self.plugin.participant.room_id {
            return fail("strategy room mismatch");
        }
        let precision = v
            .public
            .instrument
            .as_ref()
            .map_or((1, 1), |i| (i.tick_size, i.lot_size));
        let old = self.state.clone();
        let result = self.decide_view(&v.view(), precision);
        if result.is_err() {
            self.state = old;
        }
        result
    }
    fn snapshot(&self) -> PersistedAgentKindState {
        PersistedAgentKindState::Plugin {
            plugin_id: ID.into(),
            plugin_version: VERSION.into(),
            state_version: 1,
            data: Arc::new(serde_json::to_value(&self.state).expect("finite strategy state")),
        }
    }
}

#[cfg(test)]
#[path = "technical_trader_tests.rs"]
mod tests;
