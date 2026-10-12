// Included in actor::tests; compare only the two account-scope optimizations
// against their full-scan predecessors, including exact execution receipts.
fn account_scope_reference<T>(operation: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            FULL_ACCOUNT_SCOPE_REFERENCE.set(self.0);
        }
    }
    let _reset = Reset(FULL_ACCOUNT_SCOPE_REFERENCE.replace(true));
    operation()
}

fn account_scope_fixture() -> ExchangeActor {
    let mut actor = ExchangeActor::new(
        "account-scopes",
        ExchangeConfig::new(
            "binance",
            vec![
                venue_spot_config("a", "BTC", "USDT"),
                venue_spot_config("b", "BTC", "USDT"),
                venue_perp_config("p", "ETH", "USDT"),
            ],
        )
        .unwrap(),
    )
    .unwrap();
    for id in 1..=128 {
        actor.create_account(id, if id == 128 { 0 } else { 1_000_000 });
        actor.venue_accounts.set_balance(id, "BTC", 1000);
    }
    actor
}

#[test]
fn account_scopes_match_full_on_peer_clearing_transfers_new_accounts_and_restore() {
    let mut optimized = account_scope_fixture();
    let mut reference = optimized.clone();
    let mut trades = 0;
    let mut rejections = 0;
    for n in 0..192_u64 {
        let action = |actor: &mut ExchangeActor| {
            match n % 12 {
                6 => {
                    actor.apply_venue_asset_delta(17, "USDT", -20).unwrap();
                }
                7 => {
                    actor.venue_accounts.reserve(37, "USDT", 3).unwrap();
                }
                8 => {
                    actor.create_account(200 + n, 5000);
                }
                9 => {
                    actor.market_mut("b").unwrap().create_account(41, 1234);
                }
                10 => {
                    *actor = serde_json::from_value(canonical_actor(actor)).unwrap();
                    assert!(actor.spot_sync_baselines.is_empty());
                    assert!(actor.venue_validation_baseline.is_none());
                    actor.normalize_after_restore().unwrap();
                }
                _ => {}
            }
            let (instrument, command) = match n % 12 {
                0 => ("a", limit(1000 + n, 1, Side::Buy, 90, 1)),
                1 => ("b", limit(1000 + n, 1, Side::Buy, 80, 1)),
                2 => ("a", limit(1000 + n, 2, Side::Sell, 90, 1)),
                3 => ("b", limit(1000 + n, 2, Side::Sell, 80, 1)),
                4 => ("p", limit(1000 + n, 1, Side::Buy, 70, 1)),
                5 => ("p", Command::CancelOrder(CancelOrder { order_id: 999 + n })),
                11 => ("a", limit(1000 + n, 128, Side::Buy, 90, 1)),
                _ => (
                    "b",
                    Command::CancelOrder(CancelOrder {
                        order_id: 99_000 + n,
                    }),
                ),
            };
            actor.apply_to_instrument(instrument, command).unwrap()
        };
        let a = action(&mut optimized);
        let b = account_scope_reference(|| action(&mut reference));
        assert_eq!(a, b, "receipt n={n}");
        assert_eq!(
            canonical_actor(&optimized),
            canonical_actor(&reference),
            "state n={n}"
        );
        trades += format!("{a:?}").matches("TradePrinted").count();
        rejections += format!("{a:?}").matches("Insufficient").count();
    }
    assert!(trades > 0 && rejections > 0);
}

#[test]
fn account_scopes_visit_only_changes_and_restored_inputs_rebuild_full_baselines() {
    let mut actor = account_scope_fixture();
    actor
        .apply_to_instrument("a", limit(1, 1, Side::Buy, 90, 1))
        .unwrap();
    SPOT_SCOPE_VISITS.set(0);
    VENUE_SCOPE_VISITS.set(0);
    actor
        .apply_to_instrument("a", Command::CancelOrder(CancelOrder { order_id: 1 }))
        .unwrap();
    assert!(SPOT_SCOPE_VISITS.get() <= 2);
    assert!(VENUE_SCOPE_VISITS.get() <= 4);
    let wire = canonical_actor(&actor);
    assert!(wire.get("spot_sync_baselines").is_none());
    assert!(wire.get("venue_validation_baseline").is_none());
    let mut restored: ExchangeActor = serde_json::from_value(wire).unwrap();
    SPOT_SCOPE_VISITS.set(0);
    VENUE_SCOPE_VISITS.set(0);
    restored
        .apply_to_instrument("a", limit(2, 1, Side::Buy, 90, 1))
        .unwrap();
    assert_eq!(SPOT_SCOPE_VISITS.get(), 128);
    assert_eq!(VENUE_SCOPE_VISITS.get(), 256);
}

#[test]
fn account_scopes_reject_dormant_invalid_balances_and_do_not_advance_failed_baselines() {
    let mut actor = account_scope_fixture();
    actor.validate_venue_after_clearing().unwrap();
    actor
        .venue_accounts
        .apply_signed_delta(127, "BTC", -1001)
        .unwrap();
    let before = canonical_actor(&actor);
    let baseline = actor
        .venue_validation_baseline
        .as_ref()
        .unwrap()
        .wallet
        .clone();
    assert_eq!(
        actor.validate_venue_after_clearing(),
        Err(ClearingError::InsufficientAvailableBalance)
    );
    assert_eq!(canonical_actor(&actor), before);
    assert_eq!(
        actor
            .venue_accounts
            .changed_wallet_accounts(&actor.venue_validation_baseline.as_ref().unwrap().wallet),
        vec![127]
    );
    assert_eq!(
        actor.venue_accounts.changed_wallet_accounts(&baseline),
        vec![127]
    );
    // Unknown/restored state must also reject corruption in an uninvolved account.
    let mut restored: ExchangeActor = serde_json::from_value(before).unwrap();
    let mut full = restored.clone();
    let a = restored
        .apply_to_instrument("a", limit(100, 1, Side::Buy, 90, 1))
        .unwrap();
    let b = account_scope_reference(|| {
        full.apply_to_instrument("a", limit(100, 1, Side::Buy, 90, 1))
            .unwrap()
    });
    assert_eq!(a, b);
    assert!(matches!(
        a.result,
        ActorExecutionResult::Rejected(ActorRejectReason::Clearing(_))
    ));
    assert!(restored.spot_sync_baselines.is_empty());
    assert!(restored.venue_validation_baseline.is_none());
    assert_eq!(canonical_actor(&restored), canonical_actor(&full));
    actor
        .venue_accounts
        .apply_signed_delta(127, "BTC", 1001)
        .unwrap();
    actor.validate_venue_after_clearing().unwrap();
}

#[test]
fn account_scopes_recheck_funding_exception_after_positions_close_without_cash_change() {
    for hedge in [false, true] {
        let mut actor = adaptive_risk_fixture(128, 8, hedge, true);
        actor
            .venue_accounts
            .apply_signed_delta(1, "USDT", -2_000_000)
            .unwrap();
        actor.validate_venue_after_clearing().unwrap();
        assert!(
            actor
                .venue_validation_baseline
                .as_ref()
                .unwrap()
                .underfunded
                .contains(&1)
        );
        // Change only position inputs to remove the previously allowed liability.
        // Keep the wallet root identical to exercise the exception revisit path.
        for market in actor.markets.values_mut() {
            if market.kind() != MarketKind::Perp {
                continue;
            }
            let mut wire = serde_json::to_value(&*market).unwrap();
            let account = &mut wire["engine"]["Perp"]["accounts"]["accounts"]["1"];
            account["position_qty"] = 0.into();
            account["avg_entry_price_tick"] = 0.into();
            account["hedge_positions"] = serde_json::Value::Null;
            *market = serde_json::from_value(wire).unwrap();
        }
        let before = &actor.venue_validation_baseline.as_ref().unwrap().wallet;
        assert!(
            actor
                .venue_accounts
                .changed_wallet_accounts(before)
                .is_empty()
        );
        let mut full = actor.clone();
        assert_eq!(
            actor.validate_venue_after_clearing(),
            Err(ClearingError::InsufficientAvailableBalance)
        );
        assert_eq!(
            account_scope_reference(|| full.validate_venue_after_clearing()),
            Err(ClearingError::InsufficientAvailableBalance)
        );
    }
}
