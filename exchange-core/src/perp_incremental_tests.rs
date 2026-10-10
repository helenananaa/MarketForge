#[test]
fn adaptive_mark_sparse_dense_same_price_and_context_match_full_scan() {
    for active in [2, 32, 128] {
        for hedge in [false, true] {
            let mut optimized = PerpAccountStore::new(
                PerpClearingConfig {
                    leverage: 10,
                    position_mode: if hedge {
                        PositionMode::Hedge
                    } else {
                        PositionMode::OneWay
                    },
                    ..PerpClearingConfig::default()
                },
                100,
            )
            .unwrap();
            for id in 1..=128 {
                optimized.create_account(id, 100_000);
            }
            for id in (1..=active).step_by(2) {
                let mut fill = trade(id, id, id + 1, 100, 2, Side::Buy);
                if hedge {
                    fill.maker_position_side = PositionSide::Short;
                    fill.taker_position_side = PositionSide::Long;
                }
                optimized.settle_trade(&fill).unwrap();
            }
            optimized.set_mark_price_tick(100).unwrap();
            let mut reference = optimized.clone();
            for n in 0..48_u64 {
                let operation = |store: &mut PerpAccountStore| -> Result<Vec<PerpClearingEvent>, ClearingError> {
                    match n % 8 {
                        0 => store.reserve_resting_order(10000 + n, 120, Side::Buy, 90, 3)?,
                        1 => store.amend_order_reservation(10000 + n - 1, 95, 1)?,
                        2 => store.release_order_reservation(10000 + n - 2)?,
                        3 => { store.sync_cross_margin_account(121, 100_000, PerpCrossMarginContext {
                            other_position_open: true, other_unrealized_pnl: -99_999,
                            other_initial_margin: 10, other_maintenance_margin: 5, ..PerpCrossMarginContext::default()
                        })?; }
                        4 => { store.create_account(200 + n, 1234); }
                        _ => {}
                    }
                    store.set_mark_price_tick(if n % 8 == 5 { store.mark_price_tick() } else { 90 + (n % 21) as i64 })
                };
                assert_eq!(
                    operation(&mut optimized),
                    crate::risk_scope::reference(|| operation(&mut reference)),
                    "n={n} active={active} hedge={hedge}"
                );
                assert_eq!(
                    serde_json::to_value(&optimized).unwrap(),
                    serde_json::to_value(&reference).unwrap()
                );
                assert_eq!(
                    optimized.liquidatable_account_ids(),
                    crate::risk_scope::reference(|| reference.liquidatable_account_ids())
                );
                if n == 24 {
                    optimized =
                        serde_json::from_value(serde_json::to_value(&optimized).unwrap()).unwrap();
                    reference =
                        serde_json::from_value(serde_json::to_value(&reference).unwrap()).unwrap();
                }
            }
        }
    }
}

#[test]
fn adaptive_mark_failed_prefix_stays_unknown_and_retries_identically() {
    let mut optimized = PerpAccountStore::new(
        PerpClearingConfig {
            leverage: 10,
            ..PerpClearingConfig::default()
        },
        100,
    )
    .unwrap();
    for id in 1..=128 {
        optimized.create_account(id, 100_000);
    }
    optimized
        .settle_trade(&trade(1, 1, 2, 100, 2, Side::Buy))
        .unwrap();
    optimized.set_mark_price_tick(100).unwrap();
    optimized.account_mut(2).cash_balance = i128::MAX;
    let mut reference = optimized.clone();
    for _ in 0..2 {
        assert_eq!(
            optimized.set_mark_price_tick(101),
            crate::risk_scope::reference(|| reference.set_mark_price_tick(101))
        );
        assert!(optimized.risk.mark_changes.accounts().is_none());
        assert!(optimized.risk.margin_changes.accounts().is_none());
        assert_eq!(
            optimized.accounts.iter().collect::<Vec<_>>(),
            reference.accounts.iter().collect::<Vec<_>>()
        );
        assert_eq!(optimized.mark_price_tick(), reference.mark_price_tick());
    }
    optimized.account_mut(2).cash_balance = 100_000;
    reference.account_mut(2).cash_balance = 100_000;
    assert_eq!(
        optimized.set_mark_price_tick(101),
        crate::risk_scope::reference(|| reference.set_mark_price_tick(101))
    );
    assert_eq!(
        serde_json::to_value(&optimized).unwrap(),
        serde_json::to_value(&reference).unwrap()
    );
}
#[test]
fn adaptive_mark_legacy_signed_hedge_legs_are_not_price_independent() {
    let mut optimized = PerpAccountStore::new(
        PerpClearingConfig {
            leverage: 10,
            position_mode: PositionMode::Hedge,
            ..PerpClearingConfig::default()
        },
        100,
    )
    .unwrap();
    for id in 1..=128 {
        optimized.create_account(id, 100_000);
    }
    // An old checkpoint can deserialize signed legs whose gross sum is zero.
    // Preserve the original arithmetic instead of classifying that as flat.
    let positions = optimized.account_mut(1).hedge_positions.as_mut().unwrap();
    positions.long.qty = 1;
    positions.long.avg_entry_price_tick = 100;
    positions.short.qty = -1;
    positions.short.avg_entry_price_tick = 100;
    let mut reference = optimized.clone();
    for mark in [100, 110, 110, 90] {
        assert_eq!(
            optimized.set_mark_price_tick(mark),
            crate::risk_scope::reference(|| reference.set_mark_price_tick(mark))
        );
        assert_eq!(
            optimized.sync_cross_margin_account(1, 100_000, PerpCrossMarginContext::default()),
            crate::risk_scope::reference(|| reference.sync_cross_margin_account(
                1,
                100_000,
                PerpCrossMarginContext::default()
            ))
        );
        assert_eq!(
            serde_json::to_value(&optimized).unwrap(),
            serde_json::to_value(&reference).unwrap()
        );
    }
}
