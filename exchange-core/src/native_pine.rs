//! Native ports of bundled examples and thirteen frozen Pine formula adapters.
//!
//! These are deliberately fixed strategies, not a Pine compiler. Decisions use
//! the normal gateway and real fill receipts. Warmup replays indicators only;
//! only the newest unseen closed bar may submit an order. Complete candle
//! prefixes are audited by a digest. After the host rolls its candle window,
//! the saved cursor and up to 64 retained bars must still overlap exactly;
//! corrections to history the host no longer supplies cannot be audited.
use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    AccountSnapshot, AgentTemplate, BotConfig, GatewayExecution, OrderAction,
    ParticipantObservation, PersistedAgentKindState, Side, SpotAccountSnapshot,
    bots::{
        BotDescriptor, BotError, BotEventFilter, BotFactory, BotMarketDataRequest, BotParameter,
        BotRegistry, ParameterType, ScheduledBot,
    },
    model::{PositionSide, ProtectedOrderType},
    observation::{BotMarketData, SharedParticipantObservation},
};

const VERSION: &str = "1.0.0";
const SCHEMA: u16 = 1;
const AUDIT_OVERLAP: usize = 64;
const MAX_EXACT: i128 = (1i128 << 53) - 1;
// Integer rolling sums remain exactly representable by the Pine f64 path.
const MAX_INDICATOR_TICK: i64 = (MAX_EXACT / 4096) as i64;

#[path = "native_pine_extended/mod.rs"]
pub(crate) mod extended;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
enum Strategy {
    Ma,
    Rsi,
    Breakout,
    Extended(extended::Kind),
}

impl Strategy {
    fn id(self) -> &'static str {
        match self {
            Self::Ma => "NativePineMa",
            Self::Rsi => "NativePineRsi",
            Self::Breakout => "NativePineBreakout",
            Self::Extended(kind) => kind.id(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    bar_interval_ms: u64,
    history_limit: usize,
    max_qty: u64,
    inventory_cap: u64,
    slippage_ticks: u64,
    fee_buffer_ppm: u64,
    initial_avg_price_tick: i64,
    fast: usize,
    slow: usize,
    length: usize,
    low: u64,
    high: u64,
    qty: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bar_interval_ms: 10_000,
            history_limit: 512,
            max_qty: 2,
            inventory_cap: 10,
            slippage_ticks: 2,
            fee_buffer_ppm: 1000,
            initial_avg_price_tick: 0,
            fast: 3,
            slow: 8,
            length: 7,
            low: 35,
            high: 65,
            qty: 2,
        }
    }
}

struct Factory {
    strategy: Strategy,
    descriptor: BotDescriptor,
}

fn parameter(default: i64, minimum: i64, maximum: i64) -> BotParameter {
    BotParameter {
        kind: ParameterType::Integer,
        required: false,
        default: Some(json!(default)),
        minimum: Some(minimum),
        maximum: Some(maximum),
        choices: vec![],
    }
}

pub fn register_native_pine_bots(registry: &mut BotRegistry) {
    for strategy in [Strategy::Ma, Strategy::Rsi, Strategy::Breakout]
        .into_iter()
        .chain(extended::KINDS.into_iter().map(Strategy::Extended))
    {
        let mut parameters = BTreeMap::from([
            (
                "bar_interval_ms".into(),
                parameter(10_000, 1000, 86_400_000),
            ),
            ("history_limit".into(), parameter(512, 16, 4095)),
            ("max_qty".into(), parameter(2, 1, 1_000_000)),
            ("inventory_cap".into(), parameter(10, 1, 1_000_000)),
            ("slippage_ticks".into(), parameter(2, 0, 10_000)),
            ("fee_buffer_ppm".into(), parameter(1000, 0, 1_000_000)),
            (
                "initial_avg_price_tick".into(),
                parameter(0, 0, MAX_INDICATOR_TICK),
            ),
            ("qty".into(), parameter(2, 1, 1_000_000)),
        ]);
        match strategy {
            Strategy::Ma => {
                parameters.insert("fast".into(), parameter(3, 1, 4096));
                parameters.insert("slow".into(), parameter(8, 2, 4096));
            }
            Strategy::Rsi => {
                parameters.insert("length".into(), parameter(7, 2, 4096));
                parameters.insert("low".into(), parameter(35, 1, 49));
                parameters.insert("high".into(), parameter(65, 51, 99));
            }
            Strategy::Breakout => {
                parameters.insert("length".into(), parameter(6, 2, 4096));
            }
            Strategy::Extended(_) => {
                // Formula-adapter indicator parameters are frozen to the
                // installed Pine references. Trading quantity stays configurable.
                parameters.insert("qty".into(), parameter(1, 1, 1_000_000));
            }
        }
        let descriptor = BotDescriptor {
            id: strategy.id().into(),
            name: match strategy {
                Strategy::Ma => "原生 Pine 均线策略",
                Strategy::Rsi => "原生 Pine RSI 策略",
                Strategy::Breakout => "原生 Pine 突破策略",
                Strategy::Extended(kind) => kind.id(),
            }
            .into(),
            version: VERSION.into(),
            protocol_version: crate::BOT_PROTOCOL_VERSION.into(),
            state_version: SCHEMA,
            runtime: "builtin".into(),
            parameters,
        };
        registry
            .register(Factory {
                strategy,
                descriptor,
            })
            .expect("valid native Pine descriptor");
    }
}

impl Factory {
    fn config(&self, template: &AgentTemplate) -> Result<(BotConfig, Config), BotError> {
        let AgentTemplate::Plugin(plugin) = template else {
            return fail("plugin config required");
        };
        if plugin.plugin_id != self.strategy.id() || plugin.plugin_version != VERSION {
            return fail("native Pine plugin identity mismatch");
        }
        let mut plugin = plugin.clone();
        plugin.config = self.descriptor.validate_config(&plugin.config)?;
        let config: Config = serde_json::from_value(plugin.config.clone()).map_err(json_error)?;
        if !config.bar_interval_ms.is_multiple_of(1000) {
            return fail("bar_interval_ms must be integral seconds");
        }
        Ok((plugin, config))
    }
}

impl BotFactory for Factory {
    fn descriptor(&self) -> &BotDescriptor {
        &self.descriptor
    }
    fn supports_native_batching(&self) -> bool {
        true
    }
    fn supports_instance_reuse(&self) -> bool {
        true
    }
    fn event_filter(&self) -> Option<BotEventFilter> {
        Some(BotEventFilter {
            closed_bars: true,
            account: true,
            interval_ms: None,
        })
    }
    fn market_data_request(
        &self,
        template: &AgentTemplate,
    ) -> Result<Option<BotMarketDataRequest>, BotError> {
        let (_, config) = self.config(template)?;
        Ok(Some(BotMarketDataRequest {
            interval_ms: config.bar_interval_ms,
            max_bars: config.history_limit + 1,
        }))
    }
    fn create(
        &self,
        template: &AgentTemplate,
        saved: &PersistedAgentKindState,
    ) -> Result<Box<dyn ScheduledBot>, BotError> {
        let (plugin, config) = self.config(template)?;
        let identity = hex(&Sha256::digest(
            serde_json::to_vec(&plugin).map_err(json_error)?,
        ));
        let state = load_state(saved, &plugin, &identity, self.strategy, &config)?;
        Ok(Box::new(NativeBot {
            plugin,
            config,
            strategy: self.strategy,
            identity,
            state,
        }))
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Indicators {
    fast_ma: Option<f64>,
    slow_ma: Option<f64>,
    rsi: Option<f64>,
    upper: Option<f64>,
    lower: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    extended: Option<extended::Signals>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Window {
    values: VecDeque<i64>,
    sum: f64,
}
impl Window {
    fn push(&mut self, price: i64, length: usize) -> Option<f64> {
        if self.values.len() == length {
            self.sum -= self.values.pop_front().unwrap() as f64;
        }
        self.values.push_back(price);
        self.sum += price as f64;
        (self.values.len() == length).then_some(self.sum / length as f64)
    }
    fn valid(&self, length: usize, bars: u64) -> bool {
        self.values.len() == (bars as usize).min(length)
            && self.values.iter().all(|p| valid_price(*p))
            && self.sum == self.values.iter().map(|p| *p as f64).sum::<f64>()
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Extreme {
    index: u64,
    price: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum IndicatorState {
    Ma {
        fast: Window,
        slow: Window,
    },
    Rsi {
        previous: Option<i64>,
        samples: usize,
        gain_sum: f64,
        loss_sum: f64,
        average_gain: Option<f64>,
        average_loss: Option<f64>,
    },
    Breakout {
        highs: VecDeque<Extreme>,
        lows: VecDeque<Extreme>,
    },
    Extended {
        strategy: extended::Kind,
        state: Box<extended::State>,
    },
}

impl IndicatorState {
    fn new(strategy: Strategy) -> Self {
        match strategy {
            Strategy::Ma => Self::Ma {
                fast: Window::default(),
                slow: Window::default(),
            },
            Strategy::Rsi => Self::Rsi {
                previous: None,
                samples: 0,
                gain_sum: 0.0,
                loss_sum: 0.0,
                average_gain: None,
                average_loss: None,
            },
            Strategy::Breakout => Self::Breakout {
                highs: VecDeque::new(),
                lows: VecDeque::new(),
            },
            Strategy::Extended(strategy) => Self::Extended {
                strategy,
                state: Box::new(extended::State::new(strategy)),
            },
        }
    }
    fn valid(&self, strategy: Strategy, config: &Config, bars: u64) -> bool {
        match (self, strategy) {
            (
                Self::Extended {
                    strategy: kind,
                    state,
                },
                Strategy::Extended(expected),
            ) => *kind == expected && state.valid(expected, bars),
            (Self::Ma { fast, slow }, Strategy::Ma) => {
                fast.valid(config.fast, bars) && slow.valid(config.slow, bars)
            }
            (
                Self::Rsi {
                    previous,
                    samples,
                    gain_sum,
                    loss_sum,
                    average_gain,
                    average_loss,
                },
                Strategy::Rsi,
            ) => {
                previous.is_some() == (bars > 0)
                    && previous.is_none_or(valid_price)
                    && *samples == (bars.saturating_sub(1) as usize).min(config.length)
                    && [*gain_sum, *loss_sum]
                        .iter()
                        .all(|x| x.is_finite() && *x >= 0.0 && *x <= MAX_EXACT as f64)
                    && average_gain.is_some() == (*samples == config.length)
                    && average_loss.is_some() == (*samples == config.length)
                    && [average_gain, average_loss].iter().all(|x| {
                        x.is_none_or(|x| {
                            x.is_finite() && x >= 0.0 && x <= MAX_INDICATOR_TICK as f64
                        })
                    })
            }
            (Self::Breakout { highs, lows }, Strategy::Breakout) => {
                let valid = |queue: &VecDeque<Extreme>, high: bool| {
                    queue.len() <= config.length
                        && queue.is_empty() == (bars == 0)
                        && queue.iter().all(|x| {
                            valid_price(x.price)
                                && x.index < bars
                                && x.index >= bars.saturating_sub(config.length as u64)
                        })
                        && queue.iter().zip(queue.iter().skip(1)).all(|(a, b)| {
                            a.index < b.index
                                && if high {
                                    a.price > b.price
                                } else {
                                    a.price < b.price
                                }
                        })
                };
                valid(highs, true) && valid(lows, false)
            }
            _ => false,
        }
    }
    fn update(&mut self, bar: &Bar, config: &Config, index: u64) -> Indicators {
        let mut result = Indicators::default();
        match self {
            Self::Extended { strategy, state } => {
                result.extended = Some(state.update(
                    *strategy,
                    extended::BarInput {
                        high: bar.high as f64,
                        low: bar.low as f64,
                        close: bar.close as f64,
                    },
                ));
            }
            Self::Ma { fast, slow } => {
                result.fast_ma = fast.push(bar.close, config.fast);
                result.slow_ma = slow.push(bar.close, config.slow);
            }
            Self::Rsi {
                previous,
                samples,
                gain_sum,
                loss_sum,
                average_gain,
                average_loss,
            } => {
                if let Some(last) = previous.replace(bar.close) {
                    let delta = bar.close as f64 - last as f64;
                    let gain = delta.max(0.0);
                    let loss = (-delta).max(0.0);
                    if *samples < config.length {
                        *samples += 1;
                        *gain_sum += gain;
                        *loss_sum += loss;
                        if *samples == config.length {
                            *average_gain = Some(*gain_sum / config.length as f64);
                            *average_loss = Some(*loss_sum / config.length as f64);
                        }
                    } else {
                        *average_gain = Some(
                            (average_gain.unwrap() * (config.length - 1) as f64 + gain)
                                / config.length as f64,
                        );
                        *average_loss = Some(
                            (average_loss.unwrap() * (config.length - 1) as f64 + loss)
                                / config.length as f64,
                        );
                    }
                    if let (Some(gain), Some(loss)) = (*average_gain, *average_loss) {
                        result.rsi = Some(if loss == 0.0 {
                            100.0
                        } else if gain == 0.0 {
                            0.0
                        } else {
                            100.0 - 100.0 / (1.0 + gain / loss)
                        });
                    }
                }
            }
            Self::Breakout { highs, lows } => {
                // ta.highest/lowest require a complete window, and [1] reads
                // that window BEFORE appending the current candle.
                if index >= config.length as u64 {
                    result.upper = highs.front().map(|x| x.price as f64);
                    result.lower = lows.front().map(|x| x.price as f64);
                }
                for (queue, price, high) in [(highs, bar.high, true), (lows, bar.low, false)] {
                    while queue
                        .front()
                        .is_some_and(|x| x.index + config.length as u64 <= index)
                    {
                        queue.pop_front();
                    }
                    while queue.back().is_some_and(|x| {
                        if high {
                            x.price <= price
                        } else {
                            x.price >= price
                        }
                    }) {
                        queue.pop_back();
                    }
                    queue.push_back(Extreme { index, price });
                }
            }
        }
        result
    }
}

/// Fields read by the Pine example, with interval closure validated separately.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Bar {
    time: u64,
    open: i64,
    high: i64,
    low: i64,
    close: i64,
    volume: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema: u16,
    identity: String,
    initialized: bool,
    last_observation: Option<(u64, u64)>,
    instrument_precision: Option<(i64, u64)>,
    evaluated_bars: u64,
    candle_digest: [u8; 32],
    recent_bars: VecDeque<Bar>,
    /// None means every supplied prefix was audited from the beginning.
    rolling_audit_start_ms: Option<u64>,
    indicator_state: IndicatorState,
    indicators: Indicators,
    last_intent: Option<String>,
    last_actions: Vec<OrderAction>,
    skipped_bar_decisions: u64,
    observed_position: i64,
    opening_position: i64,
    entry_qty: u64,
    fill_count: usize,
    fill_digest: [u8; 32],
    last_fill_id: Option<u64>,
    orders: BTreeMap<String, TrackedOrder>,
    #[serde(
        default,
        rename = "_host_receipts",
        skip_serializing_if = "Vec::is_empty"
    )]
    host_receipts: Vec<Receipt>,
}

impl State {
    fn new(identity: &str, strategy: Strategy) -> Self {
        Self {
            schema: SCHEMA,
            identity: identity.into(),
            initialized: false,
            last_observation: None,
            instrument_precision: None,
            evaluated_bars: 0,
            candle_digest: [0; 32],
            recent_bars: VecDeque::new(),
            rolling_audit_start_ms: None,
            indicator_state: IndicatorState::new(strategy),
            indicators: Indicators::default(),
            last_intent: None,
            last_actions: vec![],
            skipped_bar_decisions: 0,
            observed_position: 0,
            opening_position: 0,
            entry_qty: 0,
            fill_count: 0,
            fill_digest: [0; 32],
            last_fill_id: None,
            orders: BTreeMap::new(),
            host_receipts: vec![],
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct TrackedOrder {
    side: Side,
    qty: u64,
    pending: bool,
    order_id: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    key: String,
    instrument_id: String,
    command_seq: u64,
    market_time_ms: u64,
    order_id: Option<u64>,
    action: OrderAction,
    accepted: bool,
    error: Option<String>,
}

fn fail<T>(message: &str) -> Result<T, BotError> {
    Err(BotError(format!("native Pine: {message}")))
}
fn json_error(error: serde_json::Error) -> BotError {
    BotError(format!("native Pine: {error}"))
}
fn valid_price(price: i64) -> bool {
    (1..=MAX_INDICATOR_TICK).contains(&price)
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|x| format!("{x:02x}")).collect()
}
fn digest<T: Serialize>(previous: &[u8; 32], value: &T) -> Result<[u8; 32], BotError> {
    let mut hasher = Sha256::new();
    hasher.update(previous);
    hasher.update(serde_json::to_vec(value).map_err(json_error)?);
    Ok(hasher.finalize().into())
}

fn load_state(
    saved: &PersistedAgentKindState,
    plugin: &BotConfig,
    identity: &str,
    strategy: Strategy,
    config: &Config,
) -> Result<State, BotError> {
    let PersistedAgentKindState::Plugin {
        plugin_id,
        plugin_version,
        state_version,
        data,
    } = saved
    else {
        return fail("plugin state required");
    };
    if plugin_id != &plugin.plugin_id || plugin_version != VERSION || *state_version != SCHEMA {
        return fail("state identity mismatch");
    }
    if data.is_null() || data.as_object().is_some_and(|o| o.is_empty()) {
        return Ok(State::new(identity, strategy));
    }
    let state: State = serde_json::from_value(data.as_ref().clone()).map_err(json_error)?;
    if state.schema != SCHEMA
        || state.identity != identity
        || !state
            .indicator_state
            .valid(strategy, config, state.evaluated_bars)
        || state.recent_bars.len() != (state.evaluated_bars as usize).min(AUDIT_OVERLAP)
        || state.recent_bars.iter().any(|bar| {
            !valid_price(bar.open)
                || !valid_price(bar.high)
                || !valid_price(bar.low)
                || !valid_price(bar.close)
                || bar.high < bar.open.max(bar.close)
                || bar.low > bar.open.min(bar.close)
                || bar.volume == 0
        })
        || state
            .recent_bars
            .iter()
            .zip(state.recent_bars.iter().skip(1))
            .any(|(a, b)| a.time >= b.time)
        || state.observed_position < 0
        || state.opening_position < 0
        || i128::from(state.observed_position) > MAX_EXACT
        || state.entry_qty > state.observed_position as u64
        || (state.fill_count == 0) != state.last_fill_id.is_none()
        || state.fill_count >= 4096
        || state.orders.len() > 2
        || state.initialized != state.last_observation.is_some()
        || state.initialized != state.instrument_precision.is_some()
        || state
            .instrument_precision
            .is_some_and(|(tick, lot)| tick <= 0 || lot == 0)
        || state.host_receipts.len() > crate::bots::MAX_BOT_ACTIONS
        || state.last_actions.len() > 1
        || state.indicators.extended.as_ref().is_some_and(
            |signals| !matches!(strategy, Strategy::Extended(kind) if signals.valid(kind)),
        )
        || state.indicators.extended.is_some()
            != (matches!(strategy, Strategy::Extended(_)) && state.evaluated_bars > 0)
        || state
            .orders
            .values()
            .any(|order| order.qty == 0 || order.qty > config.max_qty)
        || ![
            state.indicators.fast_ma,
            state.indicators.slow_ma,
            state.indicators.rsi,
            state.indicators.upper,
            state.indicators.lower,
        ]
        .iter()
        .all(|x| x.is_none_or(f64::is_finite))
        || (!state.initialized
            && (state.evaluated_bars != 0 || state.fill_count != 0 || !state.orders.is_empty()))
    {
        return fail("invalid or incompatible saved state/config");
    }
    Ok(state)
}

struct NativeBot {
    plugin: BotConfig,
    config: Config,
    strategy: Strategy,
    identity: String,
    state: State,
}

impl NativeBot {
    fn order_key(&self, instrument: &str, close: bool) -> String {
        // Same scoped key construction as Pine broker, with a native identity.
        hex(&Sha256::digest(
            format!(
                "{}::{instrument}::{}",
                self.identity,
                if close { "L_close" } else { "L" }
            )
            .as_bytes(),
        ))[..48]
            .into()
    }
    fn sync_account(
        &mut self,
        data: &BotMarketData,
        account: &SpotAccountSnapshot,
        now: u64,
        instrument: &str,
    ) -> Result<(), BotError> {
        if account.account_id != self.plugin.participant.account_id
            || account.position_qty < 0
            || account.position_qty > MAX_EXACT
            || !(-MAX_EXACT..=MAX_EXACT).contains(&account.cash_balance)
            || account.fees_paid < 0
            || account.fees_paid > MAX_EXACT
            || account.available_position < 0
            || account.available_position > account.position_qty
            || account.available_cash < 0
            || account.available_cash > account.cash_balance
        {
            return fail("invalid authoritative spot account");
        }
        for receipt in std::mem::take(&mut self.state.host_receipts) {
            let Some(order) = self.state.orders.get_mut(&receipt.key) else {
                return fail("receipt refers to an unknown order");
            };
            if !order.pending
                || receipt.instrument_id != instrument
                || receipt.market_time_ms > now
                || !matches!(receipt.action, OrderAction::PlaceProtected { side, qty, .. } if side == order.side && qty == order.qty)
            {
                return fail("execution receipt does not match pending order");
            }
            order.pending = false;
            order.order_id = if receipt.accepted {
                Some(receipt.order_id.ok_or_else(|| {
                    BotError("native Pine: accepted receipt lacks order ID".into())
                })?)
            } else {
                None
            };
        }
        if data.own_fills.len() != data.fill_details.len()
            || data.own_fills.len() < self.state.fill_count
            || data.own_fills.len() >= 4096
        {
            return fail("complete authoritative own-fill history required");
        }
        let mut hash = [0; 32];
        let mut new_position = i128::from(self.state.observed_position);
        let mut all_delta = 0i128;
        let mut previous_id = None;
        let mut previous_time = None;
        for (index, (fill, detail)) in data.own_fills.iter().zip(&data.fill_details).enumerate() {
            let maker = fill.maker_account_id == account.account_id;
            let taker = fill.taker_account_id == account.account_id;
            let fee = detail
                .fee_paid
                .ok_or_else(|| BotError("native Pine: authoritative fill fees required".into()))?;
            if maker == taker
                || fill.trade_id != detail.trade_id
                || fill.qty == 0
                || !valid_price(fill.price_tick)
                || detail.market_time_ms > now
                || !(0..=MAX_EXACT).contains(&fee)
                || previous_id.is_some_and(|id| fill.trade_id <= id)
                || previous_time.is_some_and(|time| detail.market_time_ms < time)
            {
                return fail("invalid or unordered fill receipts");
            }
            previous_id = Some(fill.trade_id);
            previous_time = Some(detail.market_time_ms);
            let buy = if taker {
                fill.taker_side == Side::Buy
            } else {
                fill.taker_side == Side::Sell
            };
            let order_id = if taker {
                fill.taker_order_id
            } else {
                fill.maker_order_id
            };
            let delta = i128::from(fill.qty) * if buy { 1 } else { -1 };
            all_delta += delta;
            hash = digest(&hash, &(fill, detail))?;
            if index + 1 == self.state.fill_count && hash != self.state.fill_digest {
                return fail("committed own-fill history changed or disappeared");
            }
            if self.state.initialized && index >= self.state.fill_count {
                if !buy {
                    self.state.entry_qty = self
                        .state
                        .entry_qty
                        .saturating_sub(fill.qty.min(new_position.max(0) as u64));
                } else if self
                    .state
                    .orders
                    .values()
                    .any(|order| order.order_id == Some(order_id) && order.side == Side::Buy)
                {
                    self.state.entry_qty =
                        self.state.entry_qty.checked_add(fill.qty).ok_or_else(|| {
                            BotError("native Pine: entry quantity overflow".into())
                        })?;
                }
                new_position += delta;
                if !(0..=MAX_EXACT).contains(&new_position) {
                    return fail("fill history creates invalid spot position");
                }
            }
        }
        if !self.state.initialized {
            if data.truncated {
                return fail("cold initialization requires complete candle/fill history");
            }
            let opening = account.position_qty - all_delta;
            if !(0..=MAX_EXACT).contains(&opening) {
                return fail("invalid opening spot inventory");
            }
            if opening != 0 && self.config.initial_avg_price_tick == 0 {
                return fail("pre-existing spot inventory requires initial_avg_price_tick");
            }
            self.state.opening_position = opening as i64;
        } else if new_position != account.position_qty {
            return fail("spot inventory changed without complete fill history");
        }
        if self.state.entry_qty > account.position_qty as u64 {
            return fail("tracked entry exceeds authoritative inventory");
        }
        self.state.observed_position = account.position_qty as i64;
        self.state.fill_count = data.own_fills.len();
        self.state.fill_digest = hash;
        self.state.last_fill_id = previous_id;
        self.state.initialized = true;
        Ok(())
    }

    fn new_bars(&mut self, data: &BotMarketData, now: u64) -> Result<Vec<Bar>, BotError> {
        let mut bars = Vec::with_capacity(data.candles.len());
        for candle in &data.candles {
            if candle.schema_version != crate::CANDLE_SCHEMA_VERSION
                || !candle.is_final
                || candle.close_time_ms > now
                || candle.open_time_ms % self.config.bar_interval_ms != 0
                || candle.open_time_ms.checked_add(self.config.bar_interval_ms)
                    != Some(candle.close_time_ms)
                || candle.volume == 0
                || candle.trades == 0
                || ![
                    candle.open_tick,
                    candle.high_tick,
                    candle.low_tick,
                    candle.close_tick,
                ]
                .iter()
                .all(|p| valid_price(*p))
                || candle.high_tick < candle.open_tick.max(candle.close_tick)
                || candle.low_tick > candle.open_tick.min(candle.close_tick)
                || bars
                    .last()
                    .is_some_and(|b: &Bar| b.time >= candle.open_time_ms)
            {
                return fail("forming, future, unordered or invalid candle");
            }
            bars.push(Bar {
                time: candle.open_time_ms,
                open: candle.open_tick,
                high: candle.high_tick,
                low: candle.low_tick,
                close: candle.close_tick,
                volume: candle.volume,
            });
        }
        let count = self.state.evaluated_bars as usize;
        let start;
        if !data.truncated {
            if bars.len() < count {
                return fail("committed candle history disappeared");
            }
            let mut hash = [0; 32];
            for bar in &bars[..count] {
                hash = digest(&hash, bar)?;
            }
            if hash != self.state.candle_digest {
                return fail("committed candle history changed");
            }
            start = count;
        } else {
            let Some(last) = self.state.recent_bars.back() else {
                return fail("cold initialization requires complete candle history");
            };
            let Some(cursor) = bars.iter().position(|bar| bar.time == last.time) else {
                return fail("rolling candle window lost committed cursor");
            };
            let first = bars[0].time;
            for saved in self
                .state
                .recent_bars
                .iter()
                .filter(|saved| saved.time >= first)
            {
                if !bars.iter().any(|bar| bar == saved) {
                    return fail("retained candle history changed or disappeared");
                }
            }
            // A rolling window must not introduce a new bar behind our cursor.
            for bar in bars.iter().take(cursor + 1).filter(|bar| {
                self.state
                    .recent_bars
                    .front()
                    .is_some_and(|first| bar.time >= first.time)
            }) {
                if !self.state.recent_bars.contains(bar) {
                    return fail("late correction to retained candle history");
                }
            }
            self.state.rolling_audit_start_ms =
                Some(first.max(self.state.recent_bars.front().unwrap().time));
            start = cursor + 1;
        }
        Ok(bars.into_iter().skip(start).collect())
    }

    fn intent(&self, bar: &Bar, position: i128) -> Option<&'static str> {
        let i = &self.state.indicators;
        let (buy, sell) = match self.strategy {
            Strategy::Ma => (
                i.fast_ma.zip(i.slow_ma).is_some_and(|(f, s)| f > s),
                i.fast_ma.zip(i.slow_ma).is_some_and(|(f, s)| f < s),
            ),
            Strategy::Rsi => (
                i.rsi.is_some_and(|r| r < self.config.low as f64),
                i.rsi.is_some_and(|r| r > self.config.high as f64),
            ),
            Strategy::Breakout => (
                i.upper.is_some_and(|upper| bar.close as f64 > upper),
                i.lower.is_some_and(|lower| (bar.close as f64) < lower),
            ),
            Strategy::Extended(_) => i
                .extended
                .as_ref()
                .map_or((false, false), |signals| (signals.buy, signals.sell)),
        };
        if buy && position == 0 {
            Some("entry")
        } else if sell && position > 0 {
            Some("close")
        } else {
            None
        }
    }

    fn action(
        &mut self,
        public: &ParticipantObservation,
        account: &SpotAccountSnapshot,
        own_orders: &[crate::Order],
        intent: &str,
    ) -> Result<Vec<OrderAction>, BotError> {
        let buy = intent == "entry";
        let side = if buy { Side::Buy } else { Side::Sell };
        let key = self.order_key(&public.instrument_id, !buy);
        if self.state.orders.values().any(|order| order.pending) {
            return fail("host execution receipt missing for pending order");
        }
        let (tick, lot) = public.instrument.as_ref().map_or((1, 1), |instrument| {
            (instrument.tick_size, instrument.lot_size)
        });
        if tick <= 0 || lot == 0 {
            return fail("invalid instrument precision");
        }
        let requested = if buy {
            self.config.qty
        } else {
            self.state.entry_qty.min(account.position_qty as u64)
        };
        if requested == 0 {
            return Ok(vec![]);
        }
        if requested % lot != 0 {
            return fail("order quantity must align with exchange lot size");
        }
        let mut qty = requested.min(self.config.max_qty);
        let resting_buy_qty = own_orders
            .iter()
            .filter(|order| order.side == Side::Buy)
            .try_fold(0u64, |sum, order| sum.checked_add(order.remaining_qty))
            .ok_or_else(|| BotError("native Pine: resting quantity overflow".into()))?;
        qty = if buy {
            qty.min(
                self.config
                    .inventory_cap
                    .saturating_sub(account.position_qty as u64)
                    .saturating_sub(resting_buy_qty),
            )
        } else {
            qty.min(account.available_position as u64)
        };
        qty = qty / lot * lot;
        let levels = if buy {
            &public.book.asks
        } else {
            &public.book.bids
        };
        let Some(top) = levels.first() else {
            return Ok(vec![]);
        };
        if top.price_tick <= 0 || top.price_tick % tick != 0 {
            return fail("invalid top of book price");
        }
        let slippage = i128::from(self.config.slippage_ticks) * i128::from(tick);
        let price = if buy {
            i128::from(top.price_tick) + slippage
        } else {
            (i128::from(top.price_tick) - slippage).max(i128::from(tick))
        };
        if price > MAX_EXACT || price > i128::from(i64::MAX) {
            return fail("protected order price overflow");
        }
        if buy {
            let unit_cost =
                price + (price * i128::from(self.config.fee_buffer_ppm) + 999_999) / 1_000_000;
            qty = qty.min((account.available_cash / unit_cost) as u64);
            qty = qty / lot * lot;
        }
        if qty == 0 {
            return Ok(vec![]);
        }
        self.state.orders.insert(
            key.clone(),
            TrackedOrder {
                side,
                qty,
                pending: true,
                order_id: None,
            },
        );
        Ok(vec![OrderAction::BotOrder {
            instrument_id: public.instrument_id.clone(),
            key,
            action: Box::new(OrderAction::PlaceProtected {
                side,
                position_side: PositionSide::Both,
                qty,
                order_type: ProtectedOrderType::ImmediateOrCancel,
                price_tick: price as i64,
                reduce_only: false,
                valid_until_market_time_ms: None,
                expires_at_market_time_ms: None,
            }),
        }])
    }

    fn decide_view(
        &mut self,
        public: &ParticipantObservation,
        own_account: &Option<AccountSnapshot>,
        own_orders: &[crate::Order],
        data: &Option<BotMarketData>,
    ) -> Result<Vec<OrderAction>, BotError> {
        if public.room_id != self.plugin.participant.room_id
            || self.plugin.participant.instrument_id.as_deref()
                != Some(public.instrument_id.as_str())
        {
            return fail("observation room/instrument identity mismatch");
        }
        // Manual /clock/step evaluates a paused room and the scheduler uses the
        // normal gateway while temporarily permitting that explicit step.
        // Realtime dispatch still controls when paused rooms are scheduled.
        if !matches!(
            public.status,
            crate::MarketStatus::Running | crate::MarketStatus::Paused
        ) {
            return fail("decision requires a running or manually stepped paused market");
        }
        if self
            .state
            .last_observation
            .is_some_and(|(step, time)| public.step < step || public.market_time_ms < time)
        {
            return fail("observation clock moved backwards");
        }
        let precision = public.instrument.as_ref().map_or((1, 1), |instrument| {
            (instrument.tick_size, instrument.lot_size)
        });
        if precision.0 <= 0
            || precision.1 == 0
            || self
                .state
                .instrument_precision
                .is_some_and(|saved| saved != precision)
        {
            return fail("instrument precision changed or is invalid");
        }
        let Some(AccountSnapshot::Spot(account)) = own_account else {
            return fail("fixed native Pine strategies require a spot account");
        };
        let Some(data) = data else {
            return fail("configured closed-bar stream required");
        };
        if data.interval_ms != self.config.bar_interval_ms {
            return fail("closed-bar interval mismatch");
        }
        if !self.state.initialized && data.candles.len() > self.config.history_limit {
            return fail("cold initialization history_limit exceeded");
        }
        // Work on a private checkpoint so a failed audit leaves the committed
        // strategy state reusable. No order is emitted before every audit passes.
        let committed = self.state.clone();
        let result = (|| {
            self.sync_account(data, account, public.market_time_ms, &public.instrument_id)?;
            let bars = self.new_bars(data, public.market_time_ms)?;
            if bars.is_empty() {
                self.state.last_actions.clear();
                return Ok(vec![]);
            }
            let previous_count = self.state.evaluated_bars;
            let last = bars.last().unwrap().clone();
            for bar in &bars {
                self.state.indicators =
                    self.state
                        .indicator_state
                        .update(bar, &self.config, self.state.evaluated_bars);
                self.state.candle_digest = digest(&self.state.candle_digest, bar)?;
                self.state.evaluated_bars += 1;
                if self.state.recent_bars.len() == AUDIT_OVERLAP {
                    self.state.recent_bars.pop_front();
                }
                self.state.recent_bars.push_back(bar.clone());
            }
            if previous_count > 0 {
                self.state.skipped_bar_decisions += bars.len().saturating_sub(1) as u64;
            }
            let intent = self.intent(&last, account.position_qty);
            self.state.last_intent = intent.map(str::to_owned);
            let actions = if let Some(intent) = intent {
                self.action(public, account, own_orders, intent)?
            } else {
                vec![]
            };
            self.state.last_actions = actions.clone();
            Ok(actions)
        })();
        if result.is_err() {
            self.state = committed;
        } else {
            self.state.last_observation = Some((public.step, public.market_time_ms));
            self.state.instrument_precision = Some(precision);
        }
        result
    }
}

impl ScheduledBot for NativeBot {
    fn decide(
        &mut self,
        observation: &ParticipantObservation,
    ) -> Result<Vec<OrderAction>, BotError> {
        self.decide_view(
            observation,
            &observation.own_account,
            &observation.own_orders,
            &observation.bot_market_data,
        )
    }
    fn decide_shared(
        &mut self,
        observation: &SharedParticipantObservation,
    ) -> Result<Vec<OrderAction>, BotError> {
        self.decide_view(
            &observation.public,
            &observation.own_account,
            &observation.own_orders,
            &observation.bot_market_data,
        )
    }
    fn snapshot(&self) -> PersistedAgentKindState {
        PersistedAgentKindState::Plugin {
            plugin_id: self.plugin.plugin_id.clone(),
            plugin_version: VERSION.into(),
            state_version: SCHEMA,
            data: serde_json::to_value(&self.state)
                .expect("bounded native Pine state serializes")
                .into(),
        }
    }
    fn prepare_reused_call(&mut self, saved: &PersistedAgentKindState) -> Result<(), BotError> {
        self.state = load_state(
            saved,
            &self.plugin,
            &self.identity,
            self.strategy,
            &self.config,
        )?;
        Ok(())
    }
    fn execution_feedback(&mut self, action: &OrderAction, execution: &GatewayExecution) {
        // The synchronous scheduler and realtime writer use the same durable
        // host receipt shape. Consumption still happens with authoritative fills.
        let mut saved = self.snapshot();
        crate::bots::record_bot_execution(&mut saved, action, execution);
        if let PersistedAgentKindState::Plugin { data, .. } = saved {
            self.state = serde_json::from_value(data.as_ref().clone())
                .expect("host-generated receipt is valid");
        }
    }
}

#[cfg(test)]
#[path = "native_pine_tests.rs"]
mod tests;
