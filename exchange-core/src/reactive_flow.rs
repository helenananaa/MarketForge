//! Small takers whose activity and direction react to the market they observe.
//! Arrival intensity rises when the observed trade rate exceeds its own
//! longer-run average (self-exciting bursts and volume clustering); direction
//! leans with (chasers) or against (faders) the recent observed move and the
//! observed flow imbalance, with a pull toward the bot's own inventory target.
//! Sizes are heavy-tailed. Every input is the bot's own observation, account
//! or seeded randomness.
use crate::endogenous_value::ewma_weight;
use crate::market_bots::Budget;
use crate::observation::{ParticipantObservationView, SharedParticipantObservation};
use crate::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc};

pub(crate) const ID: &str = "ReactiveFlowTrader";
pub(crate) const VERSION: &str = "1.0.0";
/// Experimental execution policy: aggressive demands persist as finite parent
/// orders, with each IOC child limited to observed external touch liquidity.
/// Version 1.0.0 and its random draws/snapshots remain unchanged.
const SLICED_VERSION: &str = "1.1.0";

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Config {
    base_interval_ms: u64,
    short_half_life_ms: u64,
    long_half_life_ms: u64,
    excitation_ppm: u32,
    attention_ppm: u32,
    move_half_life_ms: u64,
    move_scale_ppm: u32,
    chase_ppm: i64,
    herd_ppm: i64,
    inventory_target: i64,
    inventory_pull_ppm: u32,
    min_qty: u64,
    max_qty: u64,
    size_alpha_ppm: u32,
    aggressive_ppm: u32,
    max_slippage_ppm: u32,
    order_ttl_ms: u64,
    inventory_cap: i64,
    target_leverage: u32,
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
        ("base_interval_ms".into(), n(5000, 1000, 86400000)),
        ("short_half_life_ms".into(), n(20000, 1000, 86400000)),
        ("long_half_life_ms".into(), n(600000, 1000, 86400000)),
        ("excitation_ppm".into(), n(1000000, 0, 5000000)),
        ("attention_ppm".into(), n(0, 0, 20000000)),
        ("move_half_life_ms".into(), n(60000, 1000, 86400000)),
        ("move_scale_ppm".into(), n(500, 1, 1000000)),
        ("chase_ppm".into(), n(500000, -2000000, 2000000)),
        ("herd_ppm".into(), n(300000, -2000000, 2000000)),
        ("inventory_target".into(), n(0, -1000000, 1000000)),
        ("inventory_pull_ppm".into(), n(500000, 0, 5000000)),
        ("min_qty".into(), n(1, 1, 1000000)),
        ("max_qty".into(), n(50, 1, 1000000)),
        ("size_alpha_ppm".into(), n(1500000, 500000, 5000000)),
        ("aggressive_ppm".into(), n(800000, 0, 1000000)),
        ("max_slippage_ppm".into(), n(1000, 0, 100000)),
        ("order_ttl_ms".into(), n(20000, 1000, 86400000)),
        ("inventory_cap".into(), n(200, 1, 1000000)),
        ("target_leverage".into(), n(1, 1, 20)),
        ("fee_buffer_ppm".into(), n(1000, 0, 100000)),
        ("fallback_price_tick".into(), n(10000, 1, 1000000000)),
    ]);
    r.register(Factory(BotDescriptor {
        id: ID.into(),
        name: "活跃度自激的反应型小额交易者".into(),
        version: VERSION.into(),
        protocol_version: BOT_PROTOCOL_VERSION.into(),
        state_version: 1,
        runtime: "builtin".into(),
        parameters,
    }))
    .expect("valid reactive flow trader");
}

struct Factory(BotDescriptor);
impl Factory {
    fn config(&self, t: &AgentTemplate) -> Result<(BotConfig, Config, String), BotError> {
        let AgentTemplate::Plugin(p) = t else {
            return Err(BotError("reactive plugin required".into()));
        };
        if p.plugin_id != ID
            || ![VERSION, SLICED_VERSION].contains(&p.plugin_version.as_str())
            || p.state_version != 1
        {
            return Err(BotError("reactive trader identity mismatch".into()));
        }
        let mut p = p.clone();
        p.config = self.0.validate_config(&p.config)?;
        let c: Config = serde_json::from_value(p.config.clone()).map_err(err)?;
        if c.short_half_life_ms >= c.long_half_life_ms
            || c.min_qty > c.max_qty
            || c.inventory_target.unsigned_abs() > c.inventory_cap as u64
        {
            return Err(BotError("invalid reactive trader limits".into()));
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
        [VERSION, SLICED_VERSION].contains(&version)
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
            return Err(BotError("reactive trader state required".into()));
        };
        if plugin_id != ID || *plugin_version != p.plugin_version || *state_version != 1 {
            return Err(BotError("reactive trader state version mismatch".into()));
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
        if state.identity != identity
            || !state.valid()
            || (p.plugin_version == VERSION
                && (state.parent.is_some()
                    || state.last_child_feedback.is_some()
                    || state.slicing.is_some()))
            || state.parent.as_ref().is_some_and(|x| {
                x.remaining > c.max_qty || x.deadline_ms.saturating_sub(x.born_ms) != c.order_ttl_ms
            })
        {
            return Err(BotError("invalid reactive trader saved state".into()));
        }
        Ok(Box::new(Trader { p, c, state }))
    }
}

fn err(e: impl std::fmt::Display) -> BotError {
    BotError(e.to_string())
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    identity: String,
    rng: u64,
    last_ms: Option<u64>,
    next_ms: u64,
    last_trade_id: Option<u64>,
    rate_short: f64,
    rate_long: f64,
    reference: Option<f64>,
    flow_signed: f64,
    flow_total: f64,
    born: BTreeMap<u64, u64>,
    orders: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<ParentDemand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_child_feedback: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    slicing: Option<SlicingLedger>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SlicingLedger {
    parents: u64,
    demand_lots: u64,
    filled_lots: u64,
    expired_lots: u64,
    children: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ParentDemand {
    side: Side,
    remaining: u64,
    limit: i64,
    born_ms: u64,
    deadline_ms: u64,
}

impl State {
    fn valid(&self) -> bool {
        [self.rate_short, self.rate_long, self.flow_total]
            .iter()
            .all(|v| v.is_finite() && *v >= 0.)
            && self.flow_signed.is_finite()
            && self.flow_total >= self.flow_signed.abs()
            && self.reference.is_none_or(|r| r.is_finite() && r > 0.)
            && self.rng != 0
            && self.born.len() <= 64
            && self.parent.as_ref().is_none_or(|p| {
                p.remaining > 0
                    && p.limit > 0
                    && p.born_ms < p.deadline_ms
                    && self.last_ms.is_some_and(|ms| p.born_ms <= ms)
            })
            && match &self.slicing {
                Some(s) => {
                    s.parents > 0
                        && u128::from(s.demand_lots)
                            == u128::from(s.filled_lots)
                                + u128::from(s.expired_lots)
                                + u128::from(self.parent.as_ref().map_or(0, |p| p.remaining))
                }
                None => self.parent.is_none(),
            }
    }

    fn uniform(&mut self) -> f64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        ((self.rng >> 11) as f64 + 1.) / (1u64 << 53) as f64
    }
}

/// Arrival-rate multiplier from observed short/long trade activity.
pub(crate) fn excitation(rate_short: f64, rate_long: f64, exponent: f64) -> f64 {
    if rate_short <= 0. || rate_long <= 0. {
        return 1.;
    }
    (rate_short / rate_long).powf(exponent).clamp(0.25, 8.)
}

/// Arrival-rate multiplier from the size of the recent observed move, in
/// units of `move_scale`; capped so a crash cannot make the rate unbounded.
pub(crate) fn attention(moved: f64, weight: f64) -> f64 {
    1. + weight * moved.abs().min(5.)
}

/// Probability of buying. `pressure` combines chasing/fading and herding;
/// `inventory` is the signed distance from target as a fraction of the cap.
pub(crate) fn buy_probability(pressure: f64, inventory: f64, pull: f64) -> f64 {
    (0.5 + 0.45 * (pressure - pull * inventory).tanh()).clamp(0.05, 0.95)
}

/// Pareto size with tail exponent `alpha`, floored at `min`, capped at `max`.
pub(crate) fn pareto_qty(u: f64, min: u64, max: u64, alpha: f64) -> u64 {
    ((min as f64 * u.powf(-1. / alpha)).floor() as u64).clamp(min, max)
}

struct Trader {
    p: BotConfig,
    c: Config,
    state: State,
}

impl Trader {
    fn parent_child(
        &mut self,
        v: &ParticipantObservationView<'_>,
        precision: (i64, u64),
    ) -> Vec<OrderAction> {
        let Some(parent) = self.state.parent.as_ref() else {
            return vec![];
        };
        let now = v.market_time_ms;
        if now >= parent.deadline_ms {
            // An unfilled IOC is not a fill. Outstanding demand remains live
            // only for the original finite TTL; never move its price boundary.
            self.state
                .slicing
                .as_mut()
                .expect("parent ledger")
                .expired_lots += parent.remaining;
            self.state.parent = None;
            return vec![];
        }
        let side = parent.side;
        let Some(mut budget) = Budget::for_limits(
            v,
            self.c.inventory_cap,
            self.c.fee_buffer_ppm,
            self.c.target_leverage,
        ) else {
            return vec![];
        };
        let reducing = (budget.position > 0 && side == Side::Sell)
            || (budget.position < 0 && side == Side::Buy);
        if budget.reduce_only && !reducing {
            return vec![];
        }
        let levels = if side == Side::Buy {
            &v.book.asks
        } else {
            &v.book.bids
        };
        let Some(top) = levels.first() else {
            return vec![];
        };
        if (side == Side::Buy && top.price_tick > parent.limit)
            || (side == Side::Sell && top.price_tick < parent.limit)
        {
            return vec![];
        }
        let crossing: Vec<_> = v
            .own_orders
            .iter()
            .filter(|o| {
                o.side != side
                    && if side == Side::Buy {
                        o.price_tick <= top.price_tick
                    } else {
                        o.price_tick >= top.price_tick
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
        let own_touch = v
            .own_orders
            .iter()
            .filter(|o| o.side != side && o.price_tick == top.price_tick)
            .map(|o| o.remaining_qty)
            .sum::<u64>();
        let requested = parent.remaining.min(top.qty.saturating_sub(own_touch));
        let lot = precision.1.max(1);
        let qty = budget.allocate(side, top.price_tick, requested) / lot * lot;
        if qty == 0 {
            return vec![];
        }
        self.state.orders += 1;
        self.state.slicing.as_mut().expect("parent ledger").children += 1;
        vec![OrderAction::PlaceProtected {
            side,
            position_side: PositionSide::Both,
            qty,
            order_type: model::ProtectedOrderType::ImmediateOrCancel,
            price_tick: top.price_tick,
            reduce_only: budget.reduce_only,
            valid_until_market_time_ms: Some(parent.deadline_ms),
            expires_at_market_time_ms: None,
        }]
    }

    fn observe(&mut self, v: &ParticipantObservationView<'_>, mid: f64) {
        let now = v.market_time_ms;
        let s = &mut self.state;
        let mut new_qty = 0.;
        let mut newest = s.last_trade_id;
        let dt = s.last_ms.map_or(0., |old| (now - old) as f64);
        if dt > 0. {
            let decay = 1. - ewma_weight(dt, self.c.short_half_life_ms);
            s.flow_signed *= decay;
            s.flow_total *= decay;
        }
        for t in v.public_trades {
            if s.last_trade_id.is_some_and(|id| t.trade_id <= id) {
                continue;
            }
            let q = t.qty as f64;
            new_qty += q;
            s.flow_signed += if t.taker_side == Side::Buy { q } else { -q };
            s.flow_total += q;
            newest = newest.max(Some(t.trade_id));
        }
        s.last_trade_id = newest;
        s.flow_total = s.flow_total.max(s.flow_signed.abs());
        if dt > 0. {
            let inst = new_qty / (dt / 1000.);
            s.rate_short += (inst - s.rate_short) * ewma_weight(dt, self.c.short_half_life_ms);
            s.rate_long += (inst - s.rate_long) * ewma_weight(dt, self.c.long_half_life_ms);
            let r = s.reference.unwrap_or(mid);
            s.reference = Some(r + (mid - r) * ewma_weight(dt, self.c.move_half_life_ms));
        } else if s.reference.is_none() {
            s.reference = Some(mid);
        }
        s.last_ms = Some(now);
    }

    fn decide_view(
        &mut self,
        v: &ParticipantObservationView<'_>,
        precision: (i64, u64),
    ) -> Result<Vec<OrderAction>, BotError> {
        if v.instrument_id != self.p.participant.instrument_id.as_deref().unwrap_or("") {
            return Err(BotError("reactive trader instrument mismatch".into()));
        }
        if !matches!(v.status, MarketStatus::Running | MarketStatus::Paused) {
            return Ok(vec![]);
        }
        let now = v.market_time_ms;
        if self.state.last_ms.is_some_and(|ms| now < ms) {
            return Err(BotError("reactive trader clock regressed".into()));
        }
        if self.state.last_ms == Some(now) {
            return Ok(vec![]);
        }
        let (tick, lot) = (precision.0.max(1), precision.1.max(1));
        let (bid, ask) = (v.book.bids.first(), v.book.asks.first());
        let mid = match (bid, ask) {
            (Some(b), Some(a)) => (b.price_tick as f64 + a.price_tick as f64) / 2.,
            _ => v
                .public_trades
                .last()
                .map(|t| t.price_tick as f64)
                .or(self.state.reference)
                .unwrap_or(self.c.fallback_price_tick as f64),
        };
        self.observe(v, mid);
        if self.p.plugin_version == SLICED_VERSION && self.state.parent.is_some() {
            // Retry on the next authoritative observation, without resampling
            // direction/size or forgetting the residual after a partial fill.
            return Ok(self.parent_child(v, precision));
        }
        if now < self.state.next_ms {
            return Ok(vec![]);
        }
        let s = &mut self.state;
        let moved = s.reference.map_or(0., |r| mid / r - 1.) / (self.c.move_scale_ppm as f64 / 1e6);
        // Attention: a moving price draws traders in, so volume comes with
        // large candles and fades when the price is quiet.
        let factor = excitation(
            s.rate_short,
            s.rate_long,
            self.c.excitation_ppm as f64 / 1e6,
        ) * attention(moved, self.c.attention_ppm as f64 / 1e6);
        let wait = (-s.uniform().ln() * self.c.base_interval_ms as f64 / factor)
            .clamp(1000., self.c.base_interval_ms as f64 * 20.);
        s.next_ms = now.saturating_add(wait as u64);

        s.born
            .retain(|id, _| v.own_orders.iter().any(|o| o.order_id == *id));
        for o in v.own_orders.iter().take(64) {
            s.born.entry(o.order_id).or_insert(now);
        }
        let expired: Vec<_> = v
            .own_orders
            .iter()
            .filter(|o| now - s.born[&o.order_id] >= self.c.order_ttl_ms)
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
        let imbalance = if s.flow_total > 0. {
            s.flow_signed / s.flow_total
        } else {
            0.
        };
        let pressure = self.c.chase_ppm as f64 / 1e6 * moved.clamp(-3., 3.)
            + self.c.herd_ppm as f64 / 1e6 * imbalance;
        let inventory =
            (budget.position as f64 - self.c.inventory_target as f64) / self.c.inventory_cap as f64;
        let p_buy = buy_probability(pressure, inventory, self.c.inventory_pull_ppm as f64 / 1e6);
        let side = if s.uniform() < p_buy {
            Side::Buy
        } else {
            Side::Sell
        };
        let wanted = pareto_qty(
            s.uniform(),
            self.c.min_qty,
            self.c.max_qty,
            self.c.size_alpha_ppm as f64 / 1e6,
        ) / lot
            * lot;
        let reducing = (budget.position > 0 && side == Side::Sell)
            || (budget.position < 0 && side == Side::Buy);
        if wanted == 0 || (budget.reduce_only && !reducing) {
            return Ok(vec![]);
        }
        let aggressive = s.uniform() * 1e6 < self.c.aggressive_ppm as f64;
        if aggressive && self.p.plugin_version == SLICED_VERSION {
            let slip = mid * self.c.max_slippage_ppm as f64 / 1e6;
            let limit = match side {
                Side::Buy => ((mid + slip) / tick as f64).floor() as i64 * tick,
                Side::Sell => ((mid - slip) / tick as f64).ceil() as i64 * tick,
            }
            .max(tick);
            s.parent = Some(ParentDemand {
                side,
                remaining: wanted,
                limit,
                born_ms: now,
                deadline_ms: now.saturating_add(self.c.order_ttl_ms),
            });
            let ledger = s.slicing.get_or_insert_with(SlicingLedger::default);
            ledger.parents += 1;
            ledger.demand_lots += wanted;
            return Ok(self.parent_child(v, precision));
        }
        let action = if aggressive {
            let slip = mid * self.c.max_slippage_ppm as f64 / 1e6;
            let limit = match side {
                Side::Buy => ((mid + slip) / tick as f64).floor() as i64 * tick,
                Side::Sell => ((mid - slip) / tick as f64).ceil() as i64 * tick,
            }
            .max(tick);
            let reachable = match side {
                Side::Buy => ask.is_some_and(|a| a.price_tick <= limit),
                Side::Sell => bid.is_some_and(|b| b.price_tick >= limit),
            };
            if !reachable {
                return Ok(vec![]);
            }
            // Never cross our own resting quote: cancel it first.
            let crossing: Vec<_> = v
                .own_orders
                .iter()
                .filter(|o| {
                    o.side != side
                        && match side {
                            Side::Buy => o.price_tick <= limit,
                            Side::Sell => o.price_tick >= limit,
                        }
                })
                .take(64)
                .map(|o| OrderAction::Cancel {
                    order_id: o.order_id,
                })
                .collect();
            if !crossing.is_empty() {
                return Ok(crossing);
            }
            let qty = budget.allocate(side, limit, wanted) / lot * lot;
            if qty == 0 {
                return Ok(vec![]);
            }
            OrderAction::PlaceProtected {
                side,
                position_side: PositionSide::Both,
                qty,
                order_type: model::ProtectedOrderType::ImmediateOrCancel,
                price_tick: limit,
                reduce_only: budget.reduce_only,
                valid_until_market_time_ms: Some(now.saturating_add(10_000)),
                expires_at_market_time_ms: None,
            }
        } else {
            let Some(price) = (match side {
                Side::Buy => bid.map(|b| b.price_tick),
                Side::Sell => ask.map(|a| a.price_tick),
            }) else {
                return Ok(vec![]);
            };
            let qty = budget.allocate(side, price, wanted) / lot * lot;
            if qty == 0 {
                return Ok(vec![]);
            }
            OrderAction::PlaceProtected {
                side,
                position_side: PositionSide::Both,
                qty,
                order_type: model::ProtectedOrderType::PostOnly,
                price_tick: price,
                reduce_only: budget.reduce_only,
                valid_until_market_time_ms: None,
                expires_at_market_time_ms: None,
            }
        };
        s.orders += 1;
        Ok(vec![action])
    }
}

impl ScheduledBot for Trader {
    fn execution_feedback(&mut self, action: &OrderAction, e: &GatewayExecution) {
        if self.p.plugin_version != SLICED_VERSION
            || e.participant_id != self.p.participant.participant_id
            || e.room_id != self.p.participant.room_id
            || *action != e.action
            || e.account_id != self.p.participant.account_id
            || e.instrument_id != self.p.participant.instrument_id.as_deref().unwrap_or("")
            || self
                .state
                .last_child_feedback
                .is_some_and(|seq| e.execution.command_seq <= seq)
        {
            return;
        }
        let OrderAction::PlaceProtected {
            side,
            qty,
            order_type: model::ProtectedOrderType::ImmediateOrCancel,
            ..
        } = action
        else {
            return;
        };
        let Some(parent) = self.state.parent.as_mut() else {
            return;
        };
        if *side != parent.side
            || e.execution.market_time_ms < parent.born_ms
            || e.execution.market_time_ms > parent.deadline_ms
        {
            return;
        }
        self.state.last_child_feedback = Some(e.execution.command_seq);
        let ActorExecutionResult::Accepted(r) = &e.execution.result else {
            return;
        };
        let events = match r {
            MarketExecution::Spot(r) => &r.events,
            MarketExecution::Perp(r) => &r.events,
        };
        let filled = events
            .iter()
            .filter_map(|x| match &x.event {
                Event::TradePrinted(t)
                    if t.taker_account_id == e.account_id && t.taker_side == *side =>
                {
                    Some(t.qty)
                }
                _ => None,
            })
            .sum::<u64>()
            .min(*qty);
        let applied = filled.min(parent.remaining);
        parent.remaining -= applied;
        self.state
            .slicing
            .as_mut()
            .expect("parent ledger")
            .filled_lots += applied;
        if parent.remaining == 0 {
            self.state.parent = None;
        }
    }
    fn decide(&mut self, v: &ParticipantObservation) -> Result<Vec<OrderAction>, BotError> {
        if v.room_id != self.p.participant.room_id {
            return Err(BotError("reactive trader room mismatch".into()));
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
            return Err(BotError("reactive trader room mismatch".into()));
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
            data: Arc::new(serde_json::to_value(&self.state).expect("finite reactive state")),
        }
    }
}

#[cfg(test)]
#[path = "reactive_flow_tests.rs"]
mod tests;
