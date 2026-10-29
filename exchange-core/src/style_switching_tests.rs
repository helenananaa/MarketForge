use super::*;
use crate::{BookLevel, BookSnapshot, ParticipantConfig, ParticipantKind, SpotAccount};
use serde_json::Value;

fn template(seed: u64, config: Value) -> AgentTemplate {
    let mut c = json!({"warmup_ms":0,"decision_interval_ms":1000,"fast_half_life_ms":2000,
        "slow_half_life_ms":10000,"anchor_half_life_ms":60000,"fitness_half_life_ms":10000,
        "inventory_cap":100,"equity_use_ppm":1000000,"fallback_price_tick":100,"saturation_ppm":10000,
        "stop_lookback_ms":20000,"max_slippage_ppm":50000});
    c.as_object_mut()
        .unwrap()
        .extend(config.as_object().unwrap().clone());
    AgentTemplate::Plugin(BotConfig {
        participant: ParticipantConfig {
            participant_id: "style-test".into(),
            kind: ParticipantKind::Strategy,
            room_id: "style".into(),
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

fn spot(second: u64, mid: i64, position: i128) -> ParticipantObservation {
    ParticipantObservation {
        version: 1,
        room_id: "style".into(),
        venue_id: "default-venue".into(),
        instrument_id: "TEST".into(),
        instrument: None,
        status: MarketStatus::Running,
        step: second,
        market_time_ms: second * 1000,
        book: BookSnapshot {
            bids: vec![BookLevel {
                price_tick: mid - 1,
                qty: 1000,
            }],
            asks: vec![BookLevel {
                price_tick: mid + 1,
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

fn perp(second: u64, mid: i64, position: i128) -> ParticipantObservation {
    let mut v = spot(second, mid, 0);
    v.own_account = Some(AccountSnapshot::Perp(
        serde_json::from_value(
            json!({"account_id":20,"cash_balance":100000,"position_qty":position,
        "avg_entry_price_tick":mid,"realized_pnl":0,"unrealized_pnl":0,"equity":100000,
        "initial_margin":position.abs()*i128::from(mid),"fees_paid":0,
        "margin_status":if position==0{"flat"}else{"healthy"}}),
        )
        .unwrap(),
    ));
    v
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

fn fills(actions: &[OrderAction]) -> i128 {
    actions
        .iter()
        .map(|a| match a {
            OrderAction::PlaceProtected { side, qty, .. } => {
                if *side == Side::Buy {
                    *qty as i128
                } else {
                    -(*qty as i128)
                }
            }
            _ => 0,
        })
        .sum()
}

fn touch_template(version: &str) -> AgentTemplate {
    let mut t = template(
        3,
        json!({"switch_ppm":0,"child_ppm":1000000,"min_trade_ppm":0,"stop_mode":"Internal"}),
    );
    if let AgentTemplate::Plugin(p) = &mut t {
        p.plugin_version = version.into();
    }
    t
}

fn prepared(t: &AgentTemplate, style: Style) -> Box<dyn ScheduledBot> {
    let mut saved = make(t).snapshot();
    if let PersistedAgentKindState::Plugin { data, .. } = &mut saved {
        let mut j = data.as_ref().clone();
        j["style"] = json!(style);
        j["fast"] = json!(110.);
        j["slow"] = json!(100.);
        j["anchor"] = json!(100.);
        j["first_ms"] = json!(0);
        j["last_ms"] = json!(0);
        j["last_mid"] = json!(100.);
        *data = Arc::new(j);
    }
    BotRegistry::with_builtins().create(t, &saved).unwrap()
}

fn child(actions: &[OrderAction]) -> Option<(Side, u64, i64)> {
    actions.iter().find_map(|a| match a {
        OrderAction::PlaceProtected {
            side,
            qty,
            price_tick,
            ..
        } => Some((*side, *qty, *price_tick)),
        _ => None,
    })
}

#[test]
fn touch_execution_keeps_the_same_target_signals_rng_and_stops() {
    let mut old = prepared(&touch_template(VERSION), Style::Momentum);
    let mut new = prepared(&touch_template(TOUCH_VERSION), Style::Momentum);
    let mut v = spot(1, 100, 0);
    v.book.asks[0].qty = 3;
    let a = old.decide(&v).unwrap();
    let b = new.decide(&v).unwrap();
    assert_eq!(child(&a), Some((Side::Buy, 100, 105)));
    assert_eq!(child(&b), Some((Side::Buy, 3, 101)));
    let mut os = serde_json::to_value(state(old.as_ref())).unwrap();
    let mut ns = serde_json::to_value(state(new.as_ref())).unwrap();
    os.as_object_mut().unwrap().remove("identity");
    ns.as_object_mut().unwrap().remove("identity");
    assert_eq!(
        os, ns,
        "the execution policy cannot change target/fitness/RNG/stop state on the same input"
    );
    assert_eq!(
        state(new.as_ref()).target,
        100,
        "submitted size does not complete the target"
    );
}

#[test]
fn touch_execution_works_remaining_exits_from_actual_position_after_restore() {
    let t = touch_template(TOUCH_VERSION);
    let mut bot = prepared(&t, Style::Out);
    let mut v = spot(1, 100, 11);
    v.book.bids[0].qty = 2;
    assert_eq!(child(&bot.decide(&v).unwrap()), Some((Side::Sell, 2, 99)));
    assert_eq!(state(bot.as_ref()).target, 0);
    let mut restored = BotRegistry::with_builtins()
        .create(&t, &bot.snapshot())
        .unwrap();
    let now = state(bot.as_ref()).next_ms / 1000 + 1;
    v = spot(now, 100, 10);
    v.book.bids[0].qty = 2;
    let a = bot.decide(&v).unwrap();
    assert_eq!(a, restored.decide(&v).unwrap());
    assert_eq!(
        child(&a),
        Some((Side::Sell, 2, 99)),
        "partial actual exit leaves a real position to work"
    );
    assert_eq!(state(bot.as_ref()).last_position, 10);
    assert_eq!(state(bot.as_ref()).target, 0);
}

#[test]
fn touch_execution_requires_external_liquidity_and_original_price_boundary() {
    let t = touch_template(TOUCH_VERSION);
    let mut bot = prepared(&t, Style::Momentum);
    let mut v = spot(1, 100, 0);
    v.book.asks[0].qty = 3;
    v.own_orders = vec![Order {
        order_id: 77,
        account_id: 20,
        side: Side::Sell,
        position_side: Default::default(),
        price_tick: 101,
        remaining_qty: 3,
        seq: 1,
    }];
    assert!(child(&bot.decide(&v).unwrap()).is_none());
    assert_eq!(state(bot.as_ref()).target, 100);
    let mut bot = prepared(&t, Style::Momentum);
    let mut v = spot(1, 100, 0);
    v.book.bids[0].price_tick = 90;
    v.book.asks[0].price_tick = 110;
    assert!(
        child(&bot.decide(&v).unwrap()).is_none(),
        "touch outside original5% boundary is not executable"
    );
}

#[test]
fn touch_style_snapshot_version_cannot_be_used_as_a_legacy_snapshot() {
    let old = touch_template(VERSION);
    let new = touch_template(TOUCH_VERSION);
    let bot = prepared(&new, Style::Momentum);
    assert!(
        BotRegistry::with_builtins()
            .create(&old, &bot.snapshot())
            .is_err()
    );
    assert!(
        BotRegistry::with_builtins()
            .create(&new, &bot.snapshot())
            .is_ok()
    );
}

#[test]
fn logit_choice_follows_scores_and_intensity() {
    assert_eq!(choose_style([0., 0., 0.], 1., 0.2), Style::Out);
    assert_eq!(choose_style([0., 0., 0.], 1., 0.5), Style::Momentum);
    assert_eq!(choose_style([0., 0., 0.], 1., 0.9), Style::Reversion);
    // High intensity concentrates on the best score.
    assert_eq!(choose_style([0., 10., 0.], 5., 0.999), Style::Momentum);
    assert_eq!(choose_style([0., -10., 3.], 5., 0.01), Style::Reversion);
    // Zero intensity ignores scores.
    assert_eq!(choose_style([0., 100., 0.], 0., 0.1), Style::Out);
}

#[test]
fn stop_levels_use_own_recent_extremes() {
    let samples: VecDeque<_> = [(0, 90.), (5000, 110.), (10000, 100.)].into();
    assert_eq!(stop_level(&samples, 10000, 20000, true, 0.), Some(90.));
    assert_eq!(stop_level(&samples, 10000, 6000, true, 0.), Some(100.));
    assert_eq!(
        stop_level(&samples, 10000, 20000, false, 0.1),
        Some(121.00000000000001)
    );
    assert_eq!(stop_level(&VecDeque::new(), 0, 1, true, 0.), None);
}

#[test]
fn trending_market_rewards_momentum_and_bots_switch_into_it() {
    let t = template(3, json!({"switch_ppm":1000000,"intensity_ppm":2000000}));
    let mut bot = make(&t);
    let mut position = 0;
    for s in 0..120u64 {
        let mid = 100 + s as i64;
        let actions = bot.decide(&spot(s, mid, position)).unwrap();
        position += fills(&actions);
        assert!(position >= 0);
    }
    let st = state(bot.as_ref());
    assert!(st.fitness_momentum > 0. && st.fitness_reversion < 0.);
    assert!(
        st.style_decisions[Style::Momentum as usize]
            > st.style_decisions[Style::Reversion as usize]
    );
    assert!(position > 0, "momentum should hold a long in an uptrend");
}

#[test]
fn perpetual_positions_carry_exchange_stops_and_stop_outs_cool_down() {
    let t = template(
        5,
        json!({"switch_ppm":1000000,"intensity_ppm":5000000,"target_leverage":3,"stop_cooldown_ms":30000}),
    );
    let mut bot = make(&t);
    let mut position = 0;
    let mut stop_seen = None;
    for s in 0..80u64 {
        let actions = bot.decide(&perp(s, 100 + s as i64, position)).unwrap();
        for a in &actions {
            if let OrderAction::SetPositionProtection {
                protection: Some(spec),
                ..
            } = a
            {
                assert_eq!(
                    spec.trigger,
                    crate::position_protection::ProtectionTrigger::Last
                );
                stop_seen = spec.stop_loss_tick;
            }
        }
        position += fills(&actions);
    }
    assert!(position > 0);
    let stop = stop_seen.expect("long perp position must carry a stop");
    assert!(stop < 180);
    // The venue stop fires: position disappears while the bot still wants it.
    // Detection happens at the bot's next own decision, never with a re-entry.
    for s in 80..100 {
        let actions = bot.decide(&perp(s, stop - 5, 0)).unwrap();
        assert_eq!(fills(&actions), 0, "no re-entry after a stop-out");
    }
    let st = state(bot.as_ref());
    assert_eq!(st.stop_outs, 1);
    assert_eq!(st.style, Style::Out);
    assert!(st.cooldown_until_ms >= 80_000 + 30_000);
}

#[test]
fn internal_stop_mode_never_sends_venue_protection_and_exits_itself() {
    let t = template(
        5,
        json!({"switch_ppm":1000000,"intensity_ppm":5000000,"target_leverage":3,"stop_mode":"Internal"}),
    );
    let mut bot = make(&t);
    let mut position = 0;
    for s in 0..80u64 {
        let actions = bot.decide(&perp(s, 100 + s as i64, position)).unwrap();
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, OrderAction::SetPositionProtection { .. }))
        );
        position += fills(&actions);
    }
    assert!(position > 0);
    let stop = state(bot.as_ref()).stop_tick.expect("internal stop level");
    for s in 80..140 {
        let actions = bot.decide(&perp(s, stop - 5, position)).unwrap();
        let f = fills(&actions);
        assert!(f <= 0, "a stopped long only sells");
        position += f;
    }
    assert_eq!(position, 0, "internal stop must flatten the whole position");
    assert_eq!(state(bot.as_ref()).style, Style::Out);
}

#[test]
fn snapshot_restore_matches_continuous_execution() {
    let t = template(
        9,
        json!({"switch_ppm":300000,"herding_ppm":5000000,"target_leverage":2}),
    );
    let registry = BotRegistry::with_builtins();
    let mut continuous = make(&t);
    let mut saved = t.initial_state();
    let mut position = 0;
    for s in 0..90u64 {
        let mid = 100 + ((s as f64 / 7.).sin() * 15.) as i64;
        let v = perp(s, mid, position);
        let expected = continuous.decide(&v).unwrap();
        let mut restored = registry.create(&t, &saved).unwrap();
        assert_eq!(restored.decide(&v).unwrap(), expected, "second {s}");
        saved = restored.snapshot();
        assert_eq!(saved, continuous.snapshot());
        position += fills(&expected);
    }
}

#[test]
fn rejects_inverted_horizons_and_foreign_state() {
    let registry = BotRegistry::with_builtins();
    let bad = template(
        1,
        json!({"fast_half_life_ms":10000,"slow_half_life_ms":10000}),
    );
    assert!(registry.create(&bad, &bad.initial_state()).is_err());
    let t = template(1, json!({}));
    let mut bot = make(&t);
    bot.decide(&spot(0, 100, 0)).unwrap();
    assert!(
        registry
            .create(&template(2, json!({})), &bot.snapshot())
            .is_err()
    );
}
