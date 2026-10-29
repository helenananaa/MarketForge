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
/// 1.0.0 retains the historical midpoint policy. 1.1.0 evaluates executable
/// two-leg quotes, waits for the actual hedge, and sizes against both books.
const EXECUTABLE_VERSION: &str = "1.1.0";
/// 1.2.0 uses the same executable-pair policy, with both legs protected at
/// their observed touch and sized there. A committed hedge is not stranded
/// merely because half the current spread exceeds a midpoint slippage band.
const TOUCH_HEDGE_VERSION: &str = "1.2.0";

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
        if p.plugin_id != ID
            || ![VERSION, EXECUTABLE_VERSION, TOUCH_HEDGE_VERSION]
                .contains(&p.plugin_version.as_str())
            || p.state_version != 1
        {
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
    fn supports_plugin_version(&self, version: &str) -> bool {
        [VERSION, EXECUTABLE_VERSION, TOUCH_HEDGE_VERSION].contains(&version)
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
        if plugin_id != ID || *plugin_version != p.plugin_version || *state_version != 1 {
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

/// A basis is only tradable at the two prices paid/received. Entry thresholds
/// keep their configured meaning (including the population's fee allowance);
/// this policy does not add or fit a new spread/fee coefficient. Exit uses the
/// reverse executable pair, so a wide spread cannot masquerade as convergence.
fn executable_target(
    perp: i64,
    spot: &BookSnapshot,
    future: &BookSnapshot,
    c: &Config,
    long_limit: i64,
) -> Option<i64> {
    let (sb, sa, pb, pa) = (
        spot.bids.first()?,
        spot.asks.first()?,
        future.bids.first()?,
        future.asks.first()?,
    );
    let cheap = pa.price_tick as f64 / sb.price_tick as f64 - 1.;
    let rich = pb.price_tick as f64 / sa.price_tick as f64 - 1.;
    let entry = c.entry_ppm as f64 / 1e6;
    let exit = c.exit_ppm as f64 / 1e6;
    let step = c.step_qty as i64;
    if (perp > 0 && rich >= -exit) || (perp < 0 && cheap <= exit) {
        Some(perp - perp.signum() * step.min(perp.abs()))
    } else if cheap < -entry {
        Some((perp + step).min(c.max_hedge.min(long_limit)))
    } else if rich > entry {
        Some((perp - step).max(-c.max_hedge))
    } else {
        Some(perp)
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
        let executable = self.p.plugin_version != VERSION;
        let touch_hedge = self.p.plugin_version == TOUCH_HEDGE_VERSION;
        let (spot, perp) = if lead { (peer, v) } else { (v, peer) };
        let (Some(s), Some(p)) = (position(spot), position(perp)) else {
            return Ok(vec![]);
        };
        let (spot_mid, perp_mid) = (mid(spot), mid(perp));
        let basis = spot_mid.zip(perp_mid).map(|(s, p)| p / s - 1.);
        if let Some(basis) = basis {
            self.state.last_basis_ppm = Some((basis * 1e6).round() as i64);
        } else if lead || !touch_hedge {
            return Ok(vec![]);
        }
        // Once the lead has really filled, repairing the spot residual only
        // needs its executable side and the actual perp position. A missing
        // opposite quote (here or on perp) is not a reason to leave it unhedged.
        let (tick, lot) = (precision.0.max(1), precision.1.max(1) as i64);
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
        let (side, mut qty, limit) = if lead {
            // Lead only while hedged so legging risk stays within one step.
            let residual = s + p - self.c.neutral_spot;
            if (executable && residual != 0)
                || (!executable && residual.abs() > self.c.step_qty as i64)
            {
                return Ok(vec![]);
            }
            let target = if executable {
                executable_target(p, spot.book, perp.book, &self.c, s.max(0) + p.max(0))
                    .unwrap_or(p)
            } else {
                lead_target(
                    p,
                    basis.unwrap(),
                    self.c.entry_ppm as f64 / 1e6,
                    self.c.exit_ppm as f64 / 1e6,
                    self.c.step_qty as i64,
                    self.c.max_hedge,
                    s.max(0) + p.max(0),
                )
            };
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
            let (side, limit) = if executable {
                // Do not sweep beyond the observed executable price. The two
                // legs still settle independently; no fill or hedge is assumed.
                if delta > 0 {
                    (Side::Buy, perp.book.asks[0].price_tick as f64)
                } else {
                    (Side::Sell, perp.book.bids[0].price_tick as f64)
                }
            } else if delta > 0 {
                let l = perp_mid.unwrap() * (1. + slip);
                (
                    Side::Buy,
                    if unwinding {
                        l
                    } else {
                        l.min(spot_mid.unwrap() * (1. - exit))
                    },
                )
            } else {
                let l = perp_mid.unwrap() * (1. - slip);
                (
                    Side::Sell,
                    if unwinding {
                        l
                    } else {
                        l.max(spot_mid.unwrap() * (1. + exit))
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
            if touch_hedge {
                let side = if delta > 0 { Side::Buy } else { Side::Sell };
                let top = if delta > 0 {
                    spot.book.asks.first()
                } else {
                    spot.book.bids.first()
                };
                let Some(top) = top else { return Ok(vec![]) };
                (
                    side,
                    delta.unsigned_abs().min(top.qty),
                    top.price_tick as f64,
                )
            } else if delta > 0 {
                (Side::Buy, delta as u64, spot_mid.unwrap() * (1. + slip))
            } else {
                (
                    Side::Sell,
                    delta.unsigned_abs(),
                    spot_mid.unwrap() * (1. - slip),
                )
            }
        };
        if executable && lead {
            let (peer_side, peer_top, own_top, peer_limit) = if side == Side::Buy {
                (
                    Side::Sell,
                    &spot.book.bids[0],
                    &perp.book.asks[0],
                    spot_mid.unwrap() * (1. - slip),
                )
            } else {
                (
                    Side::Buy,
                    &spot.book.asks[0],
                    &perp.book.bids[0],
                    spot_mid.unwrap() * (1. + slip),
                )
            };
            let Some(mut peer_budget) = Budget::for_limits(
                spot,
                self.c.neutral_spot + self.c.max_hedge,
                self.c.fee_buffer_ppm,
                1,
            ) else {
                return Ok(vec![]);
            };
            // Capacity is observable, not reserved across venues. Other agents
            // may consume it before the hedge; the next lead waits for real fills.
            qty = qty.min(peer_top.qty).min(own_top.qty);
            // 1.1 used the midpoint band even when its price did not reach the
            // observed hedge touch. In 1.2 the proposed IOC and its budget agree.
            let hedge_price = if touch_hedge {
                peer_top.price_tick
            } else {
                peer_limit.ceil() as i64
            };
            qty = peer_budget.allocate(peer_side, hedge_price, qty);
        }
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
            plugin_version: self.p.plugin_version.clone(),
            state_version: 1,
            data: Arc::new(serde_json::to_value(&self.state).expect("basis state")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(bid: i64, ask: i64, qty: u64) -> BookSnapshot {
        BookSnapshot {
            bids: vec![BookLevel {
                price_tick: bid,
                qty,
            }],
            asks: vec![BookLevel {
                price_tick: ask,
                qty,
            }],
        }
    }

    fn template(version: &str) -> AgentTemplate {
        AgentTemplate::Plugin(BotConfig {
            participant: ParticipantConfig {
                participant_id: "basis-test".into(),
                kind: ParticipantKind::RuleAgent,
                room_id: "basis".into(),
                account_id: 20,
                instrument_id: Some("PERP".into()),
            },
            plugin_id: ID.into(),
            plugin_version: version.into(),
            state_version: 1,
            config_version: 1,
            seed: 7,
            config: json!({"leg":"Perp","hedge_instrument_id":"SPOT","decision_interval_ms":1000,"jitter_ms":0}),
        })
    }

    fn observation(s: i128, p: i128, cash: i128) -> ParticipantObservation {
        let mut spot = ParticipantObservation {
            version: 1,
            room_id: "basis".into(),
            venue_id: "test".into(),
            instrument_id: "SPOT".into(),
            instrument: None,
            status: MarketStatus::Running,
            step: 0,
            market_time_ms: 0,
            book: book(9999, 10001, 3),
            public_trades: vec![],
            own_orders: vec![],
            related_markets: vec![],
            market_events: vec![],
            own_account: Some(AccountSnapshot::Spot(
                SpotAccount {
                    account_id: 20,
                    cash_balance: cash,
                    position_qty: s,
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
        };
        let mut lead = spot.clone();
        lead.instrument_id = "PERP".into();
        lead.book = book(10060, 10062, 10);
        lead.own_account = Some(AccountSnapshot::Perp(serde_json::from_value(json!({
            "account_id":20,"cash_balance":10000000,"position_qty":p,"avg_entry_price_tick":10000,
            "realized_pnl":0,"unrealized_pnl":0,"equity":10000000,"initial_margin":0,"fees_paid":0
        })).unwrap()));
        spot.related_markets.clear();
        lead.related_markets.push(spot);
        lead
    }

    fn decide(version: &str, v: &ParticipantObservation) -> Vec<OrderAction> {
        let registry = BotRegistry::with_builtins();
        let t = template(version);
        registry
            .create(&t, &t.initial_state())
            .unwrap()
            .decide(v)
            .unwrap()
    }

    #[test]
    fn executable_policy_rejects_midpoint_only_profit() {
        let mut v = observation(100, 0, 10000000);
        v.book = book(10021, 10039, 10);
        v.related_markets[0].book = book(9975, 10025, 10);
        // A 30 bp midpoint basis, but selling perp and buying spot loses money.
        assert!(!decide(VERSION, &v).is_empty());
        assert!(decide(EXECUTABLE_VERSION, &v).is_empty());
    }

    #[test]
    fn executable_policy_checks_both_depths_and_real_hedge_capacity() {
        let v = observation(100, 0, 10000000);
        assert!(matches!(
            decide(EXECUTABLE_VERSION, &v).as_slice(),
            [OrderAction::PlaceProtected {
                side: Side::Sell,
                qty: 3,
                price_tick: 10060,
                ..
            }]
        ));
        assert!(decide(EXECUTABLE_VERSION, &observation(100, 0, 0)).is_empty());
        let mut thin = v.clone();
        thin.book.bids[0].qty = 1;
        assert!(matches!(
            decide(EXECUTABLE_VERSION, &thin).as_slice(),
            [OrderAction::PlaceProtected { qty: 1, .. }]
        ));
        let mut long = v;
        long.book = book(9940, 9942, 10);
        assert!(matches!(
            decide(EXECUTABLE_VERSION, &long).as_slice(),
            [OrderAction::PlaceProtected {
                side: Side::Buy,
                qty: 3,
                price_tick: 9942,
                ..
            }]
        ));
        if let Some(AccountSnapshot::Spot(a)) = &mut long.related_markets[0].own_account {
            a.available_position = 0;
            a.reserved_position = a.position_qty;
        }
        assert!(decide(EXECUTABLE_VERSION, &long).is_empty());
    }

    #[test]
    fn touch_hedge_prices_and_budget_use_the_same_executable_quote() {
        let mut v = observation(100, 0, 10035);
        v.book = book(10100, 10102, 10);
        v.related_markets[0].book = book(9960, 10040, 3);
        // The 1.1 lead sizes the spot buy at 10020 (mid + 20bp), which
        // cannot buy at 10040 and understates its real funded cost.
        assert!(!decide(EXECUTABLE_VERSION, &v).is_empty());
        assert!(decide(TOUCH_HEDGE_VERSION, &v).is_empty());
        if let Some(AccountSnapshot::Spot(a)) = &mut v.related_markets[0].own_account {
            a.cash_balance = 10051;
            a.available_cash = 10051;
        }
        assert!(matches!(
            decide(TOUCH_HEDGE_VERSION, &v).as_slice(),
            [OrderAction::PlaceProtected {
                side: Side::Sell,
                qty: 1,
                price_tick: 10100,
                ..
            }]
        ));
    }

    #[test]
    fn touch_hedge_repairs_real_residual_at_visible_depth_without_sweeping() {
        let registry = BotRegistry::with_builtins();
        for (spot_position, perp_position, side, price) in
            [(100, -10, Side::Buy, 10040), (100, 10, Side::Sell, 9960)]
        {
            let mut lead = observation(spot_position, perp_position, 10000000);
            let mut v = lead.related_markets.remove(0);
            v.book = book(9960, 10040, 3);
            v.related_markets.push(lead);
            let mut t = template(TOUCH_HEDGE_VERSION);
            if let AgentTemplate::Plugin(p) = &mut t {
                p.participant.instrument_id = Some("SPOT".into());
                p.config["leg"] = json!("Spot");
                p.config["hedge_instrument_id"] = json!("PERP");
            }
            let mut bot = registry.create(&t, &t.initial_state()).unwrap();
            assert!(
                matches!(bot.decide(&v).unwrap().as_slice(),[OrderAction::PlaceProtected{side:s,qty:3,price_tick,..}] if *s==side && *price_tick==price)
            );
            assert!(bot.decide(&v).unwrap().is_empty());
            let mut restored = registry.create(&t, &bot.snapshot()).unwrap();
            v.market_time_ms = 1000;
            assert_eq!(bot.decide(&v).unwrap(), restored.decide(&v).unwrap());
            // A hedge still executes when only its consumed side is quoted;
            // computing a two-sided midpoint is unnecessary for this leg.
            v.market_time_ms = 2000;
            v.related_markets[0].book.bids.clear();
            if side == Side::Buy {
                v.book.bids.clear();
            } else {
                v.book.asks.clear();
            }
            assert!(
                matches!(bot.decide(&v).unwrap().as_slice(),[OrderAction::PlaceProtected{side:s,qty:3,price_tick,..}] if *s==side && *price_tick==price)
            );
            // The actual residual still gates the lead, independent of intent.
            assert!(
                decide(
                    TOUCH_HEDGE_VERSION,
                    &observation(100, perp_position, 10000000)
                )
                .is_empty()
            );
        }
    }

    #[test]
    fn executable_policy_waits_for_partial_hedge_and_exit_prices() {
        assert!(!decide(VERSION, &observation(100, -10, 10000000)).is_empty());
        assert!(decide(EXECUTABLE_VERSION, &observation(100, -10, 10000000)).is_empty());
        assert!(decide(EXECUTABLE_VERSION, &observation(105, -10, 10000000)).is_empty());
        assert!(!decide(EXECUTABLE_VERSION, &observation(110, -10, 10000000)).is_empty());
        let mut wide = observation(90, 10, 10000000);
        wide.book = book(9975, 10025, 10);
        wide.related_markets[0].book = book(9975, 10025, 10);
        assert!(decide(EXECUTABLE_VERSION, &wide).is_empty());
        wide.book = book(9999, 10001, 10);
        wide.related_markets[0].book = book(9999, 10001, 10);
        assert!(matches!(
            decide(EXECUTABLE_VERSION, &wide).as_slice(),
            [OrderAction::PlaceProtected {
                side: Side::Sell,
                ..
            }]
        ));
    }

    #[test]
    fn executable_policy_restores_versioned_state_without_inventing_fills() {
        let registry = BotRegistry::with_builtins();
        let t = template(EXECUTABLE_VERSION);
        let mut bot = registry.create(&t, &t.initial_state()).unwrap();
        let mut v = observation(100, 0, 10000000);
        assert!(!bot.decide(&v).unwrap().is_empty());
        assert!(bot.decide(&v).unwrap().is_empty());
        let mut restored = registry.create(&t, &bot.snapshot()).unwrap();
        let legacy = template(VERSION);
        assert!(registry.create(&legacy, &bot.snapshot()).is_err());
        v.market_time_ms = 1000;
        v.step = 1;
        assert_eq!(bot.decide(&v).unwrap(), restored.decide(&v).unwrap());
        assert_eq!(bot.snapshot(), restored.snapshot());
        let mut spot_t = t.clone();
        if let AgentTemplate::Plugin(p) = &mut spot_t {
            p.participant.instrument_id = Some("SPOT".into());
            p.config["leg"] = json!("Spot");
            p.config["hedge_instrument_id"] = json!("PERP");
        }
        // An unfilled lead decision leaves both actual positions unchanged.
        let mut hedge = v.related_markets[0].clone();
        v.related_markets.clear();
        hedge.related_markets.push(v);
        let mut spot_bot = registry.create(&spot_t, &spot_t.initial_state()).unwrap();
        assert!(spot_bot.decide(&hedge).unwrap().is_empty());
    }

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
