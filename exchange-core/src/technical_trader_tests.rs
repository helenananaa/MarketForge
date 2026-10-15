use super::*;
use crate::{
    BookLevel, BookSnapshot, BotMarketData, ParticipantConfig, ParticipantKind, SpotAccount,
    TradingApi,
};

fn template(family: &str, config: Value) -> AgentTemplate {
    let mut c = json!({"family":family,"bar_interval_ms":1000,"parameters":{"fast":1,"slow":2},"position_size":5,"max_qty":2,"inventory_cap":10});
    c.as_object_mut()
        .unwrap()
        .extend(config.as_object().unwrap().clone());
    AgentTemplate::Plugin(BotConfig {
        participant: ParticipantConfig {
            participant_id: "technical-test".into(),
            kind: ParticipantKind::Strategy,
            room_id: "technical".into(),
            account_id: 20,
            instrument_id: Some("TEST".into()),
        },
        plugin_id: ID.into(),
        plugin_version: VERSION.into(),
        state_version: 1,
        config_version: 1,
        seed: 7,
        config: c,
    })
}
fn candle(i: usize, c: i64) -> Candle {
    Candle {
        schema_version: 1,
        open_time_ms: i as u64 * 1000,
        close_time_ms: (i + 1) as u64 * 1000,
        open_tick: c,
        high_tick: c + 2,
        low_tick: c - 2,
        close_tick: c,
        volume: 10 + i as u64,
        quote_volume: i128::from(c) * (10 + i as i128),
        trades: 1,
        taker_buy_base: None,
        taker_buy_quote: None,
        is_final: true,
    }
}
fn observation(prices: &[i64]) -> ParticipantObservation {
    ParticipantObservation {
        version: 1,
        room_id: "technical".into(),
        venue_id: "default-venue".into(),
        instrument_id: "TEST".into(),
        instrument: None,
        status: MarketStatus::Running,
        step: prices.len() as u64,
        market_time_ms: prices.len() as u64 * 1000,
        book: BookSnapshot {
            bids: vec![BookLevel {
                price_tick: 99,
                qty: 100,
            }],
            asks: vec![BookLevel {
                price_tick: 101,
                qty: 100,
            }],
        },
        public_trades: vec![],
        own_orders: vec![],
        related_markets: vec![],
        market_events: vec![],
        own_account: Some(AccountSnapshot::Spot(
            SpotAccount {
                account_id: 20,
                cash_balance: 10000,
                position_qty: 0,
                reserved_cash: 0,
                reserved_position: 0,
                fees_paid: 0,
            }
            .snapshot(),
        )),
        bot_market_data: Some(BotMarketData {
            interval_ms: 1000,
            external_volume_qty: "0".into(),
            candles: prices
                .iter()
                .enumerate()
                .map(|(i, c)| candle(i, *c))
                .collect(),
            history_proof: None,
            private_history_complete: true,
            own_fills: vec![],
            fill_details: vec![],
            truncated: false,
            streams: vec![],
            account_events: vec![],
            conditional_orders: vec![],
        }),
        position_protections: vec![],
        risk: None,
        perp_price: None,
    }
}
fn perpetual(v: &mut ParticipantObservation, position: i128, entry: i64) {
    v.own_account=Some(AccountSnapshot::Perp(serde_json::from_value(json!({"account_id":20,"cash_balance":10000,"position_qty":position,
        "avg_entry_price_tick":entry,"realized_pnl":0,"unrealized_pnl":0,"equity":10000,"initial_margin":position.abs()*100,"fees_paid":0,"margin_status":if position==0{"flat"}else{"healthy"}})).unwrap()));
}
fn make(t: &AgentTemplate) -> Box<dyn ScheduledBot> {
    BotRegistry::with_builtins()
        .create(t, &t.initial_state())
        .unwrap()
}
fn state(b: &dyn ScheduledBot) -> State {
    let PersistedAgentKindState::Plugin { data, .. } = b.snapshot() else {
        panic!()
    };
    serde_json::from_value(data.as_ref().clone()).unwrap()
}

#[test]
fn spot_signal_changes_with_periods_and_cannot_create_short_inventory() {
    let t = template("EmaCross", json!({}));
    let mut b = make(&t);
    let view = observation(&[100, 90]);
    assert!(b.decide(&view).unwrap().is_empty());
    assert!(state(&*b).frame.short_entry);
    assert_eq!(state(&*b).target, 0);
    let view = observation(&[100, 90, 120]);
    assert!(matches!(
        b.decide(&view).unwrap()[0],
        OrderAction::PlaceImmediateOrCancel {
            side: Side::Buy,
            qty: 2,
            ..
        }
    ));
    assert!(b.decide(&view).unwrap().is_empty());
    let slow = template("SmaCross", json!({"parameters":{"fast":3,"slow":8}}));
    let mut slow = make(&slow);
    assert!(slow.decide(&view).unwrap().is_empty());
    assert!(!state(&*slow).frame.ready);
}

#[test]
fn sustained_signal_delay_executes_instead_of_resetting_on_each_bar() {
    let t = template("EmaCross", json!({"signal_delay_ms":3000}));
    let mut bot = make(&t);
    for n in 2..5 {
        let prices: Vec<_> = (0..n).map(|i| 100 + i as i64 * 2).collect();
        assert!(bot.decide(&observation(&prices)).unwrap().is_empty());
    }
    assert!(matches!(
        bot.decide(&observation(&[100, 102, 104, 106, 108]))
            .unwrap()[0],
        OrderAction::PlaceImmediateOrCancel {
            side: Side::Buy,
            ..
        }
    ));
}

#[test]
fn unrelated_parameters_are_rejected_instead_of_silently_ignored() {
    let t = template("EmaCross", json!({"parameters":{"length":30}}));
    assert!(
        BotRegistry::with_builtins()
            .create(&t, &t.initial_state())
            .is_err()
    );
}

#[test]
fn perpetual_reversal_waits_for_actual_partial_close_before_opening_other_side() {
    let t = template("EmaCross", json!({}));
    let mut b = make(&t);
    let mut v = observation(&[100, 90]);
    perpetual(&mut v, 0, 0);
    assert!(matches!(
        b.decide(&v).unwrap()[0],
        OrderAction::PlaceImmediateOrCancel {
            side: Side::Sell,
            qty: 2,
            ..
        }
    ));
    // A rejected/unfilled sell leaves the account flat: no imagined short.
    v.market_time_ms += 1000;
    assert!(matches!(
        b.decide(&v).unwrap()[0],
        OrderAction::PlaceImmediateOrCancel {
            side: Side::Sell,
            qty: 2,
            ..
        }
    ));
    let mut v = observation(&[100, 90, 120, 130]);
    perpetual(&mut v, -3, 99);
    assert!(matches!(
        b.decide(&v).unwrap()[0],
        OrderAction::PlaceReduceOnlyImmediateOrCancel {
            side: Side::Buy,
            qty: 2,
            ..
        }
    ));
    v.market_time_ms += 1000;
    perpetual(&mut v, -1, 99);
    assert!(matches!(
        b.decide(&v).unwrap()[0],
        OrderAction::PlaceReduceOnlyImmediateOrCancel {
            side: Side::Buy,
            qty: 1,
            ..
        }
    ));
    v.market_time_ms += 1000;
    perpetual(&mut v, 0, 0);
    assert!(matches!(
        b.decide(&v).unwrap()[0],
        OrderAction::PlaceImmediateOrCancel {
            side: Side::Buy,
            qty: 2,
            ..
        }
    ));
}

#[test]
fn intrabar_stop_is_latched_until_flat_then_cools_down() {
    let t = template(
        "EmaCross",
        json!({"stop_loss_ppm":50000,"cooldown_ms":5000}),
    );
    let mut b = make(&t);
    let mut v = observation(&[100, 110]);
    perpetual(&mut v, 3, 100);
    v.book.bids[0].price_tick = 89;
    v.book.asks[0].price_tick = 91;
    assert!(matches!(
        b.decide(&v).unwrap()[0],
        OrderAction::PlaceReduceOnlyImmediateOrCancel {
            side: Side::Sell,
            qty: 2,
            ..
        }
    ));
    v.market_time_ms += 1000;
    perpetual(&mut v, 1, 100);
    assert!(matches!(
        b.decide(&v).unwrap()[0],
        OrderAction::PlaceReduceOnlyImmediateOrCancel { qty: 1, .. }
    ));
    v.market_time_ms += 1000;
    perpetual(&mut v, 0, 0);
    assert!(b.decide(&v).unwrap().is_empty());
    assert_eq!(state(&*b).exit_reason.as_deref(), Some("stop_loss"));
    assert!(state(&*b).cooldown_until_ms > v.market_time_ms);
}

#[test]
fn future_or_changed_candles_and_changed_templates_leave_state_unchanged() {
    let t = template("EmaCross", json!({}));
    let mut b = make(&t);
    let v = observation(&[100, 110]);
    b.decide(&v).unwrap();
    let before = b.snapshot();
    let mut bad = v.clone();
    bad.bot_market_data.as_mut().unwrap().candles[0].high_tick += 1;
    assert!(b.decide(&bad).is_err());
    assert_eq!(b.snapshot(), before);
    let mut future = v.clone();
    future.market_time_ms = 1000;
    assert!(b.decide(&future).is_err());
    assert_eq!(b.snapshot(), before);
    let changed = template("EmaCross", json!({"parameters":{"fast":2,"slow":4}}));
    assert!(
        BotRegistry::with_builtins()
            .create(&changed, &before)
            .is_err()
    );
}

#[test]
fn all_families_restore_from_real_json_and_keep_prefix_causality() {
    for family in signals::FAMILIES {
        let p = signals::Params::defaults(family);
        let mut live = signals::Signals::default();
        let mut restored = signals::Signals::default();
        let mut entries = 0;
        for i in 0..320 {
            let close = 10000
                + ((i as f64 / 9.).sin() * 1200.) as i64
                + if (i / 80) % 2 == 0 {
                    i as i64 * 5
                } else {
                    -(i as i64) * 5
                };
            let mut bar = candle(i, close);
            bar.high_tick += 80;
            bar.low_tick -= 80;
            let a = live.update(family, &p, &bar);
            let b = restored.update(family, &p, &bar);
            assert_eq!(a, b, "{family} bar {i}");
            entries += usize::from(a.long_entry || a.short_entry);
            restored = serde_json::from_slice(&serde_json::to_vec(&restored).unwrap()).unwrap();
            assert!(restored.valid());
        }
        // A family may legitimately be inactive in this particular regime.
        assert!(live.valid(), "{family} {entries}");
    }
}

#[test]
fn channels_exclude_current_bar_and_parameters_reject_inadequate_history() {
    let mut s = signals::Signals::default();
    let mut p = signals::Params::defaults("Donchian");
    p.length = 2;
    p.exit_length = 2;
    s.update("Donchian", &p, &candle(0, 100));
    s.update("Donchian", &p, &candle(1, 101));
    let out = s.update("Donchian", &p, &candle(2, 110));
    assert!(out.long_entry);
    assert_eq!(out.plots["upper"], Some(103.));
    let t = template("Turtle", json!({"history_limit":16,"parameters":{}}));
    assert!(
        BotRegistry::with_builtins()
            .create(&t, &t.initial_state())
            .is_err()
    );
}

#[test]
fn signals_execute_on_real_perpetual_gateway_and_restore_account_feedback() {
    use crate::{GatewayRequest, OrderGateway, RoomManager, ScenarioConfig};
    let scenario:ScenarioConfig=serde_json::from_value(json!({"room_id":"technical","market":{"Perp":{"instrument":{"symbol":"TEST","base_asset":"T","quote_asset":"USD","tick_size":1,"lot_size":1},"clearing":{"leverage":2,"maker_fee_ppm":100,"taker_fee_ppm":300},"risk":{},"initial_mark_price_tick":100}},
        "accounts":[{"Basic":{"account_id":10,"cash_balance":1000000}},{"Basic":{"account_id":20,"cash_balance":10000}}],"seed_orders":[
            {"NewOrder":{"order_id":1,"account_id":10,"side":"Buy","kind":{"Limit":{"price_tick":99}},"qty":100}},
            {"NewOrder":{"order_id":2,"account_id":10,"side":"Sell","kind":{"Limit":{"price_tick":101}},"qty":100}}]})).unwrap();
    let mut rooms = RoomManager::new();
    rooms.create_room(scenario).unwrap();
    let t = template("EmaCross", json!({}));
    let registry = BotRegistry::with_builtins();
    let mut b = make(&t);
    let mut id = 100;
    for (prices, want, reduce) in [
        (&[100, 90][..], -2, false),
        (&[100, 90, 120, 130][..], 0, true),
        (&[100, 90, 120, 130][..], 2, false),
    ] {
        rooms.advance_clock("technical", 1).unwrap();
        let mut v = rooms
            .bot_observation("technical", "TEST", 20, None)
            .unwrap();
        let mut data = observation(prices).bot_market_data.unwrap();
        for bar in &mut data.candles {
            bar.open_time_ms += 10000;
            bar.close_time_ms += 10000;
        }
        v.market_time_ms = 20000 + id;
        v.bot_market_data = Some(data);
        let actions = b.decide(&v).unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(
            matches!(
                actions[0],
                OrderAction::PlaceReduceOnlyImmediateOrCancel { .. }
            ),
            reduce
        );
        let mut gateway = OrderGateway::new_scheduler(&mut rooms, id);
        gateway
            .submit_action(GatewayRequest {
                participant_id: "technical-test".into(),
                room_id: "technical".into(),
                instrument_id: Some("TEST".into()),
                account_id: 20,
                action: actions[0].clone(),
            })
            .unwrap();
        id = gateway.next_order_id();
        let AccountSnapshot::Perp(a) = rooms
            .account_snapshot_for("technical", "TEST", 20)
            .unwrap()
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(a.position_qty, want);
        let saved: PersistedAgentKindState =
            serde_json::from_slice(&serde_json::to_vec(&b.snapshot()).unwrap()).unwrap();
        b = registry.create(&t, &saved).unwrap();
    }
}
