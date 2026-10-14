use super::*;
use crate::{
    BookLevel, BookSnapshot, ParticipantConfig, ParticipantKind, SpotAccount, Trade,
    model::PositionSide, observation::BotFillDetail,
};
use std::sync::Arc;

fn template(id: &str, config: serde_json::Value) -> AgentTemplate {
    let mut params = json!({"bar_interval_ms":1000});
    params
        .as_object_mut()
        .unwrap()
        .extend(config.as_object().unwrap().clone());
    AgentTemplate::Plugin(BotConfig {
        participant: ParticipantConfig {
            participant_id: "native-pine-test".into(),
            kind: ParticipantKind::Strategy,
            room_id: "test".into(),
            account_id: 20,
            instrument_id: Some("V-BTC-SPOT".into()),
        },
        plugin_id: id.into(),
        plugin_version: VERSION.into(),
        state_version: SCHEMA,
        config_version: 1,
        seed: 7,
        config: params,
    })
}

fn candle(index: usize, close: i64) -> crate::Candle {
    crate::Candle {
        schema_version: 1,
        open_time_ms: index as u64 * 1000,
        close_time_ms: (index as u64 + 1) * 1000,
        open_tick: close,
        high_tick: close + 1,
        low_tick: close - 1,
        close_tick: close,
        volume: 10,
        quote_volume: i128::from(close) * 10,
        trades: 1,
        taker_buy_base: Some(10),
        taker_buy_quote: Some(i128::from(close) * 10),
        is_final: true,
    }
}

fn observation(prices: &[i64], position: i128) -> ParticipantObservation {
    ParticipantObservation {
        version: 1,
        room_id: "test".into(),
        venue_id: "default-venue".into(),
        instrument_id: "V-BTC-SPOT".into(),
        instrument: None,
        status: crate::MarketStatus::Running,
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
                cash_balance: 10_000,
                position_qty: position,
                reserved_cash: 0,
                reserved_position: 0,
                fees_paid: 0,
            }
            .snapshot(),
        )),
        bot_market_data: Some(BotMarketData {
            interval_ms: 1000,
            history_proof: None,
            private_history_complete: true,
            external_volume_qty: "0".into(),
            candles: prices
                .iter()
                .enumerate()
                .map(|(i, p)| candle(i, *p))
                .collect(),
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

fn state(bot: &dyn ScheduledBot) -> State {
    let PersistedAgentKindState::Plugin { data, .. } = bot.snapshot() else {
        panic!()
    };
    serde_json::from_value(data.as_ref().clone()).unwrap()
}

fn ack(
    registry: &BotRegistry,
    template: &AgentTemplate,
    bot: &dyn ScheduledBot,
    action: &OrderAction,
    accepted: bool,
    order_id: u64,
    now: u64,
) -> Box<dyn ScheduledBot> {
    let OrderAction::BotOrder {
        key,
        instrument_id,
        action: inner,
    } = action
    else {
        panic!()
    };
    let mut saved = bot.snapshot();
    let PersistedAgentKindState::Plugin { data, .. } = &mut saved else {
        panic!()
    };
    Arc::make_mut(data).as_object_mut().unwrap().insert("_host_receipts".into(), json!([
        {"key":key,"instrument_id":instrument_id,"command_seq":order_id,"market_time_ms":now,
            "order_id":order_id,"action":inner,"accepted":accepted,"error":if accepted { None::<String> } else { Some("RiskRejected".into()) }}
    ]));
    registry.create(template, &saved).unwrap()
}

fn append_fill(
    view: &mut ParticipantObservation,
    trade_id: u64,
    order_id: u64,
    qty: u64,
    side: Side,
) {
    let data = view.bot_market_data.as_mut().unwrap();
    data.own_fills.push(Trade {
        trade_id,
        maker_order_id: 500 + trade_id,
        maker_account_id: 10,
        taker_order_id: order_id,
        taker_account_id: 20,
        price_tick: 100,
        qty,
        taker_side: side,
        maker_position_side: PositionSide::Both,
        taker_position_side: PositionSide::Both,
    });
    data.fill_details.push(BotFillDetail {
        trade_id,
        market_time_ms: view.market_time_ms,
        fee_paid: Some(1),
    });
    let Some(AccountSnapshot::Spot(account)) = &mut view.own_account else {
        panic!()
    };
    let signed = i128::from(qty) * if side == Side::Buy { 1 } else { -1 };
    account.position_qty += signed;
    account.available_position += signed;
    account.cash_balance -= signed * 100 + 1;
    account.available_cash = account.cash_balance;
    account.fees_paid += 1;
}

fn inner(action: &OrderAction) -> &OrderAction {
    match action {
        OrderAction::BotOrder { action, .. } => action,
        _ => panic!(),
    }
}

#[test]
fn extended_catalog_defaults_and_real_json_resume_are_explicit() {
    let registry = BotRegistry::with_builtins();
    for kind in extended::KINDS {
        let id = kind.id();
        let descriptor = registry
            .descriptors()
            .into_iter()
            .find(|d| d.id == id)
            .unwrap();
        assert_eq!(
            descriptor.validate_config(&json!({})).unwrap()["qty"],
            json!(1)
        );
        assert!(descriptor.validate_config(&json!({"length": 7})).is_err());
        let t = template(id, json!({}));
        assert!(registry.supports_instance_reuse(&t));
        assert!(registry.supports_native_batching(&t));
        let mut bot = registry.create(&t, &t.initial_state()).unwrap();
        // No orders: still exercise all warmups and true durable JSON bytes.
        for count in 1..=96 {
            let prices = (0..count)
                .map(|i| 100 + ((i * 17) % 31) as i64)
                .collect::<Vec<_>>();
            let mut view = observation(&prices, 0);
            if let Some(AccountSnapshot::Spot(account)) = &mut view.own_account {
                account.cash_balance = 0;
                account.available_cash = 0;
            }
            assert!(bot.decide(&view).unwrap().is_empty(), "{id}/{count}");
            let saved = bot.snapshot();
            let json_bytes = serde_json::to_vec(&saved).unwrap();
            let restored = serde_json::from_slice(&json_bytes).unwrap();
            bot = registry
                .create(&t, &restored)
                .unwrap_or_else(|e| panic!("{id}/{count}: {e}"));
        }
        let saved = bot.snapshot();
        for missing in [false, true] {
            let mut corrupt = saved.clone();
            let PersistedAgentKindState::Plugin { data, .. } = &mut corrupt else {
                panic!()
            };
            let object = Arc::make_mut(data).as_object_mut().unwrap();
            if missing {
                object["indicators"]
                    .as_object_mut()
                    .unwrap()
                    .remove("extended");
            } else {
                object["indicators"]["extended"]["plots"] = json!({"wrong": 0});
            }
            assert!(
                registry.create(&t, &corrupt).is_err(),
                "{id} malformed plots"
            );
        }
    }
}

#[test]
fn defaults_catalog_and_config_identity_are_explicit() {
    let registry = BotRegistry::with_builtins();
    for id in ["NativePineMa", "NativePineRsi", "NativePineBreakout"] {
        let descriptor = registry
            .descriptors()
            .into_iter()
            .find(|d| d.id == id)
            .unwrap();
        assert_eq!(descriptor.version, "1.0.0");
        let defaults = descriptor.validate_config(&json!({})).unwrap();
        for (key, value) in [
            ("bar_interval_ms", 10_000),
            ("history_limit", 512),
            ("max_qty", 2),
            ("inventory_cap", 10),
            ("slippage_ticks", 2),
            ("fee_buffer_ppm", 1000),
            ("qty", 2),
        ] {
            assert_eq!(defaults[key], json!(value));
        }
        let t = template(id, json!({}));
        assert!(registry.supports_native_batching(&t));
        assert!(registry.supports_instance_reuse(&t));
        assert!(!registry.account_events(&t));
        let mut bot = registry.create(&t, &t.initial_state()).unwrap();
        let mut view = observation(&[100, 99], 0);
        bot.decide(&view).unwrap();
        let saved = bot.snapshot();
        let AgentTemplate::Plugin(mut changed) = t.clone() else {
            panic!()
        };
        changed.config["qty"] = json!(3);
        assert!(
            registry
                .create(&AgentTemplate::Plugin(changed), &saved)
                .is_err()
        );
        view.instrument_id = "other".into();
        assert!(bot.decide(&view).is_err());
    }
    assert!(
        registry
            .validate_template(&template("NativePineMa", json!({"script":"ma.pine"})))
            .is_err()
    );
    assert!(
        registry
            .validate_template(&template("NativePineMa", json!({"bar_interval_ms":1001})))
            .is_err()
    );
}

#[test]
fn rsi_wilder_warmup_and_flat_up_down_edges_match_pine() {
    let registry = BotRegistry::with_builtins();
    for (prices, expected) in [
        (vec![100; 9], 100.0),
        ((100..109).collect(), 100.0),
        ((100..109).rev().collect(), 0.0),
    ] {
        let t = template("NativePineRsi", json!({"initial_avg_price_tick":100}));
        let mut bot = registry.create(&t, &t.initial_state()).unwrap();
        for count in 1..=prices.len() {
            assert!(
                bot.decide(&observation(&prices[..count], 1))
                    .unwrap()
                    .is_empty()
            );
            let snapshot = state(&*bot);
            assert_eq!(
                snapshot.indicators.rsi,
                if count < 8 { None } else { Some(expected) }
            );
            assert_eq!(snapshot.evaluated_bars, count as u64);
        }
    }
}

#[test]
fn moving_average_is_a_level_condition_and_rejected_entries_retry_next_bar() {
    let registry = BotRegistry::with_builtins();
    let t = template("NativePineMa", json!({"fast":1,"slow":2}));
    let mut bot = registry.create(&t, &t.initial_state()).unwrap();
    assert!(bot.decide(&observation(&[100], 0)).unwrap().is_empty());
    assert_eq!(state(&*bot).indicators.fast_ma, Some(100.0));
    assert_eq!(state(&*bot).indicators.slow_ma, None);
    let actions = bot.decide(&observation(&[100, 110], 0)).unwrap();
    assert!(matches!(
        inner(&actions[0]),
        OrderAction::PlaceProtected {
            side: Side::Buy,
            qty: 2,
            price_tick: 103,
            ..
        }
    ));
    let pending = bot.snapshot();
    assert!(bot.decide(&observation(&[100, 110, 120], 0)).is_err());
    assert_eq!(pending, bot.snapshot());
    bot = ack(&registry, &t, &*bot, &actions[0], false, 50, 2000);
    // The fast MA remains above the slow MA; no crossover occurs here.
    let retried = bot.decide(&observation(&[100, 110, 120], 0)).unwrap();
    assert_eq!(inner(&retried[0]), inner(&actions[0]));
    assert_eq!(state(&*bot).entry_qty, 0);
}

#[test]
fn breakout_uses_previous_complete_high_low_window() {
    let registry = BotRegistry::with_builtins();
    let t = template("NativePineBreakout", json!({"length":2}));
    let mut bot = registry.create(&t, &t.initial_state()).unwrap();
    let mut view = observation(&[100, 100, 110], 0);
    view.bot_market_data.as_mut().unwrap().candles[1].high_tick = 120;
    view.bot_market_data.as_mut().unwrap().candles[1].low_tick = 80;
    assert!(bot.decide(&view).unwrap().is_empty());
    assert_eq!(state(&*bot).indicators.upper, Some(120.0));
    assert_eq!(state(&*bot).indicators.lower, Some(80.0));
    view.bot_market_data
        .as_mut()
        .unwrap()
        .candles
        .push(candle(3, 121));
    view.bot_market_data.as_mut().unwrap().candles[3].high_tick = 300;
    view.market_time_ms = 4000;
    let actions = bot.decide(&view).unwrap();
    assert_eq!(state(&*bot).indicators.upper, Some(120.0));
    assert!(matches!(
        inner(&actions[0]),
        OrderAction::PlaceProtected {
            side: Side::Buy,
            ..
        }
    ));
}

#[test]
fn actual_partial_fills_determine_holdings_and_close_retries_residual_next_bar() {
    let registry = BotRegistry::with_builtins();
    let t = template("NativePineMa", json!({"fast":1,"slow":2}));
    let mut bot = registry.create(&t, &t.initial_state()).unwrap();
    let enter = bot.decide(&observation(&[100, 110], 0)).unwrap().remove(0);
    bot = ack(&registry, &t, &*bot, &enter, true, 50, 2000);
    let mut filled = observation(&[100, 110], 0);
    append_fill(&mut filled, 0, 50, 1, Side::Buy);
    assert!(bot.decide(&filled).unwrap().is_empty());
    assert_eq!(state(&*bot).entry_qty, 1);
    assert_eq!(state(&*bot).observed_position, 1);
    filled
        .bot_market_data
        .as_mut()
        .unwrap()
        .candles
        .push(candle(2, 90));
    filled.market_time_ms = 3000;
    let exit = bot.decide(&filled).unwrap().remove(0);
    assert!(matches!(
        inner(&exit),
        OrderAction::PlaceProtected {
            side: Side::Sell,
            qty: 1,
            ..
        }
    ));
    // Accepted IOC with no fill leaves a real open position. The next bearish
    // bar closes again; the submitted intention never sets position to zero.
    bot = ack(&registry, &t, &*bot, &exit, true, 51, 3000);
    filled
        .bot_market_data
        .as_mut()
        .unwrap()
        .candles
        .push(candle(3, 80));
    filled.market_time_ms = 4000;
    let retry = bot.decide(&filled).unwrap().remove(0);
    assert!(matches!(
        inner(&retry),
        OrderAction::PlaceProtected {
            side: Side::Sell,
            qty: 1,
            ..
        }
    ));
    bot = ack(&registry, &t, &*bot, &retry, true, 52, 4000);
    append_fill(&mut filled, 1, 52, 1, Side::Sell);
    assert!(bot.decide(&filled).unwrap().is_empty());
    assert_eq!(state(&*bot).entry_qty, 0);
    assert_eq!(state(&*bot).observed_position, 0);
}

#[test]
fn initial_inventory_is_visible_but_not_owned_by_entry_id() {
    let registry = BotRegistry::with_builtins();
    let t = template(
        "NativePineMa",
        json!({"fast":1,"slow":2,"initial_avg_price_tick":100}),
    );
    let mut bot = registry.create(&t, &t.initial_state()).unwrap();
    assert!(bot.decide(&observation(&[100, 110], 5)).unwrap().is_empty());
    assert!(
        bot.decide(&observation(&[100, 110, 90], 5))
            .unwrap()
            .is_empty()
    );
    assert_eq!(state(&*bot).last_intent.as_deref(), Some("close"));
    assert_eq!(state(&*bot).entry_qty, 0);
    assert_eq!(state(&*bot).opening_position, 5);
    let fresh = template("NativePineMa", json!({}));
    assert!(
        registry
            .create(&fresh, &fresh.initial_state())
            .unwrap()
            .decide(&observation(&[100], 5))
            .is_err()
    );
}

#[test]
fn snapshots_reuse_shared_delivery_and_rolling_history_match_complete_replay() {
    let registry = BotRegistry::with_builtins();
    for id in ["NativePineMa", "NativePineRsi", "NativePineBreakout"] {
        let t = template(id, json!({"initial_avg_price_tick":100,"history_limit":16}));
        let mut hot = registry.create(&t, &t.initial_state()).unwrap();
        let mut complete = registry.create(&t, &t.initial_state()).unwrap();
        let mut saved = t.initial_state();
        let mut prices = vec![];
        for index in 0..130 {
            prices.push(100 + ((index * 17) % 41) as i64);
            let mut view = observation(&prices, 1);
            complete.decide(&view).unwrap();
            if prices.len() > 17 {
                let data = view.bot_market_data.as_mut().unwrap();
                data.candles.drain(..data.candles.len() - 17);
                data.truncated = true;
            }
            // A durable restart crosses JSON bytes, including Wilder's
            // fractional averages; a Value clone is insufficient evidence.
            let encoded = serde_json::to_vec(&saved).unwrap();
            let decoded: PersistedAgentKindState = serde_json::from_slice(&encoded).unwrap();
            assert_eq!(decoded, saved, "{id} durable JSON state changed");
            let mut restored = registry.create(&t, &decoded).unwrap();
            let restored_actions = restored.decide(&view).unwrap();
            hot.prepare_reused_call(&saved).unwrap();
            assert_eq!(hot.decide(&view).unwrap(), restored_actions);
            let mut public = view.clone();
            let shared = SharedParticipantObservation {
                own_account: public.own_account.take(),
                own_orders: std::mem::take(&mut public.own_orders),
                bot_market_data: public.bot_market_data.take(),
                public: Arc::new(public),
                related_markets: vec![],
                position_protections: vec![],
                risk: None,
            };
            let mut borrowed = registry.create(&t, &saved).unwrap();
            assert_eq!(borrowed.decide_shared(&shared).unwrap(), restored_actions);
            saved = restored.snapshot();
            assert_eq!(hot.snapshot(), saved);
            assert_eq!(borrowed.snapshot(), saved);
            assert_eq!(state(&*hot).indicators, state(&*complete).indicators);
            assert_eq!(
                state(&*hot).indicator_state,
                state(&*complete).indicator_state
            );
            assert_eq!(state(&*hot).evaluated_bars, prices.len() as u64);
        }
        assert_eq!(state(&*hot).recent_bars.len(), 64);
        assert!(serde_json::to_vec(&hot.snapshot()).unwrap().len() < 12_000);
        let mut corrected = observation(&prices, 1);
        corrected
            .bot_market_data
            .as_mut()
            .unwrap()
            .candles
            .last_mut()
            .unwrap()
            .volume += 1;
        let before = hot.snapshot();
        assert!(hot.decide(&corrected).is_err());
        assert_eq!(before, hot.snapshot());
        let mut no_cursor = observation(&prices, 1);
        no_cursor.bot_market_data.as_mut().unwrap().truncated = true;
        no_cursor.bot_market_data.as_mut().unwrap().candles.clear();
        assert!(hot.decide(&no_cursor).is_err());
    }
}

#[test]
fn cold_warmup_and_missed_bars_only_submit_latest_intent() {
    let registry = BotRegistry::with_builtins();
    let t = template("NativePineMa", json!({"fast":1,"slow":2}));
    let mut bot = registry.create(&t, &t.initial_state()).unwrap();
    // Earlier historical bars signal entries but the latest bar is bearish.
    assert!(
        bot.decide(&observation(&[100, 110, 120, 90], 0))
            .unwrap()
            .is_empty()
    );
    assert_eq!(state(&*bot).evaluated_bars, 4);
    assert_eq!(state(&*bot).skipped_bar_decisions, 0);
    assert!(
        bot.decide(&observation(&[100, 110, 120, 90, 100, 110, 80], 0))
            .unwrap()
            .is_empty()
    );
    assert_eq!(state(&*bot).skipped_bar_decisions, 2);
    assert!(
        bot.decide(&observation(&[100, 110, 120, 90, 100, 110, 80], 0))
            .unwrap()
            .is_empty()
    );
    assert_eq!(state(&*bot).evaluated_bars, 7);
}

#[test]
fn forming_future_corrected_and_incomplete_inputs_fail_without_state_mutation() {
    let registry = BotRegistry::with_builtins();
    let t = template("NativePineMa", json!({}));
    for mutation in 0..4 {
        let mut bot = registry.create(&t, &t.initial_state()).unwrap();
        let before = bot.snapshot();
        let mut view = observation(&[100], 0);
        match mutation {
            0 => view.bot_market_data.as_mut().unwrap().candles[0].is_final = false,
            1 => view.market_time_ms = 999,
            2 => view.bot_market_data.as_mut().unwrap().truncated = true,
            3 => view.bot_market_data.as_mut().unwrap().candles[0].close_time_ms = 2000,
            _ => unreachable!(),
        }
        assert!(bot.decide(&view).is_err());
        assert_eq!(before, bot.snapshot());
    }
    let mut bot = registry.create(&t, &t.initial_state()).unwrap();
    bot.decide(&observation(&[100, 99, 98], 0)).unwrap();
    let before = bot.snapshot();
    assert!(bot.decide(&observation(&[100, 97, 98], 0)).is_err());
    assert_eq!(before, bot.snapshot());
    assert!(bot.decide(&observation(&[100, 99], 0)).is_err());
    let mut unreported = observation(&[100, 99, 98], 1);
    assert!(bot.decide(&unreported).is_err());
    let mut corrected = before;
    let PersistedAgentKindState::Plugin { data, .. } = &mut corrected else {
        panic!()
    };
    Arc::make_mut(data)["schema"] = json!(2);
    assert!(registry.create(&t, &corrected).is_err());
    unreported.own_account = None;
    assert!(bot.decide(&unreported).is_err());
}

#[test]
fn fill_prefix_corrections_and_missing_host_receipts_are_refused() {
    let registry = BotRegistry::with_builtins();
    let t = template("NativePineMa", json!({"fast":1,"slow":2}));
    let mut bot = registry.create(&t, &t.initial_state()).unwrap();
    let action = bot.decide(&observation(&[100, 110], 0)).unwrap().remove(0);
    bot = ack(&registry, &t, &*bot, &action, true, 50, 2000);
    let mut filled = observation(&[100, 110], 0);
    append_fill(&mut filled, 0, 50, 1, Side::Buy);
    bot.decide(&filled).unwrap();
    let before = bot.snapshot();
    filled.bot_market_data.as_mut().unwrap().fill_details[0].fee_paid = Some(2);
    assert!(bot.decide(&filled).is_err());
    assert_eq!(before, bot.snapshot());
    filled.bot_market_data.as_mut().unwrap().own_fills.clear();
    filled
        .bot_market_data
        .as_mut()
        .unwrap()
        .fill_details
        .clear();
    assert!(bot.decide(&filled).is_err());
    assert_eq!(before, bot.snapshot());
}

#[test]
fn unrelated_resting_orders_precision_and_clock_are_accounted_for() {
    let registry = BotRegistry::with_builtins();
    let t = template(
        "NativePineMa",
        json!({"fast":1,"slow":2,"qty":10,"max_qty":10}),
    );
    let mut bot = registry.create(&t, &t.initial_state()).unwrap();
    let mut view = observation(&[100, 110], 0);
    view.own_orders.push(crate::Order {
        order_id: 99,
        account_id: 20,
        side: Side::Buy,
        position_side: PositionSide::Both,
        price_tick: 90,
        remaining_qty: 3,
        seq: 0,
    });
    let Some(AccountSnapshot::Spot(account)) = &mut view.own_account else {
        panic!()
    };
    account.reserved_cash = 270;
    account.available_cash -= 270;
    let action = bot.decide(&view).unwrap().remove(0);
    assert!(matches!(
        inner(&action),
        OrderAction::PlaceProtected { qty: 7, .. }
    ));
    assert_eq!(state(&*bot).last_observation, Some((2, 2000)));
    let before = bot.snapshot();
    view.step = 1;
    assert!(bot.decide(&view).is_err());
    assert_eq!(before, bot.snapshot());
    view.step = 2;
    view.instrument = Some(crate::InstrumentConfig::new("V-BTC-SPOT", 2, 1).unwrap());
    assert!(bot.decide(&view).is_err());
    assert_eq!(before, bot.snapshot());
    let lot_template = template("NativePineMa", json!({"fast":1,"slow":2,"qty":3}));
    let mut lot_bot = registry
        .create(&lot_template, &lot_template.initial_state())
        .unwrap();
    let mut lot_view = observation(&[100, 110], 0);
    lot_view.instrument = Some(crate::InstrumentConfig::new("V-BTC-SPOT", 1, 2).unwrap());
    assert!(lot_bot.decide(&lot_view).is_err());
}

#[test]
fn host_fill_completeness_boundary_accepts_4095_and_refuses_4096() {
    let registry = BotRegistry::with_builtins();
    let t = template("NativePineMa", json!({}));
    let mut bot = registry.create(&t, &t.initial_state()).unwrap();
    let mut view = observation(&[100], 0);
    for id in 0..4095 {
        append_fill(
            &mut view,
            id,
            50 + id,
            1,
            if id % 2 == 0 { Side::Buy } else { Side::Sell },
        );
    }
    assert!(bot.decide(&view).unwrap().is_empty());
    assert_eq!(state(&*bot).fill_count, 4095);
    registry.create(&t, &bot.snapshot()).unwrap();
    let before = bot.snapshot();
    append_fill(&mut view, 4095, 4145, 1, Side::Sell);
    assert!(bot.decide(&view).is_err());
    assert_eq!(before, bot.snapshot());
    let mut invalid = before;
    let PersistedAgentKindState::Plugin { data, .. } = &mut invalid else {
        panic!()
    };
    Arc::make_mut(data)["fill_count"] = json!(4096);
    assert!(registry.create(&t, &invalid).is_err());
}

#[test]
fn manual_paused_market_decisions_match_running_and_closed_markets_refuse() {
    let registry = BotRegistry::with_builtins();
    for (id, prices) in [
        ("NativePineMa", vec![100, 101, 102, 103, 104, 105, 106, 107]),
        (
            "NativePineRsi",
            vec![108, 107, 106, 105, 104, 103, 102, 101],
        ),
        (
            "NativePineBreakout",
            vec![100, 101, 102, 103, 104, 105, 110],
        ),
    ] {
        let t = template(id, json!({}));
        let mut running = registry.create(&t, &t.initial_state()).unwrap();
        let mut paused = registry.create(&t, &t.initial_state()).unwrap();
        let mut view = observation(&prices, 0);
        let running_actions = running.decide(&view).unwrap();
        assert_eq!(
            running_actions.len(),
            1,
            "{id} should emit a real entry intention"
        );
        view.status = crate::MarketStatus::Paused;
        assert_eq!(
            paused.decide(&view).unwrap(),
            running_actions,
            "{id} manual paused step"
        );
        assert_eq!(
            paused.snapshot(),
            running.snapshot(),
            "{id} manual paused state"
        );
        view.status = crate::MarketStatus::Closed;
        let mut closed = registry.create(&t, &t.initial_state()).unwrap();
        let before = closed.snapshot();
        assert!(closed.decide(&view).is_err());
        assert_eq!(
            closed.snapshot(),
            before,
            "{id} closed state must remain unchanged"
        );
    }
}
