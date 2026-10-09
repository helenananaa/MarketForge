use super::behaviors::perp_view;
use super::*;

#[test]
fn poisson_arrivals_have_independent_first_delays_and_restore_the_same_decisions() {
    let registry = BotRegistry::with_builtins();
    let mut delays = std::collections::BTreeSet::new();
    for seed in 1..=16 {
        let mut spec = template(
            "AdaptiveNoiseTrader",
            json!({"arrival_mode":"Poisson","activity_ppm":1000000}),
        );
        let AgentTemplate::Plugin(config) = &mut spec else {
            unreachable!()
        };
        config.seed = seed;
        let mut trader = registry.create(&spec, &spec.initial_state()).unwrap();
        assert!(trader.decide(&view(0, 100, 10)).unwrap().is_empty());
        let next = state(trader.as_ref()).next_decision_ms;
        assert!(next > 0);
        delays.insert(next);
        let mut restored = registry.create(&spec, &trader.snapshot()).unwrap();
        for time in (1000..=30000).step_by(1000) {
            let observation = view(time, 100, 10);
            assert_eq!(
                trader.decide(&observation).unwrap(),
                restored.decide(&observation).unwrap()
            );
            assert_eq!(trader.snapshot(), restored.snapshot());
        }
    }
    assert!(delays.len() > 8);
}

#[test]
fn depleted_quotes_replenish_only_the_deficit_and_keep_existing_queue_positions() {
    let mut maker = bot(
        "DynamicMarketMaker",
        json!({"max_qty":10,"levels":1,"inventory_target":10,
        "replenish_depth_ppm":800000,"size_volatility_ticks":10}),
    );
    maker.decide(&view(0, 100, 10)).unwrap();
    let mut observation = view(1000, 100, 10);
    observation.own_orders = vec![
        resting(1, Side::Buy, 99, 10),
        resting(2, Side::Sell, 101, 3),
    ];
    assert_eq!(
        maker.decide(&observation).unwrap(),
        vec![OrderAction::PlacePostOnly {
            side: Side::Sell,
            price_tick: 101,
            qty: 7
        }]
    );
    observation.market_time_ms = 2000;
    observation.step = 2;
    observation.own_orders[1].remaining_qty = 9;
    assert!(maker.decide(&observation).unwrap().is_empty());
}

fn funding_view(time: u64, position: i128, rate: Option<i32>, next: u64) -> ParticipantObservation {
    let mut observation = perp_view(time, 100, position);
    observation.perp_price.as_mut().unwrap().funding = Some(crate::FundingSnapshot {
        market_time_ms: time,
        interval_ms: 10000,
        base_rate_ppm: 100,
        max_rate_ppm: 10000,
        min_coverage_ppm: 800000,
        estimated_rate_ppm: rate,
        next_funding_time_ms: next,
        covered_ms: time % 10000,
        last_settlement: None,
    });
    observation
}

#[test]
fn maker_pressure_changes_price_and_side_sizes_and_excludes_own_depth() {
    let mut maker = bot(
        "DynamicMarketMaker",
        json!({"max_qty":10,"inventory_target":10,
        "book_pressure_ticks":5,"volatility_spread_multiplier":0}),
    );
    let mut observation = view(0, 100, 10);
    observation.book.bids[0].qty = 90;
    observation.book.asks[0].qty = 10;
    let actions = maker.decide(&observation).unwrap();
    assert!(actions.iter().any(|a| matches!(
        a,
        OrderAction::PlacePostOnly {
            side: Side::Buy,
            qty: 18,
            price_tick: 100
        }
    )));
    assert!(actions.iter().any(|a| matches!(
        a,
        OrderAction::PlacePostOnly {
            side: Side::Sell,
            qty: 2,
            ..
        }
    )));
    // Self-owned depth must not generate positive feedback in price pressure.
    observation.market_time_ms = 1000;
    observation.step = 1;
    observation.own_orders.push(resting(7, Side::Buy, 99, 80));
    assert_eq!(
        maker.decide(&observation).unwrap(),
        vec![OrderAction::Cancel { order_id: 7 }]
    );
    assert_eq!(state(maker.as_ref()).last_quote_center, Some(104));
    observation.own_orders.clear();
    observation.book.bids[0].qty = 10;
    observation.market_time_ms = 2000;
    observation.step = 2;
    let actions = maker.decide(&observation).unwrap();
    assert!(actions.iter().any(|a| matches!(
        a,
        OrderAction::PlacePostOnly {
            side: Side::Buy,
            qty: 10,
            price_tick: 99
        }
    )));
}

#[test]
fn maker_flow_withdrawal_recovery_and_duplicate_trade_recovery_are_durable() {
    let template = template(
        "DynamicMarketMaker",
        json!({"max_qty":10,"inventory_target":10,
        "toxic_flow_threshold_ppm":800000,"toxic_flow_min_qty":3,"toxic_cooldown_ms":3000,"recovery_ramp_ms":4000}),
    );
    let registry = BotRegistry::with_builtins();
    let mut maker = registry
        .create(&template, &template.initial_state())
        .unwrap();
    maker.decide(&view(0, 100, 10)).unwrap();
    let mut observation = view(1000, 100, 10);
    observation.own_orders.push(resting(1, Side::Sell, 101, 7));
    observation.public_trades.push(crate::Trade {
        trade_id: 1,
        maker_order_id: 1,
        maker_account_id: 20,
        taker_order_id: 2,
        taker_account_id: 30,
        price_tick: 101,
        qty: 3,
        taker_side: Side::Buy,
        maker_position_side: Default::default(),
        taker_position_side: Default::default(),
    });
    assert_eq!(
        maker.decide(&observation).unwrap(),
        vec![OrderAction::Cancel { order_id: 1 }]
    );
    let mut restored = registry.create(&template, &maker.snapshot()).unwrap();
    observation.own_orders.clear();
    for time in [2000, 4000, 5000, 6000, 8000] {
        observation.market_time_ms = time;
        observation.step = time / 1000;
        let actions = maker.decide(&observation).unwrap();
        assert_eq!(actions, restored.decide(&observation).unwrap());
        assert_eq!(maker.snapshot(), restored.snapshot());
        assert_eq!(state(maker.as_ref()).toxic_until_ms, 4000);
        if time == 5000 {
            assert!(
                actions
                    .iter()
                    .any(|a| matches!(a, OrderAction::PlacePostOnly { qty: 2, .. }))
            );
        }
        if time == 8000 {
            assert!(
                actions
                    .iter()
                    .any(|a| matches!(a, OrderAction::PlacePostOnly { qty: 10, .. }))
            );
        }
    }
}

#[test]
fn funding_observation_window_real_positions_rate_flip_and_next_cycle_close() {
    let mut trader = bot(
        "FundingRateTrader",
        json!({"position_size":5,"max_qty":5,
        "funding_entry_rate_ppm":1000,"funding_exit_rate_ppm":200,"funding_entry_window_ms":5000}),
    );
    assert!(
        trader
            .decide(&funding_view(0, 0, Some(2000), 10000))
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        trader
            .decide(&funding_view(6000, 0, Some(2000), 10000))
            .unwrap()
            .as_slice(),
        [OrderAction::PlaceProtected {
            side: Side::Sell,
            qty: 5,
            reduce_only: false,
            ..
        }]
    ));
    assert!(matches!(
        trader
            .decide(&funding_view(7000, -2, Some(2000), 10000))
            .unwrap()
            .as_slice(),
        [OrderAction::PlaceProtected {
            side: Side::Sell,
            qty: 3,
            reduce_only: false,
            ..
        }]
    ));
    // Rate reversal first flattens the actually filled short, not the target 5.
    assert!(matches!(
        trader
            .decide(&funding_view(8000, -2, Some(-2000), 10000))
            .unwrap()
            .as_slice(),
        [OrderAction::PlaceProtected {
            side: Side::Buy,
            qty: 2,
            reduce_only: true,
            ..
        }]
    ));
    assert!(matches!(
        trader
            .decide(&funding_view(9000, 0, Some(-2000), 10000))
            .unwrap()
            .as_slice(),
        [OrderAction::PlaceProtected {
            side: Side::Buy,
            qty: 5,
            reduce_only: false,
            ..
        }]
    ));
    assert!(matches!(
        trader
            .decide(&funding_view(10000, 3, Some(-2000), 20000))
            .unwrap()
            .as_slice(),
        [OrderAction::PlaceProtected {
            side: Side::Sell,
            qty: 3,
            reduce_only: true,
            ..
        }]
    ));
}

#[test]
fn leveraged_target_trims_loss_exposure_before_cadence_and_stale_index_flattens() {
    let mut trader = bot(
        "LeveragedTrendTrader",
        json!({"target_leverage":5,"position_size":500,
        "inventory_cap":500,"max_qty":100,"decision_interval_ms":30000}),
    );
    let mut observation = perp_view(0, 100, 120);
    let Some(AccountSnapshot::Perp(account)) = &mut observation.own_account else {
        panic!()
    };
    account.equity = 2000;
    account.initial_margin = 1200;
    account.portfolio_initial_margin = 1200;
    account.margin_status = PerpMarginStatus::Healthy;
    assert!(matches!(
        trader.decide(&observation).unwrap().as_slice(),
        [OrderAction::PlaceProtected {
            side: Side::Sell,
            qty: 20,
            reduce_only: true,
            ..
        }]
    ));
    observation.market_time_ms = 1000;
    observation.step = 1;
    let Some(AccountSnapshot::Perp(account)) = &mut observation.own_account else {
        panic!()
    };
    account.position_qty = 100;
    account.equity = 1000;
    assert!(matches!(
        trader.decide(&observation).unwrap().as_slice(),
        [OrderAction::PlaceProtected {
            side: Side::Sell,
            qty: 50,
            reduce_only: true,
            ..
        }]
    ));
    observation.perp_price.as_mut().unwrap().status = crate::PriceLinkStatus::Stale;
    observation.market_time_ms = 2000;
    observation.step = 2;
    assert!(matches!(
        trader.decide(&observation).unwrap().as_slice(),
        [OrderAction::PlaceProtected {
            side: Side::Sell,
            qty: 100,
            reduce_only: true,
            ..
        }]
    ));
    assert!(trader.decide(&view(3000, 100, 0)).is_err());
}

#[test]
fn leverage_budget_has_explicit_cap_and_legacy_budget_remains_unleveraged() {
    let mut observation = perp_view(0, 100, 0);
    let Some(AccountSnapshot::Perp(account)) = &mut observation.own_account else {
        panic!()
    };
    account.equity = 1000;
    let config = Config {
        target_leverage: 5,
        inventory_cap: 500,
        fee_buffer_ppm: 0,
        ..Config::default()
    };
    let mut budget = Budget::new(&observation, &config).unwrap();
    assert_eq!(budget.allocate(Side::Buy, 100, 100), 50);
    assert_eq!(budget.allocate(Side::Buy, 100, 100), 0);
    let mut legacy = Budget::new(
        &observation,
        &Config {
            fee_buffer_ppm: 0,
            ..Config::default()
        },
    )
    .unwrap();
    assert_eq!(legacy.allocate(Side::Buy, 100, 100), 10);
}
