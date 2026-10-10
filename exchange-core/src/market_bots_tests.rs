use super::*;
use crate::{
    BookLevel, BookSnapshot, BotConfig, Order, ParticipantConfig, ParticipantKind, SpotAccount,
};

#[path = "market_behaviors_tests.rs"]
mod behaviors;

#[path = "market_microstructure_tests.rs"]
mod microstructure;

fn template(id: &str, params: serde_json::Value) -> AgentTemplate {
    let mut config = json!({"decision_interval_ms":1000,"jitter_ms":0});
    config
        .as_object_mut()
        .unwrap()
        .extend(params.as_object().unwrap().clone());
    AgentTemplate::Plugin(BotConfig {
        participant: ParticipantConfig {
            participant_id: "test".into(),
            kind: ParticipantKind::RuleAgent,
            room_id: "test".into(),
            account_id: 20,
            instrument_id: Some("V-BTC-SPOT".into()),
        },
        plugin_id: id.into(),
        plugin_version: "1".into(),
        state_version: 1,
        config_version: 1,
        seed: 7,
        config,
    })
}

fn view(time: u64, mid: i64, position: i128) -> ParticipantObservation {
    ParticipantObservation {
        version: 1,
        room_id: "test".into(),
        venue_id: "default-venue".into(),
        instrument_id: "V-BTC-SPOT".into(),
        status: MarketStatus::Running,
        step: time / 1000,
        market_time_ms: time,
        book: BookSnapshot {
            bids: vec![BookLevel {
                price_tick: mid - 1,
                qty: 100,
            }],
            asks: vec![BookLevel {
                price_tick: mid + 1,
                qty: 100,
            }],
        },
        public_trades: vec![],
        own_orders: vec![],
        related_markets: vec![],
        market_events: vec![],
        bot_market_data: None,
        position_protections: Vec::new(),
        risk: None,
        perp_price: None,
        own_account: Some(AccountSnapshot::Spot(
            SpotAccount {
                account_id: 20,
                cash_balance: 10000,
                position_qty: position,
                reserved_cash: 0,
                reserved_position: 0,
                fees_paid: 0,
            }
            .snapshot(),
        )),
    }
}

fn bot(id: &str, params: serde_json::Value) -> Box<dyn ScheduledBot> {
    let template = template(id, params);
    BotRegistry::with_builtins()
        .create(&template, &template.initial_state())
        .unwrap()
}

fn state(bot: &dyn ScheduledBot) -> State {
    let PersistedAgentKindState::Plugin { data, .. } = bot.snapshot() else {
        panic!()
    };
    serde_json::from_value(data).unwrap()
}

fn resting(id: u64, side: Side, price: i64, qty: u64) -> Order {
    Order {
        position_side: Default::default(),
        order_id: id,
        account_id: 20,
        side,
        price_tick: price,
        remaining_qty: qty,
        seq: id,
    }
}

#[test]
fn linked_perp_maker_prices_actual_orders_from_spot_index_and_withdraws_when_stale() {
    let mut maker = bot("DynamicMarketMaker", json!({"inventory_target": 0}));
    let mut observation = view(0, 100, 0);
    observation.perp_price = Some(crate::PerpPriceSnapshot {
        instrument_id: "V-BTC-PERP".into(),
        spot_instrument_id: "V-BTC-SPOT".into(),
        index_price_tick: Some(200),
        mark_price_tick: 200,
        source: Some(crate::IndexPriceSource::SpotMid),
        source_time_ms: Some(0),
        max_age_ms: 1000,
        status: crate::PriceLinkStatus::Live,
        funding: None,
    });
    let actions = maker.decide(&observation).unwrap();
    assert!(actions.iter().any(|action| matches!(
        action,
        OrderAction::PlacePostOnly {
            side: Side::Buy,
            price_tick: 199,
            ..
        }
    )));
    observation.step = 2;
    observation.market_time_ms = 2000;
    observation.perp_price.as_mut().unwrap().status = crate::PriceLinkStatus::Stale;
    observation.own_orders = vec![resting(1, Side::Buy, 199, 1)];
    assert_eq!(
        maker.decide(&observation).unwrap(),
        vec![OrderAction::Cancel { order_id: 1 }]
    );
}

#[test]
fn factory_rejects_invalid_bounds_relationships_and_state() {
    let registry = BotRegistry::with_builtins();
    for (id, params) in [
        (
            "DynamicMarketMaker",
            json!({"inventory_target":101,"inventory_cap":100}),
        ),
        ("TrendTrader", json!({"lookback":1})),
        ("TrendTrader", json!({"lookback":129})),
        ("TrendTrader", json!({"position_size":101})),
        ("ExecutionTrader", json!({"horizon_ms":0})),
        ("ExecutionTrader", json!({"side":"Both"})),
        ("AdaptiveNoiseTrader", json!({"activity_ppm":1000001})),
        ("ValueTrader", json!({"invented_parameter":1})),
    ] {
        assert!(registry.validate_template(&template(id, params)).is_err());
    }
    let t = template("TrendTrader", json!({}));
    let mut saved = t.initial_state();
    if let PersistedAgentKindState::Plugin { data, .. } = &mut saved {
        *data = serde_json::to_value(State::default()).unwrap();
    }
    assert!(registry.create(&t, &saved).is_err());
}

#[test]
fn maker_skews_quotes_to_target_and_counts_resting_exposure() {
    let params = json!({"inventory_target":40,"inventory_cap":100,"inventory_skew_ticks":10,"levels":1,"max_qty":10});
    let mut balanced = bot("DynamicMarketMaker", params.clone());
    let at_target = balanced.decide(&view(1000, 100, 40)).unwrap();
    assert_eq!(
        at_target[0],
        OrderAction::PlacePostOnly {
            side: Side::Buy,
            price_tick: 99,
            qty: 10
        }
    );
    let mut long = bot("DynamicMarketMaker", params);
    let mut observation = view(1000, 100, 80);
    observation.own_orders.push(resting(1, Side::Buy, 95, 20));
    // Existing orders must be canceled before repricing, without assuming success.
    assert_eq!(
        long.decide(&observation).unwrap(),
        vec![OrderAction::Cancel { order_id: 1 }]
    );
    observation.own_orders.clear();
    observation.market_time_ms = 2000;
    observation.step = 2;
    let quotes = long.decide(&observation).unwrap();
    assert_eq!(
        quotes[0],
        OrderAction::PlacePostOnly {
            side: Side::Buy,
            price_tick: 95,
            qty: 10
        }
    );
    assert_eq!(
        quotes[1],
        OrderAction::PlacePostOnly {
            side: Side::Sell,
            price_tick: 97,
            qty: 10
        }
    );
    let mut budget = Budget::new(&view(1000, 100, 98), &Config::default()).unwrap();
    budget.buy_open = 1;
    assert_eq!(budget.allocate(Side::Buy, 100, 10), 1);
    assert_eq!(budget.allocate(Side::Buy, 100, 10), 0);
}

#[test]
fn maker_widens_and_shrinks_then_withdraws_during_shock_even_before_due() {
    let mut trader = bot(
        "DynamicMarketMaker",
        json!({"levels":1,"max_qty":8,"withdraw_volatility_ticks":10,"decision_interval_ms":10000}),
    );
    trader.decide(&view(1000, 100, 40)).unwrap();
    let mut observation = view(2000, 150, 40);
    observation.own_orders.push(resting(1, Side::Buy, 99, 8));
    assert_eq!(
        trader.decide(&observation).unwrap(),
        vec![OrderAction::Cancel { order_id: 1 }]
    );
    let mut trader = bot("DynamicMarketMaker", json!({"levels":1,"max_qty":8}));
    trader.decide(&view(1000, 100, 40)).unwrap();
    let quotes = trader.decide(&view(2000, 108, 40)).unwrap();
    assert_eq!(
        quotes[0],
        OrderAction::PlacePostOnly {
            side: Side::Buy,
            price_tick: 102,
            qty: 2
        }
    );
    assert_eq!(
        quotes[1],
        OrderAction::PlacePostOnly {
            side: Side::Sell,
            price_tick: 112,
            qty: 2
        }
    );
}

#[test]
fn noise_expires_orders_without_spending_reserved_cash_or_shorting_spot() {
    let mut trader = bot(
        "AdaptiveNoiseTrader",
        json!({"activity_ppm":0,"order_ttl_ms":2000}),
    );
    let mut observation = view(1000, 100, 1);
    observation.own_orders.push(resting(10, Side::Buy, 95, 1));
    assert!(trader.decide(&observation).unwrap().is_empty());
    observation.market_time_ms = 3000;
    observation.step = 3;
    assert_eq!(
        trader.decide(&observation).unwrap(),
        vec![OrderAction::Cancel { order_id: 10 }]
    );
    let mut observation = view(1000, 100, 0);
    let Some(AccountSnapshot::Spot(a)) = &mut observation.own_account else {
        panic!()
    };
    a.available_cash = 100;
    a.reserved_cash = 9900;
    let mut budget = Budget::new(&observation, &Config::default()).unwrap();
    assert_eq!(budget.allocate(Side::Buy, 100, 2), 0); // reserve estimated fee
    assert_eq!(budget.allocate(Side::Sell, 100, 2), 0);
}

#[test]
fn value_waits_for_delayed_information_and_never_changes_the_book_directly() {
    let mut trader = bot(
        "ValueTrader",
        json!({"fair_price_tick":100,"value_shift_ticks":10,"value_shift_at_ms":2000,"information_delay_ms":2000}),
    );
    assert!(trader.decide(&view(1000, 100, 20)).unwrap().is_empty());
    assert!(trader.decide(&view(3000, 100, 20)).unwrap().is_empty());
    assert_eq!(
        trader.decide(&view(4000, 100, 20)).unwrap(),
        vec![OrderAction::PlaceImmediateOrCancel {
            side: Side::Buy,
            price_tick: Some(109),
            qty: 2
        }]
    );
    assert_eq!(
        trader.decide(&view(5000, 120, 20)).unwrap(),
        vec![OrderAction::PlaceImmediateOrCancel {
            side: Side::Sell,
            price_tick: Some(111),
            qty: 2
        }]
    );
}

#[test]
fn trend_warms_up_builds_a_target_and_exits_when_signal_fades() {
    let mut trader = bot(
        "TrendTrader",
        json!({"lookback":3,"inventory_target":20,"position_size":5,"signal_threshold_ticks":1,"max_qty":10}),
    );
    assert!(trader.decide(&view(1000, 100, 20)).unwrap().is_empty());
    assert!(trader.decide(&view(2000, 102, 20)).unwrap().is_empty());
    assert_eq!(
        trader.decide(&view(3000, 104, 20)).unwrap(),
        vec![OrderAction::PlaceImmediateOrCancel {
            side: Side::Buy,
            price_tick: Some(105),
            qty: 5
        }]
    );
    trader.decide(&view(4000, 104, 25)).unwrap();
    assert_eq!(
        trader.decide(&view(5000, 104, 25)).unwrap(),
        vec![OrderAction::PlaceImmediateOrCancel {
            side: Side::Sell,
            price_tick: Some(103),
            qty: 5
        }]
    );
}

#[test]
fn execution_uses_fills_retries_debt_respects_limit_target_and_deadline() {
    let mut trader = bot(
        "ExecutionTrader",
        json!({"target_qty":4,"horizon_ms":4000,"max_qty":2,"max_slippage_ticks":1}),
    );
    assert_eq!(
        trader.decide(&view(1000, 100, 10)).unwrap(),
        vec![OrderAction::PlaceImmediateOrCancel {
            side: Side::Buy,
            price_tick: Some(101),
            qty: 1
        }]
    );
    // No fill: submitting the first slice did not mark it complete.
    let orders = trader.decide(&view(2000, 100, 10)).unwrap();
    assert_eq!(
        orders,
        vec![OrderAction::PlaceImmediateOrCancel {
            side: Side::Buy,
            price_tick: Some(101),
            qty: 2
        }]
    );
    assert_eq!(state(trader.as_ref()).completed_qty, 0);
    assert_eq!(
        trader.decide(&view(3000, 100, 12)).unwrap(),
        vec![OrderAction::PlaceImmediateOrCancel {
            side: Side::Buy,
            price_tick: Some(101),
            qty: 1
        }]
    );
    assert!(trader.decide(&view(4000, 100, 14)).unwrap().is_empty());
    assert_eq!(state(trader.as_ref()).completed_qty, 4);
    assert!(trader.decide(&view(5000, 100, 14)).unwrap().is_empty());
    assert!(state(trader.as_ref()).deadline_reached);
    let mut trader = bot("ExecutionTrader", json!({"horizon_ms":2000}));
    let mut empty = view(1000, 100, 10);
    empty.book.asks.clear();
    assert!(trader.decide(&empty).unwrap().is_empty());
    assert!(trader.decide(&view(3000, 100, 10)).unwrap().is_empty());
    assert_eq!(state(trader.as_ref()).completed_qty, 0);
}

#[test]
fn sell_execution_and_start_delay_are_bounded_by_acquired_inventory() {
    let mut trader = bot(
        "ExecutionTrader",
        json!({"side":"Sell","start_after_ms":2000,"target_qty":3,"horizon_ms":3000,"max_qty":10}),
    );
    assert!(trader.decide(&view(1000, 100, 5)).unwrap().is_empty());
    assert_eq!(
        trader.decide(&view(2000, 100, 5)).unwrap(),
        vec![OrderAction::PlaceImmediateOrCancel {
            side: Side::Sell,
            price_tick: Some(97),
            qty: 1
        }]
    );
    assert_eq!(
        trader.decide(&view(3000, 100, 4)).unwrap(),
        vec![OrderAction::PlaceImmediateOrCancel {
            side: Side::Sell,
            price_tick: Some(97),
            qty: 1
        }]
    );
    assert!(trader.decide(&view(4000, 100, 2)).unwrap().is_empty());
    assert_eq!(state(trader.as_ref()).completed_qty, 3);
}

#[test]
fn state_roundtrip_preserves_randomness_history_order_ages_and_execution_progress() {
    let registry = BotRegistry::with_builtins();
    for id in MARKET_BOT_IDS
        .into_iter()
        .filter(|id| *id != "BasisArbitrageTrader")
    {
        let t = template(id, json!({"jitter_ms":1000}));
        let observe = |step: u64| {
            if matches!(id, "FundingRateTrader" | "LeveragedTrendTrader") {
                behaviors::perp_view(step * 1000, 100 + step as i64 % 4, 20)
            } else {
                view(step * 1000, 100 + step as i64 % 4, 20)
            }
        };
        let mut uninterrupted = registry.create(&t, &t.initial_state()).unwrap();
        for step in 1..10 {
            uninterrupted.decide(&observe(step)).unwrap();
        }
        let encoded = serde_json::to_vec(&uninterrupted.snapshot()).unwrap();
        let saved = serde_json::from_slice(&encoded).unwrap();
        let mut recovered = registry.create(&t, &saved).unwrap();
        for step in 10..30 {
            let observation = observe(step);
            assert_eq!(
                uninterrupted.decide(&observation).unwrap(),
                recovered.decide(&observation).unwrap(),
                "{id}"
            );
            assert_eq!(uninterrupted.snapshot(), recovered.snapshot(), "{id}");
        }
    }
}

#[test]
fn unhealthy_perpetual_only_reduces_and_healthy_capital_is_not_assumed_leveraged() {
    let mut observation = view(1000, 100, 0);
    let store = crate::PerpAccountStore::new(crate::PerpClearingConfig::default(), 100).unwrap();
    // Deserialize a complete venue snapshot to exercise the same wire shape.
    let mut store = store;
    store.create_account(20, 1000);
    let mut account = store.account_snapshot(20).unwrap();
    account.position_qty = 5;
    account.equity = 0;
    account.margin_status = PerpMarginStatus::MarginCall;
    observation.own_account = Some(AccountSnapshot::Perp(account));
    let mut trader = bot("ValueTrader", json!({"fair_price_tick":200}));
    assert_eq!(
        trader.decide(&observation).unwrap(),
        vec![OrderAction::PlaceReduceOnlyImmediateOrCancel {
            side: Side::Sell,
            price_tick: Some(99),
            qty: 2
        }]
    );
    let mut budget = Budget::new(&observation, &Config::default()).unwrap();
    assert_eq!(budget.allocate(Side::Buy, 100, 10), 0);
    assert_eq!(budget.allocate(Side::Sell, 100, 10), 5);
    assert_eq!(budget.allocate(Side::Sell, 100, 10), 0);
}

#[test]
fn paused_and_duplicate_observations_do_not_advance_state() {
    let mut trader = bot("AdaptiveNoiseTrader", json!({"activity_ppm":1000000}));
    let mut observation = view(1000, 100, 10);
    observation.status = MarketStatus::Paused;
    let before = trader.snapshot();
    assert!(trader.decide(&observation).unwrap().is_empty());
    assert_eq!(before, trader.snapshot());
    observation.status = MarketStatus::Running;
    trader.decide(&observation).unwrap();
    let before = trader.snapshot();
    assert!(trader.decide(&observation).unwrap().is_empty());
    assert_eq!(before, trader.snapshot());
}

#[test]
fn marketable_actions_cancel_own_opposite_orders_before_trading() {
    let mut trader = bot("ValueTrader", json!({"fair_price_tick":120}));
    let mut observation = view(1000, 100, 20);
    observation.own_orders.push(resting(1, Side::Sell, 101, 2));
    assert_eq!(
        trader.decide(&observation).unwrap(),
        vec![OrderAction::Cancel { order_id: 1 }]
    );
    observation.market_time_ms = 2000;
    observation.step = 2;
    // A rejected cancellation remains visible and must never enable a self-fill.
    assert_eq!(
        trader.decide(&observation).unwrap(),
        vec![OrderAction::Cancel { order_id: 1 }]
    );
    observation.market_time_ms = 3000;
    observation.step = 3;
    observation.own_orders.clear();
    assert!(matches!(
        trader.decide(&observation).unwrap().first(),
        Some(OrderAction::PlaceImmediateOrCancel { .. })
    ));
}
