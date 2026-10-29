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
            OrderAction::PlaceProtected { side, qty, .. }
            | OrderAction::PlaceMarket { side, qty }
            | OrderAction::PlaceReduceOnlyMarket { side, qty } => {
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

#[test]
fn stop_market_exit_is_latched_through_partial_fill_empty_book_and_restore() {
    let mut source: Value = serde_json::from_str(include_str!(
        "../../scripts/fixtures/microstructure_market.json"
    ))
    .unwrap();
    let scenario = &mut source["scenario"];
    scenario["room_id"] = json!("style");
    scenario["accounts"] = json!([{"Basic":{"account_id":10,"cash_balance":1000000}},
        {"Basic":{"account_id":20,"cash_balance":1000000}}, {"Basic":{"account_id":30,"cash_balance":1000000}}]);
    scenario["seed_orders"] = json!([]);
    scenario["routed_seed_orders"] = json!([]);
    let market = &mut scenario["extra_markets"][0]["Perp"];
    market["instrument"]["instrument_id"] = json!("TEST");
    market["instrument"]["symbol"] = json!("TEST");
    market["initial_mark_price_tick"] = json!(100);
    market["price_link"] = Value::Null;
    market["funding"] = Value::Null;
    let mut rooms = RoomManager::new();
    rooms
        .create_room(serde_json::from_value(scenario.clone()).unwrap())
        .unwrap();
    let mut id = 10000;
    let submit = |rooms: &mut RoomManager, id: &mut u64, account, action| {
        let mut gateway = OrderGateway::new_scheduler(rooms, *id);
        let execution = gateway
            .submit_action(GatewayRequest {
                participant_id: "fixture".into(),
                room_id: "style".into(),
                instrument_id: Some("TEST".into()),
                account_id: account,
                action,
            })
            .unwrap();
        *id = gateway.next_order_id();
        execution
    };
    submit(
        &mut rooms,
        &mut id,
        10,
        OrderAction::PlaceLimit {
            side: Side::Sell,
            price_tick: 100,
            qty: 10,
        },
    );
    submit(
        &mut rooms,
        &mut id,
        20,
        OrderAction::PlaceMarket {
            side: Side::Buy,
            qty: 10,
        },
    );
    submit(
        &mut rooms,
        &mut id,
        30,
        OrderAction::PlaceLimit {
            side: Side::Buy,
            price_tick: 70,
            qty: 3,
        },
    );
    let stale_ask = id;
    submit(
        &mut rooms,
        &mut id,
        10,
        OrderAction::PlaceLimit {
            side: Side::Sell,
            price_tick: 90,
            qty: 10,
        },
    );
    let t = template(
        9,
        json!({"stop_mode":"Internal", "max_slippage_ppm":1, "stop_cooldown_ms":30000}),
    );
    let mut bot = make(&t);
    let mut saved = state(bot.as_ref());
    saved.stop_tick = Some(95);
    saved.last_position = 10;
    saved.target = 10;
    saved.next_ms = 999999; // A risk exit must bypass the signal clock.
    let mut persisted = t.initial_state();
    let PersistedAgentKindState::Plugin { data, .. } = &mut persisted else {
        unreachable!()
    };
    *data = Arc::new(serde_json::to_value(saved).unwrap());
    bot = BotRegistry::with_builtins().create(&t, &persisted).unwrap();
    for second in 1..=3 {
        rooms.advance_clock("style", 1).unwrap();
        if second == 3 {
            submit(
                &mut rooms,
                &mut id,
                10,
                OrderAction::Cancel {
                    order_id: stale_ask,
                },
            );
            submit(
                &mut rooms,
                &mut id,
                10,
                OrderAction::PlaceLimit {
                    side: Side::Sell,
                    price_tick: 130,
                    qty: 10,
                },
            );
            submit(
                &mut rooms,
                &mut id,
                30,
                OrderAction::PlaceLimit {
                    side: Side::Buy,
                    price_tick: 120,
                    qty: 7,
                },
            );
        }
        let observation = rooms.participant_observation("style", "TEST", 20).unwrap();
        let actions = bot.decide(&observation).unwrap();
        assert!(actions.iter().any(|a| matches!(a, OrderAction::PlaceReduceOnlyMarket { side:Side::Sell, qty } if *qty == if second==1 {10} else {7})));
        for action in actions {
            let execution = submit(&mut rooms, &mut id, 20, action.clone());
            bot.execution_feedback(&action, &execution);
        }
        assert_eq!(state(bot.as_ref()).stop_triggers, 1);
        assert!(state(bot.as_ref()).exit_pending);
        if second == 1 {
            assert_eq!(state(bot.as_ref()).exit_filled_qty, 3);
            let snapshot = bot.snapshot();
            bot = BotRegistry::with_builtins().create(&t, &snapshot).unwrap();
        }
    }
    rooms.advance_clock("style", 1).unwrap();
    assert!(
        bot.decide(&rooms.participant_observation("style", "TEST", 20).unwrap())
            .unwrap()
            .is_empty()
    );
    let s = state(bot.as_ref());
    assert!(!s.exit_pending);
    assert_eq!(s.exit_completions, 1);
    assert_eq!(s.exit_submissions, 3);
    assert_eq!(s.exit_filled_qty, 10);
    assert_eq!(s.exit_remaining_qty, 0);
    assert!(s.cooldown_until_ms >= 34000);
}
