//! Heterogeneous-agent style switching (after Lux–Marchesi and Brock–Hommes).
//! Each instance scores three styles on the prices it has itself observed:
//! momentum (fast/slow EWMA spread), reversion toward its own long EWMA
//! anchor, and staying out. It occasionally re-draws its style from a logit
//! over those scores, with an optional herding bonus to momentum when the
//! taker flow it observes is one-sided. Perpetual positions carry
//! exchange-side stops at recent extremes of its own samples, so stops and
//! venue liquidations can cascade. Every input is the bot's own observation,
//! account or seeded randomness; no shared signal is read.
use crate::endogenous_value::ewma_weight;
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

pub(crate) const ID: &str = "StyleSwitchingTrader";
pub(crate) const VERSION: &str = "1.0.0";
const MAX_SAMPLES: usize = 512;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Config {
    inventory_cap: i64,
    equity_use_ppm: u32,
    target_leverage: u32,
    decision_interval_ms: u64,
    fast_half_life_ms: u64,
    slow_half_life_ms: u64,
    anchor_half_life_ms: u64,
    saturation_ppm: u32,
    fitness_half_life_ms: u64,
    intensity_ppm: u32,
    switch_ppm: u32,
    herding_ppm: u32,
    flow_half_life_ms: u64,
    child_ppm: u32,
    min_trade_ppm: u32,
    max_slippage_ppm: u32,
    stop_lookback_ms: u64,
    stop_buffer_ppm: u32,
    stop_cooldown_ms: u64,
    warmup_ms: u64,
    fee_buffer_ppm: u32,
    fallback_price_tick: i64,
    stop_mode: String,
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
        ("inventory_cap".into(), n(100, 1, 1000000)),
        ("equity_use_ppm".into(), n(300000, 1, 1000000)),
        ("target_leverage".into(), n(1, 1, 20)),
        ("decision_interval_ms".into(), n(10000, 1000, 86400000)),
        ("fast_half_life_ms".into(), n(60000, 1000, 86400000)),
        ("slow_half_life_ms".into(), n(300000, 1000, 86400000)),
        ("anchor_half_life_ms".into(), n(3600000, 1000, 604800000)),
        ("saturation_ppm".into(), n(3000, 1, 1000000)),
        ("fitness_half_life_ms".into(), n(900000, 1000, 86400000)),
        ("intensity_ppm".into(), n(1000000, 0, 100000000)),
        ("switch_ppm".into(), n(100000, 0, 1000000)),
        ("herding_ppm".into(), n(0, 0, 100000000)),
        ("flow_half_life_ms".into(), n(60000, 1000, 86400000)),
        ("child_ppm".into(), n(250000, 1, 1000000)),
        ("min_trade_ppm".into(), n(50000, 0, 1000000)),
        ("max_slippage_ppm".into(), n(3000, 0, 100000)),
        ("stop_lookback_ms".into(), n(900000, 1000, 86400000)),
        ("stop_buffer_ppm".into(), n(1000, 0, 500000)),
        ("stop_cooldown_ms".into(), n(300000, 0, 86400000)),
        ("warmup_ms".into(), n(600000, 0, 86400000)),
        ("fee_buffer_ppm".into(), n(1000, 0, 100000)),
        ("fallback_price_tick".into(), n(10000, 1, 1000000000)),
    ]);
    parameters.insert(
        "stop_mode".into(),
        BotParameter {
            kind: ParameterType::String,
            required: false,
            default: Some(json!("Exchange")),
            minimum: None,
            maximum: None,
            choices: vec![json!("Exchange"), json!("Internal")],
        },
    );
    r.register(Factory(BotDescriptor {
        id: ID.into(),
        name: "风格切换与止损级联交易者".into(),
        version: VERSION.into(),
        protocol_version: BOT_PROTOCOL_VERSION.into(),
        state_version: 1,
        runtime: "builtin".into(),
        parameters,
    }))
    .expect("valid style switching trader");
}

struct Factory(BotDescriptor);
impl Factory {
    fn config(&self, t: &AgentTemplate) -> Result<(BotConfig, Config, String), BotError> {
        let AgentTemplate::Plugin(p) = t else {
            return Err(BotError("style plugin required".into()));
        };
        if p.plugin_id != ID || p.plugin_version != VERSION || p.state_version != 1 {
            return Err(BotError("style trader identity mismatch".into()));
        }
        let mut p = p.clone();
        p.config = self.0.validate_config(&p.config)?;
        let c: Config = serde_json::from_value(p.config.clone()).map_err(err)?;
        if c.fast_half_life_ms >= c.slow_half_life_ms {
            return Err(BotError("fast half-life must be below slow".into()));
        }
        let identity = format!("{:x}", Sha256::digest(serde_json::to_vec(&p).map_err(err)?));
        Ok((p, c, identity))
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
            return Err(BotError("style trader state required".into()));
        };
        if plugin_id != ID || plugin_version != VERSION || *state_version != 1 {
            return Err(BotError("style trader state version mismatch".into()));
        }
        let state = if data.is_null() {
            State {
                identity: identity.clone(),
                rng: p.seed.max(1),
                ..Default::default()
            }
        } else {
            serde_json::from_value::<State>(data.as_ref().clone()).map_err(err)?
        };
        if state.identity != identity || !state.valid() {
            return Err(BotError("invalid style trader saved state".into()));
        }
        Ok(Box::new(Trader { p, c, state }))
    }
}

fn err(e: impl std::fmt::Display) -> BotError {
    BotError(e.to_string())
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum Style {
    #[default]
    Out,
    Momentum,
    Reversion,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    identity: String,
    rng: u64,
    first_ms: Option<u64>,
    last_ms: Option<u64>,
    last_mid: Option<f64>,
    next_ms: u64,
    fast: f64,
    slow: f64,
    anchor: f64,
    /// Style exposures in [-1, 1] at the previous decision, scored against
    /// the return that followed.
    last_momentum: f64,
    last_reversion: f64,
    last_decision_mid: Option<f64>,
    last_decision_ms: Option<u64>,
    fitness_momentum: f64,
    fitness_reversion: f64,
    flow_signed: f64,
    flow_total: f64,
    last_trade_id: Option<u64>,
    style: Style,
    samples: VecDeque<(u64, f64)>,
    target: i64,
    last_position: i64,
    stop_tick: Option<i64>,
    cooldown_until_ms: u64,
    switches: u64,
    stop_outs: u64,
    style_decisions: [u64; 3],
}

impl State {
    fn valid(&self) -> bool {
        [
            self.fast,
            self.slow,
            self.anchor,
            self.last_momentum,
            self.last_reversion,
            self.fitness_momentum,
            self.fitness_reversion,
            self.flow_signed,
            self.flow_total,
        ]
        .iter()
        .all(|v| v.is_finite())
            && self.rng != 0
            && self.flow_total >= self.flow_signed.abs()
            && self.samples.len() <= MAX_SAMPLES
            && self.samples.iter().all(|(_, m)| m.is_finite() && *m > 0.)
            && self.last_mid.is_none_or(|m| m.is_finite() && m > 0.)
            && self
                .last_decision_mid
                .is_none_or(|m| m.is_finite() && m > 0.)
    }

    fn uniform(&mut self) -> f64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        ((self.rng >> 11) as f64 + 1.) / (1u64 << 53) as f64
    }
}

/// Logit choice over styles. Scores are in basis points of hypothetical return;
/// `intensity` is per basis point. Returns the drawn style for uniform `u`.
pub(crate) fn choose_style(scores: [f64; 3], intensity: f64, u: f64) -> Style {
    let best = scores.iter().cloned().fold(f64::MIN, f64::max);
    let weights = scores.map(|s| ((s - best) * intensity).exp());
    let total: f64 = weights.iter().sum();
    let mut acc = 0.;
    for (w, style) in weights
        .iter()
        .zip([Style::Out, Style::Momentum, Style::Reversion])
    {
        acc += w / total;
        if u <= acc {
            return style;
        }
    }
    Style::Reversion
}

/// Lowest (long) or highest (short) sample inside the lookback, pushed out by
/// the buffer. Similar lookbacks across bots produce clustered stop levels.
pub(crate) fn stop_level(
    samples: &VecDeque<(u64, f64)>,
    now: u64,
    lookback_ms: u64,
    long: bool,
    buffer: f64,
) -> Option<f64> {
    let recent = samples
        .iter()
        .filter(|(t, _)| now.saturating_sub(*t) <= lookback_ms)
        .map(|(_, m)| *m);
    if long {
        recent.reduce(f64::min).map(|m| m * (1. - buffer))
    } else {
        recent.reduce(f64::max).map(|m| m * (1. + buffer))
    }
}

struct Trader {
    p: BotConfig,
    c: Config,
    state: State,
}

impl Trader {
    fn observe(&mut self, v: &ParticipantObservationView<'_>, mid: f64) {
        let now = v.market_time_ms;
        let s = &mut self.state;
        if s.first_ms.is_none() {
            s.first_ms = Some(now);
            (s.fast, s.slow, s.anchor) = (mid, mid, mid);
        }
        if let Some(old) = s.last_ms {
            let dt = (now - old) as f64;
            s.fast += (mid - s.fast) * ewma_weight(dt, self.c.fast_half_life_ms);
            s.slow += (mid - s.slow) * ewma_weight(dt, self.c.slow_half_life_ms);
            s.anchor += (mid - s.anchor) * ewma_weight(dt, self.c.anchor_half_life_ms);
            let decay = 1. - ewma_weight(dt, self.c.flow_half_life_ms);
            s.flow_signed *= decay;
            s.flow_total *= decay;
        }
        let mut newest = s.last_trade_id;
        for t in v.public_trades {
            if s.last_trade_id.is_some_and(|id| t.trade_id <= id) {
                continue;
            }
            let q = t.qty as f64;
            s.flow_signed += if t.taker_side == Side::Buy { q } else { -q };
            s.flow_total += q;
            newest = newest.max(Some(t.trade_id));
        }
        s.last_trade_id = newest;
        s.flow_total = s.flow_total.max(s.flow_signed.abs());
        if s.samples.back().is_none_or(|(t, _)| now >= t + 1000) {
            s.samples.push_back((now, mid));
        }
        while s.samples.len() > MAX_SAMPLES
            || s.samples.front().is_some_and(|(t, _)| {
                now.saturating_sub(*t) > self.c.stop_lookback_ms.max(self.c.slow_half_life_ms)
            })
        {
            s.samples.pop_front();
        }
        s.last_ms = Some(now);
        s.last_mid = Some(mid);
    }

    fn decide_view(
        &mut self,
        v: &ParticipantObservationView<'_>,
        precision: (i64, u64),
    ) -> Result<Vec<OrderAction>, BotError> {
        if v.instrument_id != self.p.participant.instrument_id.as_deref().unwrap_or("") {
            return Err(BotError("style trader instrument mismatch".into()));
        }
        if !matches!(v.status, MarketStatus::Running | MarketStatus::Paused) {
            return Ok(vec![]);
        }
        if self.state.last_ms.is_some_and(|ms| v.market_time_ms < ms) {
            return Err(BotError("style trader clock regressed".into()));
        }
        if self.state.last_ms == Some(v.market_time_ms) {
            return Ok(vec![]);
        }
        let now = v.market_time_ms;
        let (tick, lot) = (precision.0.max(1), precision.1.max(1));
        let (bid, ask) = (v.book.bids.first(), v.book.asks.first());
        let mid = match (bid, ask) {
            (Some(b), Some(a)) => (b.price_tick as f64 + a.price_tick as f64) / 2.,
            _ => v
                .public_trades
                .last()
                .map(|t| t.price_tick as f64)
                .or(self.state.last_mid)
                .unwrap_or(self.c.fallback_price_tick as f64),
        };
        self.observe(v, mid);
        if now < self.state.next_ms {
            return Ok(vec![]);
        }
        let wait = (-self.state.uniform().ln() * self.c.decision_interval_ms as f64)
            .clamp(1000., self.c.decision_interval_ms as f64 * 10.);
        self.state.next_ms = now.saturating_add(wait as u64);

        let Some(mut budget) = Budget::for_limits(
            v,
            self.c.inventory_cap,
            self.c.fee_buffer_ppm,
            self.c.target_leverage,
        ) else {
            return Ok(vec![]);
        };
        let spot = matches!(v.own_account, Some(AccountSnapshot::Spot(_)));
        let position = budget.position.clamp(i64::MIN.into(), i64::MAX.into()) as i64;
        let equity = match v.own_account.as_ref() {
            Some(AccountSnapshot::Spot(a)) => a.cash_balance as f64 + a.position_qty as f64 * mid,
            Some(AccountSnapshot::Perp(a)) => a.equity as f64,
            None => return Ok(vec![]),
        };
        let s = &mut self.state;

        // Score each style's previous exposure against the return since.
        if let (Some(old_mid), Some(old_ms)) = (s.last_decision_mid, s.last_decision_ms) {
            let ret_bps = (mid / old_mid).ln() * 1e4;
            let w = ewma_weight((now - old_ms) as f64, self.c.fitness_half_life_ms);
            s.fitness_momentum += (s.last_momentum * ret_bps - s.fitness_momentum) * w;
            s.fitness_reversion += (s.last_reversion * ret_bps - s.fitness_reversion) * w;
        }
        let saturation = self.c.saturation_ppm as f64 / 1e6;
        let momentum = ((s.fast - s.slow) / mid / saturation).clamp(-1., 1.);
        let reversion = ((s.anchor - mid) / mid / saturation).clamp(-1., 1.);
        s.last_momentum = momentum;
        s.last_reversion = reversion;
        s.last_decision_mid = Some(mid);
        s.last_decision_ms = Some(now);

        // A position that vanished while we still wanted it was stopped or
        // liquidated: sit out for the cooldown.
        if s.last_position != 0 && position == 0 && s.target.signum() == s.last_position.signum() {
            s.stop_outs += 1;
            s.style = Style::Out;
            s.cooldown_until_ms = now.saturating_add(self.c.stop_cooldown_ms);
        }
        s.last_position = position;

        let warm = now.saturating_sub(s.first_ms.unwrap_or(now)) >= self.c.warmup_ms;
        if warm && now >= s.cooldown_until_ms && s.uniform() * 1e6 < self.c.switch_ppm as f64 {
            let imbalance = if s.flow_total > 0. {
                (s.flow_signed / s.flow_total).abs()
            } else {
                0.
            };
            let scores = [
                0.,
                s.fitness_momentum + imbalance * self.c.herding_ppm as f64 / 1e6,
                s.fitness_reversion,
            ];
            let u = s.uniform();
            let next = choose_style(scores, self.c.intensity_ppm as f64 / 1e6, u);
            if next != s.style {
                s.switches += 1;
                s.style = next;
            }
        }
        s.style_decisions[s.style as usize] += 1;

        let capacity = (equity.max(0.) * self.c.equity_use_ppm as f64 / 1e6
            * self.c.target_leverage as f64
            / mid)
            .min(self.c.inventory_cap as f64);
        let exposure = match s.style {
            Style::Out => 0.,
            Style::Momentum => momentum,
            Style::Reversion => reversion,
        };
        let floor = if spot { 0 } else { -self.c.inventory_cap };
        s.target = ((exposure * capacity).round() as i64).clamp(floor, self.c.inventory_cap);

        let mut actions = vec![];
        // Spot, or perpetual in Internal mode, checks the stop itself.
        let internal = spot || self.c.stop_mode == "Internal";
        let stopped_internally = internal
            && position != 0
            && s.stop_tick.is_some_and(|stop| {
                if position > 0 {
                    mid <= stop as f64
                } else {
                    mid >= stop as f64
                }
            });
        if stopped_internally {
            s.stop_outs += 1;
            s.style = Style::Out;
            s.target = 0;
            s.cooldown_until_ms = now.saturating_add(self.c.stop_cooldown_ms);
        }
        let delta = s.target - position;
        let min_trade = (capacity * self.c.min_trade_ppm as f64 / 1e6).max(lot as f64);
        if (delta.unsigned_abs() as f64) >= min_trade || (s.target == 0 && delta != 0) {
            let side = if delta > 0 { Side::Buy } else { Side::Sell };
            let child_cap = if stopped_internally || s.style == Style::Out {
                delta.unsigned_abs()
            } else {
                ((capacity * self.c.child_ppm as f64 / 1e6) as u64).max(lot)
            };
            let wanted = delta.unsigned_abs().min(child_cap) / lot * lot;
            let slip = mid * self.c.max_slippage_ppm as f64 / 1e6;
            let limit = match side {
                Side::Buy => ((mid + slip) / tick as f64).floor() as i64 * tick,
                Side::Sell => ((mid - slip) / tick as f64).ceil() as i64 * tick,
            }
            .max(tick);
            let top = if side == Side::Buy { ask } else { bid };
            let reachable = top.is_some_and(|t| match side {
                Side::Buy => t.price_tick <= limit,
                Side::Sell => t.price_tick >= limit,
            });
            let reducing =
                (position > 0 && side == Side::Sell) || (position < 0 && side == Side::Buy);
            if reachable && wanted > 0 && (!budget.reduce_only || reducing) {
                let qty = budget.allocate(side, limit, wanted) / lot * lot;
                if qty > 0 {
                    actions.push(OrderAction::PlaceProtected {
                        side,
                        position_side: PositionSide::Both,
                        qty,
                        order_type: model::ProtectedOrderType::ImmediateOrCancel,
                        price_tick: limit,
                        reduce_only: budget.reduce_only,
                        valid_until_market_time_ms: Some(now.saturating_add(30_000)),
                        expires_at_market_time_ms: None,
                    });
                }
            }
        }

        // Stops follow recent extremes of this bot's own samples.
        let stop = (position != 0)
            .then(|| {
                stop_level(
                    &s.samples,
                    now,
                    self.c.stop_lookback_ms,
                    position > 0,
                    self.c.stop_buffer_ppm as f64 / 1e6,
                )
            })
            .flatten()
            .map(|p| {
                // The venue rejects stops at or through the touch; keep at
                // least one tick outside it.
                if position > 0 {
                    let floor = (p / tick as f64).floor() as i64 * tick;
                    bid.map_or(floor, |b| floor.min(b.price_tick - tick))
                } else {
                    let ceil = (p / tick as f64).ceil() as i64 * tick;
                    ask.map_or(ceil, |a| ceil.max(a.price_tick + tick))
                }
                .max(tick)
            });
        if stop != s.stop_tick {
            s.stop_tick = stop;
            if !internal && let Some(stop_loss_tick) = stop {
                actions.insert(
                    0,
                    OrderAction::SetPositionProtection {
                        position_side: PositionSide::Both,
                        protection: Some(PositionProtectionSpec {
                            stop_loss_tick: Some(stop_loss_tick),
                            trigger: crate::position_protection::ProtectionTrigger::Last,
                            ..Default::default()
                        }),
                    },
                );
            }
        }
        Ok(actions)
    }
}

impl ScheduledBot for Trader {
    fn decide(&mut self, v: &ParticipantObservation) -> Result<Vec<OrderAction>, BotError> {
        if v.room_id != self.p.participant.room_id {
            return Err(BotError("style trader room mismatch".into()));
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
            return Err(BotError("style trader room mismatch".into()));
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
            plugin_version: VERSION.into(),
            state_version: 1,
            data: Arc::new(serde_json::to_value(&self.state).expect("finite style state")),
        }
    }
}

#[cfg(test)]
#[path = "style_switching_tests.rs"]
mod tests;
