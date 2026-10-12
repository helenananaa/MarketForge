// Frozen predecessors for the two transaction-entry optimizations only.
fn transaction_entry_reference<T>(operation: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            FULL_TRANSACTION_ENTRY_REFERENCE.set(self.0);
        }
    }
    let _reset = Reset(FULL_TRANSACTION_ENTRY_REFERENCE.replace(true));
    operation()
}

pub(super) fn withdrawal_reject_reason_reference(
    actor: &ExchangeActor,
    account_id: AccountId,
    asset_id: &str,
    amount: Money,
) -> Option<VenueTransferRejectReason> {
    if amount <= 0 {
        return None;
    }
    let mut net_unrealized_pnl = 0i128;
    for market in actor.markets.values() {
        if market.config().instrument().quote_asset != asset_id {
            continue;
        }
        let AccountSnapshots::Perp(accounts) = market.account_snapshots() else {
            continue;
        };
        let Some(account) = accounts.into_iter().find(|a| a.account_id == account_id) else {
            continue;
        };
        net_unrealized_pnl = match net_unrealized_pnl.checked_add(account.unrealized_pnl) {
            Some(total) => total,
            None => return Some(VenueTransferRejectReason::BalanceOverflow),
        };
    }
    let loss = if net_unrealized_pnl < 0 {
        match net_unrealized_pnl.checked_neg() {
            Some(loss) => loss,
            None => return Some(VenueTransferRejectReason::BalanceOverflow),
        }
    } else {
        0
    };
    let available = actor
        .venue_accounts
        .balance_snapshot(account_id, asset_id)
        .map(|b| b.available)
        .unwrap_or_default();
    match available.checked_sub(loss) {
        Some(withdrawable) if withdrawable >= amount => None,
        Some(_) => Some(VenueTransferRejectReason::InsufficientAvailableBalance),
        None => Some(VenueTransferRejectReason::BalanceOverflow),
    }
}

fn entry_compare<T: std::fmt::Debug + PartialEq>(
    optimized: &mut ExchangeActor,
    reference: &mut ExchangeActor,
    operation: impl Fn(&mut ExchangeActor) -> T,
) -> T {
    let result = operation(optimized);
    let expected = transaction_entry_reference(|| operation(reference));
    assert_eq!(result, expected);
    assert_eq!(canonical_actor(optimized), canonical_actor(reference));
    result
}

fn entry_fixture(hedge: bool, liquidation: bool, waterfall: u8) -> ExchangeActor {
    let mut markets = vec![venue_spot_config("s", "BTC", "USDT")];
    for (id, quote) in [("p", "USDT"), ("q", "USDT"), ("r", "USDC")] {
        let mut config = venue_perp_config(id, "BTC", quote);
        if let MarketConfig::Perp(perp) = &mut config {
            if hedge {
                perp.clearing.position_mode = crate::PositionMode::Hedge;
            }
            if liquidation {
                perp.clearing.liquidation_fee_ppm = 10_000;
                perp.clearing.initial_insurance_fund = if waterfall == 1 { 50 } else { 0 };
                perp.clearing.socialized_loss_enabled = waterfall >= 2;
                perp.clearing.auto_deleveraging_enabled = waterfall == 3;
            }
        }
        markets.push(config);
    }
    let mut actor = ExchangeActor::new(
        "transaction-entry",
        ExchangeConfig::new("binance", markets).unwrap(),
    )
    .unwrap();
    for id in 1..=128 {
        actor.create_account(
            id,
            if id == 20 {
                if liquidation { 190 } else { 1000 }
            } else {
                10_000
            },
        );
        actor.venue_accounts.set_balance(id, "BTC", 100);
    }
    for (instrument, command) in [
        ("p", risk_order(1, 10, Side::Sell, 100, 10, hedge)),
        ("p", risk_order(2, 20, Side::Buy, 100, 10, hedge)),
    ] {
        assert!(matches!(
            actor
                .apply_to_instrument(instrument, command)
                .unwrap()
                .result,
            ActorExecutionResult::Accepted(_)
        ));
    }
    if liquidation && hedge {
        for command in [
            risk_order(3, 40, Side::Buy, 100, 6, true),
            risk_order(4, 20, Side::Sell, 100, 6, true),
        ] {
            assert!(matches!(
                actor.apply_to_instrument("p", command).unwrap().result,
                ActorExecutionResult::Accepted(_)
            ));
        }
    } else if !liquidation {
        for command in [
            risk_order(3, 11, Side::Buy, 100, 10, hedge),
            risk_order(4, 20, Side::Sell, 100, 10, hedge),
        ] {
            actor.apply_to_instrument("q", command).unwrap();
        }
        actor
            .apply_to_instrument("q", Command::SetMarkPrice(SetMarkPrice { price_tick: 102 }))
            .unwrap();
    }
    if liquidation {
        actor
            .apply_to_instrument("p", risk_order(5, 20, Side::Buy, 70, 1, hedge))
            .unwrap();
    }
    actor
        .apply_to_instrument(
            "p",
            Command::SetMarkPrice(SetMarkPrice {
                price_tick: if liquidation {
                    if hedge { 60 } else { 80 }
                } else {
                    95
                },
            }),
        )
        .unwrap();
    actor
}

#[test]
fn transaction_entries_withdrawal_lookup_preserves_cross_market_loss_and_asset_boundaries() {
    for hedge in [false, true] {
        let actor = entry_fixture(hedge, false, 0);
        for restored in [false, true] {
            let actor = if restored {
                let mut restored: ExchangeActor =
                    serde_json::from_value(canonical_actor(&actor)).unwrap();
                restored.normalize_after_restore().unwrap();
                restored
            } else {
                actor.clone()
            };
            // Two markets contribute -50 and -20; collateral is 200 plus
            // the short position's 2-unit mark-sensitive risk reservation.
            assert_eq!(
                actor.perp_withdrawal_reject_reason(20, "USDT", 728),
                None,
                "hedge={hedge} balance={:?} p={:?} q={:?}",
                actor.venue_balance_snapshot(20, "USDT"),
                actor.account_snapshot_for("p", 20),
                actor.account_snapshot_for("q", 20)
            );
            assert_eq!(
                actor.perp_withdrawal_reject_reason(20, "USDT", 729),
                Some(VenueTransferRejectReason::InsufficientAvailableBalance)
            );
            for id in [20, 128, 999] {
                for asset in ["USDT", "USDC", "BTC", "UNKNOWN"] {
                    for amount in [i128::MIN, -1, 0, 1, 730, 731, 10_001, i128::MAX] {
                        assert_eq!(
                            actor.perp_withdrawal_reject_reason(id, asset, amount),
                            transaction_entry_reference(
                                || actor.perp_withdrawal_reject_reason(id, asset, amount)
                            )
                        );
                    }
                }
            }
            // Mixed profit/loss remains a net buffer, not a sum of losses.
            let mut positive = actor.clone();
            positive
                .apply_to_instrument("q", Command::SetMarkPrice(SetMarkPrice { price_tick: 90 }))
                .unwrap();
            assert_eq!(
                positive.perp_withdrawal_reject_reason(20, "USDT", 800),
                None
            );
        }
    }
}

#[test]
fn transaction_entries_withdrawal_delay_restore_and_failed_sync_keep_money_and_transfer_ids() {
    for delay in [0, 2] {
        let mut actor = entry_fixture(false, false, 0);
        actor.venue_rules = VenueRuleEngine::new(VenueRuleConfig {
            transfers: TransferPolicyConfig {
                withdrawal_delay_steps: delay,
                ..TransferPolicyConfig::default()
            },
            ..VenueRuleConfig::default()
        })
        .unwrap();
        let mut reference = actor.clone();
        let blocked = entry_compare(&mut actor, &mut reference, |a| {
            a.submit_venue_withdrawal(20, "USDT", 729)
        });
        assert_eq!(blocked.unwrap().status, VenueTransferStatus::Rejected);
        let accepted = entry_compare(&mut actor, &mut reference, |a| {
            a.submit_venue_withdrawal(20, "USDT", 728)
        })
        .unwrap();
        assert_eq!(
            accepted.status,
            if delay == 0 {
                VenueTransferStatus::Completed
            } else {
                VenueTransferStatus::Pending
            }
        );
        actor = serde_json::from_value(canonical_actor(&actor)).unwrap();
        reference = serde_json::from_value(canonical_actor(&reference)).unwrap();
        entry_compare(
            &mut actor,
            &mut reference,
            ExchangeActor::normalize_after_restore,
        )
        .unwrap();
        entry_compare(&mut actor, &mut reference, |a| a.advance_clock(2)).unwrap();
        let balance = actor.venue_balance_snapshot(20, "USDT").unwrap();
        assert_eq!(
            (balance.total, balance.reserved, balance.available),
            (272, 202, 70)
        );
        assert_eq!(
            entry_compare(&mut actor, &mut reference, |a| a
                .submit_venue_withdrawal(20, "USDT", 1))
            .unwrap()
            .status,
            VenueTransferStatus::Rejected
        );
    }
    let mut actor = entry_fixture(false, false, 0);
    actor
        .venue_accounts
        .apply_signed_delta(127, "BTC", -101)
        .unwrap();
    let before = canonical_actor(&actor);
    let mut reference = actor.clone();
    assert_eq!(
        entry_compare(&mut actor, &mut reference, |a| a
            .submit_venue_withdrawal(20, "USDT", 1)),
        Err(ClearingError::InsufficientAvailableBalance)
    );
    assert_eq!(canonical_actor(&actor), before); // Includes transfers and next transfer id.
    for a in [&mut actor, &mut reference] {
        a.venue_accounts.apply_signed_delta(127, "BTC", 1).unwrap();
    }
    assert_eq!(
        entry_compare(&mut actor, &mut reference, |a| a
            .submit_venue_withdrawal(20, "USDT", 1))
        .unwrap()
        .transfer_id,
        0
    );
}

#[test]
fn transaction_entries_withdrawal_rejects_signed_balance_subtraction_overflow() {
    let mut actor = entry_fixture(false, false, 0);
    let reserved = actor.venue_balance_snapshot(20, "USDT").unwrap().reserved;
    actor.venue_accounts.release(20, "USDT", reserved).unwrap();
    actor
        .venue_accounts
        .apply_signed_delta(20, "USDT", -1000)
        .unwrap();
    actor
        .venue_accounts
        .apply_signed_delta(20, "USDT", i128::MIN)
        .unwrap();
    // A representable signed liability still cannot subtract the 70-unit loss.
    assert_eq!(
        actor.perp_withdrawal_reject_reason(20, "USDT", 1),
        Some(VenueTransferRejectReason::BalanceOverflow)
    );
    assert_eq!(
        transaction_entry_reference(|| actor.perp_withdrawal_reject_reason(20, "USDT", 1)),
        Some(VenueTransferRejectReason::BalanceOverflow)
    );
}

#[test]
fn transaction_entries_liquidation_partial_hedge_waterfall_and_restore_match_full_invalidation() {
    for hedge in [false, true] {
        for waterfall in 0..4 {
            let mut actor = entry_fixture(hedge, true, waterfall);
            if waterfall % 2 == 0 {
                actor = serde_json::from_value(canonical_actor(&actor)).unwrap();
                assert!(actor.venue_accounts.margin_changes("USDT").is_none());
            }
            let mut reference = actor.clone();
            let price = if hedge { 60 } else { 80 };
            // No depth: keep a pending liquidation, cancel the account's orders.
            let no_fill = entry_compare(&mut actor, &mut reference, |a| {
                a.liquidate_account("p", 20, 100)
            })
            .unwrap();
            assert!(matches!(no_fill.result, ActorExecutionResult::Accepted(_)));
            assert_eq!(
                actor.pending_liquidation_accounts_for("p").unwrap(),
                vec![20]
            );
            assert_eq!(actor.order_owner_for("p", 5).unwrap(), None);
            for a in [&mut actor, &mut reference] {
                *a = serde_json::from_value(canonical_actor(a)).unwrap();
            }
            entry_compare(
                &mut actor,
                &mut reference,
                ExchangeActor::normalize_after_restore,
            )
            .unwrap();
            entry_compare(&mut actor, &mut reference, |a| {
                a.apply_to_instrument("p", risk_order(101, 30, Side::Buy, price, 4, hedge))
            })
            .unwrap();
            entry_compare(&mut actor, &mut reference, |a| {
                a.liquidate_account("p", 20, 102)
            })
            .unwrap();
            let Some(AccountSnapshot::Perp(partial)) = actor.account_snapshot_for("p", 20).unwrap()
            else {
                panic!("perp")
            };
            assert!(partial.has_open_position());
            // Pending status blocks this account on a peer market too.
            let peer = entry_compare(&mut actor, &mut reference, |a| {
                a.apply_to_instrument("q", risk_order(103, 20, Side::Buy, 100, 1, hedge))
            })
            .unwrap();
            let ActorExecutionResult::Accepted(MarketExecution::Perp(peer)) = peer.result else {
                panic!("perp")
            };
            assert!(
                peer.events
                    .iter()
                    .any(|e| matches!(e.event, Event::RiskRejected { .. }))
            );
            entry_compare(&mut actor, &mut reference, |a| {
                a.apply_to_instrument("q", risk_order(104, 128, Side::Buy, 100, 1, hedge))
            })
            .unwrap();
            entry_compare(&mut actor, &mut reference, |a| {
                a.apply_to_instrument("p", risk_order(105, 30, Side::Buy, price, 6, hedge))
            })
            .unwrap();
            let mut settlements = 0;
            let mut waterfall_amounts = None;
            for (id, side) in [(106, Side::Sell), (108, Side::Buy)] {
                if side == Side::Buy {
                    entry_compare(&mut actor, &mut reference, |a| {
                        a.apply_to_instrument("p", risk_order(107, 50, Side::Sell, price, 6, hedge))
                    })
                    .unwrap();
                }
                let execution = entry_compare(&mut actor, &mut reference, |a| {
                    a.liquidate_account("p", 20, id)
                })
                .unwrap();
                if let ActorExecutionResult::Accepted(MarketExecution::Perp(p)) = execution.result {
                    settlements += p
                        .clearing_events
                        .iter()
                        .filter(|e| matches!(e, PerpClearingEvent::LiquidationSettled { .. }))
                        .count();
                    for event in &p.clearing_events {
                        if let PerpClearingEvent::LiquidationSettled {
                            liquidation_fee,
                            insurance_fund_payment,
                            auto_deleveraging_loss,
                            socialized_loss,
                            bad_debt,
                            ..
                        } = event
                        {
                            waterfall_amounts = Some((
                                *liquidation_fee,
                                *insurance_fund_payment,
                                *auto_deleveraging_loss,
                                *socialized_loss,
                                *bad_debt,
                            ));
                        }
                    }
                }
                if actor
                    .pending_liquidation_accounts_for("p")
                    .unwrap()
                    .is_empty()
                {
                    break;
                }
            }
            assert_eq!(settlements, 1); // Waterfall is charged once after all IOC legs.
            assert_eq!(
                waterfall_amounts,
                Some(if hedge {
                    (9, 0, 0, 0, 0)
                } else {
                    match waterfall {
                        0 => (8, 8, 0, 0, 10),
                        1 => (8, 18, 0, 0, 0),
                        2 => (8, 8, 0, 10, 0),
                        _ => (8, 8, 10, 0, 0),
                    }
                })
            );
            let Some(AccountSnapshot::Perp(flat)) = actor.account_snapshot_for("p", 20).unwrap()
            else {
                panic!("perp")
            };
            assert!(!flat.has_open_position());
            assert!(
                actor
                    .pending_liquidation_accounts_for("p")
                    .unwrap()
                    .is_empty()
            );
            entry_compare(&mut actor, &mut reference, |a| {
                a.apply_to_instrument("r", risk_order(110, 128, Side::Buy, 100, 1, hedge))
            })
            .unwrap();
            entry_compare(&mut actor, &mut reference, |a| {
                a.submit_venue_withdrawal(10, "USDT", 1)
            })
            .unwrap();
        }
    }
}

#[test]
fn transaction_entries_liquidation_failure_rolls_back_settlement_and_public_mutation_stays_unknown()
{
    let mut actor = entry_fixture(false, true, 0);
    let mut reference = actor.clone();
    let healthy = entry_compare(&mut actor, &mut reference, |a| {
        a.liquidate_account("p", 127, 100)
    })
    .unwrap();
    assert!(matches!(
        healthy.result,
        ActorExecutionResult::Rejected(ActorRejectReason::Clearing(
            ClearingError::AccountNotLiquidatable
        ))
    ));
    let before_missing = canonical_actor(&actor);
    assert!(actor.liquidate_account("missing", 20, 101).is_err());
    assert_eq!(canonical_actor(&actor), before_missing);
    for a in [&mut actor, &mut reference] {
        a.market_mut("q").unwrap().create_account(127, 7);
        assert!(a.venue_accounts.margin_changes("USDT").is_none());
        assert!(a.venue_accounts.reservation_changes.accounts().is_none());
    }
    entry_compare(&mut actor, &mut reference, |a| {
        a.apply_to_instrument("p", limit(102, 30, Side::Buy, 80, 10))
    })
    .unwrap();
    assert_eq!(actor.order_owner_for("p", 102).unwrap(), Some(30));
    for a in [&mut actor, &mut reference] {
        a.venue_accounts
            .apply_signed_delta(126, "BTC", -101)
            .unwrap();
    }
    let mut expected = canonical_actor(&actor);
    expected["next_command_seq"] = (actor.next_command_seq + 1).into();
    let failed = entry_compare(&mut actor, &mut reference, |a| {
        a.liquidate_account("p", 20, 103)
    })
    .unwrap();
    assert!(matches!(
        failed.result,
        ActorExecutionResult::Rejected(ActorRejectReason::Clearing(
            ClearingError::InsufficientAvailableBalance
        ))
    ));
    assert_eq!(canonical_actor(&actor), expected); // Only the venue command sequence is consumed.
    for a in [&mut actor, &mut reference] {
        a.venue_accounts.apply_signed_delta(126, "BTC", 1).unwrap();
    }
    let retried = entry_compare(&mut actor, &mut reference, |a| {
        a.liquidate_account("p", 20, 104)
    })
    .unwrap();
    assert!(matches!(retried.result, ActorExecutionResult::Accepted(_)));
    assert!(
        actor
            .pending_liquidation_accounts_for("p")
            .unwrap()
            .is_empty()
    );
    entry_compare(&mut actor, &mut reference, |a| {
        a.apply_to_instrument("q", limit(105, 127, Side::Buy, 100, 1))
    })
    .unwrap();
}

#[test]
fn transaction_entries_funding_deficit_liquidation_and_cold_restore_keep_liability_semantics() {
    let mut actor = crate::funding_tests::scenario(200_000)
        .bootstrap()
        .unwrap()
        .exchange;
    for id in 1..=128 {
        if ![10, 20, 30].contains(&id) {
            actor.create_account(id, 100_000);
        }
    }
    actor.create_account(20, 1000);
    actor
        .apply_to_instrument("V-USD-PERP", limit(3, 10, Side::Sell, 10000, 1))
        .unwrap();
    actor
        .apply_to_instrument("V-USD-PERP", limit(4, 20, Side::Buy, 10000, 1))
        .unwrap();
    actor
        .apply_to_instrument("V-USD-PERP", limit(5, 30, Side::Buy, 9999, 10))
        .unwrap();
    actor
        .apply_to_instrument("V-USD-PERP", limit(6, 30, Side::Sell, 10001, 10))
        .unwrap();
    let mut reference = actor.clone();
    entry_compare(&mut actor, &mut reference, |a| a.advance_clock(2)).unwrap();
    assert!(
        actor.venue_balance_snapshot(20, "USD").unwrap().total < 0,
        "balance={:?} p={:?} funding={:?}",
        actor.venue_balance_snapshot(20, "USD"),
        actor.account_snapshot_for("V-USD-PERP", 20),
        actor.funding_states
    );
    assert_eq!(
        entry_compare(&mut actor, &mut reference, |a| a
            .submit_venue_withdrawal(20, "USD", 1))
        .unwrap()
        .status,
        VenueTransferStatus::Rejected
    );
    for a in [&mut actor, &mut reference] {
        *a = serde_json::from_value(canonical_actor(a)).unwrap();
    }
    assert!(actor.venue_accounts.margin_changes("USD").is_none());
    entry_compare(
        &mut actor,
        &mut reference,
        ExchangeActor::normalize_after_restore,
    )
    .unwrap();
    let execution = entry_compare(&mut actor, &mut reference, |a| {
        a.liquidate_account("V-USD-PERP", 20, 7)
    })
    .unwrap();
    assert!(matches!(
        execution.result,
        ActorExecutionResult::Accepted(_)
    ));
    assert_eq!(actor.venue_balance_snapshot(20, "USD").unwrap().total, 0);
    assert!(
        !actor
            .venue_validation_baseline
            .as_ref()
            .unwrap()
            .underfunded
            .contains(&20)
    );
    entry_compare(&mut actor, &mut reference, |a| a.advance_clock(2)).unwrap();
}
