use super::*;

pub(super) fn perp_view(time: u64, mid: i64, position: i128) -> ParticipantObservation {
    let mut v = view(time, mid, 0);
    v.instrument_id = "V-BTC-PERP".into();
    v.own_account = Some(AccountSnapshot::Perp(
        crate::PerpAccount {
            hedge_positions: None,
            account_id: 20,
            cash_balance: 10000,
            position_qty: position,
            avg_entry_price_tick: 100,
            realized_pnl: 0,
            fees_paid: 0,
            funding_pnl: 0,
            reserved_margin: 0,
        }
        .snapshot(crate::PerpClearingConfig::default(), 100),
    ));
    v.perp_price = Some(crate::PerpPriceSnapshot {
        instrument_id: "V-BTC-PERP".into(),
        spot_instrument_id: "V-BTC-SPOT".into(),
        index_price_tick: Some(100),
        mark_price_tick: 100,
        source: Some(crate::IndexPriceSource::SpotMid),
        source_time_ms: Some(time),
        max_age_ms: 30000,
        status: crate::PriceLinkStatus::Live,
        funding: None,
    });
    v
}

fn volume(v: &mut ParticipantObservation, amount: u128, truncated: bool) {
    v.bot_market_data = Some(crate::BotMarketData {
        interval_ms: 1000,
        external_volume_qty: amount.to_string(),
        candles: vec![],
        own_fills: vec![],
        fill_details: vec![],
        truncated,
    });
}

#[test]
fn trailing_stop_cancels_then_latches_until_actual_flat_and_cools_down() {
    let mut b = bot(
        "ValueTrader",
        json!({"fair_price_tick":200,"risk":{"trailing_stop_ppm":100000,"cooldown_ms":5000}}),
    );
    b.decide(&view(0, 100, 10)).unwrap();
    b.decide(&view(1000, 120, 10)).unwrap();
    let mut v = view(2000, 105, 10);
    v.own_orders.push(resting(9, Side::Buy, 90, 1));
    assert_eq!(
        b.decide(&v).unwrap(),
        vec![OrderAction::Cancel { order_id: 9 }]
    );
    let actions = b.decide(&view(3000, 125, 8)).unwrap();
    assert!(matches!(
        actions.as_slice(),
        [OrderAction::PlaceProtected {
            side: Side::Sell,
            qty: 2,
            reduce_only: false,
            ..
        }]
    ));
    assert!(b.decide(&view(4000, 125, 0)).unwrap().is_empty());
    assert!(b.decide(&view(8000, 125, 0)).unwrap().is_empty());
    assert!(!b.decide(&view(9000, 125, 0)).unwrap().is_empty());
    assert_eq!(state(b.as_ref()).risk.exits, 1);
}

#[test]
fn healthy_perp_can_exit_before_exchange_liquidation_threshold() {
    let mut b = bot(
        "TrendTrader",
        json!({"risk":{"min_margin_buffer_ppm":999999}}),
    );
    let v = perp_view(0, 100, 10);
    assert!(
        matches!(&v.own_account,Some(AccountSnapshot::Perp(a)) if a.margin_status == PerpMarginStatus::Healthy)
    );
    assert!(matches!(
        b.decide(&v).unwrap().as_slice(),
        [OrderAction::PlaceProtected {
            side: Side::Sell,
            reduce_only: true,
            ..
        }]
    ));
}

#[test]
fn drawdown_tracks_equity_not_only_price_and_survives_recovery() {
    let t = template("ValueTrader", json!({"risk":{"max_drawdown_ppm":100000}}));
    let registry = BotRegistry::with_builtins();
    let mut b = registry.create(&t, &t.initial_state()).unwrap();
    b.decide(&view(0, 100, 10)).unwrap();
    let mut next = view(1000, 100, 10);
    let Some(AccountSnapshot::Spot(a)) = &mut next.own_account else {
        panic!()
    };
    a.cash_balance = 8000;
    a.available_cash = 8000;
    let saved = b.snapshot();
    let mut restored = registry.create(&t, &saved).unwrap();
    assert_eq!(b.decide(&next).unwrap(), restored.decide(&next).unwrap());
    assert_eq!(state(b.as_ref()).risk.trigger.as_deref(), Some("drawdown"));
}

#[test]
fn pov_uses_external_volume_actual_fills_and_hard_deadline() {
    let mut b = bot(
        "PovExecutionTrader",
        json!({"participation_ppm":100000,"max_qty":10,"target_qty":5,"horizon_ms":5000}),
    );
    let mut v = view(0, 100, 0);
    volume(&mut v, 100, false);
    assert!(b.decide(&v).unwrap().is_empty());
    v = view(1000, 100, 0);
    volume(&mut v, 120, false);
    assert!(matches!(
        b.decide(&v).unwrap().as_slice(),
        [OrderAction::PlaceProtected { qty: 2, .. }]
    ));
    // A partial fill leaves exactly one unit of volume entitlement.
    v = view(2000, 100, 1);
    volume(&mut v, 120, false);
    assert!(matches!(
        b.decide(&v).unwrap().as_slice(),
        [OrderAction::PlaceProtected { qty: 1, .. }]
    ));
    v = view(3000, 100, 2);
    volume(&mut v, 120, false);
    assert!(b.decide(&v).unwrap().is_empty());
    v = view(5000, 100, 2);
    volume(&mut v, 200, false);
    assert!(b.decide(&v).unwrap().is_empty());
    assert_eq!(state(b.as_ref()).completed_qty, 2);
    assert!(state(b.as_ref()).deadline_reached);
}

#[test]
fn pov_refuses_missing_or_truncated_history_even_with_urgency() {
    let mut b = bot(
        "PovExecutionTrader",
        json!({"deadline_urgency_ms":4000,"horizon_ms":5000}),
    );
    assert!(b.decide(&view(0, 100, 0)).unwrap().is_empty());
    let mut v = view(1000, 100, 0);
    volume(&mut v, 100, true);
    assert!(b.decide(&v).unwrap().is_empty());
}

#[test]
fn pov_recovery_preserves_volume_baseline_and_explicit_urgency() {
    let t = template(
        "PovExecutionTrader",
        json!({"horizon_ms":5000,"deadline_urgency_ms":1000,"max_qty":4}),
    );
    let registry = BotRegistry::with_builtins();
    let mut b = registry.create(&t, &t.initial_state()).unwrap();
    let mut v = view(0, 100, 0);
    volume(&mut v, 100, false);
    b.decide(&v).unwrap();
    let mut recovered = registry.create(&t, &b.snapshot()).unwrap();
    v = view(4000, 100, 0);
    volume(&mut v, 100, false);
    let actions = b.decide(&v).unwrap();
    assert_eq!(actions, recovered.decide(&v).unwrap());
    assert!(matches!(
        actions.as_slice(),
        [OrderAction::PlaceProtected {
            qty: 4,
            valid_until_market_time_ms: Some(5000),
            ..
        }]
    ));
}

#[test]
fn stale_link_does_not_block_an_already_needed_reduce_only_risk_exit() {
    let mut b = bot(
        "DynamicMarketMaker",
        json!({"risk":{"min_margin_buffer_ppm":999999}}),
    );
    let mut v = perp_view(0, 100, 10);
    v.perp_price.as_mut().unwrap().status = crate::PriceLinkStatus::Stale;
    assert!(matches!(
        b.decide(&v).unwrap().as_slice(),
        [OrderAction::PlaceProtected {
            side: Side::Sell,
            reduce_only: true,
            ..
        }]
    ));
}

#[test]
fn net_inventory_strategies_refuse_hedge_mode_instead_of_ignoring_gross_exposure() {
    let mut b = bot("DynamicMarketMaker", json!({}));
    let mut v = perp_view(0, 100, 0);
    let Some(AccountSnapshot::Perp(a)) = &mut v.own_account else {
        panic!()
    };
    a.hedge_positions = Some(Default::default());
    assert!(b.decide(&v).unwrap_err().0.contains("one-way"));
}

#[test]
fn common_event_arrives_with_individual_delay_and_expires_without_price_write() {
    let event = crate::market_events::MarketEvent {
        id: "news-1".into(),
        instrument_id: "V-BTC-SPOT".into(),
        published_at_ms: 1000,
        expires_at_ms: 5000,
        impact_ticks: 20,
        headline: "synthetic news".into(),
    };
    let mut fast = bot("MarketEventTrader", json!({"inventory_target":10}));
    let mut slow = bot(
        "MarketEventTrader",
        json!({"inventory_target":10,"information_delay_ms":2000,"confidence_ppm":500000}),
    );
    for time in [0, 1000, 2000, 3000, 5000] {
        let mut v = view(time, 100, 10);
        v.market_events = vec![event.clone()];
        let book = v.book.clone();
        fast.decide(&v).unwrap();
        slow.decide(&v).unwrap();
        assert_eq!(book, v.book);
        assert_eq!(
            state(fast.as_ref()).event_fair_price,
            Some(if (1000..5000).contains(&time) {
                120
            } else {
                100
            })
        );
        assert_eq!(
            state(slow.as_ref()).event_fair_price,
            Some(if (3000..5000).contains(&time) {
                110
            } else {
                100
            })
        );
    }
    assert_eq!(state(fast.as_ref()).received_event_ids, vec!["news-1"]);
    assert_eq!(state(slow.as_ref()).received_event_ids, vec!["news-1"]);
}

#[test]
fn arbitrage_enters_only_executable_profitable_basis_and_waits_for_actual_hedge() {
    let mut b = bot("BasisArbitrageTrader", json!({"fee_buffer_ppm":5000}));
    let mut v = view(0, 100, 0);
    v.related_markets = vec![perp_view(0, 110, 0)];
    assert!(matches!(
        b.decide(&v).unwrap().as_slice(),
        [OrderAction::PlaceProtected {
            side: Side::Buy,
            qty: 2,
            ..
        }]
    ));
    v = view(1000, 100, 1);
    v.related_markets = vec![perp_view(1000, 110, 0)];
    assert!(b.decide(&v).unwrap().is_empty());
    let mut no_edge = bot("BasisArbitrageTrader", json!({}));
    v = view(0, 100, 0);
    v.related_markets = vec![perp_view(0, 104, 0)];
    assert!(no_edge.decide(&v).unwrap().is_empty());
}

#[test]
fn arbitrage_perp_leg_hedges_partial_fills_and_never_opens_without_spot() {
    let mut b = bot(
        "BasisArbitrageTrader",
        json!({"leg":"Perp","hedge_instrument_id":"V-BTC-SPOT"}),
    );
    let mut v = perp_view(0, 110, 0);
    v.related_markets = vec![view(0, 100, 1)];
    assert!(matches!(
        b.decide(&v).unwrap().as_slice(),
        [OrderAction::PlaceProtected {
            side: Side::Sell,
            qty: 1,
            reduce_only: false,
            ..
        }]
    ));
    v = perp_view(1000, 110, -1);
    v.related_markets = vec![view(1000, 100, 1)];
    assert!(b.decide(&v).unwrap().is_empty());
    v = perp_view(2000, 110, -1);
    v.related_markets = vec![view(2000, 100, 0)];
    assert!(matches!(
        b.decide(&v).unwrap().as_slice(),
        [OrderAction::PlaceProtected {
            side: Side::Buy,
            qty: 1,
            reduce_only: true,
            ..
        }]
    ));
}

#[test]
fn arbitrage_hedge_timeout_unwinds_and_recovery_keeps_the_deadline() {
    let t = template("BasisArbitrageTrader", json!({"hedge_timeout_ms":3000}));
    let registry = BotRegistry::with_builtins();
    let mut b = registry.create(&t, &t.initial_state()).unwrap();
    let mut v = view(1000, 100, 2);
    v.related_markets = vec![perp_view(1000, 110, 0)];
    assert!(b.decide(&v).unwrap().is_empty());
    let mut recovered = registry.create(&t, &b.snapshot()).unwrap();
    v = view(4000, 100, 2);
    v.related_markets = vec![perp_view(4000, 110, 0)];
    let actions = b.decide(&v).unwrap();
    assert_eq!(actions, recovered.decide(&v).unwrap());
    assert!(matches!(
        actions.as_slice(),
        [OrderAction::PlaceProtected {
            side: Side::Sell,
            qty: 2,
            ..
        }]
    ));
}

#[test]
fn arbitrage_stale_link_exits_spot_and_forbids_new_perp_openings() {
    let mut spot = bot("BasisArbitrageTrader", json!({}));
    let mut v = view(0, 100, 2);
    let mut peer = perp_view(0, 110, -2);
    peer.perp_price.as_mut().unwrap().status = crate::PriceLinkStatus::Stale;
    v.related_markets = vec![peer];
    assert!(matches!(
        spot.decide(&v).unwrap().as_slice(),
        [OrderAction::PlaceProtected {
            side: Side::Sell,
            ..
        }]
    ));
    let mut perp = bot(
        "BasisArbitrageTrader",
        json!({"leg":"Perp","hedge_instrument_id":"V-BTC-SPOT"}),
    );
    let mut v = perp_view(0, 110, 0);
    v.perp_price.as_mut().unwrap().status = crate::PriceLinkStatus::Stale;
    v.related_markets = vec![view(0, 100, 2)];
    assert!(perp.decide(&v).unwrap().is_empty());
}
