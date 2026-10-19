// Included in actor::tests to exercise the same transaction and reference helpers.
fn risk_order(id: u64, account: u64, side: Side, price: i64, qty: u64, hedge: bool) -> Command {
    let mut command = limit(id, account, side, price, qty);
    if hedge && let Command::NewOrder(order) = &mut command {
        order.position_side = if side == Side::Buy {
            crate::PositionSide::Long
        } else {
            crate::PositionSide::Short
        };
    }
    command
}

fn adaptive_risk_fixture(count: u64, active: u64, hedge: bool, linked: bool) -> ExchangeActor {
    let mut configs = vec![venue_spot_config("spot", "BTC", "USDT")];
    for (id, base) in [("btc", "BTC"), ("eth", "ETH"), ("sol", "SOL")] {
        let mut config = venue_perp_config(id, base, "USDT");
        if let MarketConfig::Perp(perp) = &mut config {
            if hedge {
                perp.clearing.position_mode = crate::PositionMode::Hedge;
            }
            if linked && id == "btc" {
                perp.price_link = Some(crate::PerpPriceLinkConfig {
                    spot_instrument_id: "spot".into(),
                    max_age_ms: 2000,
                    mark_interval_ms: None,
                });
                perp.funding = Some(crate::FundingConfig {
                    interval_ms: 2000,
                    base_rate_ppm: 1000,
                    max_rate_ppm: 1000,
                    min_coverage_ppm: 1_000_000,
                });
            }
        }
        configs.push(config);
    }
    let mut actor = ExchangeActor::new(
        "adaptive-risk",
        ExchangeConfig::new("binance", configs).unwrap(),
    )
    .unwrap();
    for id in 1..=count {
        actor.create_account(id, if id == count { 0 } else { 1_000_000 });
    }
    actor.venue_accounts.set_balance(1, "BTC", 10_000);
    for command in [
        limit(900010, 1, Side::Buy, 99, 100),
        limit(900011, 1, Side::Sell, 101, 100),
    ] {
        assert!(matches!(
            actor.apply_to_instrument("spot", command).unwrap().result,
            ActorExecutionResult::Accepted(_)
        ));
    }
    for (market_index, instrument) in ["btc", "eth", "sol"].into_iter().enumerate() {
        for id in (1..=active).step_by(2) {
            for (offset, account, side) in [(0, id, Side::Sell), (1, id + 1, Side::Buy)] {
                let command = risk_order(
                    100000 + market_index as u64 * 10000 + id * 2 + offset,
                    account,
                    side,
                    100,
                    2,
                    hedge,
                );
                let result = actor.apply_to_instrument(instrument, command).unwrap();
                assert!(
                    matches!(result.result, ActorExecutionResult::Accepted(_)),
                    "{result:?}"
                );
            }
        }
    }
    if hedge {
        // Real opposing fills leave account 1 with zero net but nonzero gross.
        actor
            .apply_to_instrument("eth", risk_order(800000, 2, Side::Sell, 100, 2, true))
            .unwrap();
        actor
            .apply_to_instrument("eth", risk_order(800001, 1, Side::Buy, 100, 2, true))
            .unwrap();
        let AccountSnapshot::Perp(a) = actor.market("eth").unwrap().account_snapshot(1).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(a.position_qty, 0);
        assert!(a.has_open_position());
    }
    actor
        .apply_to_instrument("eth", risk_order(900001, 5, Side::Buy, 70, 2, hedge))
        .unwrap();
    actor
        .apply_to_instrument("eth", risk_order(900002, 6, Side::Sell, 130, 2, hedge))
        .unwrap();
    let mut ttl = risk_order(900003, 7, Side::Buy, 60, 1, hedge);
    if let Command::NewOrder(order) = &mut ttl {
        order.kind = OrderKind::Protected {
            order_type: crate::model::ProtectedOrderType::Limit,
            price_tick: 60,
            valid_until_market_time_ms: None,
            expires_at_market_time_ms: Some(1000),
        };
    }
    actor.apply_to_instrument("eth", ttl).unwrap();
    actor
}

#[test]
fn adaptive_risk_matches_frozen_scan_on_transactions_funding_and_restore() {
    for hedge in [false, true] {
        for linked in [false, true] {
            let mut optimized = adaptive_risk_fixture(128, 8, hedge, linked);
            let mut reference = optimized.clone();
            let mut funding = 0;
            let mut trades = 0;
            let mut risk_rejections = 0;
            for n in 0..256_u64 {
                let action = |actor: &mut ExchangeActor| {
                    let result = match n % 16 {
                        5 => actor.apply_to_instrument(
                            "eth",
                            Command::CancelOrder(CancelOrder { order_id: 900002 }),
                        ),
                        6 => actor.apply_to_instrument(
                            "eth",
                            Command::AmendOrder(crate::model::AmendOrder {
                                order_id: 900001,
                                price_tick: Some(71),
                                qty: Some(1),
                            }),
                        ),
                        7 => actor.apply_to_instrument(
                            "eth",
                            Command::ExpireOrder {
                                order_id: 900003,
                                market_time_ms: 1000,
                            },
                        ),
                        8 if linked => actor.apply_to_instrument(
                            "spot",
                            Command::AmendOrder(crate::model::AmendOrder {
                                order_id: 900010,
                                price_tick: Some(98 + (n % 2) as i64),
                                qty: Some(99 - n / 16),
                            }),
                        ),
                        8 => actor.apply_to_instrument(
                            "btc",
                            Command::SetMarkPrice(SetMarkPrice {
                                price_tick: 95 + (n % 9) as i64,
                            }),
                        ),
                        9 => actor.apply_to_instrument(
                            "eth",
                            Command::SetMarkPrice(SetMarkPrice {
                                price_tick: 100 + (n % 3) as i64,
                            }),
                        ),
                        10 => return format!("{:?}", actor.advance_clock(1)),
                        11 => return format!("{:?}", actor.submit_venue_deposit(3, "USDT", 1)),
                        12 => return format!("{:?}", actor.submit_venue_withdrawal(4, "USDT", 1)),
                        13 => return format!("{:?}", actor.create_account(1000 + n, 100_000)),
                        14 => {
                            actor
                                .market_mut("eth")
                                .unwrap()
                                .create_account(1, 1_000_000);
                            return format!(
                                "{:?}",
                                actor.sync_all_accounts_after_external_balance_change_inner()
                            );
                        }
                        15 => actor.apply_to_instrument(
                            "sol",
                            risk_order(1000 + n, 128, Side::Buy, 100, 10000, hedge),
                        ),
                        _ => actor.apply_to_instrument(
                            if n % 2 == 0 { "btc" } else { "sol" },
                            risk_order(
                                1000 + n,
                                n % 8 + 1,
                                if n % 4 < 2 { Side::Sell } else { Side::Buy },
                                100,
                                1,
                                hedge,
                            ),
                        ),
                    };
                    format!("{result:?}")
                };
                let a = action(&mut optimized);
                let b = crate::risk_scope::reference(|| action(&mut reference));
                assert_eq!(a, b, "action={n} hedge={hedge} linked={linked}");
                trades += a.matches("TradePrinted").count();
                risk_rejections += a.matches("Insufficient").count();
                assert_eq!(
                    canonical_actor(&optimized),
                    canonical_actor(&reference),
                    "state={n} hedge={hedge} linked={linked}"
                );
                let a = optimized.take_clock_executions();
                let b = reference.take_clock_executions();
                assert_eq!(a, b);
                funding += a.iter().filter(|e| e.funding_settlement.is_some()).count();
                if n % 64 == 63 {
                    optimized = serde_json::from_value(canonical_actor(&optimized)).unwrap();
                    reference = serde_json::from_value(canonical_actor(&reference)).unwrap();
                    let a = optimized.normalize_after_restore();
                    let b = crate::risk_scope::reference(|| reference.normalize_after_restore());
                    assert_eq!(a, b);
                    assert_eq!(canonical_actor(&optimized), canonical_actor(&reference));
                }
            }
            assert!(
                trades > 0 && risk_rejections > 0,
                "hedge={hedge} linked={linked} trades={trades} rejections={risk_rejections}"
            );
            if linked {
                assert!(funding > 0);
            }
        }
    }
}

#[test]
fn adaptive_risk_preserves_unknown_scopes_cold_repairs_and_failure_order() {
    let actor = adaptive_risk_fixture(128, 8, false, false);
    for malformed in [false, true] {
        let mut wire = canonical_actor(&actor);
        if malformed {
            wire["markets"]["btc"]["engine"]["Perp"]["accounts"]["accounts"]["1"]["reserved_margin"] =
                serde_json::json!(123);
        }
        let mut a: ExchangeActor = serde_json::from_value(wire).unwrap();
        let mut b = a.clone();
        for _ in 0..3 {
            let x = a.normalize_after_restore();
            let y = crate::risk_scope::reference(|| b.normalize_after_restore());
            assert_eq!(x, y);
            assert_eq!(canonical_actor(&a), canonical_actor(&b));
        }
        a.venue_accounts.set_balance(2, "USDT", 0);
        b.venue_accounts.set_balance(2, "USDT", 0);
        let before = canonical_actor(&a);
        let command = Command::SetMarkPrice(SetMarkPrice { price_tick: 999 });
        let x = a.apply_to_instrument("btc", command.clone());
        let y = crate::risk_scope::reference(|| b.apply_to_instrument("btc", command));
        assert_eq!(x, y);
        assert_eq!(canonical_actor(&a), canonical_actor(&b));
        // Sequence allocation is intentionally durable even when clearing rejects.
        let mut after = canonical_actor(&a);
        after["next_command_seq"] = before["next_command_seq"].clone();
        assert_eq!(after, before);
    }
}
