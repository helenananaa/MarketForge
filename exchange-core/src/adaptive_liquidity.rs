//! Inventory-aware adaptations of Hummingbot's Avellaneda quoting and
//! hftbacktest's GLFT equations. Not a port of their connectors or profit proof.
//! Sources/license and the candle-envelope intensity approximation are recorded
//! in docs/BACKGROUND_POPULATION_RESEARCH.md.
use crate::market_bots::Budget;
use crate::observation::{ParticipantObservationView, SharedParticipantObservation};
use crate::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

const ID: &str = "AdaptiveLiquidityMaker";
/// 1.0.0 is frozen. 1.1.0 adds optional reaction to observed one-sided taker
/// flow; its parameters have no descriptor defaults, so 1.0.0 configs,
/// identities and snapshots are byte-identical to the original release.
/// `external_reference` (1.1.0 only, also without a default) prices quotes
/// from other participants' resting orders instead of the whole book.
const VERSION: &str = "1.0.0";
const FLOW_VERSION: &str = "1.1.0";
const FLOW_PARAMETERS: [&str; 3] = ["flow_skew_ppm", "flow_widen_ppm", "flow_half_life_ms"];
const EXTERNAL_REFERENCE: &str = "external_reference";
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Config {
    model: String,
    inventory_target: i64,
    inventory_cap: i64,
    max_qty: u64,
    levels: u32,
    gamma_ppm: u32,
    eta_ppm: u32,
    min_half_spread_ticks: i64,
    max_half_spread_ticks: i64,
    level_spacing_ticks: i64,
    refresh_ms: u64,
    quote_ttl_ms: u64,
    window: usize,
    min_samples: usize,
    skew_ppm: u32,
    microprice_ppm: u32,
    index_weight_ppm: u32,
    fee_buffer_ppm: u32,
    target_leverage: u32,
    fallback_price_tick: i64,
    jitter_ms: u64,
    #[serde(default)]
    flow_skew_ppm: u32,
    #[serde(default)]
    flow_widen_ppm: u32,
    #[serde(default)]
    flow_half_life_ms: u64,
    #[serde(default)]
    external_reference: bool,
}
fn n(default: i64, min: i64, max: i64) -> BotParameter {
    BotParameter {
        kind: ParameterType::Integer,
        required: false,
        default: Some(json!(default)),
        minimum: Some(min),
        maximum: Some(max),
        choices: vec![],
    }
}
pub(crate) fn register(r: &mut BotRegistry) {
    let mut parameters = BTreeMap::from([
        ("inventory_target".into(), n(0, -1000000, 1000000)),
        ("inventory_cap".into(), n(100, 1, 1000000)),
        ("max_qty".into(), n(4, 1, 100000)),
        ("levels".into(), n(4, 1, 8)),
        ("gamma_ppm".into(), n(50000, 100, 10000000)),
        ("eta_ppm".into(), n(500000, 0, 3000000)),
        ("min_half_spread_ticks".into(), n(2, 1, 1000000)),
        ("max_half_spread_ticks".into(), n(40, 1, 1000000)),
        ("level_spacing_ticks".into(), n(3, 1, 1000000)),
        ("refresh_ms".into(), n(7000, 1000, 86400000)),
        ("quote_ttl_ms".into(), n(90000, 1000, 86400000)),
        ("window".into(), n(120, 16, 1024)),
        ("min_samples".into(), n(24, 4, 1024)),
        ("skew_ppm".into(), n(100000, 0, 1000000)),
        ("microprice_ppm".into(), n(500000, 0, 1000000)),
        ("index_weight_ppm".into(), n(0, 0, 1000000)),
        ("fee_buffer_ppm".into(), n(1000, 0, 1000000)),
        ("target_leverage".into(), n(1, 1, 20)),
        ("fallback_price_tick".into(), n(10000, 1, 1000000000)),
        ("jitter_ms".into(), n(3000, 0, 86400000)),
    ]);
    for (name, max) in FLOW_PARAMETERS.into_iter().zip([2000000, 3000000, 3600000]) {
        parameters.insert(
            name.into(),
            BotParameter {
                default: None,
                ..n(0, 0, max)
            },
        );
    }
    parameters.insert(
        EXTERNAL_REFERENCE.into(),
        BotParameter {
            kind: ParameterType::Boolean,
            required: false,
            default: None,
            minimum: None,
            maximum: None,
            choices: vec![],
        },
    );
    parameters.insert(
        "model".into(),
        BotParameter {
            kind: ParameterType::String,
            required: false,
            default: Some(json!("GLFT")),
            minimum: None,
            maximum: None,
            choices: vec![json!("GLFT"), json!("Avellaneda")],
        },
    );
    r.register(Factory(BotDescriptor {
        id: ID.into(),
        name: "自适应库存风险做市".into(),
        version: VERSION.into(),
        protocol_version: BOT_PROTOCOL_VERSION.into(),
        state_version: 1,
        runtime: "builtin".into(),
        parameters,
    }))
    .expect("valid liquidity maker");
}
struct Factory(BotDescriptor);
impl Factory {
    fn config(&self, t: &AgentTemplate) -> Result<(BotConfig, Config, String), BotError> {
        let AgentTemplate::Plugin(p) = t else {
            return Err(BotError("maker plugin required".into()));
        };
        if p.plugin_id != ID
            || ![VERSION, FLOW_VERSION].contains(&p.plugin_version.as_str())
            || p.state_version != 1
        {
            return Err(BotError("maker identity mismatch".into()));
        }
        let flow_keys = p
            .config
            .as_object()
            .is_some_and(|c| {
                c.contains_key(EXTERNAL_REFERENCE)
                    || FLOW_PARAMETERS.iter().any(|k| c.contains_key(*k))
            });
        if p.plugin_version == VERSION && flow_keys {
            return Err(BotError("flow parameters require maker 1.1.0".into()));
        }
        let mut p = p.clone();
        p.config = self.0.validate_config(&p.config)?;
        let c: Config = serde_json::from_value(p.config.clone()).map_err(err)?;
        if c.min_samples > c.window
            || c.min_half_spread_ticks > c.max_half_spread_ticks
            || c.inventory_target.unsigned_abs() > c.inventory_cap as u64
            || ((c.flow_skew_ppm > 0 || c.flow_widen_ppm > 0) && c.flow_half_life_ms < 1000)
        {
            return Err(BotError("invalid maker limits".into()));
        }
        let identity = format!("{:x}", Sha256::digest(serde_json::to_vec(&p).map_err(err)?));
        Ok((p, c, identity))
    }
}
impl BotFactory for Factory {
    fn descriptor(&self) -> &BotDescriptor {
        &self.0
    }
    fn supports_plugin_version(&self, version: &str) -> bool {
        [VERSION, FLOW_VERSION].contains(&version)
    }
    fn supports_native_batching(&self) -> bool {
        true
    }
    fn supports_instance_reuse(&self) -> bool {
        true
    }
    fn market_data_request(
        &self,
        t: &AgentTemplate,
    ) -> Result<Option<bots::BotMarketDataRequest>, BotError> {
        let (_, c, _) = self.config(t)?;
        Ok(Some(bots::BotMarketDataRequest {
            interval_ms: 1000,
            max_bars: c.window + 1,
        }))
    }
    fn create(
        &self,
        t: &AgentTemplate,
        s: &PersistedAgentKindState,
    ) -> Result<Box<dyn ScheduledBot>, BotError> {
        let (p, c, identity) = self.config(t)?;
        let PersistedAgentKindState::Plugin {
            plugin_id,
            plugin_version,
            state_version,
            data,
        } = s
        else {
            return Err(BotError("maker state required".into()));
        };
        if plugin_id != ID || *plugin_version != p.plugin_version || *state_version != 1 {
            return Err(BotError("maker state version mismatch".into()));
        }
        let state = if data.is_null() {
            State {
                identity: identity.clone(),
                rng: p.seed.max(1),
                a: 1.,
                k: 0.5,
                ..Default::default()
            }
        } else {
            serde_json::from_value::<State>(data.as_ref().clone()).map_err(err)?
        };
        if state.identity != identity
            || state.frames.len() > c.window
            || ![
                state.a,
                state.k,
                state.sigma,
                state.center,
                state.half_spread,
            ]
            .iter()
            .all(|v| v.is_finite())
            || state.a <= 0.
            || state.k <= 0.
            || state.rng == 0
            || state.sigma < 0.
            || !state.flow_signed.is_finite()
            || !state.flow_total.is_finite()
            || state.flow_total < state.flow_signed.abs()
            || !state.pending_signed.is_finite()
            || state.pending_total < state.pending_signed.abs()
            || state.frames.iter().any(|f| {
                !f.dt.is_finite()
                    || f.dt <= 0.
                    || !f.delta.is_finite()
                    || f.depth.is_some_and(|d| !d.is_finite() || d < 0.)
            })
        {
            return Err(BotError("invalid maker saved state".into()));
        }
        Ok(Box::new(Maker { p, c, state }))
    }
}
fn err(e: impl std::fmt::Display) -> BotError {
    BotError(e.to_string())
}
/// Best price and quantity resting from other participants on one side: the
/// maker's own orders are taken out, so its previous quotes (and the skew
/// already in them) never come back as its reference price.
fn external_best(levels: &[BookLevel], side: Side, own: &[Order]) -> Option<(i64, u64)> {
    levels.iter().find_map(|l| {
        let mine = own
            .iter()
            .filter(|o| o.side == side && o.price_tick == l.price_tick)
            .map(|o| o.remaining_qty)
            .sum::<u64>();
        (l.qty > mine).then(|| (l.price_tick, l.qty - mine))
    })
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    dt: f64,
    delta: f64,
    depth: Option<f64>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    identity: String,
    rng: u64,
    last_ms: Option<u64>,
    last_mid: Option<f64>,
    next_ms: u64,
    frames: VecDeque<Frame>,
    a: f64,
    k: f64,
    sigma: f64,
    center: f64,
    half_spread: f64,
    calibrated: bool,
    calibrations: u64,
    quoted: u64,
    cancelled: u64,
    born: BTreeMap<u64, u64>,
    // 1.1.0 only; omitted from 1.0.0 snapshots.
    #[serde(default, skip_serializing_if = "is_zero")]
    flow_signed: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    flow_total: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_trade_id: Option<u64>,
    // Taker quantity since the last quote; consumed when quoting.
    #[serde(default, skip_serializing_if = "is_zero")]
    pending_signed: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pending_total: f64,
}
fn is_zero(v: &f64) -> bool {
    *v == 0.
}
struct Maker {
    p: BotConfig,
    c: Config,
    state: State,
}

// hftbacktest GLFT equations: delta=1, xi=gamma; prices are in ticks,
// volatility in ticks/sqrt(second), A in interval arrivals/second.
fn glft(gamma: f64, a: f64, k: f64) -> (f64, f64) {
    let c1 = (1. + gamma / k).ln() / gamma;
    let c2 = (gamma / (2. * a * k) * (1. + gamma / k).powf(k / gamma + 1.)).sqrt();
    (c1, c2)
}
impl Maker {
    /// Decayed signed/total taker quantity from public trades not yet seen.
    fn observe_flow(&mut self, v: &ParticipantObservationView<'_>) {
        if let Some(old_ms) = self.state.last_ms {
            let decay =
                0.5f64.powf((v.market_time_ms - old_ms) as f64 / self.c.flow_half_life_ms as f64);
            self.state.flow_signed *= decay;
            self.state.flow_total *= decay;
        }
        let mut newest = self.state.last_trade_id;
        for t in v.public_trades {
            if self.state.last_trade_id.is_some_and(|id| t.trade_id <= id) {
                continue;
            }
            let qty = t.qty as f64;
            self.state.flow_signed += if t.taker_side == Side::Buy { qty } else { -qty };
            self.state.flow_total += qty;
            self.state.pending_signed += if t.taker_side == Side::Buy { qty } else { -qty };
            self.state.pending_total += qty;
            newest = newest.max(Some(t.trade_id));
        }
        self.state.last_trade_id = newest;
        // Keep tiny residues from round-off from tripping the state check.
        self.state.flow_total = self.state.flow_total.max(self.state.flow_signed.abs());
    }

    fn decide_view(
        &mut self,
        v: &ParticipantObservationView<'_>,
        precision: (i64, u64),
    ) -> Result<Vec<OrderAction>, BotError> {
        if v.instrument_id != self.p.participant.instrument_id.as_deref().unwrap_or("") {
            return Err(BotError("maker instrument mismatch".into()));
        }
        if !matches!(v.status, MarketStatus::Running | MarketStatus::Paused) {
            return Ok(vec![]);
        }
        if self.state.last_ms.is_some_and(|ms| v.market_time_ms < ms) {
            return Err(BotError("maker clock regressed".into()));
        }
        if self.state.last_ms == Some(v.market_time_ms) {
            return Ok(vec![]);
        }
        let tick = precision.0.max(1) as f64;
        let bid = v.book.bids.first();
        let ask = v.book.asks.first();
        // Reference touch: the whole book, or only other participants' orders.
        // A maker alone at the touch then keeps its last external reference
        // instead of re-applying inventory skew to its own skewed quotes.
        let (ref_bid, ref_ask) = if self.c.external_reference {
            (
                external_best(&v.book.bids, Side::Buy, v.own_orders),
                external_best(&v.book.asks, Side::Sell, v.own_orders),
            )
        } else {
            (
                bid.map(|l| (l.price_tick, l.qty)),
                ask.map(|l| (l.price_tick, l.qty)),
            )
        };
        let mid = match (ref_bid, ref_ask) {
            (Some(b), Some(a)) => (b.0 as f64 + a.0 as f64) / 2.,
            _ => self
                .state
                .last_mid
                .unwrap_or(self.c.fallback_price_tick as f64),
        };
        if let (Some(old_ms), Some(old_mid)) = (self.state.last_ms, self.state.last_mid) {
            let dt = (v.market_time_ms - old_ms) as f64 / 1000.;
            // Complete closed candle envelopes avoid the last-32-public-trade
            // censoring. This estimates interval reach, not individual arrivals.
            let depth = v.bot_market_data.as_ref().and_then(|d| {
                d.candles
                    .iter()
                    .filter(|b| {
                        b.is_final && b.open_time_ms >= old_ms && b.open_time_ms < v.market_time_ms
                    })
                    .map(|b| {
                        ((b.high_tick as f64 - old_mid).max(old_mid - b.low_tick as f64) / tick)
                            .max(0.)
                    })
                    .reduce(f64::max)
            });
            self.state.frames.push_back(Frame {
                dt,
                delta: (mid - old_mid) / tick,
                depth,
            });
            while self.state.frames.len() > self.c.window {
                self.state.frames.pop_front();
            }
            let seconds = self.state.frames.iter().map(|f| f.dt).sum::<f64>();
            // Zero-drift RMS variation, matching the Hummingbot instant-vol
            // convention, rescaled to observed elapsed time.
            self.state.sigma = (self
                .state
                .frames
                .iter()
                .map(|f| f.delta * f.delta)
                .sum::<f64>()
                / seconds)
                .sqrt();
            if self.state.frames.len() >= self.c.min_samples {
                let pairs = (1..=24)
                    .filter_map(|d| {
                        let hits = self
                            .state
                            .frames
                            .iter()
                            .filter(|f| f.depth.is_some_and(|x| x >= d as f64))
                            .count();
                        (hits > 0).then(|| (d as f64, (hits as f64 / seconds).ln()))
                    })
                    .collect::<Vec<_>>();
                if pairs.len() >= 4 {
                    let n = pairs.len() as f64;
                    let sx = pairs.iter().map(|p| p.0).sum::<f64>();
                    let sy = pairs.iter().map(|p| p.1).sum::<f64>();
                    let slope = (n * pairs.iter().map(|p| p.0 * p.1).sum::<f64>() - sx * sy)
                        / (n * pairs.iter().map(|p| p.0 * p.0).sum::<f64>() - sx * sx);
                    let a = ((sy - slope * sx) / n).exp();
                    let k = -slope;
                    if a.is_finite()
                        && k.is_finite()
                        && k > 0.005
                        && k < 10.
                        && a > 0.00001
                        && a < 1000.
                    {
                        self.state.a = a;
                        self.state.k = k;
                        self.state.calibrated = true;
                        self.state.calibrations += 1;
                    }
                }
            }
        }
        if self.p.plugin_version == FLOW_VERSION && self.c.flow_half_life_ms > 0 {
            self.observe_flow(v);
        }
        self.state.last_ms = Some(v.market_time_ms);
        self.state.last_mid = Some(mid);
        if v.market_time_ms < self.state.next_ms {
            return Ok(vec![]);
        }
        self.state.rng ^= self.state.rng << 13;
        self.state.rng ^= self.state.rng >> 7;
        self.state.rng ^= self.state.rng << 17;
        self.state.next_ms = v
            .market_time_ms
            .saturating_add(self.c.refresh_ms)
            .saturating_add(self.state.rng % (self.c.jitter_ms + 1));
        let Some(mut budget) = Budget::for_limits(
            v,
            self.c.inventory_cap,
            self.c.fee_buffer_ppm,
            self.c.target_leverage,
        ) else {
            return Ok(vec![]);
        };
        self.state
            .born
            .retain(|id, _| v.own_orders.iter().any(|o| o.order_id == *id));
        for o in v.own_orders {
            self.state
                .born
                .entry(o.order_id)
                .or_insert(v.market_time_ms);
        }
        let q = (budget.position - self.c.inventory_target as i128) as f64
            / self.c.inventory_cap as f64;
        let gamma = self.c.gamma_ppm as f64 / 1e6;
        let (half, skew) = if self.c.model == "GLFT" {
            let (c1, c2) = glft(gamma, self.state.a, self.state.k);
            (
                c1 + 0.5 * c2 * self.state.sigma,
                c2 * self.state.sigma * self.c.skew_ppm as f64 / 1e6 * self.c.inventory_cap as f64,
            )
        } else {
            (
                gamma * self.state.sigma / 2. + (1. + gamma / self.state.k).ln() / gamma,
                gamma * self.state.sigma,
            )
        };
        let micro = match (ref_bid, ref_ask) {
            (Some(b), Some(a)) if b.1 as f64 + a.1 as f64 > 0. => {
                (b.0 as f64 * a.1 as f64 + a.0 as f64 * b.1 as f64) / (b.1 as f64 + a.1 as f64)
            }
            _ => mid,
        };
        let reference = mid + (micro - mid) * self.c.microprice_ppm as f64 / 1e6;
        let index = v
            .perp_price
            .as_ref()
            .and_then(|p| p.index_price_tick)
            .map_or(reference, |i| {
                reference + (i as f64 - reference) * self.c.index_weight_ppm as f64 / 1e6
            });
        self.state.center = index - q * skew * tick;
        self.state.half_spread = half.clamp(
            self.c.min_half_spread_ticks as f64,
            self.c.max_half_spread_ticks as f64,
        );
        if self.p.plugin_version == FLOW_VERSION {
            // Persistent one-sided flow (decayed memory) only widens against
            // adverse selection. The quote leans once per unit of *new* flow
            // since the previous quote: a permanent, non-compounding impact,
            // so the lean is not re-applied on top of a mid that already
            // contains it.
            if self.state.flow_total > 0. {
                let imbalance = self.state.flow_signed / self.state.flow_total;
                self.state.half_spread = (self.state.half_spread
                    * (1. + imbalance.abs() * self.c.flow_widen_ppm as f64 / 1e6))
                    .min(self.c.max_half_spread_ticks as f64);
            }
            let own_depth = (self.c.max_qty * u64::from(self.c.levels)) as f64;
            let fresh = self.state.pending_signed / (self.state.pending_total + own_depth);
            self.state.center +=
                fresh * self.state.half_spread * self.c.flow_skew_ppm as f64 / 1e6 * tick;
            // Inventory skew scales with volatility in ticks and is otherwise
            // unbounded; at fine tick sizes it could park a bid percent away
            // from the market. Keep the center within the widest half-spread.
            let reach = self.c.max_half_spread_ticks as f64 * tick;
            self.state.center = self.state.center.clamp(mid - reach, mid + reach);
        }
        let mut desired = BTreeMap::new();
        for level in 0..self.c.levels {
            let dist =
                (self.state.half_spread + level as f64 * self.c.level_spacing_ticks as f64) * tick;
            for side in [Side::Buy, Side::Sell] {
                let raw = if side == Side::Buy {
                    ((self.state.center - dist) / tick).floor() * tick
                } else {
                    ((self.state.center + dist) / tick).ceil() * tick
                };
                let price = if side == Side::Buy {
                    ask.map_or(raw, |a| raw.min(a.price_tick as f64 - tick))
                } else {
                    bid.map_or(raw, |b| raw.max(b.price_tick as f64 + tick))
                } as i64;
                if price <= 0 {
                    continue;
                }
                let reducing = (budget.position > 0 && side == Side::Sell)
                    || (budget.position < 0 && side == Side::Buy);
                if budget.reduce_only && !reducing {
                    continue;
                }
                let factor = if side == Side::Buy {
                    (-(self.c.eta_ppm as f64) / 1e6 * q).exp()
                } else {
                    (self.c.eta_ppm as f64 / 1e6 * q).exp()
                }
                .clamp(0.25, 2.);
                let qty =
                    ((self.c.max_qty as f64 * factor).round() as u64 / precision.1) * precision.1;
                desired.insert((if side == Side::Buy { 0 } else { 1 }, price), qty);
            }
        }
        let mut cancels = vec![];
        for o in v.own_orders {
            let key = (if o.side == Side::Buy { 0 } else { 1 }, o.price_tick);
            let expired = v.market_time_ms - self.state.born[&o.order_id] >= self.c.quote_ttl_ms;
            if expired || !desired.contains_key(&key) || budget.reduce_only {
                cancels.push(OrderAction::Cancel {
                    order_id: o.order_id,
                });
            }
        }
        if !cancels.is_empty() {
            self.state.cancelled += cancels.len() as u64;
            return Ok(cancels);
        }
        // Pending flow is consumed only by quotes actually placed; a
        // cancel-only decision keeps it for the replacement quotes.
        self.state.pending_signed = 0.;
        self.state.pending_total = 0.;
        let mut actions = vec![];
        for ((s, price), target) in desired {
            let side = if s == 0 { Side::Buy } else { Side::Sell };
            let existing = v
                .own_orders
                .iter()
                .filter(|o| o.side == side && o.price_tick == price)
                .map(|o| o.remaining_qty)
                .sum::<u64>();
            let qty = budget.allocate(side, price, target.saturating_sub(existing)) / precision.1
                * precision.1;
            if qty > 0 {
                actions.push(if budget.reduce_only {
                    OrderAction::PlaceProtected {
                        side,
                        position_side: PositionSide::Both,
                        qty,
                        order_type: model::ProtectedOrderType::PostOnly,
                        price_tick: price,
                        reduce_only: true,
                        valid_until_market_time_ms: None,
                        expires_at_market_time_ms: None,
                    }
                } else {
                    OrderAction::PlacePostOnly {
                        side,
                        price_tick: price,
                        qty,
                    }
                });
            }
        }
        self.state.quoted += actions.len() as u64;
        Ok(actions)
    }
}
impl ScheduledBot for Maker {
    fn decide(&mut self, v: &ParticipantObservation) -> Result<Vec<OrderAction>, BotError> {
        if v.room_id != self.p.participant.room_id {
            return Err(BotError("maker room mismatch".into()));
        }
        let precision = v
            .instrument
            .as_ref()
            .map_or((1, 1), |i| (i.tick_size, i.lot_size));
        self.decide_view(&ParticipantObservationView::from(v), precision)
    }
    fn decide_shared(
        &mut self,
        v: &SharedParticipantObservation,
    ) -> Result<Vec<OrderAction>, BotError> {
        if v.public.room_id != self.p.participant.room_id {
            return Err(BotError("maker room mismatch".into()));
        }
        let precision = v
            .public
            .instrument
            .as_ref()
            .map_or((1, 1), |i| (i.tick_size, i.lot_size));
        self.decide_view(&v.view(), precision)
    }
    fn snapshot(&self) -> PersistedAgentKindState {
        PersistedAgentKindState::Plugin {
            plugin_id: ID.into(),
            plugin_version: self.p.plugin_version.clone(),
            state_version: 1,
            data: Arc::new(serde_json::to_value(&self.state).expect("finite maker state")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BookLevel, BookSnapshot, ParticipantConfig, ParticipantKind, SpotAccount};

    fn template(version: &str, config: serde_json::Value) -> AgentTemplate {
        AgentTemplate::Plugin(BotConfig {
            participant: ParticipantConfig {
                participant_id: "maker-test".into(),
                kind: ParticipantKind::MarketMaker,
                room_id: "maker".into(),
                account_id: 20,
                instrument_id: Some("TEST".into()),
            },
            plugin_id: ID.into(),
            plugin_version: version.into(),
            state_version: 1,
            config_version: 1,
            seed: 7,
            config,
        })
    }

    fn observation(second: u64, trades: Vec<Trade>) -> ParticipantObservation {
        ParticipantObservation {
            version: 1,
            room_id: "maker".into(),
            venue_id: "default-venue".into(),
            instrument_id: "TEST".into(),
            instrument: None,
            status: MarketStatus::Running,
            step: second,
            market_time_ms: second * 1000,
            book: BookSnapshot {
                bids: vec![BookLevel {
                    price_tick: 9990,
                    qty: 100,
                }],
                asks: vec![BookLevel {
                    price_tick: 10010,
                    qty: 100,
                }],
            },
            public_trades: trades,
            own_orders: vec![],
            related_markets: vec![],
            market_events: vec![],
            own_account: Some(AccountSnapshot::Spot(
                SpotAccount {
                    account_id: 20,
                    cash_balance: 10_000_000,
                    position_qty: 50,
                    reserved_cash: 0,
                    reserved_position: 0,
                    fees_paid: 0,
                }
                .snapshot(),
            )),
            bot_market_data: None,
            position_protections: vec![],
            risk: None,
            perp_price: None,
        }
    }

    fn buy(id: u64) -> Trade {
        Trade {
            trade_id: id,
            maker_order_id: 1,
            maker_account_id: 1,
            taker_order_id: 100 + id,
            taker_account_id: 2,
            price_tick: 10010,
            qty: 10,
            taker_side: Side::Buy,
            maker_position_side: Default::default(),
            taker_position_side: Default::default(),
        }
    }

    fn data(bot: &dyn ScheduledBot) -> serde_json::Value {
        let PersistedAgentKindState::Plugin { data, .. } = bot.snapshot() else {
            panic!("plugin state")
        };
        data.as_ref().clone()
    }

    #[test]
    fn flow_parameters_are_versioned_and_frozen_release_is_unchanged() {
        let registry = BotRegistry::with_builtins();
        let flow = serde_json::json!({"inventory_target":50,"flow_skew_ppm":500000,"flow_half_life_ms":10000});
        let old = template(VERSION, flow.clone());
        assert!(registry.create(&old, &old.initial_state()).is_err());
        let new = template(FLOW_VERSION, flow);
        let mut bot = registry.create(&new, &new.initial_state()).unwrap();
        bot.decide(&observation(0, vec![buy(1)])).unwrap();
        assert_eq!(data(bot.as_ref())["last_trade_id"], 1);
        let missing_life = template(FLOW_VERSION, serde_json::json!({"flow_skew_ppm":1}));
        assert!(
            registry
                .create(&missing_life, &missing_life.initial_state())
                .is_err()
        );

        let external = template(VERSION, serde_json::json!({"external_reference":true}));
        assert!(registry.create(&external, &external.initial_state()).is_err());

        let plain = template(VERSION, serde_json::json!({"inventory_target":50}));
        let mut bot = registry.create(&plain, &plain.initial_state()).unwrap();
        bot.decide(&observation(0, vec![buy(1)])).unwrap();
        let state = data(bot.as_ref());
        for key in [
            "flow_signed",
            "flow_total",
            "last_trade_id",
            "pending_signed",
            "pending_total",
        ] {
            assert!(state.get(key).is_none(), "{key} leaked into 1.0.0 state");
        }
        let restored = registry.create(&plain, &bot.snapshot()).unwrap();
        assert_eq!(restored.snapshot(), bot.snapshot());
    }

    #[test]
    fn flow_version_keeps_quote_center_near_the_market() {
        let registry = BotRegistry::with_builtins();
        // Huge inventory relative to the cap would skew the 1.0.0 center far away.
        let config = serde_json::json!({"inventory_target":0,"inventory_cap":10,"skew_ppm":1000000,
            "max_half_spread_ticks":5,"flow_half_life_ms":10000,"jitter_ms":0,"refresh_ms":1000,"min_samples":4,"window":16});
        let t = template(FLOW_VERSION, config);
        let mut bot = registry.create(&t, &t.initial_state()).unwrap();
        for s in 0..20u64 {
            let mut v = observation(s, vec![]);
            // A volatile mid so the volatility-scaled skew is large.
            let shift = if s % 2 == 0 { 0 } else { 40 };
            v.book.bids[0].price_tick += shift;
            v.book.asks[0].price_tick += shift;
            bot.decide(&v).unwrap();
            let state = data(bot.as_ref());
            let mid = 10000. + shift as f64;
            let center = state["center"].as_f64().unwrap();
            assert!(
                (center - mid).abs() <= 5. + 1e-9,
                "second {s}: center {center} mid {mid}"
            );
        }
    }

    /// A long maker alone in the book, no trades: returns how far its quote
    /// center moved over two minutes.
    fn lone_maker_drift(external: bool) -> f64 {
        let registry = BotRegistry::with_builtins();
        let mut config = serde_json::json!({"inventory_target":0,"inventory_cap":100,"skew_ppm":1000000,
            "max_half_spread_ticks":5,"flow_half_life_ms":10000,"jitter_ms":0,"refresh_ms":1000,"min_samples":4,"window":16});
        if external {
            config[EXTERNAL_REFERENCE] = true.into();
        }
        let t = template(FLOW_VERSION, config);
        let mut bot = registry.create(&t, &t.initial_state()).unwrap();
        // A volatile market first, so the inventory skew is not zero.
        for s in 0..20u64 {
            let mut v = observation(s, vec![]);
            let shift = if s % 2 == 0 { 0 } else { 40 };
            v.book.bids[0].price_tick += shift;
            v.book.asks[0].price_tick += shift;
            bot.decide(&v).unwrap();
        }
        let start = data(bot.as_ref())["center"].as_f64().unwrap();
        let mut own: Vec<Order> = vec![];
        for s in 20..140u64 {
            let mut v = observation(s, vec![]);
            let mut levels = BTreeMap::<(u8, i64), u64>::new();
            for o in &own {
                *levels.entry((o.side as u8, o.price_tick)).or_default() += o.remaining_qty;
            }
            let level = |((_, price_tick), qty): (&(u8, i64), &u64)| BookLevel {
                price_tick: *price_tick,
                qty: *qty,
            };
            let book = |side: Side| levels.iter().filter(move |((s, _), _)| *s == side as u8);
            v.book.bids = book(Side::Buy).rev().map(level).collect();
            v.book.asks = book(Side::Sell).map(level).collect();
            v.own_orders = own.clone();
            for action in bot.decide(&v).unwrap() {
                match action {
                    OrderAction::Cancel { order_id } => own.retain(|o| o.order_id != order_id),
                    OrderAction::PlacePostOnly {
                        side,
                        price_tick,
                        qty,
                    } => own.push(Order {
                        order_id: s * 100 + own.len() as u64,
                        account_id: 20,
                        side,
                        position_side: Default::default(),
                        price_tick,
                        remaining_qty: qty,
                        seq: s,
                    }),
                    other => panic!("unexpected {other:?}"),
                }
            }
        }
        data(bot.as_ref())["center"].as_f64().unwrap() - start
    }

    #[test]
    fn external_reference_stops_a_lone_maker_walking_on_its_own_skew() {
        let book = lone_maker_drift(false);
        assert!(book < -50., "whole-book reference re-applies skew: {book}");
        let external = lone_maker_drift(true);
        assert!(external.abs() <= 5., "external reference drifted {external}");
    }

    #[test]
    fn one_sided_buy_flow_leans_and_widens_quotes() {
        let registry = BotRegistry::with_builtins();
        let config = serde_json::json!({"inventory_target":50,"flow_skew_ppm":1000000,
            "flow_widen_ppm":1000000,"flow_half_life_ms":30000,"jitter_ms":0,"refresh_ms":1000});
        let t = template(FLOW_VERSION, config);
        let (mut quiet, mut flowing) = (
            registry.create(&t, &t.initial_state()).unwrap(),
            registry.create(&t, &t.initial_state()).unwrap(),
        );
        for s in 0..5u64 {
            quiet.decide(&observation(s, vec![])).unwrap();
            flowing
                .decide(&observation(s, (s * 3 + 1..s * 3 + 4).map(buy).collect()))
                .unwrap();
        }
        let (q, f) = (data(quiet.as_ref()), data(flowing.as_ref()));
        assert!(f["center"].as_f64() > q["center"].as_f64(), "{q} {f}");
        assert!(f["half_spread"].as_f64() > q["half_spread"].as_f64());
        assert_eq!(f["flow_signed"], f["flow_total"]);
        // Once flow stops, the next quote carries no lean: only the decayed
        // memory keeps the spread wider.
        let mut stopped = registry.create(&t, &flowing.snapshot()).unwrap();
        let mut calm = registry.create(&t, &quiet.snapshot()).unwrap();
        stopped.decide(&observation(5, vec![])).unwrap();
        calm.decide(&observation(5, vec![])).unwrap();
        let (c, st) = (data(calm.as_ref()), data(stopped.as_ref()));
        assert_eq!(st["center"], c["center"]);
        assert!(st["half_spread"].as_f64() > c["half_spread"].as_f64());
        // Already-seen trades are not double counted.
        let before = f["flow_total"].as_f64().unwrap();
        flowing.decide(&observation(5, vec![buy(15)])).unwrap();
        assert!(data(flowing.as_ref())["flow_total"].as_f64().unwrap() < before);
    }
    #[test]
    fn glft_coefficients_match_reference_and_inventory_risk() {
        let (c1, c2) = glft(0.05, 2., 0.4);
        assert!((c1 - 2.355660713127669).abs() < 1e-12);
        assert!((c2 - 0.3003387451171875).abs() < 1e-12);
        assert!(glft(0.05, 1., 0.4).1 > c2);
        assert!(glft(0.05, 2., 0.2).0 > c1);
    }
}
