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
const REQUOTE_ON_CANCEL_ACK: &str = "requote_on_cancel_ack";
const RESERVATION_INNOVATIONS: &str = "reservation_innovations";
const MARKET_DEPTH_FLOW: &str = "market_depth_flow";
const TOUCH_EXCURSION_CALIBRATION: &str = "touch_excursion_calibration";
const CLOSED_CANDLE_FLOW: &str = "closed_candle_flow";
const PERSISTENT_INDEX_ANCHOR: &str = "persistent_index_anchor";
const BOOK_SLOPE_FLOW: &str = "book_slope_flow";
const FLOW_SURPRISE: &str = "flow_surprise";
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
    #[serde(default)]
    requote_on_cancel_ack: bool,
    #[serde(default)]
    reservation_innovations: bool,
    #[serde(default)]
    market_depth_flow: bool,
    #[serde(default)]
    touch_excursion_calibration: bool,
    #[serde(default)]
    closed_candle_flow: bool,
    #[serde(default)]
    persistent_index_anchor: bool,
    #[serde(default)]
    book_slope_flow: bool,
    #[serde(default)]
    flow_surprise: bool,
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
    for name in [
        EXTERNAL_REFERENCE,
        REQUOTE_ON_CANCEL_ACK,
        RESERVATION_INNOVATIONS,
        MARKET_DEPTH_FLOW,
        TOUCH_EXCURSION_CALIBRATION,
        CLOSED_CANDLE_FLOW,
        PERSISTENT_INDEX_ANCHOR,
        BOOK_SLOPE_FLOW,
        FLOW_SURPRISE,
    ] {
        parameters.insert(
            name.into(),
            BotParameter {
                kind: ParameterType::Boolean,
                required: false,
                default: None,
                minimum: None,
                maximum: None,
                choices: vec![],
            },
        );
    }
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
        let flow_keys = p.config.as_object().is_some_and(|c| {
            c.contains_key(EXTERNAL_REFERENCE)
                || c.contains_key(REQUOTE_ON_CANCEL_ACK)
                || c.contains_key(RESERVATION_INNOVATIONS)
                || c.contains_key(MARKET_DEPTH_FLOW)
                || c.contains_key(TOUCH_EXCURSION_CALIBRATION)
                || c.contains_key(CLOSED_CANDLE_FLOW)
                || c.contains_key(PERSISTENT_INDEX_ANCHOR)
                || c.contains_key(BOOK_SLOPE_FLOW)
                || c.contains_key(FLOW_SURPRISE)
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
            || (c.persistent_index_anchor && !c.reservation_innovations)
            || (c.book_slope_flow && !c.market_depth_flow)
            || (c.flow_surprise && (!c.book_slope_flow || c.flow_half_life_ms < 1000))
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
    /// Quotes use candles and public trades, never the own fill history.
    fn reads_own_fills(&self) -> bool {
        false
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
            || state
                .last_reservation_offset
                .is_some_and(|v| !v.is_finite())
            || !state.flow_signed.is_finite()
            || state
                .pending_reservation_offset
                .is_some_and(|v| !v.is_finite())
            || state.pending_reservation_quotes.len() > 16
            || state.calibration_bid_tick.is_some_and(|p| p <= 0)
            || state.calibration_ask_tick.is_some_and(|p| p <= 0)
            || state
                .last_flow_close_ms
                .is_some_and(|t| state.last_ms.is_none_or(|ms| t > ms))
            || state
                .pending_reservation_quotes
                .iter()
                .any(|(_, p)| *p <= 0)
            || !state.flow_total.is_finite()
            || state.flow_total < state.flow_signed.abs()
            || !state.pending_signed.is_finite()
            || !state.pending_expected_flow.is_finite()
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

fn external_flow_depth(
    v: &ParticipantObservationView<'_>,
    side: Side,
    mid: f64,
    reach: f64,
) -> f64 {
    let levels = if side == Side::Buy {
        &v.book.bids
    } else {
        &v.book.asks
    };
    levels
        .iter()
        .filter(|l| (l.price_tick as f64 - mid).abs() <= reach)
        .map(|l| {
            let mine: u64 = v
                .own_orders
                .iter()
                .filter(|o| o.side == side && o.price_tick == l.price_tick)
                .map(|o| o.remaining_qty)
                .sum();
            l.qty.saturating_sub(mine) as f64
        })
        .sum()
}

/// Inverse of the visible external supply curve, interpolated between its
/// cumulative-quantity knots (0, mid), (q1, p1), (q1+q2, p2), ... . This is a
/// quote-response hypothesis, not a claim that this hypothetical walk traded.
/// Do not extrapolate beyond observed liquidity or the existing quote reach.
/// Unlike half-spread scaling it measures price per unit of displayed depth;
/// the same flow can have different impact on steep and flat supply curves.
fn external_flow_displacement(
    v: &ParticipantObservationView<'_>,
    side: Side,
    mid: f64,
    reach: f64,
    volume: f64,
) -> f64 {
    let levels = if side == Side::Buy {
        &v.book.bids
    } else {
        &v.book.asks
    };
    let mut remaining = volume.max(0.);
    let mut previous = 0.;
    for l in levels {
        let distance = match side {
            Side::Buy => mid - l.price_tick as f64,
            Side::Sell => l.price_tick as f64 - mid,
        };
        if distance < 0. || distance > reach {
            continue;
        }
        let mine: u64 = v
            .own_orders
            .iter()
            .filter(|o| o.side == side && o.price_tick == l.price_tick)
            .map(|o| o.remaining_qty)
            .sum();
        let qty = l.qty.saturating_sub(mine) as f64;
        if qty == 0. {
            continue;
        }
        if remaining <= qty {
            return previous + (distance - previous) * remaining / qty;
        }
        remaining -= qty;
        previous = distance;
    }
    previous
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
    // Optional expected quantity accumulated from *prior* public-flow memory.
    // Omitted from every frozen policy/state when the surprise mode is off.
    #[serde(default, skip_serializing_if = "is_zero")]
    pending_expected_flow: f64,
    // Optional closed-loop policy: the bounded reservation bias represented
    // in the maker's last submitted quotes. Missing in all legacy snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_reservation_offset: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_reservation_offset: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending_reservation_quotes: Vec<(Side, i64)>,
    // Optional penetration estimator: the book actually visible at the start
    // of the interval, never the book observed after its trades have happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    calibration_bid_tick: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    calibration_ask_tick: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_flow_close_ms: Option<u64>,
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
    /// Full signed volume from already-closed public one-second candles. These
    /// fields are visible to every participant; no fill history or future bar
    /// is used. Legacy OHLCV without aggressor volume contributes no invented
    /// direction, and never falls back to a truncated tape in the same mode.
    fn observe_closed_flow(&mut self, v: &ParticipantObservationView<'_>) {
        if let Some(old_ms) = self.state.last_ms {
            let decay =
                0.5f64.powf((v.market_time_ms - old_ms) as f64 / self.c.flow_half_life_ms as f64);
            if self.c.flow_surprise {
                self.state.pending_expected_flow += self.state.flow_signed * (1. - decay);
            }
            self.state.flow_signed *= decay;
            self.state.flow_total *= decay;
        }
        let cursor = self.state.last_flow_close_ms.unwrap_or(v.market_time_ms);
        let mut newest = cursor;
        if let Some(data) = v.bot_market_data.as_ref().filter(|d| d.interval_ms == 1000) {
            for bar in &data.candles {
                if !bar.is_final
                    || bar.close_time_ms > v.market_time_ms
                    || bar.open_time_ms < newest
                    || bar.close_time_ms.saturating_sub(bar.open_time_ms) != 1000
                {
                    continue;
                }
                newest = newest.max(bar.close_time_ms);
                let Some(buys) = bar.taker_buy_base.filter(|b| *b <= bar.volume) else {
                    continue;
                };
                let total = bar.volume as f64;
                let signed = 2. * buys as f64 - total;
                let decay = 0.5f64.powf(
                    (v.market_time_ms - bar.close_time_ms) as f64 / self.c.flow_half_life_ms as f64,
                );
                self.state.flow_signed += signed * decay;
                self.state.flow_total += total * decay;
                self.state.pending_signed += signed;
                self.state.pending_total += total;
            }
        }
        self.state.last_flow_close_ms = Some(newest);
        self.state.flow_total = self.state.flow_total.max(self.state.flow_signed.abs());
    }

    /// Decayed signed/total taker quantity from public trades not yet seen.
    fn observe_flow(&mut self, v: &ParticipantObservationView<'_>) {
        if let Some(old_ms) = self.state.last_ms {
            let decay =
                0.5f64.powf((v.market_time_ms - old_ms) as f64 / self.c.flow_half_life_ms as f64);
            if self.c.flow_surprise {
                // A stationary signed-flow rate replenishes the amount lost
                // through exponential decay. Forecast before observing trades,
                // using the existing memory horizon, never a fitted new gain.
                self.state.pending_expected_flow += self.state.flow_signed * (1. - decay);
            }
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
        if self.c.reservation_innovations
            && v.own_orders.iter().any(|o| {
                self.state
                    .pending_reservation_quotes
                    .contains(&(o.side, o.price_tick))
            })
        {
            // Observation-based confirmation also supports restored decisions
            // on hosts without direct execution feedback.
            self.state.last_reservation_offset = self.state.pending_reservation_offset.take();
            self.state.pending_reservation_quotes.clear();
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
                        if self.c.touch_excursion_calibration {
                            if b.close_time_ms > v.market_time_ms {
                                return None;
                            }
                            let (Some(bid), Some(ask)) = (
                                self.state.calibration_bid_tick,
                                self.state.calibration_ask_tick,
                            ) else {
                                return None;
                            };
                            Some(
                                ((b.high_tick as f64 - ask as f64)
                                    .max(bid as f64 - b.low_tick as f64)
                                    / tick)
                                    .max(0.),
                            )
                        } else {
                            Some(
                                ((b.high_tick as f64 - old_mid).max(old_mid - b.low_tick as f64)
                                    / tick)
                                    .max(0.),
                            )
                        }
                    })
                    .flatten()
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
            if self.c.closed_candle_flow {
                self.observe_closed_flow(v);
            } else {
                self.observe_flow(v);
            }
        }
        if self.c.touch_excursion_calibration {
            self.state.calibration_bid_tick = bid.map(|b| b.price_tick);
            self.state.calibration_ask_tick = ask.map(|a| a.price_tick);
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
            .filter(|p| !self.c.persistent_index_anchor || p.status == PriceLinkStatus::Live)
            .and_then(|p| p.index_price_tick)
            .map_or(reference, |i| {
                reference + (i as f64 - reference) * self.c.index_weight_ppm as f64 / 1e6
            });
        self.state.center = index - q * skew * tick;
        // The index is an independent public price anchor, not an inventory
        // concession already embedded in our old quotes. Differencing a stable
        // index gap erases its restoring force after the first accepted quote.
        // With the opt-in split, memory contains only the local reservation
        // component; the currently observed index gap is applied afresh.
        let anchor_offset = if self.c.persistent_index_anchor {
            index - reference
        } else {
            0.
        };
        let reservation_offset = (self.state.center - mid - anchor_offset).clamp(
            -(self.c.max_half_spread_ticks as f64 * tick),
            self.c.max_half_spread_ticks as f64 * tick,
        );
        if self.c.reservation_innovations {
            // Other makers' quotes can already contain our previous inventory /
            // microprice bias. Re-applying its full level to every new mid feeds
            // an unchanged reservation discount around the book indefinitely.
            // Price only the *change* in that bounded bias. The public mid still
            // moves freely; fresh order-flow impact below is still applied once.
            self.state.center = mid + reservation_offset
                - self.state.last_reservation_offset.unwrap_or(0.)
                + anchor_offset;
        }
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
            let depth = if self.c.market_depth_flow {
                // A public trade is seen by every maker. Its price impact must
                // reflect the observed market's liquidity, not the size of one
                // bot's intended quote. Exclude our own reservations and use
                // the consumed side within the existing observable quote reach.
                let side = if self.state.pending_signed >= 0. {
                    Side::Sell
                } else {
                    Side::Buy
                };
                external_flow_depth(v, side, mid, self.c.max_half_spread_ticks as f64 * tick)
            } else {
                (self.c.max_qty * u64::from(self.c.levels)) as f64
            };
            let fresh = if self.state.pending_total + depth > 0. {
                self.state.pending_signed / (self.state.pending_total + depth)
            } else {
                0.
            };
            if self.c.book_slope_flow {
                // The public mid can already contain predictable pressure.
                // Extra quote impact responds only to the unanticipated part;
                // actual liquidity consumption and raw-flow toxicity remain.
                let signed = self.state.pending_signed
                    - if self.c.flow_surprise {
                        self.state.pending_expected_flow
                    } else {
                        0.
                    };
                let side = if signed >= 0. { Side::Sell } else { Side::Buy };
                let impact = signed.signum()
                    * external_flow_displacement(
                        v,
                        side,
                        mid,
                        self.c.max_half_spread_ticks as f64 * tick,
                        signed.abs(),
                    );
                self.state.center += impact * self.c.flow_skew_ppm as f64 / 1e6;
            } else {
                self.state.center +=
                    fresh * self.state.half_spread * self.c.flow_skew_ppm as f64 / 1e6 * tick;
            }
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
            if self.c.requote_on_cancel_ack {
                // The refresh interval controls when to reconsider resting quotes,
                // not an additional delay after withdrawing them. Re-evaluate at
                // the next authoritative observation. Still-open orders are cancelled
                // again; reservations are never released speculatively, and new orders
                // remain gated by the real account budget and cancellation visibility.
                self.state.next_ms = v.market_time_ms;
            }
            return Ok(cancels);
        }
        // Pending flow is consumed only by quotes actually placed; a
        // cancel-only decision keeps it for the replacement quotes.
        self.state.pending_signed = 0.;
        self.state.pending_total = 0.;
        self.state.pending_expected_flow = 0.;
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
        if self.c.reservation_innovations && !actions.is_empty() {
            // Submission alone is insufficient: post-only/risk rejection is
            // possible. Only authoritative confirmation advances this memory.
            self.state.pending_reservation_offset = Some(reservation_offset);
            self.state.pending_reservation_quotes = actions
                .iter()
                .filter_map(|a| match a {
                    OrderAction::PlacePostOnly {
                        side, price_tick, ..
                    }
                    | OrderAction::PlaceProtected {
                        side, price_tick, ..
                    } => Some((*side, *price_tick)),
                    _ => None,
                })
                .collect();
        }
        Ok(actions)
    }
}
impl ScheduledBot for Maker {
    fn execution_feedback(&mut self, action: &OrderAction, execution: &GatewayExecution) {
        if !self.c.reservation_innovations || execution.account_id != self.p.participant.account_id
        {
            return;
        }
        let pair = match action {
            OrderAction::PlacePostOnly {
                side, price_tick, ..
            }
            | OrderAction::PlaceProtected {
                side, price_tick, ..
            } => (*side, *price_tick),
            _ => return,
        };
        if !self.state.pending_reservation_quotes.contains(&pair) {
            return;
        }
        let ActorExecutionResult::Accepted(result) = &execution.execution.result else {
            return;
        };
        let events = match result {
            MarketExecution::Spot(r) => &r.events,
            MarketExecution::Perp(r) => &r.events,
        };
        if events
            .iter()
            .any(|e| matches!(e.event,Event::OrderRested{price_tick,..} if price_tick==pair.1))
        {
            self.state.last_reservation_offset = self.state.pending_reservation_offset.take();
            self.state.pending_reservation_quotes.clear();
        }
    }
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

    fn market_data(candles: Vec<Candle>) -> crate::observation::BotMarketData {
        crate::observation::BotMarketData {
            interval_ms: 1000,
            external_volume_qty: "0".into(),
            candles,
            history_proof: None,
            private_history_complete: false,
            own_fills: vec![],
            fill_details: vec![],
            truncated: false,
            streams: vec![],
            account_events: vec![],
            conditional_orders: vec![],
        }
    }

    fn closed_bar(open_ms: u64, high: i64, low: i64, buys: u64, sells: u64) -> Candle {
        aggregate_candles(
            &[
                TimedTrade {
                    market_time_ms: open_ms,
                    price_tick: high,
                    qty: buys,
                    taker_side: Side::Buy,
                },
                TimedTrade {
                    market_time_ms: open_ms,
                    price_tick: low,
                    qty: sells,
                    taker_side: Side::Sell,
                },
            ]
            .into_iter()
            .filter(|t| t.qty > 0)
            .collect::<Vec<_>>(),
            1000,
            open_ms + 1000,
        )
        .unwrap()
        .remove(0)
    }

    #[test]
    fn persistent_anchor_survives_quote_ack_and_restore_but_not_stale_index() {
        let registry = BotRegistry::with_builtins();
        let make = |enabled| {
            template(
                FLOW_VERSION,
                json!({"reservation_innovations":true,
            "persistent_index_anchor":enabled,"inventory_target":50,"index_weight_ppm":200000,
            "microprice_ppm":0,"refresh_ms":1000,"jitter_ms":0}),
            )
        };
        let old = make(false);
        let new = make(true);
        let mut legacy = registry.create(&old, &old.initial_state()).unwrap();
        let mut corrected = registry.create(&new, &new.initial_state()).unwrap();
        let mut v = observation(0, vec![]);
        v.perp_price = Some(PerpPriceSnapshot {
            instrument_id: "TEST".into(),
            spot_instrument_id: "SPOT".into(),
            index_price_tick: Some(10100),
            mark_price_tick: 10000,
            source: None,
            source_time_ms: Some(0),
            max_age_ms: 10000,
            status: PriceLinkStatus::Live,
            funding: None,
        });
        let mut act = vec![];
        for bot in [&mut legacy, &mut corrected] {
            act = bot.decide(&v).unwrap();
        }
        assert!((data(corrected.as_ref())["center"].as_f64().unwrap() - 10020.).abs() < 1e-8);
        // The next authoritative observation confirms a previously quoted price.
        let (side, price_tick) = act
            .iter()
            .find_map(|a| match a {
                OrderAction::PlacePostOnly {
                    side, price_tick, ..
                } => Some((*side, *price_tick)),
                _ => None,
            })
            .unwrap();
        v.own_orders.push(Order {
            order_id: 42,
            account_id: 20,
            side,
            position_side: Default::default(),
            price_tick,
            remaining_qty: 1,
            seq: 1,
        });
        v.market_time_ms = 1000;
        v.step = 1;
        let mut restored = registry.create(&new, &corrected.snapshot()).unwrap();
        legacy.decide(&v).unwrap();
        corrected.decide(&v).unwrap();
        restored.decide(&v).unwrap();
        assert_eq!(corrected.snapshot(), restored.snapshot());
        assert_eq!(data(legacy.as_ref())["center"], 10000.);
        assert_eq!(data(corrected.as_ref())["center"], 10020.);
        assert_eq!(data(corrected.as_ref())["last_reservation_offset"], 0.);
        v.own_orders.clear();
        v.market_time_ms = 2000;
        v.step = 2;
        v.perp_price.as_mut().unwrap().status = PriceLinkStatus::Stale;
        corrected.decide(&v).unwrap();
        assert_eq!(data(corrected.as_ref())["center"], 10000.);
        v.market_time_ms = 3000;
        v.step = 3;
        v.perp_price = None;
        corrected.decide(&v).unwrap();
        assert_eq!(data(corrected.as_ref())["center"], 10000.);
    }

    #[test]
    fn inverse_supply_curve_respects_geometry_external_depth_and_observation_reach() {
        let mut v = observation(0, vec![]);
        v.book.asks = vec![
            BookLevel {
                price_tick: 10010,
                qty: 20,
            },
            BookLevel {
                price_tick: 10030,
                qty: 10,
            },
            BookLevel {
                price_tick: 10100,
                qty: 100,
            },
        ];
        v.own_orders.push(Order {
            order_id: 1,
            account_id: 20,
            side: Side::Sell,
            position_side: Default::default(),
            price_tick: 10010,
            remaining_qty: 10,
            seq: 1,
        });
        let impact = |v: &ParticipantObservation, q| {
            external_flow_displacement(
                &ParticipantObservationView::from(v),
                Side::Sell,
                10000.,
                40.,
                q,
            )
        };
        assert_eq!(impact(&v, 0.), 0.);
        assert_eq!(impact(&v, 5.), 5.);
        assert_eq!(impact(&v, 10.), 10.);
        assert_eq!(impact(&v, 15.), 20.);
        assert_eq!(impact(&v, 1000.), 30.); // neither hidden liquidity nor extrapolation
        v.book.asks[1].price_tick = 10020;
        assert_eq!(impact(&v, 15.), 15.);
        v.book.asks.clear();
        assert_eq!(impact(&v, 100.), 0.);
        assert_eq!(
            external_flow_displacement(
                &ParticipantObservationView::from(&v),
                Side::Buy,
                10000.,
                40.,
                50.
            ),
            5.
        );
    }

    #[test]
    fn surprise_learns_steady_flow_but_preserves_response_to_an_unexpected_reversal() {
        let registry = BotRegistry::with_builtins();
        let make = |enabled| {
            template(
                FLOW_VERSION,
                json!({"flow_surprise":enabled,
            "book_slope_flow":true,"market_depth_flow":true,"reservation_innovations":true,
            "inventory_target":50,"microprice_ppm":0,"flow_skew_ppm":1000000,
            "flow_half_life_ms":5000,"refresh_ms":1000,"jitter_ms":0}),
            )
        };
        let raw = make(false);
        let fresh = make(true);
        let mut control = registry.create(&raw, &raw.initial_state()).unwrap();
        let mut surprise = registry.create(&fresh, &fresh.initial_state()).unwrap();
        control.decide(&observation(0, vec![])).unwrap();
        surprise.decide(&observation(0, vec![])).unwrap();
        for second in 1..=64 {
            let v = observation(second, vec![buy(second)]);
            control.decide(&v).unwrap();
            surprise.decide(&v).unwrap();
        }
        let a = data(control.as_ref());
        let b = data(surprise.as_ref());
        assert_eq!(a["flow_signed"], b["flow_signed"]);
        assert_eq!(a["flow_total"], b["flow_total"]);
        assert_eq!(a["rng"], b["rng"]);
        assert_eq!(a["center"], 10001.);
        assert!((b["center"].as_f64().unwrap() - 10000.).abs() < 0.001);
        let mut reversal = buy(65);
        reversal.taker_side = Side::Sell;
        let v = observation(65, vec![reversal]);
        control.decide(&v).unwrap();
        surprise.decide(&v).unwrap();
        assert_eq!(data(control.as_ref())["center"], 9999.);
        assert!(
            data(surprise.as_ref())["center"].as_f64().unwrap() < 9998.01,
            "A surprise reversal must not be uniformly damped with predictable flow"
        );
    }

    #[test]
    fn pending_flow_forecast_restores_and_duplicate_observations_do_not_reprice() {
        let registry = BotRegistry::with_builtins();
        let t = template(
            FLOW_VERSION,
            json!({"flow_surprise":true,"book_slope_flow":true,
            "market_depth_flow":true,"inventory_target":50,"flow_skew_ppm":1000000,
            "flow_half_life_ms":5000,"refresh_ms":10000,"jitter_ms":0}),
        );
        let mut bot = registry.create(&t, &t.initial_state()).unwrap();
        bot.decide(&observation(0, vec![])).unwrap();
        for second in 1..=4 {
            bot.decide(&observation(second, vec![buy(second)])).unwrap();
        }
        assert!(
            data(bot.as_ref())["pending_expected_flow"]
                .as_f64()
                .unwrap()
                > 0.
        );
        let saved = bot.snapshot();
        assert!(
            bot.decide(&observation(4, vec![buy(4)]))
                .unwrap()
                .is_empty()
        );
        assert_eq!(saved, bot.snapshot());
        let mut restored = registry.create(&t, &saved).unwrap();
        for second in 5..=10 {
            let v = observation(second, vec![buy(second)]);
            assert_eq!(bot.decide(&v).unwrap(), restored.decide(&v).unwrap());
            assert_eq!(bot.snapshot(), restored.snapshot());
        }
        assert!(data(bot.as_ref()).get("pending_expected_flow").is_none());
        let mut invalid = bot.snapshot();
        if let PersistedAgentKindState::Plugin { data, .. } = &mut invalid {
            Arc::make_mut(data)["pending_expected_flow"] = json!("not a finite quantity");
        }
        assert!(registry.create(&t, &invalid).is_err());
    }

    #[test]
    fn flow_surprise_is_versioned_and_requires_an_existing_supply_curve_and_memory() {
        let registry = BotRegistry::with_builtins();
        for enabled in [false, true] {
            let legacy = template(VERSION, json!({"flow_surprise":enabled}));
            assert!(registry.create(&legacy, &legacy.initial_state()).is_err());
        }
        for config in [
            json!({"flow_surprise":true}),
            json!({"flow_surprise":true,
            "book_slope_flow":true,"market_depth_flow":true}),
        ] {
            let bad = template(FLOW_VERSION, config);
            assert!(registry.create(&bad, &bad.initial_state()).is_err());
        }
        let legacy = template(VERSION, json!({"inventory_target":50}));
        let mut old = registry.create(&legacy, &legacy.initial_state()).unwrap();
        old.decide(&observation(0, vec![])).unwrap();
        assert!(data(old.as_ref()).get("pending_expected_flow").is_none());
    }

    #[test]
    fn flow_forecast_does_not_invent_a_pre_birth_history() {
        let registry = BotRegistry::with_builtins();
        let t = template(
            FLOW_VERSION,
            json!({"flow_surprise":true,"book_slope_flow":true,
            "market_depth_flow":true,"inventory_target":50,"microprice_ppm":0,
            "flow_skew_ppm":1000000,"flow_half_life_ms":5000}),
        );
        let mut bot = registry.create(&t, &t.initial_state()).unwrap();
        bot.decide(&observation(100, vec![buy(1)])).unwrap();
        assert_eq!(data(bot.as_ref())["center"], 10001.);
        assert!(data(bot.as_ref()).get("pending_expected_flow").is_none());
    }

    #[test]
    fn supply_flow_is_once_only_and_restores_without_new_randomness() {
        let registry = BotRegistry::with_builtins();
        let t = template(
            FLOW_VERSION,
            json!({"book_slope_flow":true,"market_depth_flow":true,
            "reservation_innovations":true,"inventory_target":50,"microprice_ppm":0,
            "flow_skew_ppm":1000000,"flow_half_life_ms":10000,"refresh_ms":1000,"jitter_ms":0}),
        );
        let mut bot = registry.create(&t, &t.initial_state()).unwrap();
        bot.decide(&observation(0, vec![])).unwrap();
        bot.decide(&observation(1, vec![buy(1)])).unwrap();
        assert_eq!(data(bot.as_ref())["center"], 10001.);
        let mut restored = registry.create(&t, &bot.snapshot()).unwrap();
        let v = observation(2, vec![buy(1)]);
        bot.decide(&v).unwrap();
        restored.decide(&v).unwrap();
        assert_eq!(bot.snapshot(), restored.snapshot());
        assert_eq!(data(bot.as_ref())["center"], 10000.);
        assert_eq!(
            data(bot.as_ref())["pending_signed"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn price_formation_flags_require_their_mechanisms_and_version() {
        let r = BotRegistry::with_builtins();
        for (flag, dependency) in [
            (PERSISTENT_INDEX_ANCHOR, RESERVATION_INNOVATIONS),
            (BOOK_SLOPE_FLOW, MARKET_DEPTH_FLOW),
        ] {
            for value in [false, true] {
                let old = template(VERSION, json!({(flag):value,(dependency):true}));
                assert!(r.create(&old, &old.initial_state()).is_err());
            }
            let missing = template(FLOW_VERSION, json!({(flag):true}));
            assert!(r.create(&missing, &missing.initial_state()).is_err());
            let good = template(FLOW_VERSION, json!({(flag):true,(dependency):true}));
            assert!(r.create(&good, &good.initial_state()).is_ok());
        }
    }

    #[test]
    fn penetration_calibration_separates_touch_trades_from_book_walk() {
        let registry = BotRegistry::with_builtins();
        let settings = json!({"inventory_target":50,"refresh_ms":10000,"jitter_ms":0,"touch_excursion_calibration":true});
        let t = template(FLOW_VERSION, settings.clone());
        let legacy = template(
            FLOW_VERSION,
            json!({"inventory_target":50,"refresh_ms":10000,"jitter_ms":0}),
        );
        let mut corrected = registry.create(&t, &t.initial_state()).unwrap();
        let mut original = registry.create(&legacy, &legacy.initial_state()).unwrap();
        corrected.decide(&observation(0, vec![])).unwrap();
        original.decide(&observation(0, vec![])).unwrap();
        let mut v = observation(1, vec![]);
        v.bot_market_data = Some(market_data(vec![closed_bar(0, 10010, 9990, 40, 40)]));
        corrected.decide(&v).unwrap();
        original.decide(&v).unwrap();
        assert_eq!(data(corrected.as_ref())["frames"][0]["depth"], 0.);
        assert_eq!(data(original.as_ref())["frames"][0]["depth"], 10.);
        let mut restored = registry.create(&t, &corrected.snapshot()).unwrap();
        v = observation(2, vec![]);
        v.book.bids[0].price_tick += 100;
        v.book.asks[0].price_tick += 100;
        v.bot_market_data = Some(market_data(vec![closed_bar(1000, 10025, 9987, 40, 40)]));
        corrected.decide(&v).unwrap();
        restored.decide(&v).unwrap();
        assert_eq!(data(corrected.as_ref())["frames"][1]["depth"], 15.);
        assert_eq!(corrected.snapshot(), restored.snapshot());
    }

    #[test]
    fn penetration_calibration_requires_a_known_start_touch_and_closed_bar() {
        let registry = BotRegistry::with_builtins();
        let t = template(
            FLOW_VERSION,
            json!({"touch_excursion_calibration":true,"refresh_ms":10000,"jitter_ms":0}),
        );
        let mut bot = registry.create(&t, &t.initial_state()).unwrap();
        let mut v = observation(0, vec![]);
        v.book.asks.clear();
        bot.decide(&v).unwrap();
        v = observation(1, vec![]);
        v.bot_market_data = Some(market_data(vec![closed_bar(0, 10100, 9900, 5, 5)]));
        bot.decide(&v).unwrap();
        assert!(data(bot.as_ref())["frames"][0]["depth"].is_null());
        let mut future = closed_bar(1000, 11000, 9000, 5, 5);
        future.close_time_ms = 3000;
        future.is_final = true;
        v = observation(2, vec![]);
        v.bot_market_data = Some(market_data(vec![future]));
        bot.decide(&v).unwrap();
        assert!(data(bot.as_ref())["frames"][1]["depth"].is_null());
    }

    #[test]
    fn closed_flow_uses_all_aggressor_volume_once_not_the_last32_sign() {
        let registry = BotRegistry::with_builtins();
        let config = json!({"inventory_target":50,"refresh_ms":10000,"jitter_ms":0,"flow_skew_ppm":1000000,"flow_half_life_ms":10000,"closed_candle_flow":true});
        let t = template(FLOW_VERSION, config.clone());
        let mut legacy_config = config;
        legacy_config
            .as_object_mut()
            .unwrap()
            .remove(CLOSED_CANDLE_FLOW);
        let old = template(FLOW_VERSION, legacy_config);
        let mut bot = registry.create(&t, &t.initial_state()).unwrap();
        let mut legacy = registry.create(&old, &old.initial_state()).unwrap();
        bot.decide(&observation(0, vec![])).unwrap();
        legacy.decide(&observation(0, vec![])).unwrap();
        let tape = (89..121)
            .map(|id| {
                let mut t = buy(id);
                t.qty = 1;
                t.taker_side = Side::Sell;
                t
            })
            .collect();
        let mut v = observation(1, tape);
        v.bot_market_data = Some(market_data(vec![closed_bar(0, 10010, 9990, 80, 40)]));
        bot.decide(&v).unwrap();
        legacy.decide(&v).unwrap();
        assert_eq!(data(bot.as_ref())["pending_signed"], 40.);
        assert_eq!(data(bot.as_ref())["pending_total"], 120.);
        assert_eq!(data(legacy.as_ref())["pending_signed"], -32.);
        let before = bot.snapshot();
        assert!(bot.decide(&v).unwrap().is_empty());
        assert_eq!(bot.snapshot(), before);
        let mut restored = registry.create(&t, &before).unwrap();
        let mut future = closed_bar(2000, 10010, 9990, 100, 0);
        future.is_final = true;
        v.market_time_ms = 2000;
        v.step = 2;
        v.bot_market_data.as_mut().unwrap().candles.push(future);
        bot.decide(&v).unwrap();
        restored.decide(&v).unwrap();
        assert_eq!(bot.snapshot(), restored.snapshot());
        assert_eq!(data(bot.as_ref())["pending_total"], 120.);
        assert_eq!(data(bot.as_ref())["last_flow_close_ms"], 1000);
        let mut unknown = closed_bar(2000, 10010, 9990, 5, 5);
        unknown.taker_buy_base = None;
        v.market_time_ms = 3000;
        v.step = 3;
        v.bot_market_data = Some(market_data(vec![unknown]));
        bot.decide(&v).unwrap();
        assert_eq!(data(bot.as_ref())["pending_total"], 120.);
        assert_eq!(data(bot.as_ref())["last_flow_close_ms"], 3000);
    }

    #[test]
    fn wick_observation_modes_are_opt_in_and_absent_from_frozen_state() {
        let registry = BotRegistry::with_builtins();
        for key in [TOUCH_EXCURSION_CALIBRATION, CLOSED_CANDLE_FLOW] {
            for value in [false, true] {
                let config = json!({(key):value});
                let t = template(VERSION, config.clone());
                assert!(registry.create(&t, &t.initial_state()).is_err());
                let supported = template(FLOW_VERSION, config);
                assert!(
                    registry
                        .create(&supported, &supported.initial_state())
                        .is_ok()
                );
            }
        }
        let t = template(VERSION, json!({"inventory_target":50}));
        let mut bot = registry.create(&t, &t.initial_state()).unwrap();
        bot.decide(&observation(0, vec![])).unwrap();
        for key in [
            "calibration_bid_tick",
            "calibration_ask_tick",
            "last_flow_close_ms",
        ] {
            assert!(data(bot.as_ref()).get(key).is_none());
        }
    }

    #[test]
    fn reservation_memory_requires_confirmation_and_restores_pending_quotes() {
        let registry = BotRegistry::with_builtins();
        let settings = json!({"reservation_innovations":true,"inventory_target":0,"refresh_ms":1000,"jitter_ms":0});
        let legacy = template(VERSION, settings.clone());
        assert!(registry.create(&legacy, &legacy.initial_state()).is_err());
        let t = template(FLOW_VERSION, settings);
        let mut bot = registry.create(&t, &t.initial_state()).unwrap();
        let mut v = observation(0, vec![]);
        assert!(!bot.decide(&v).unwrap().is_empty());
        assert!(data(bot.as_ref()).get("last_reservation_offset").is_none());
        assert!(
            data(bot.as_ref())
                .get("pending_reservation_offset")
                .is_some()
        );
        // A rejected submission leaves no authoritative resting order.
        v.market_time_ms = 1000;
        v.step = 1;
        let actions = bot.decide(&v).unwrap();
        assert!(data(bot.as_ref()).get("last_reservation_offset").is_none());
        let mut restored = registry.create(&t, &bot.snapshot()).unwrap();
        let (side, price_tick) = actions
            .iter()
            .find_map(|a| match a {
                OrderAction::PlacePostOnly {
                    side, price_tick, ..
                } => Some((*side, *price_tick)),
                _ => None,
            })
            .unwrap();
        v.market_time_ms = 2000;
        v.step = 2;
        v.own_orders.push(Order {
            order_id: 42,
            account_id: 20,
            side,
            position_side: Default::default(),
            price_tick,
            remaining_qty: 1,
            seq: 1,
        });
        assert_eq!(bot.decide(&v).unwrap(), restored.decide(&v).unwrap());
        assert_eq!(bot.snapshot(), restored.snapshot());
        assert!(data(bot.as_ref()).get("last_reservation_offset").is_some());
        let saved = bot.snapshot();
        assert!(bot.decide(&v).unwrap().is_empty());
        assert_eq!(bot.snapshot(), saved);
    }

    #[test]
    fn reservation_innovations_preserve_response_to_new_public_flow() {
        let registry = BotRegistry::with_builtins();
        let t = template(
            FLOW_VERSION,
            json!({"reservation_innovations":true,"inventory_target":50,
            "flow_skew_ppm":1000000,"flow_half_life_ms":30000,"refresh_ms":1000,"jitter_ms":0}),
        );
        let mut bot = registry.create(&t, &t.initial_state()).unwrap();
        bot.decide(&observation(0, vec![])).unwrap();
        let neutral = data(bot.as_ref())["center"].as_f64().unwrap();
        bot.decide(&observation(1, vec![buy(1)])).unwrap();
        assert!(data(bot.as_ref())["center"].as_f64().unwrap() > neutral);
        bot.decide(&observation(2, vec![buy(1)])).unwrap();
        assert!((data(bot.as_ref())["center"].as_f64().unwrap() - neutral).abs() < 1e-9);
    }

    #[test]
    fn market_flow_impact_uses_visible_liquidity_not_one_makers_size() {
        let registry = BotRegistry::with_builtins();
        let quote = |depth: u64, size: u64| {
            let t = template(
                FLOW_VERSION,
                json!({"market_depth_flow":true,"inventory_target":50,
                "max_qty":size,"flow_skew_ppm":1000000,"flow_half_life_ms":30000,"refresh_ms":1000,"jitter_ms":0}),
            );
            let mut bot = registry.create(&t, &t.initial_state()).unwrap();
            bot.decide(&observation(0, vec![])).unwrap();
            let mut v = observation(1, vec![buy(1)]);
            v.book.asks[0].qty = depth;
            v.book.bids[0].qty = depth;
            bot.decide(&v).unwrap();
            let center = data(bot.as_ref())["center"].as_f64().unwrap();
            assert!(bot.decide(&v).unwrap().is_empty());
            let mut restored = registry.create(&t, &bot.snapshot()).unwrap();
            assert!(restored.decide(&v).unwrap().is_empty());
            v.market_time_ms = 2000;
            assert_eq!(bot.decide(&v).unwrap(), restored.decide(&v).unwrap());
            // The repeated public print is not fresh information.
            assert!((data(bot.as_ref())["center"].as_f64().unwrap() - 10000.).abs() < 1e-9);
            center
        };
        let thin = quote(100, 1);
        let deep = quote(1000, 1);
        assert!(thin > deep && deep > 10000.);
        assert!((quote(100, 40) - thin).abs() < 1e-9);
        let old = template(VERSION, json!({"market_depth_flow":false}));
        assert!(registry.create(&old, &old.initial_state()).is_err());
    }

    #[test]
    fn market_flow_depth_excludes_own_reservations_and_unreachable_levels() {
        let mut v = observation(0, vec![]);
        v.book.asks.extend([
            BookLevel {
                price_tick: 10020,
                qty: 50,
            },
            BookLevel {
                price_tick: 10100,
                qty: 1000,
            },
        ]);
        v.own_orders.push(Order {
            order_id: 1,
            account_id: 20,
            side: Side::Sell,
            position_side: Default::default(),
            price_tick: 10010,
            remaining_qty: 70,
            seq: 1,
        });
        assert_eq!(
            external_flow_depth(
                &ParticipantObservationView::from(&v),
                Side::Sell,
                10000.,
                40.
            ),
            80.
        );
        assert_eq!(
            external_flow_depth(
                &ParticipantObservationView::from(&v),
                Side::Buy,
                10000.,
                40.
            ),
            100.
        );
    }

    #[test]
    fn reservation_memory_consumes_real_gateway_resting_ack_but_not_rejection() {
        let spec: population::BackgroundMarket =
            serde_json::from_str(include_str!("../../scripts/fixtures/behavior_market.json"))
                .unwrap();
        let room = spec.scenario.room_id.clone();
        let mut rooms = RoomManager::new();
        rooms.create_room(spec.scenario).unwrap();
        let mut t = template(
            FLOW_VERSION,
            json!({"reservation_innovations":true,"inventory_target":20,"fallback_price_tick":100}),
        );
        if let AgentTemplate::Plugin(p) = &mut t {
            p.participant.room_id = room.clone();
            p.participant.instrument_id = Some("V-BTC-SPOT".into());
        }
        let registry = BotRegistry::with_builtins();
        let mut bot = registry.create(&t, &t.initial_state()).unwrap();
        let v = rooms
            .participant_observation(&room, "V-BTC-SPOT", 20)
            .unwrap();
        let actions = bot.decide(&v).unwrap();
        assert!(!actions.is_empty());
        let action = actions[0].clone();
        let mut gateway = OrderGateway::new_scheduler(&mut rooms, 100000);
        let execution = gateway
            .submit_action(GatewayRequest {
                participant_id: "maker-test".into(),
                room_id: room,
                instrument_id: Some("V-BTC-SPOT".into()),
                account_id: 20,
                action: action.clone(),
            })
            .unwrap();
        let mut rejected = execution.clone();
        rejected.execution.result = ActorExecutionResult::Rejected(ActorRejectReason::MarketPaused);
        bot.execution_feedback(&action, &rejected);
        assert!(data(bot.as_ref()).get("last_reservation_offset").is_none());
        bot.execution_feedback(&action, &execution);
        assert!(data(bot.as_ref()).get("last_reservation_offset").is_some());
        assert!(
            data(bot.as_ref())
                .get("pending_reservation_offset")
                .is_none()
        );
        assert!(
            data(bot.as_ref())
                .get("pending_reservation_quotes")
                .is_none()
        );
    }

    #[test]
    fn requote_waits_for_authoritative_cancel_visibility_and_survives_restore() {
        let registry = BotRegistry::with_builtins();
        let settings = json!({"inventory_target":50,"refresh_ms":7000,"jitter_ms":0,
            "requote_on_cancel_ack":true});
        let old = template(VERSION, settings.clone());
        assert!(registry.create(&old, &old.initial_state()).is_err());
        let t = template(FLOW_VERSION, settings);
        let mut bot = registry.create(&t, &t.initial_state()).unwrap();
        let mut v = observation(0, vec![]);
        v.own_orders.push(Order {
            order_id: 42,
            account_id: 20,
            side: Side::Buy,
            position_side: Default::default(),
            price_tick: 9000,
            remaining_qty: 1,
            seq: 0,
        });
        assert_eq!(
            bot.decide(&v).unwrap(),
            vec![OrderAction::Cancel { order_id: 42 }]
        );
        assert!(bot.decide(&v).unwrap().is_empty());
        let mut restored = registry.create(&t, &bot.snapshot()).unwrap();
        let mut unfunded = registry.create(&t, &bot.snapshot()).unwrap();
        // A rejected or not-yet-visible cancel must never be treated as a fill
        // or released reservation. Both restored and resident bots retry only.
        v.market_time_ms = 1000;
        v.step = 1;
        let expected = vec![OrderAction::Cancel { order_id: 42 }];
        assert_eq!(bot.decide(&v).unwrap(), expected);
        assert_eq!(restored.decide(&v).unwrap(), expected);
        v.market_time_ms = 2000;
        v.step = 2;
        v.own_orders.clear();
        let placed = bot.decide(&v).unwrap();
        assert!(!placed.is_empty());
        assert!(
            placed
                .iter()
                .all(|a| matches!(a, OrderAction::PlacePostOnly { .. }))
        );
        assert_eq!(restored.decide(&v).unwrap(), placed);
        assert_eq!(restored.snapshot(), bot.snapshot());

        // Cancellation visibility does not grant new capital or spot inventory.
        let mut empty_account = v.clone();
        empty_account.own_account = Some(AccountSnapshot::Spot(
            SpotAccount {
                account_id: 20,
                cash_balance: 0,
                position_qty: 0,
                reserved_cash: 0,
                reserved_position: 0,
                fees_paid: 0,
            }
            .snapshot(),
        ));
        assert!(unfunded.decide(&empty_account).unwrap().is_empty());

        // An unchanged configuration keeps the old delayed refresh semantics.
        let legacy = template(
            FLOW_VERSION,
            json!({"inventory_target":50,"refresh_ms":7000,"jitter_ms":0}),
        );
        let mut delayed = registry.create(&legacy, &legacy.initial_state()).unwrap();
        let mut cancel_view = observation(0, vec![]);
        cancel_view.own_orders.push(Order {
            order_id: 42,
            account_id: 20,
            side: Side::Buy,
            position_side: Default::default(),
            price_tick: 9000,
            remaining_qty: 1,
            seq: 0,
        });
        assert_eq!(delayed.decide(&cancel_view).unwrap(), expected);
        assert!(delayed.decide(&v).unwrap().is_empty());
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
        assert!(
            registry
                .create(&external, &external.initial_state())
                .is_err()
        );

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
        assert!(
            external.abs() <= 5.,
            "external reference drifted {external}"
        );
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
