//! Valuation trader whose belief is formed only from the market it observes.
//! The anchor is an EWMA of the observed mid over the bot's own horizon; a
//! private bias and an optional private random walk (the bot's own seeded
//! randomness, never a shared signal) make instances disagree. Confidence falls
//! with the account's own drawdown, and exposure scales with its own equity.
//! Desired changes become finite parent orders worked by child orders over a
//! randomized horizon, so persistence in order flow comes from participants'
//! own decisions. No oracle, scheduled path or shared value process is read.
use crate::market_bots::Budget;
use crate::observation::{ParticipantObservationView, SharedParticipantObservation};
use crate::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc};

pub(crate) const ID: &str = "EndogenousValueTrader";
pub(crate) const VERSION: &str = "1.0.0";

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Config {
    neutral_position: i64,
    inventory_cap: i64,
    equity_use_ppm: u32,
    target_leverage: u32,
    anchor_half_life_ms: u64,
    warmup_ms: u64,
    bias_ppm: i64,
    belief_drift_ppm: u32,
    belief_drift_cap_ppm: u32,
    edge_ppm: u32,
    saturation_ppm: u32,
    min_confidence_ppm: u32,
    confidence_drawdown_ppm: u32,
    rebalance_ppm: u32,
    decision_interval_ms: u64,
    child_interval_ms: u64,
    execution_horizon_ms: u64,
    passive_ppm: u32,
    max_slippage_ppm: u32,
    order_ttl_ms: u64,
    vol_short_half_life_ms: u64,
    vol_long_half_life_ms: u64,
    vol_activity_ppm: u32,
    fee_buffer_ppm: u32,
    fallback_price_tick: i64,
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
    let parameters = BTreeMap::from([
        ("neutral_position".into(), n(0, -1000000, 1000000)),
        ("inventory_cap".into(), n(100, 1, 1000000)),
        ("equity_use_ppm".into(), n(300000, 1, 1000000)),
        ("target_leverage".into(), n(1, 1, 20)),
        ("anchor_half_life_ms".into(), n(3600000, 1000, 604800000)),
        ("warmup_ms".into(), n(600000, 0, 86400000)),
        ("bias_ppm".into(), n(0, -500000, 500000)),
        ("belief_drift_ppm".into(), n(0, 0, 500000)),
        ("belief_drift_cap_ppm".into(), n(30000, 0, 500000)),
        ("edge_ppm".into(), n(2000, 0, 500000)),
        ("saturation_ppm".into(), n(10000, 1, 1000000)),
        ("min_confidence_ppm".into(), n(250000, 0, 1000000)),
        ("confidence_drawdown_ppm".into(), n(100000, 1, 1000000)),
        ("rebalance_ppm".into(), n(200000, 1, 1000000)),
        ("decision_interval_ms".into(), n(60000, 1000, 86400000)),
        ("child_interval_ms".into(), n(10000, 1000, 86400000)),
        ("execution_horizon_ms".into(), n(1800000, 1000, 86400000)),
        ("passive_ppm".into(), n(500000, 0, 1000000)),
        ("max_slippage_ppm".into(), n(2000, 0, 100000)),
        ("order_ttl_ms".into(), n(30000, 1000, 86400000)),
        ("vol_short_half_life_ms".into(), n(300000, 1000, 86400000)),
        ("vol_long_half_life_ms".into(), n(7200000, 1000, 604800000)),
        ("vol_activity_ppm".into(), n(500000, 0, 2000000)),
        ("fee_buffer_ppm".into(), n(1000, 0, 100000)),
        ("fallback_price_tick".into(), n(10000, 1, 1000000000)),
    ]);
    r.register(Factory(BotDescriptor {
        id: ID.into(),
        name: "内生估值与拆单交易者".into(),
        version: VERSION.into(),
        protocol_version: BOT_PROTOCOL_VERSION.into(),
        state_version: 1,
        runtime: "builtin".into(),
        parameters,
    }))
    .expect("valid endogenous value trader");
}

struct Factory(BotDescriptor);
impl Factory {
    fn config(&self, t: &AgentTemplate) -> Result<(BotConfig, Config, String), BotError> {
        let AgentTemplate::Plugin(p) = t else {
            return Err(BotError("value plugin required".into()));
        };
        if p.plugin_id != ID || p.plugin_version != VERSION || p.state_version != 1 {
            return Err(BotError("value trader identity mismatch".into()));
        }
        let mut p = p.clone();
        p.config = self.0.validate_config(&p.config)?;
        let c: Config = serde_json::from_value(p.config.clone()).map_err(err)?;
        if c.neutral_position.unsigned_abs() > c.inventory_cap as u64
            || c.vol_short_half_life_ms >= c.vol_long_half_life_ms
            || c.bias_ppm.unsigned_abs() >= 1_000_000
        {
            return Err(BotError("invalid value trader limits".into()));
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
            return Err(BotError("value trader state required".into()));
        };
        if plugin_id != ID || plugin_version != VERSION || *state_version != 1 {
            return Err(BotError("value trader state version mismatch".into()));
        }
        let state = if data.is_null() {
            State {
                identity: identity.clone(),
                rng: p.seed.max(1),
                confidence: 1.,
                ..Default::default()
            }
        } else {
            serde_json::from_value::<State>(data.as_ref().clone()).map_err(err)?
        };
        if state.identity != identity || !state.valid() {
            return Err(BotError("invalid value trader saved state".into()));
        }
        Ok(Box::new(Trader { p, c, state }))
    }
}

fn err(e: impl std::fmt::Display) -> BotError {
    BotError(e.to_string())
}

/// A committed parent order. Progress is measured from actual position change,
/// never from submitted quantity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Parent {
    side: Side,
    qty: u64,
    start_position: i64,
    start_ms: u64,
    horizon_ms: u64,
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
    anchor: f64,
    drift: f64,
    var_short: f64,
    var_long: f64,
    peak_equity: f64,
    confidence: f64,
    belief: f64,
    target: i64,
    parent: Option<Parent>,
    born: BTreeMap<u64, u64>,
    parents_started: u64,
    parents_completed: u64,
    parents_abandoned: u64,
    children_passive: u64,
    children_aggressive: u64,
}

impl State {
    fn valid(&self) -> bool {
        [
            self.anchor,
            self.drift,
            self.var_short,
            self.var_long,
            self.peak_equity,
            self.confidence,
            self.belief,
        ]
        .iter()
        .all(|v| v.is_finite())
            && self.rng != 0
            && self.var_short >= 0.
            && self.var_long >= 0.
            && (0. ..=1.).contains(&self.confidence)
            && self.last_mid.is_none_or(|m| m.is_finite() && m > 0.)
            && self.born.len() <= 64
            && self
                .parent
                .as_ref()
                .is_none_or(|p| p.qty > 0 && p.horizon_ms > 0)
    }

    fn next_u64(&mut self) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }

    /// Uniform in (0, 1]; never zero so logarithms stay finite.
    fn uniform(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 1.) / (1u64 << 53) as f64
    }

    fn normal(&mut self) -> f64 {
        let (u1, u2) = (self.uniform(), self.uniform());
        (-2. * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

/// Fraction of a half-life elapsed as an EWMA weight on the new sample.
fn ewma_weight(dt_ms: f64, half_life_ms: u64) -> f64 {
    1. - 0.5f64.powf(dt_ms / half_life_ms as f64)
}

/// Signed exposure in [-1, 1] for a relative mispricing; zero inside the edge.
fn signal_strength(gap: f64, edge: f64, saturation: f64) -> f64 {
    gap.signum() * ((gap.abs() - edge) / saturation).clamp(0., 1.)
}

/// Activity multiplier from the bot's own short/long realized variance ratio.
/// Above one when the market it observes is more turbulent than usual.
fn activity_factor(var_short: f64, var_long: f64, sensitivity: f64) -> f64 {
    if var_short <= 0. || var_long <= 0. {
        return 1.;
    }
    (var_short / var_long)
        .powf(0.5 * sensitivity)
        .clamp(0.25, 4.)
}

/// Quantity a parent should have completed by `elapsed` under a linear schedule.
fn scheduled_qty(qty: u64, elapsed_ms: u64, horizon_ms: u64) -> u64 {
    let fraction = (elapsed_ms as f64 / horizon_ms.max(1) as f64).clamp(0., 1.);
    (qty as f64 * fraction).ceil() as u64
}

struct Trader {
    p: BotConfig,
    c: Config,
    state: State,
}

impl Trader {
    fn observe(&mut self, now: u64, mid: f64) {
        let s = &mut self.state;
        if s.first_ms.is_none() {
            s.first_ms = Some(now);
            s.anchor = mid;
            s.peak_equity = 0.;
        }
        if let (Some(old_ms), Some(old_mid)) = (s.last_ms, s.last_mid) {
            let dt = (now - old_ms) as f64;
            if dt > 0. {
                s.anchor += (mid - s.anchor) * ewma_weight(dt, self.c.anchor_half_life_ms);
                let r = (mid / old_mid).ln();
                let rate = r * r / (dt / 1000.);
                if s.var_long == 0. {
                    s.var_short = rate;
                    s.var_long = rate;
                } else {
                    s.var_short +=
                        (rate - s.var_short) * ewma_weight(dt, self.c.vol_short_half_life_ms);
                    s.var_long +=
                        (rate - s.var_long) * ewma_weight(dt, self.c.vol_long_half_life_ms);
                }
                if self.c.belief_drift_ppm > 0 {
                    let sigma = self.c.belief_drift_ppm as f64 / 1e6 * (dt / 3_600_000.).sqrt();
                    let cap = self.c.belief_drift_cap_ppm as f64 / 1e6;
                    let z = s.normal();
                    s.drift = (s.drift + sigma * z).clamp(-cap, cap);
                }
            }
        }
        s.last_ms = Some(now);
        s.last_mid = Some(mid);
    }

    fn schedule_next(&mut self, now: u64) {
        let mean = if self.state.parent.is_some() {
            self.c.child_interval_ms
        } else {
            self.c.decision_interval_ms
        } as f64;
        let activity = activity_factor(
            self.state.var_short,
            self.state.var_long,
            self.c.vol_activity_ppm as f64 / 1e6,
        );
        // Independent exponential arrivals keep instances unsynchronized.
        let wait = (-self.state.uniform().ln() * mean / activity).clamp(1000., mean * 10.);
        self.state.next_ms = now.saturating_add(wait as u64);
    }

    fn decide_view(
        &mut self,
        v: &ParticipantObservationView<'_>,
        precision: (i64, u64),
    ) -> Result<Vec<OrderAction>, BotError> {
        if v.instrument_id != self.p.participant.instrument_id.as_deref().unwrap_or("") {
            return Err(BotError("value trader instrument mismatch".into()));
        }
        if !matches!(v.status, MarketStatus::Running | MarketStatus::Paused) {
            return Ok(vec![]);
        }
        if self.state.last_ms.is_some_and(|ms| v.market_time_ms < ms) {
            return Err(BotError("value trader clock regressed".into()));
        }
        if self.state.last_ms == Some(v.market_time_ms) {
            return Ok(vec![]);
        }
        let now = v.market_time_ms;
        let (tick, lot) = (precision.0.max(1), precision.1.max(1));
        let bid = v.book.bids.first();
        let ask = v.book.asks.first();
        let mid = match (bid, ask) {
            (Some(b), Some(a)) => (b.price_tick as f64 + a.price_tick as f64) / 2.,
            _ => v
                .public_trades
                .last()
                .map(|t| t.price_tick as f64)
                .or(self.state.last_mid)
                .unwrap_or(self.c.fallback_price_tick as f64),
        };
        self.observe(now, mid);
        if now < self.state.next_ms {
            return Ok(vec![]);
        }
        self.schedule_next(now);

        self.state
            .born
            .retain(|id, _| v.own_orders.iter().any(|o| o.order_id == *id));
        for o in v.own_orders.iter().take(64) {
            self.state.born.entry(o.order_id).or_insert(now);
        }
        let expired: Vec<_> = v
            .own_orders
            .iter()
            .filter(|o| now - self.state.born[&o.order_id] >= self.c.order_ttl_ms)
            .map(|o| OrderAction::Cancel {
                order_id: o.order_id,
            })
            .collect();
        if !expired.is_empty() {
            return Ok(expired);
        }

        let Some(mut budget) = Budget::for_limits(
            v,
            self.c.inventory_cap,
            self.c.fee_buffer_ppm,
            self.c.target_leverage,
        ) else {
            return Ok(vec![]);
        };
        let position = budget.position.clamp(i64::MIN.into(), i64::MAX.into()) as i64;
        let equity = match v.own_account.as_ref() {
            Some(AccountSnapshot::Spot(a)) => a.cash_balance as f64 + a.position_qty as f64 * mid,
            Some(AccountSnapshot::Perp(a)) => a.equity as f64,
            None => return Ok(vec![]),
        };
        let s = &mut self.state;
        s.peak_equity = s.peak_equity.max(equity);
        let drawdown = if s.peak_equity > 0. {
            (1. - equity / s.peak_equity).max(0.)
        } else {
            0.
        };
        s.confidence = (1. - drawdown / (self.c.confidence_drawdown_ppm as f64 / 1e6))
            .clamp(self.c.min_confidence_ppm as f64 / 1e6, 1.);
        if now.saturating_sub(s.first_ms.unwrap_or(now)) < self.c.warmup_ms {
            return Ok(vec![]);
        }

        s.belief = s.anchor * (1. + self.c.bias_ppm as f64 / 1e6 + s.drift);
        let gap = (s.belief - mid) / mid;
        let strength = signal_strength(
            gap,
            self.c.edge_ppm as f64 / 1e6,
            self.c.saturation_ppm as f64 / 1e6,
        ) * s.confidence;
        // Exposure scales with the account's own equity; losers shrink, winners
        // grow. Money only moves between participants through matching.
        let capacity = (equity.max(0.) * self.c.equity_use_ppm as f64 / 1e6
            * self.c.target_leverage as f64
            / mid)
            .min(self.c.inventory_cap as f64);
        let cap = self.c.inventory_cap;
        let floor = if matches!(v.own_account, Some(AccountSnapshot::Spot(_))) {
            0
        } else {
            -cap
        };
        s.target = ((self.c.neutral_position as f64 + strength * capacity).round() as i64)
            .clamp(floor, cap);
        let delta = s.target - position;
        let threshold = (capacity * self.c.rebalance_ppm as f64 / 1e6).max(lot as f64);

        if let Some(parent) = s.parent.clone() {
            let direction = if parent.side == Side::Buy { 1 } else { -1 };
            let done = ((position - parent.start_position) * direction).max(0) as u64;
            let reversed = delta.signum() == -direction && delta.unsigned_abs() as f64 >= threshold;
            if done >= parent.qty {
                s.parents_completed += 1;
                s.parent = None;
            } else if reversed || now > parent.start_ms.saturating_add(parent.horizon_ms * 2) {
                s.parents_abandoned += 1;
                s.parent = None;
            }
            if s.parent.is_none() && !v.own_orders.is_empty() {
                return Ok(v
                    .own_orders
                    .iter()
                    .take(64)
                    .map(|o| OrderAction::Cancel {
                        order_id: o.order_id,
                    })
                    .collect());
            }
        }
        if s.parent.is_none() {
            if (delta.unsigned_abs() as f64) < threshold {
                return Ok(vec![]);
            }
            let spread = 0.5 + s.uniform();
            s.parent = Some(Parent {
                side: if delta > 0 { Side::Buy } else { Side::Sell },
                qty: delta.unsigned_abs() / lot * lot,
                start_position: position,
                start_ms: now,
                horizon_ms: ((self.c.execution_horizon_ms as f64 * spread) as u64).max(1000),
            });
            s.parents_started += 1;
            if s.parent.as_ref().is_some_and(|p| p.qty == 0) {
                s.parent = None;
                return Ok(vec![]);
            }
        }
        let parent = s.parent.clone().expect("active parent");
        let direction = if parent.side == Side::Buy { 1 } else { -1 };
        let done = ((position - parent.start_position) * direction).max(0) as u64;
        let resting: u64 = v
            .own_orders
            .iter()
            .filter(|o| o.side == parent.side)
            .map(|o| o.remaining_qty)
            .sum();
        let elapsed = now - parent.start_ms;
        let due = scheduled_qty(parent.qty, elapsed, parent.horizon_ms);
        let behind = due.saturating_sub(done).saturating_sub(resting);
        let child = behind.min(parent.qty - done.min(parent.qty)) / lot * lot;
        if child == 0 {
            return Ok(vec![]);
        }
        let reducing = (position > 0 && parent.side == Side::Sell)
            || (position < 0 && parent.side == Side::Buy);
        if budget.reduce_only && !reducing {
            return Ok(vec![]);
        }
        let late = elapsed >= parent.horizon_ms;
        let passive = !late && resting == 0 && s.uniform() * 1e6 < self.c.passive_ppm as f64;
        let action = if passive {
            let price = match parent.side {
                Side::Buy => bid.map(|b| b.price_tick),
                Side::Sell => ask.map(|a| a.price_tick),
            }
            .unwrap_or((mid / tick as f64).round() as i64 * tick);
            let qty = budget.allocate(parent.side, price, child) / lot * lot;
            if qty == 0 {
                return Ok(vec![]);
            }
            s.children_passive += 1;
            OrderAction::PlaceProtected {
                side: parent.side,
                position_side: PositionSide::Both,
                qty,
                order_type: model::ProtectedOrderType::PostOnly,
                price_tick: price,
                reduce_only: budget.reduce_only,
                valid_until_market_time_ms: None,
                expires_at_market_time_ms: None,
            }
        } else {
            let top = match parent.side {
                Side::Buy => ask,
                Side::Sell => bid,
            };
            let Some(top) = top else {
                return Ok(vec![]);
            };
            let slip = mid * self.c.max_slippage_ppm as f64 / 1e6;
            let limit = match parent.side {
                Side::Buy => ((mid + slip) / tick as f64).floor() as i64 * tick,
                Side::Sell => ((mid - slip) / tick as f64).ceil() as i64 * tick,
            }
            .max(tick);
            if (parent.side == Side::Buy && top.price_tick > limit)
                || (parent.side == Side::Sell && top.price_tick < limit)
            {
                return Ok(vec![]);
            }
            let qty = budget.allocate(parent.side, limit, child) / lot * lot;
            if qty == 0 {
                return Ok(vec![]);
            }
            s.children_aggressive += 1;
            OrderAction::PlaceProtected {
                side: parent.side,
                position_side: PositionSide::Both,
                qty,
                order_type: model::ProtectedOrderType::ImmediateOrCancel,
                price_tick: limit,
                reduce_only: budget.reduce_only,
                valid_until_market_time_ms: Some(now.saturating_add(self.c.order_ttl_ms)),
                expires_at_market_time_ms: None,
            }
        };
        Ok(vec![action])
    }
}

impl ScheduledBot for Trader {
    fn decide(&mut self, v: &ParticipantObservation) -> Result<Vec<OrderAction>, BotError> {
        if v.room_id != self.p.participant.room_id {
            return Err(BotError("value trader room mismatch".into()));
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
            return Err(BotError("value trader room mismatch".into()));
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
            data: Arc::new(serde_json::to_value(&self.state).expect("finite value trader state")),
        }
    }
}

#[cfg(test)]
#[path = "endogenous_value_tests.rs"]
mod tests;
