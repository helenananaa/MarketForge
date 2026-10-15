use super::*;
use crate::{BookLevel, BookSnapshot, ParticipantConfig, ParticipantKind, SpotAccount};
use serde_json::Value;

fn template(seed: u64, config: Value) -> AgentTemplate {
    let mut c = json!({"warmup_ms":0,"decision_interval_ms":1000,"child_interval_ms":1000,
        "execution_horizon_ms":20000,"inventory_cap":100,"equity_use_ppm":1000000,"fallback_price_tick":100});
    c.as_object_mut()
        .unwrap()
        .extend(config.as_object().unwrap().clone());
    AgentTemplate::Plugin(BotConfig {
        participant: ParticipantConfig {
            participant_id: "value-test".into(),
            kind: ParticipantKind::RuleAgent,
            room_id: "value".into(),
            account_id: 20,
            instrument_id: Some("TEST".into()),
        },
        plugin_id: ID.into(),
        plugin_version: VERSION.into(),
        state_version: 1,
        config_version: 1,
        seed,
        config: c,
    })
}

fn observation(second: u64, bid: i64, ask: i64, position: i128) -> ParticipantObservation {
    ParticipantObservation {
        version: 1,
        room_id: "value".into(),
        venue_id: "default-venue".into(),
        instrument_id: "TEST".into(),
        instrument: None,
        status: MarketStatus::Running,
        step: second,
        market_time_ms: second * 1000,
        book: BookSnapshot {
            bids: vec![BookLevel {
                price_tick: bid,
                qty: 1000,
            }],
            asks: vec![BookLevel {
                price_tick: ask,
                qty: 1000,
            }],
        },
        public_trades: vec![],
        own_orders: vec![],
        related_markets: vec![],
        market_events: vec![],
        own_account: Some(AccountSnapshot::Spot(
            SpotAccount {
                account_id: 20,
                cash_balance: 100_000,
                position_qty: position,
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

fn make(t: &AgentTemplate) -> Box<dyn ScheduledBot> {
    BotRegistry::with_builtins()
        .create(t, &t.initial_state())
        .unwrap()
}

fn state(bot: &dyn ScheduledBot) -> State {
    let PersistedAgentKindState::Plugin { data, .. } = bot.snapshot() else {
        panic!("plugin state")
    };
    serde_json::from_value(data.as_ref().clone()).unwrap()
}

fn side_qty(actions: &[OrderAction]) -> Vec<(Side, u64)> {
    actions
        .iter()
        .filter_map(|a| match a {
            OrderAction::PlaceProtected { side, qty, .. } => Some((*side, *qty)),
            _ => None,
        })
        .collect()
}

#[test]
fn pure_helpers_respect_edges_schedules_and_bounds() {
    assert_eq!(signal_strength(0.001, 0.002, 0.01), 0.);
    assert!((signal_strength(0.007, 0.002, 0.01) - 0.5).abs() < 1e-12);
    assert_eq!(signal_strength(-0.5, 0.002, 0.01), -1.);
    assert_eq!(scheduled_qty(100, 0, 1000), 0);
    assert_eq!(scheduled_qty(100, 250, 1000), 25);
    assert_eq!(scheduled_qty(100, 5000, 1000), 100);
    assert!((ewma_weight(1000., 1000) - 0.5).abs() < 1e-12);
    assert_eq!(activity_factor(0., 1., 1.), 1.);
    assert!(activity_factor(4., 1., 1.) > 1.9);
    assert_eq!(activity_factor(1e9, 1., 1.), 4.);
    assert_eq!(activity_factor(1e-9, 1., 1.), 0.25);
}

#[test]
fn anchor_follows_only_observed_prices() {
    let t = template(3, json!({"anchor_half_life_ms":10000,"warmup_ms":1000000}));
    let mut bot = make(&t);
    for s in 0..60 {
        let p = 100 + s as i64;
        assert!(
            bot.decide(&observation(s, p - 1, p + 1, 0))
                .unwrap()
                .is_empty()
        );
    }
    let st = state(bot.as_ref());
    assert!(st.anchor > 100. && st.anchor < 159., "anchor {}", st.anchor);
    assert!(st.parent.is_none(), "warmup blocks trading");
}

#[test]
fn positive_private_bias_buys_through_a_worked_parent_order() {
    // Belief 5% above an unchanged market: a full-size parent is committed and
    // released in bounded children over the horizon, not as one order.
    let t = template(
        11,
        json!({"bias_ppm":50000,"passive_ppm":0,"max_slippage_ppm":20000}),
    );
    let mut bot = make(&t);
    let mut position = 0i128;
    let mut children = vec![];
    for s in 0..40 {
        let actions = bot.decide(&observation(s, 99, 101, position)).unwrap();
        for (side, qty) in side_qty(&actions) {
            assert_eq!(side, Side::Buy);
            position += qty as i128; // assume the protected IOC filled
            children.push(qty);
        }
    }
    let st = state(bot.as_ref());
    assert!(children.len() >= 4, "children {children:?}");
    assert!(children.iter().all(|q| *q < 100), "children {children:?}");
    assert!(position > 0 && position <= 100);
    assert!(st.parents_started >= 1);
    assert!(st.target > 0);
}

#[test]
fn spot_account_never_targets_a_short() {
    let t = template(5, json!({"bias_ppm":-200000}));
    let mut bot = make(&t);
    for s in 0..30 {
        let actions = bot.decide(&observation(s, 99, 101, 0)).unwrap();
        assert!(side_qty(&actions).is_empty(), "{actions:?}");
    }
    assert_eq!(state(bot.as_ref()).target, 0);
}

#[test]
fn private_drift_is_seeded_per_instance() {
    let config =
        json!({"belief_drift_ppm":200000,"belief_drift_cap_ppm":100000,"warmup_ms":1000000});
    let (mut a, mut b, mut c) = (
        make(&template(1, config.clone())),
        make(&template(2, config.clone())),
        make(&template(1, config)),
    );
    for s in 0..200 {
        let p = 100 + (s % 3) as i64;
        let v = observation(s, p - 1, p + 1, 0);
        a.decide(&v).unwrap();
        b.decide(&v).unwrap();
        c.decide(&v).unwrap();
    }
    let (sa, sb, sc) = (state(a.as_ref()), state(b.as_ref()), state(c.as_ref()));
    assert_ne!(sa.drift, sb.drift);
    assert_eq!(sa.drift, sc.drift);
    assert!(sa.drift.abs() <= 0.1);
}

#[test]
fn snapshot_restore_matches_continuous_execution() {
    let t = template(
        9,
        json!({"bias_ppm":30000,"belief_drift_ppm":50000,"passive_ppm":500000,"max_slippage_ppm":20000}),
    );
    let registry = BotRegistry::with_builtins();
    let mut continuous = make(&t);
    let mut saved = t.initial_state();
    let mut position = 0i128;
    for s in 0..60 {
        let p = 100 + (s / 7 % 4) as i64;
        let v = observation(s, p - 1, p + 1, position);
        let expected = continuous.decide(&v).unwrap();
        let mut restored = registry.create(&t, &saved).unwrap();
        assert_eq!(restored.decide(&v).unwrap(), expected, "second {s}");
        saved = restored.snapshot();
        assert_eq!(saved, continuous.snapshot());
        for (side, qty) in side_qty(&expected) {
            position += if side == Side::Buy {
                qty as i128
            } else {
                -(qty as i128)
            };
        }
    }
}

#[test]
fn rejects_foreign_state_and_invalid_limits() {
    let t = template(1, json!({}));
    let registry = BotRegistry::with_builtins();
    let mut bot = make(&t);
    bot.decide(&observation(0, 99, 101, 0)).unwrap();
    let other = template(2, json!({}));
    assert!(registry.create(&other, &bot.snapshot()).is_err());
    let bad = template(
        1,
        json!({"vol_short_half_life_ms":10000,"vol_long_half_life_ms":10000}),
    );
    assert!(registry.create(&bad, &bad.initial_state()).is_err());
}
