//! Two-sided spot/perpetual basis trader. Two instances share one funded
//! account: the perpetual leg leads, buying the perpetual when it trades below
//! spot by more than the entry threshold and selling it when above; it unwinds
//! toward flat once the gap is within the exit threshold. The spot leg keeps
//! spot inventory at `neutral_spot - perp_position`, so the pair stays hedged.
//! Spot is never shorted; capacity is limited by real inventory, cash and
//! margin. Both legs act only on observed books and their own account.
use crate::market_bots::Budget;
use crate::observation::{ParticipantObservationView, SharedParticipantObservation};
use crate::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc};

pub(crate) const ID: &str = "BasisLinkTrader";
pub(crate) const VERSION: &str = "1.0.0";

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Config {
    leg: String,
    hedge_instrument_id: String,
    neutral_spot: i64,
    max_hedge: i64,
    step_qty: u64,
    entry_ppm: u32,
    exit_ppm: u32,
    max_slippage_ppm: u32,
    decision_interval_ms: u64,
    jitter_ms: u64,
    target_leverage: u32,
    fee_buffer_ppm: u32,
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

fn s(default: &str, choices: &[&str]) -> BotParameter {
    BotParameter {
        kind: ParameterType::String,
        required: false,
        default: Some(json!(default)),
        minimum: None,
        maximum: None,
        choices: choices.iter().map(|c| json!(c)).collect(),
    }
}

pub(crate) fn register(r: &mut BotRegistry) {
    let parameters = BTreeMap::from([
        ("leg".into(), s("Spot", &["Spot", "Perp"])),
        (
            "hedge_instrument_id".into(),
            BotParameter {
                choices: vec![],
                ..s("V-BTC-PERP", &[])
            },
        ),
        ("neutral_spot".into(), n(100, 0, 1000000)),
        ("max_hedge".into(), n(100, 1, 1000000)),
        ("step_qty".into(), n(10, 1, 1000000)),
        ("entry_ppm".into(), n(2000, 1, 1000000)),
        ("exit_ppm".into(), n(500, 0, 1000000)),
        ("max_slippage_ppm".into(), n(2000, 0, 100000)),
        ("decision_interval_ms".into(), n(2000, 1000, 86400000)),
        ("jitter_ms".into(), n(1000, 0, 86400000)),
        ("target_leverage".into(), n(2, 1, 20)),
        ("fee_buffer_ppm".into(), n(1000, 0, 100000)),
    ]);
    r.register(Factory(BotDescriptor {
        id: ID.into(),
        name: "双向现货永续基差交易者".into(),
        version: VERSION.into(),
        protocol_version: BOT_PROTOCOL_VERSION.into(),
        state_version: 1,
        runtime: "builtin".into(),
        parameters,
    }))
    .expect("valid basis link trader");
}

struct Factory(BotDescriptor);
impl Factory {
    fn config(&self, t: &AgentTemplate) -> Result<(BotConfig, Config, String), BotError> {
        let AgentTemplate::Plugin(p) = t else {
            return Err(BotError("basis plugin required".into()));
        };
        if p.plugin_id != ID || p.plugin_version != VERSION || p.state_version != 1 {
            return Err(BotError("basis trader identity mismatch".into()));
        }
        let mut p = p.clone();
        p.config = self.0.validate_config(&p.config)?;
        let c: Config = serde_json::from_value(p.config.clone()).map_err(err)?;
        if c.exit_ppm >= c.entry_ppm
            || Some(&c.hedge_instrument_id) == p.participant.instrument_id.as_ref()
        {
            return Err(BotError(
                "exit must be below entry and the hedge instrument must differ".into(),
            ));
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
    fn related_instruments(&self, t: &AgentTemplate) -> Result<Vec<String>, BotError> {
        Ok(vec![self.config(t)?.1.hedge_instrument_id])
    }
    fn create(
        &self,
        t: &AgentTemplate,
        st: &PersistedAgentKindState,
    ) -> Result<Box<dyn ScheduledBot>, BotError> {
        let (p, c, identity) = self.config(t)?;
        let PersistedAgentKindState::Plugin {
            plugin_id,
            plugin_version,
            state_version,
            data,
        } = st
        else {
            return Err(BotError("basis trader state required".into()));
        };
        if plugin_id != ID || plugin_version != VERSION || *state_version != 1 {
            return Err(BotError("basis trader state version mismatch".into()));
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
        if state.identity != identity || state.rng == 0 {
            return Err(BotError("invalid basis trader saved state".into()));
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
    last_basis_ppm: Option<i64>,
    entries: u64,
    unwinds: u64,
    hedges: u64,
}

fn mid(v: &ParticipantObservationView<'_>) -> Option<f64> {
    match (v.book.bids.first(), v.book.asks.first()) {
        (Some(b), Some(a)) => Some((b.price_tick as f64 + a.price_tick as f64) / 2.),
        _ => None,
    }
}

fn position(v: &ParticipantObservationView<'_>) -> Option<i64> {
    match v.own_account.as_ref()? {
        AccountSnapshot::Spot(a) => Some(a.position_qty as i64),
        AccountSnapshot::Perp(a) => Some(a.position_qty as i64),
    }
}

/// Next perpetual position for the lead leg: step toward the side that closes
/// the gap while it exceeds `entry`, hold between exit and entry, step back
/// toward flat once within `exit`. `long_limit` is the spot inventory that can
/// be sold against a long perpetual.
pub(crate) fn lead_target(
    perp: i64,
    basis: f64,
    entry: f64,
    exit: f64,
    step: i64,
    max_hedge: i64,
    long_limit: i64,
) -> i64 {
    if basis < -entry {
        (perp + step).min(max_hedge.min(long_limit))
    } else if basis > entry {
        (perp - step).max(-max_hedge)
    } else if basis.abs() <= exit {
        perp - perp.signum() * step.min(perp.abs())
    } else {
        perp
    }
}

struct Trader {
    p: BotConfig,
    c: Config,
    state: State,
}

impl Trader {
    fn ioc(
        &self,
        side: Side,
        qty: u64,
        limit: f64,
        tick: i64,
        reduce_only: bool,
        now: u64,
    ) -> OrderAction {
        let price = match side {
            Side::Buy => (limit / tick as f64).floor() as i64 * tick,
            Side::Sell => (limit / tick as f64).ceil() as i64 * tick,
        }
        .max(tick);
        OrderAction::PlaceProtected {
            side,
            position_side: PositionSide::Both,
            qty,
            order_type: model::ProtectedOrderType::ImmediateOrCancel,
            price_tick: price,
            reduce_only,
            valid_until_market_time_ms: Some(now.saturating_add(10_000)),
            expires_at_market_time_ms: None,
        }
    }

    fn decide_view(
        &mut self,
        v: &ParticipantObservationView<'_>,
        precision: (i64, u64),
    ) -> Result<Vec<OrderAction>, BotError> {
        if v.instrument_id != self.p.participant.instrument_id.as_deref().unwrap_or("") {
            return Err(BotError("basis trader instrument mismatch".into()));
        }
        if !matches!(v.status, MarketStatus::Running | MarketStatus::Paused) {
            return Ok(vec![]);
        }
        let now = v.market_time_ms;
        if self.state.last_ms.is_some_and(|ms| now < ms) {
            return Err(BotError("basis trader clock regressed".into()));
        }
        if self.state.last_ms == Some(now) || now < self.state.next_ms {
            return Ok(vec![]);
        }
        self.state.last_ms = Some(now);
        self.state.rng ^= self.state.rng << 13;
        self.state.rng ^= self.state.rng >> 7;
        self.state.rng ^= self.state.rng << 17;
        self.state.next_ms = now
            .saturating_add(self.c.decision_interval_ms)
            .saturating_add(self.state.rng % (self.c.jitter_ms + 1));
        let peer = v
            .related_markets
            .iter()
            .find(|p| p.instrument_id == self.c.hedge_instrument_id)
            .ok_or_else(|| BotError("basis trader requires the peer market".into()))?;
        if !v.own_orders.is_empty() {
            return Ok(v
                .own_orders
                .iter()
                .take(16)
                .map(|o| OrderAction::Cancel {
                    order_id: o.order_id,
                })
                .collect());
        }
        let lead = self.c.leg == "Perp";
        let (spot, perp) = if lead { (peer, v) } else { (v, peer) };
        let (Some(spot_mid), Some(perp_mid), Some(s), Some(p)) =
            (mid(spot), mid(perp), position(spot), position(perp))
        else {
            return Ok(vec![]);
        };
        let (tick, lot) = (precision.0.max(1), precision.1.max(1) as i64);
        let basis = perp_mid / spot_mid - 1.;
        self.state.last_basis_ppm = Some((basis * 1e6).round() as i64);
        let slip = self.c.max_slippage_ppm as f64 / 1e6;
        let Some(mut budget) = Budget::for_limits(
            v,
            if lead {
                self.c.max_hedge
            } else {
                self.c.neutral_spot + self.c.max_hedge
            },
            self.c.fee_buffer_ppm,
            if lead { self.c.target_leverage } else { 1 },
        ) else {
            return Ok(vec![]);
        };
        let (side, qty, limit) = if lead {
            // Lead only while hedged so legging risk stays within one step.
            let residual = s + p - self.c.neutral_spot;
            if residual.abs() > self.c.step_qty as i64 {
                return Ok(vec![]);
            }
            let target = lead_target(
                p,
                basis,
                self.c.entry_ppm as f64 / 1e6,
                self.c.exit_ppm as f64 / 1e6,
                self.c.step_qty as i64,
                self.c.max_hedge,
                s.max(0) + p.max(0),
            );
            let delta = target - p;
            if delta == 0 {
                return Ok(vec![]);
            }
            let exit = self.c.exit_ppm as f64 / 1e6;
            let unwinding = target.abs() < p.abs();
            if unwinding {
                self.state.unwinds += 1;
            } else {
                self.state.entries += 1;
            }
            // Entries never pay through the far side of the spot price.
            let (side, limit) = if delta > 0 {
                let l = perp_mid * (1. + slip);
                (
                    Side::Buy,
                    if unwinding {
                        l
                    } else {
                        l.min(spot_mid * (1. - exit))
                    },
                )
            } else {
                let l = perp_mid * (1. - slip);
                (
                    Side::Sell,
                    if unwinding {
                        l
                    } else {
                        l.max(spot_mid * (1. + exit))
                    },
                )
            };
            (side, delta.unsigned_abs(), limit)
        } else {
            let delta = (self.c.neutral_spot - p).max(0) - s;
            if delta == 0 {
                return Ok(vec![]);
            }
            self.state.hedges += 1;
            if delta > 0 {
                (Side::Buy, delta as u64, spot_mid * (1. + slip))
            } else {
                (Side::Sell, delta.unsigned_abs(), spot_mid * (1. - slip))
            }
        };
        let top = if side == Side::Buy {
            v.book.asks.first()
        } else {
            v.book.bids.first()
        };
        if !top.is_some_and(|t| match side {
            Side::Buy => (t.price_tick as f64) <= limit,
            Side::Sell => (t.price_tick as f64) >= limit,
        }) {
            return Ok(vec![]);
        }
        let reducing = budget.position != 0 && (budget.position > 0) == (side == Side::Sell);
        if budget.reduce_only && !reducing {
            return Ok(vec![]);
        }
        let qty = budget.allocate(side, limit.ceil() as i64, qty) as i64 / lot * lot;
        if qty <= 0 {
            return Ok(vec![]);
        }
        Ok(vec![self.ioc(
            side,
            qty as u64,
            limit,
            tick,
            budget.reduce_only,
            now,
        )])
    }
}

impl ScheduledBot for Trader {
    fn decide(&mut self, v: &ParticipantObservation) -> Result<Vec<OrderAction>, BotError> {
        if v.room_id != self.p.participant.room_id {
            return Err(BotError("basis trader room mismatch".into()));
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
            return Err(BotError("basis trader room mismatch".into()));
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
            data: Arc::new(serde_json::to_value(&self.state).expect("basis state")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lead_target_closes_gaps_holds_band_and_unwinds() {
        // Perp 1% cheap: buy perp, limited by sellable spot inventory.
        assert_eq!(lead_target(0, -0.01, 0.002, 0.0005, 10, 100, 100), 10);
        assert_eq!(lead_target(95, -0.01, 0.002, 0.0005, 10, 100, 100), 100);
        assert_eq!(lead_target(0, -0.01, 0.002, 0.0005, 10, 100, 4), 4);
        // Perp dear: sell perp.
        assert_eq!(lead_target(0, 0.01, 0.002, 0.0005, 10, 100, 100), -10);
        // Between exit and entry: hold.
        assert_eq!(lead_target(30, -0.001, 0.002, 0.0005, 10, 100, 100), 30);
        // Converged: step back toward flat without overshooting.
        assert_eq!(lead_target(30, 0.0001, 0.002, 0.0005, 10, 100, 100), 20);
        assert_eq!(lead_target(-4, 0.0, 0.002, 0.0005, 10, 100, 100), 0);
    }
}
