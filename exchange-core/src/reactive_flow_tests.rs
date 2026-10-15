use super::*;
use crate::{BookLevel, BookSnapshot, ParticipantConfig, ParticipantKind, SpotAccount, Trade};
use serde_json::Value;

fn template(seed: u64, config: Value) -> AgentTemplate {
    let mut c = json!({"base_interval_ms":5000,"short_half_life_ms":5000,"long_half_life_ms":60000,
        "inventory_cap":1000,"max_qty":50,"fallback_price_tick":100});
    c.as_object_mut()
        .unwrap()
        .extend(config.as_object().unwrap().clone());
    AgentTemplate::Plugin(BotConfig {
        participant: ParticipantConfig {
            participant_id: "reactive-test".into(),
            kind: ParticipantKind::RuleAgent,
            room_id: "reactive".into(),
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

fn trade(id: u64, side: Side, qty: u64) -> Trade {
    Trade {
        trade_id: id,
        maker_order_id: 1,
        maker_account_id: 1,
        taker_order_id: 1000 + id,
        taker_account_id: 2,
        price_tick: 100,
        qty,
        taker_side: side,
        maker_position_side: Default::default(),
        taker_position_side: Default::default(),
    }
}

fn observation(second: u64, mid: i64, trades: Vec<Trade>) -> ParticipantObservation {
    ParticipantObservation {
        version: 1,
        room_id: "reactive".into(),
        venue_id: "default-venue".into(),
        instrument_id: "TEST".into(),
        instrument: None,
        status: MarketStatus::Running,
        step: second,
        market_time_ms: second * 1000,
        book: BookSnapshot {
            bids: vec![BookLevel {
                price_tick: mid - 1,
                qty: 10000,
            }],
            asks: vec![BookLevel {
                price_tick: mid + 1,
                qty: 10000,
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
                position_qty: 500,
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

fn orders(actions: &[OrderAction]) -> Vec<(Side, u64)> {
    actions
        .iter()
        .filter_map(|a| match a {
            OrderAction::PlaceProtected { side, qty, .. } => Some((*side, *qty)),
            _ => None,
        })
        .collect()
}

#[test]
fn helpers_bound_excitation_direction_and_size() {
    assert_eq!(excitation(0., 1., 1.), 1.);
    assert!((excitation(4., 1., 1.) - 4.).abs() < 1e-12);
    assert_eq!(excitation(1000., 1., 1.), 8.);
    assert_eq!(excitation(1., 1000., 1.), 0.25);
    assert!((buy_probability(0., 0., 1.) - 0.5).abs() < 1e-12);
    assert!(buy_probability(2., 0., 1.) > 0.9);
    assert!(
        buy_probability(0., 1., 2.) < 0.1,
        "long inventory pulls toward selling"
    );
    assert_eq!(pareto_qty(1., 2, 50, 1.5), 2);
    assert_eq!(pareto_qty(1e-9, 2, 50, 1.5), 50);
    let sizes: Vec<_> = (1..=1000)
        .map(|i| pareto_qty(i as f64 / 1000., 1, 1000, 1.5))
        .collect();
    let small = sizes.iter().filter(|q| **q <= 2).count();
    assert!(
        small > 500 && *sizes.iter().max().unwrap() >= 90,
        "heavy-tailed sizes"
    );
}

#[test]
fn bursts_of_observed_activity_make_the_bot_trade_more_often() {
    let t = template(
        3,
        json!({"excitation_ppm":1500000,"chase_ppm":0,"herd_ppm":0}),
    );
    let (mut calm, mut busy) = (make(&t), make(&t));
    let (mut calm_orders, mut busy_orders) = (0, 0);
    let (mut calm_id, mut busy_id) = (0, 0);
    for s in 0..600u64 {
        // Same one-trade-per-second baseline; the busy bot then sees a burst.
        let burst = if s >= 400 { 20 } else { 1 };
        let next = |id: &mut u64, k: u64| {
            (0..k)
                .map(|_| {
                    *id += 1;
                    trade(
                        *id,
                        if (*id).is_multiple_of(2) {
                            Side::Buy
                        } else {
                            Side::Sell
                        },
                        1,
                    )
                })
                .collect::<Vec<_>>()
        };
        let calm_actions = calm
            .decide(&observation(s, 100, next(&mut calm_id, 1)))
            .unwrap();
        let busy_actions = busy
            .decide(&observation(s, 100, next(&mut busy_id, burst)))
            .unwrap();
        if s >= 400 {
            calm_orders += orders(&calm_actions).len();
            busy_orders += orders(&busy_actions).len();
        }
    }
    assert!(
        busy_orders > calm_orders * 2,
        "busy {busy_orders} calm {calm_orders}"
    );
}

#[test]
fn chasers_follow_a_rising_market_and_faders_lean_against_it() {
    let count = |chase: i64| {
        let t = template(
            9,
            json!({"chase_ppm":chase,"herd_ppm":0,"inventory_pull_ppm":0,
            "move_half_life_ms":120000,"move_scale_ppm":5000,"base_interval_ms":1000}),
        );
        let mut bot = make(&t);
        let (mut buys, mut sells) = (0, 0);
        for s in 0..300u64 {
            for (side, _) in orders(&bot.decide(&observation(s, 100 + s as i64, vec![])).unwrap()) {
                if side == Side::Buy {
                    buys += 1
                } else {
                    sells += 1
                }
            }
        }
        (buys, sells)
    };
    let (cb, cs) = count(1500000);
    let (fb, fs) = count(-1500000);
    assert!(cb > cs * 2, "chaser buys {cb} sells {cs}");
    assert!(fs > fb * 2, "fader buys {fb} sells {fs}");
}

#[test]
fn a_moving_price_draws_more_orders_than_a_flat_one() {
    assert_eq!(attention(0., 3.), 1.);
    assert!((attention(-1., 3.) - 4.).abs() < 1e-12);
    assert_eq!(attention(100., 3.), 16.);
    let t = template(
        13,
        json!({"attention_ppm":4000000,"excitation_ppm":0,"move_scale_ppm":1000,
            "move_half_life_ms":120000,"base_interval_ms":10000}),
    );
    let (mut flat, mut moving) = (make(&t), make(&t));
    let (mut flat_orders, mut moving_orders) = (0, 0);
    for s in 0..600u64 {
        flat_orders += orders(&flat.decide(&observation(s, 100, vec![])).unwrap()).len();
        // 1% over ten minutes, the move scale is 0.1%.
        let mid = 100 + (s as i64 / 60);
        moving_orders += orders(&moving.decide(&observation(s, mid, vec![])).unwrap()).len();
    }
    assert!(
        moving_orders > flat_orders * 2,
        "moving {moving_orders} flat {flat_orders}"
    );
}

#[test]
fn snapshot_restore_matches_continuous_execution() {
    let t = template(11, json!({"aggressive_ppm":500000}));
    let registry = BotRegistry::with_builtins();
    let mut continuous = make(&t);
    let mut saved = t.initial_state();
    for s in 0..120u64 {
        let trades = (0..(s % 5))
            .map(|k| trade(s * 10 + k, Side::Sell, 2))
            .collect();
        let v = observation(s, 100 + (s / 9 % 3) as i64, trades);
        let expected = continuous.decide(&v).unwrap();
        let mut restored = registry.create(&t, &saved).unwrap();
        assert_eq!(restored.decide(&v).unwrap(), expected, "second {s}");
        saved = restored.snapshot();
        assert_eq!(saved, continuous.snapshot());
    }
}

#[test]
fn aggressive_orders_cancel_own_crossing_quotes_first() {
    let t = template(
        5,
        json!({"aggressive_ppm":1000000,"base_interval_ms":1000,"chase_ppm":2000000,
        "herd_ppm":0,"inventory_pull_ppm":0,"move_half_life_ms":60000,"move_scale_ppm":1000}),
    );
    let mut bot = make(&t);
    for s in 0..200u64 {
        let mut v = observation(s, 100 + s as i64, vec![]);
        // A resting own sell inside any buy limit.
        v.own_orders = vec![crate::Order {
            order_id: 77,
            account_id: 20,
            side: Side::Sell,
            position_side: Default::default(),
            price_tick: 100 + s as i64 + 1,
            remaining_qty: 1,
            seq: 1,
        }];
        let actions = bot.decide(&v).unwrap();
        for (side, _) in orders(&actions) {
            assert_eq!(side, Side::Sell, "a buy would self-trade against order 77");
        }
    }
}

#[test]
fn rejects_invalid_limits() {
    let registry = BotRegistry::with_builtins();
    let bad = template(
        1,
        json!({"short_half_life_ms":60000,"long_half_life_ms":60000}),
    );
    assert!(registry.create(&bad, &bad.initial_state()).is_err());
    let bad = template(1, json!({"min_qty":60,"max_qty":50}));
    assert!(registry.create(&bad, &bad.initial_state()).is_err());
}
