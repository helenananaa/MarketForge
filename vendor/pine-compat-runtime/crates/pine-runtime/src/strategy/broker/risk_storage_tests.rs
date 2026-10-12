use super::risk::{
    RiskAdmission, RiskDrawdownType, RiskEntryDirection, RiskRuleKind, RiskWindowKey,
    intraday_window_key, trading_day_key,
};
use super::*;

const INTRADAY_TF_SECONDS: i64 = 60;
const DAILY_TF_SECONDS: i64 = 86_400;
const TWO_DAY_TF_SECONDS: i64 = 172_800;
const WEEKLY_TF_SECONDS: i64 = 604_800;
const UTC_DAY_MS: i64 = 86_400_000;

#[test]
fn risk_rules_and_tripped_state_clone_and_restore() {
    let mut broker = BrokerState::new(100_000.0);
    broker.record_risk_rule_call(
        RiskRuleKind::MaxPositionSize,
        None,
        None,
        None,
        Some(2.0),
        None,
    );
    broker.record_risk_rule_call(
        RiskRuleKind::AllowEntryIn,
        Some(RiskEntryDirection::Long),
        None,
        None,
        None,
        None,
    );
    broker.record_risk_rule_call(
        RiskRuleKind::MaxIntradayLoss,
        Some(RiskEntryDirection::Short),
        None,
        Some(5.0),
        None,
        None,
    );
    broker.record_risk_rule_call(
        RiskRuleKind::AllowEntryIn,
        Some(RiskEntryDirection::All),
        None,
        None,
        None,
        None,
    );
    assert_eq!(broker.risk_rules().max_position_size, Some(2.0));
    assert_eq!(broker.risk_rules().max_intraday_loss, Some(5.0));
    assert_eq!(
        broker.risk_rules().allow_entry_direction,
        Some(RiskEntryDirection::All)
    );

    let snapshot = broker.snapshot();
    broker.trip_risk_rule(RiskRuleKind::MaxDrawdown);
    assert!(broker.risk_state().blocked_order_placement);
    assert!(
        broker
            .risk_state()
            .tripped_rules
            .contains(&RiskRuleKind::MaxDrawdown)
    );
    assert!(!snapshot.risk_state().blocked_order_placement);

    let mut restored = snapshot;
    restored.restore(broker.snapshot());
    assert!(restored.risk_state().blocked_order_placement);
    restored.restore(BrokerState::new(100_000.0).snapshot());
    assert!(!restored.risk_state().blocked_order_placement);
    assert!(restored.risk_rules().max_position_size.is_none());
}

#[test]
fn tripped_risk_state_rejects_later_entry_admission() {
    let mut broker = BrokerState::new(100_000.0);
    broker.trip_risk_rule(RiskRuleKind::MaxDrawdown);
    assert_eq!(broker.check_risk_before_order(), RiskAdmission::Reject);
    assert!(!broker.entry_long("L".to_owned(), 0, 10, 100.0, 1.0));
    assert_eq!(broker.position_size, 0.0);
    assert!(broker.result().orders.is_empty());
}

#[test]
fn unconfigured_risk_hooks_leave_current_entry_behavior_unchanged() {
    let mut broker = BrokerState::new(100_000.0);
    assert_eq!(broker.check_risk_before_order(), RiskAdmission::Allow);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 1.0));
    assert_eq!(broker.position_size, 1.0);
    assert_eq!(broker.risk_state().intraday_filled_orders, 1);
}

#[test]
fn after_fill_hook_counts_fills_and_can_trip_configured_limit() {
    let mut broker = BrokerState::new(100_000.0);
    broker.record_risk_rule_call(
        RiskRuleKind::MaxIntradayFilledOrders,
        None,
        None,
        None,
        None,
        Some(1),
    );
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 1.0));
    assert_eq!(broker.risk_state().intraday_filled_orders, 1);
    assert!(
        broker
            .risk_state()
            .tripped_rules
            .contains(&RiskRuleKind::MaxIntradayFilledOrders)
    );
    broker.evaluate_risk_equity_stops(0, 10, 100.0);
    assert_eq!(broker.position_size, 0.0);
    assert_eq!(broker.check_risk_before_order(), RiskAdmission::Reject);
    broker.place_pending_market_long_entry("A".to_owned(), 1.0, 1);
    assert_eq!(broker.pending_entry_count(), 0);
}

#[test]
fn intraday_reset_clears_window_counters_but_keeps_permanent_trip() {
    let mut broker = BrokerState::new(100_000.0);
    broker.reset_intraday_risk_state(0, 0, INTRADAY_TF_SECONDS, 100_000.0);
    broker.check_risk_after_fill(0, 10, 100.0);
    assert_eq!(broker.risk_state().intraday_filled_orders, 1);
    broker.trip_risk_rule(RiskRuleKind::MaxDrawdown);
    broker.reset_intraday_risk_state(0, UTC_DAY_MS, INTRADAY_TF_SECONDS, 90_000.0);
    assert_eq!(
        broker.risk_state().trading_day_key,
        Some(RiskWindowKey::Utc(trading_day_key(UTC_DAY_MS)))
    );
    assert_eq!(broker.risk_state().intraday_filled_orders, 0);
    assert_eq!(broker.risk_state().intraday_equity_baseline, Some(90_000.0));
    assert!(broker.risk_state().blocked_order_placement);
    assert!(
        broker
            .risk_state()
            .tripped_rules
            .contains(&RiskRuleKind::MaxDrawdown)
    );
}

#[test]
fn intraday_window_key_uses_utc_day_for_intraday_and_daily_timeframes() {
    assert_eq!(
        intraday_window_key(10, INTRADAY_TF_SECONDS),
        trading_day_key(10)
    );
    assert_eq!(
        intraday_window_key(10, INTRADAY_TF_SECONDS),
        intraday_window_key(20, INTRADAY_TF_SECONDS)
    );
    assert_ne!(
        intraday_window_key(0, INTRADAY_TF_SECONDS),
        intraday_window_key(UTC_DAY_MS, INTRADAY_TF_SECONDS)
    );
    assert_eq!(
        intraday_window_key(10, DAILY_TF_SECONDS),
        trading_day_key(10)
    );
    assert_eq!(
        intraday_window_key(10, DAILY_TF_SECONDS),
        intraday_window_key(20, DAILY_TF_SECONDS)
    );
}

#[test]
fn intraday_window_key_uses_bar_time_when_timeframe_higher_than_daily() {
    assert_eq!(intraday_window_key(10, TWO_DAY_TF_SECONDS), 10);
    assert_eq!(intraday_window_key(20, WEEKLY_TF_SECONDS), 20);
    assert_ne!(
        intraday_window_key(10, TWO_DAY_TF_SECONDS),
        intraday_window_key(20, TWO_DAY_TF_SECONDS)
    );
}

#[test]
fn intraday_window_key_non_positive_timeframe_fail_closes_to_utc_day() {
    assert_eq!(intraday_window_key(10, 0), trading_day_key(10));
    assert_eq!(intraday_window_key(20, -1), trading_day_key(20));
    assert_eq!(intraday_window_key(10, 0), intraday_window_key(20, 0));
    assert_eq!(intraday_window_key(10, i64::MIN), trading_day_key(10));
}

#[test]
fn same_intraday_window_keeps_counters_and_equity_baseline() {
    let mut broker = BrokerState::new(100_000.0);
    broker.reset_intraday_risk_state(0, 0, INTRADAY_TF_SECONDS, 100_000.0);
    broker.check_risk_after_fill(0, 10, 100.0);
    assert_eq!(broker.risk_state().intraday_filled_orders, 1);
    assert_eq!(
        broker.risk_state().intraday_equity_baseline,
        Some(100_000.0)
    );
    broker.reset_intraday_risk_state(0, 1_000, INTRADAY_TF_SECONDS, 99_000.0);
    assert_eq!(broker.risk_state().intraday_filled_orders, 1);
    assert_eq!(
        broker.risk_state().intraday_equity_baseline,
        Some(100_000.0)
    );
    assert_eq!(
        broker.risk_state().trading_day_key,
        Some(RiskWindowKey::Utc(trading_day_key(0)))
    );
}

#[test]
fn ordinary_session_reset_seeds_baseline_and_clears_counters() {
    let mut broker = BrokerState::new(100_000.0);
    broker.reset_intraday_risk_state(0, 10, INTRADAY_TF_SECONDS, 100_000.0);
    broker.check_risk_after_fill(0, 10, 100.0);
    broker.check_risk_after_fill(0, 10, 100.0);
    assert_eq!(broker.risk_state().intraday_filled_orders, 2);
    broker.reset_intraday_risk_state(0, 20, INTRADAY_TF_SECONDS, 99_000.0);
    assert_eq!(broker.risk_state().intraday_filled_orders, 2);
    broker.reset_intraday_risk_state(0, UTC_DAY_MS + 10, INTRADAY_TF_SECONDS, 98_000.0);
    assert_eq!(broker.risk_state().intraday_filled_orders, 0);
    assert_eq!(broker.risk_state().intraday_equity_baseline, Some(98_000.0));
    assert_eq!(
        broker.risk_state().trading_day_key,
        Some(RiskWindowKey::Utc(trading_day_key(UTC_DAY_MS + 10)))
    );
}

#[test]
fn missing_bar_gap_resets_intraday_window() {
    let mut broker = BrokerState::new(100_000.0);
    broker.reset_intraday_risk_state(0, 0, INTRADAY_TF_SECONDS, 100_000.0);
    broker.check_risk_after_fill(0, 10, 100.0);
    assert_eq!(broker.risk_state().intraday_filled_orders, 1);
    broker.reset_intraday_risk_state(0, 2 * UTC_DAY_MS, INTRADAY_TF_SECONDS, 97_000.0);
    assert_eq!(broker.risk_state().intraday_filled_orders, 0);
    assert_eq!(broker.risk_state().intraday_equity_baseline, Some(97_000.0));
    assert_eq!(
        broker.risk_state().trading_day_key,
        Some(RiskWindowKey::Utc(trading_day_key(2 * UTC_DAY_MS)))
    );
}

#[test]
fn higher_than_daily_timeframe_resets_each_bar_on_same_utc_day() {
    let mut broker = BrokerState::new(100_000.0);
    broker.reset_intraday_risk_state(0, 10, WEEKLY_TF_SECONDS, 100_000.0);
    broker.check_risk_after_fill(0, 10, 100.0);
    assert_eq!(broker.risk_state().intraday_filled_orders, 1);
    assert_eq!(
        broker.risk_state().trading_day_key,
        Some(RiskWindowKey::Utc(10))
    );
    broker.reset_intraday_risk_state(0, 20, WEEKLY_TF_SECONDS, 99_000.0);
    assert_eq!(broker.risk_state().intraday_filled_orders, 0);
    assert_eq!(broker.risk_state().intraday_equity_baseline, Some(99_000.0));
    assert_eq!(
        broker.risk_state().trading_day_key,
        Some(RiskWindowKey::Utc(20))
    );
}

#[test]
fn window_scoped_trip_clears_on_intraday_reset() {
    let mut broker = BrokerState::new(100_000.0);
    broker.record_risk_rule_call(
        RiskRuleKind::MaxIntradayFilledOrders,
        None,
        None,
        None,
        None,
        Some(1),
    );
    broker.reset_intraday_risk_state(0, 0, INTRADAY_TF_SECONDS, 100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 1.0));
    assert!(
        broker
            .risk_state()
            .tripped_rules
            .contains(&RiskRuleKind::MaxIntradayFilledOrders)
    );
    assert_eq!(broker.check_risk_before_order(), RiskAdmission::Reject);
    broker.reset_intraday_risk_state(0, UTC_DAY_MS, INTRADAY_TF_SECONDS, 100_000.0);
    assert!(
        !broker
            .risk_state()
            .tripped_rules
            .contains(&RiskRuleKind::MaxIntradayFilledOrders)
    );
    assert!(!broker.risk_state().blocked_order_placement);
    assert_eq!(broker.check_risk_before_order(), RiskAdmission::Allow);
    assert_eq!(broker.risk_state().intraday_filled_orders, 0);
}

#[test]
fn non_finite_equity_does_not_seed_intraday_baseline() {
    let mut broker = BrokerState::new(100_000.0);
    broker.reset_intraday_risk_state(0, 0, INTRADAY_TF_SECONDS, f64::NAN);
    assert_eq!(broker.risk_state().intraday_equity_baseline, None);
    broker.reset_intraday_risk_state(0, UTC_DAY_MS, INTRADAY_TF_SECONDS, f64::INFINITY);
    assert_eq!(broker.risk_state().intraday_equity_baseline, None);
    broker.reset_intraday_risk_state(0, 2 * UTC_DAY_MS, INTRADAY_TF_SECONDS, 100_000.0);
    assert_eq!(
        broker.risk_state().intraday_equity_baseline,
        Some(100_000.0)
    );
    broker.reset_intraday_risk_state(0, 3 * UTC_DAY_MS, INTRADAY_TF_SECONDS, f64::NAN);
    assert_eq!(broker.risk_state().intraday_equity_baseline, None);
}

#[test]
fn non_positive_timeframe_reset_stays_on_utc_day() {
    let mut broker = BrokerState::new(100_000.0);
    broker.reset_intraday_risk_state(0, 10, 0, 100_000.0);
    broker.check_risk_after_fill(0, 10, 100.0);
    broker.reset_intraday_risk_state(0, 20, -5, 99_000.0);
    assert_eq!(broker.risk_state().intraday_filled_orders, 1);
    assert_eq!(
        broker.risk_state().intraday_equity_baseline,
        Some(100_000.0)
    );
}

#[test]
fn intraday_window_snapshot_restores_key_baseline_and_counters() {
    let mut broker = BrokerState::new(100_000.0);
    broker.reset_intraday_risk_state(0, 0, INTRADAY_TF_SECONDS, 100_000.0);
    broker.check_risk_after_fill(0, 10, 100.0);
    let snapshot = broker.snapshot();
    broker.reset_intraday_risk_state(0, UTC_DAY_MS, INTRADAY_TF_SECONDS, 90_000.0);
    assert_eq!(broker.risk_state().intraday_filled_orders, 0);
    let mut restored = BrokerState::new(100_000.0);
    restored.restore(snapshot);
    assert_eq!(restored.risk_state().intraday_filled_orders, 1);
    assert_eq!(
        restored.risk_state().intraday_equity_baseline,
        Some(100_000.0)
    );
    assert_eq!(
        restored.risk_state().trading_day_key,
        Some(RiskWindowKey::Utc(trading_day_key(0)))
    );
}

#[test]
fn forced_close_hook_is_reachable_from_margin_evaluation() {
    let mut broker = BrokerState::new(100_000.0);
    broker.trip_risk_rule(RiskRuleKind::MaxDrawdown);
    assert_eq!(
        broker.check_risk_before_forced_close(),
        RiskAdmission::Reject
    );
    broker.evaluate_margin_call_long(1, 20, 1.0);
}

#[test]
fn allow_entry_in_long_permits_long_and_rejects_flat_short_entry() {
    let mut broker = BrokerState::new(100_000.0);
    broker.record_risk_rule_call(
        RiskRuleKind::AllowEntryIn,
        Some(RiskEntryDirection::Long),
        None,
        None,
        None,
        None,
    );
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 1.0));
    assert_eq!(broker.position_size, 1.0);

    let mut flat = BrokerState::new(100_000.0);
    flat.record_risk_rule_call(
        RiskRuleKind::AllowEntryIn,
        Some(RiskEntryDirection::Long),
        None,
        None,
        None,
        None,
    );
    assert!(!flat.entry_short("S".to_owned(), 0, 10, 100.0, 1.0));
    assert_eq!(flat.position_size, 0.0);
    flat.place_pending_market_short_entry("S".to_owned(), 1.0, 0);
    assert_eq!(flat.pending_entry_count(), 0);
}

#[test]
fn allow_entry_in_long_converts_opposite_entry_to_close_only() {
    let mut broker = BrokerState::new(100_000.0);
    broker.record_risk_rule_call(
        RiskRuleKind::AllowEntryIn,
        Some(RiskEntryDirection::Long),
        None,
        None,
        None,
        None,
    );
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    assert!(broker.entry_short("S".to_owned(), 1, 20, 110.0, 1.0));
    assert_eq!(broker.position_size, 0.0);
    assert!(broker.result().trades.iter().any(|trade| trade.id == "L"));
    assert!(
        !broker
            .result()
            .orders
            .iter()
            .any(|order| order.direction == "strategy.short")
    );
}

#[test]
fn allow_entry_in_short_converts_opposite_entry_to_close_only() {
    let mut broker = BrokerState::new(100_000.0);
    broker.record_risk_rule_call(
        RiskRuleKind::AllowEntryIn,
        Some(RiskEntryDirection::Short),
        None,
        None,
        None,
        None,
    );
    assert!(broker.entry_short("S".to_owned(), 0, 10, 100.0, 2.0));
    assert!(broker.entry_long("L".to_owned(), 1, 20, 110.0, 1.0));
    assert_eq!(broker.position_size, 0.0);
}

#[test]
fn allow_entry_in_does_not_affect_generic_orders() {
    let mut broker = BrokerState::new(100_000.0);
    broker.record_risk_rule_call(
        RiskRuleKind::AllowEntryIn,
        Some(RiskEntryDirection::Long),
        None,
        None,
        None,
        None,
    );
    broker.place_pending_market_short_order("S".to_owned(), 1.0, 0);
    assert_eq!(broker.pending_entry_count(), 1);
    broker.fill_pending_market_entries(1, 20, 110.0);
    assert_eq!(broker.position_size, -1.0);
}

#[test]
fn allow_entry_in_cancels_pending_opposite_entry_when_flat() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_short_entry("S".to_owned(), 1.0, 10.0, 0);
    assert_eq!(broker.pending_entry_count(), 1);
    broker.record_risk_rule_call(
        RiskRuleKind::AllowEntryIn,
        Some(RiskEntryDirection::Long),
        None,
        None,
        None,
        None,
    );
    assert_eq!(broker.pending_entry_count(), 0);
}

#[test]
fn allow_entry_in_converts_pending_opposite_entry_to_market_close() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    broker.place_pending_limit_short_entry("S".to_owned(), 1.0, 10.0, 0);
    broker.record_risk_rule_call(
        RiskRuleKind::AllowEntryIn,
        Some(RiskEntryDirection::Long),
        None,
        None,
        None,
        None,
    );
    let pending = broker
        .order_book
        .entries()
        .find_by_id("S")
        .expect("converted pending short");
    assert_eq!(
        pending.kind,
        super::pending_entries::PendingEntryKind::Market
    );
    broker.fill_pending_market_entries(1, 20, 110.0);
    assert_eq!(broker.position_size, 0.0);
}

#[test]
fn allow_entry_in_last_call_wins() {
    let mut broker = BrokerState::new(100_000.0);
    broker.record_risk_rule_call(
        RiskRuleKind::AllowEntryIn,
        Some(RiskEntryDirection::All),
        None,
        None,
        None,
        None,
    );
    broker.record_risk_rule_call(
        RiskRuleKind::AllowEntryIn,
        Some(RiskEntryDirection::Long),
        None,
        None,
        None,
        None,
    );
    assert_eq!(
        broker.risk_rules().allow_entry_direction,
        Some(RiskEntryDirection::Long)
    );
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 1.0));
    assert!(broker.entry_short("S".to_owned(), 1, 20, 110.0, 1.0));
    assert_eq!(broker.position_size, 0.0);
}

#[test]
fn allow_entry_in_rewrites_new_price_based_opposite_entry_to_market() {
    let mut broker = BrokerState::new(100_000.0);
    broker.record_risk_rule_call(
        RiskRuleKind::AllowEntryIn,
        Some(RiskEntryDirection::Long),
        None,
        None,
        None,
        None,
    );
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    broker.place_pending_limit_short_entry("S".to_owned(), 1.0, 10.0, 1);
    let pending = broker
        .order_book
        .entries()
        .find_by_id("S")
        .expect("close-only market");
    assert_eq!(
        pending.kind,
        super::pending_entries::PendingEntryKind::Market
    );
}

#[test]
fn max_position_size_reduces_oversized_entry() {
    let mut broker = BrokerState::new(100_000.0);
    broker.record_risk_rule_call(
        RiskRuleKind::MaxPositionSize,
        None,
        None,
        None,
        Some(2.0),
        None,
    );
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 5.0));
    assert_eq!(broker.position_size, 2.0);
}

#[test]
fn max_position_size_rejects_entry_when_already_at_limit() {
    let mut broker = BrokerState::new_with_account_settings_and_pyramiding(
        100_000.0,
        None,
        0.0,
        0.0,
        StrategyMarginSetting::default(),
        StrategyMarginSetting::default(),
        3,
    );
    broker.record_risk_rule_call(
        RiskRuleKind::MaxPositionSize,
        None,
        None,
        None,
        Some(2.0),
        None,
    );
    assert!(broker.entry_long("A".to_owned(), 0, 10, 100.0, 2.0));
    assert!(!broker.entry_long("B".to_owned(), 1, 20, 110.0, 1.0));
    assert_eq!(broker.position_size, 2.0);
    broker.place_pending_market_long_entry("C".to_owned(), 1.0, 1);
    assert_eq!(broker.pending_entry_count(), 0);
}

#[test]
fn max_position_size_clamps_reversal_to_limit_on_new_side() {
    let mut broker = BrokerState::new(100_000.0);
    broker.record_risk_rule_call(
        RiskRuleKind::MaxPositionSize,
        None,
        None,
        None,
        Some(3.0),
        None,
    );
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    assert!(broker.entry_short("S".to_owned(), 1, 20, 110.0, 5.0));
    assert_eq!(broker.position_size, -3.0);
}

#[test]
fn max_position_size_does_not_bind_generic_orders() {
    let mut broker = BrokerState::new(100_000.0);
    broker.record_risk_rule_call(
        RiskRuleKind::MaxPositionSize,
        None,
        None,
        None,
        Some(1.0),
        None,
    );
    broker.place_pending_market_long_order("O".to_owned(), 5.0, 0);
    broker.fill_pending_market_entries(1, 20, 110.0);
    assert_eq!(broker.position_size, 5.0);
}

#[test]
fn max_position_size_reduces_pending_entry_when_rule_is_recorded() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_entry("L".to_owned(), 5.0, 90.0, 0);
    broker.record_risk_rule_call(
        RiskRuleKind::MaxPositionSize,
        None,
        None,
        None,
        Some(2.0),
        None,
    );
    let pending = broker
        .order_book
        .entries()
        .find_by_id("L")
        .expect("reduced pending");
    assert_eq!(pending.quantity, 2.0);
}

#[test]
fn max_position_size_cancels_pending_entry_when_no_room_remains() {
    let mut broker = BrokerState::new_with_account_settings_and_pyramiding(
        100_000.0,
        None,
        0.0,
        0.0,
        StrategyMarginSetting::default(),
        StrategyMarginSetting::default(),
        3,
    );
    assert!(broker.entry_long("A".to_owned(), 0, 10, 100.0, 2.0));
    broker.place_pending_limit_long_entry("B".to_owned(), 1.0, 90.0, 1);
    broker.record_risk_rule_call(
        RiskRuleKind::MaxPositionSize,
        None,
        None,
        None,
        Some(2.0),
        None,
    );
    assert_eq!(broker.pending_entry_count(), 0);
}

#[test]
fn max_position_size_allows_pyramiding_until_limit() {
    let mut broker = BrokerState::new_with_account_settings_and_pyramiding(
        100_000.0,
        None,
        0.0,
        0.0,
        StrategyMarginSetting::default(),
        StrategyMarginSetting::default(),
        3,
    );
    broker.record_risk_rule_call(
        RiskRuleKind::MaxPositionSize,
        None,
        None,
        None,
        Some(2.0),
        None,
    );
    assert!(broker.entry_long("A".to_owned(), 0, 10, 100.0, 1.0));
    assert!(broker.entry_long("B".to_owned(), 1, 20, 110.0, 2.0));
    assert_eq!(broker.position_size, 2.0);
}

#[test]
fn max_drawdown_cash_flattens_cancels_pending_and_blocks_later_entries() {
    let mut broker = BrokerState::new(100_000.0);
    broker.set_max_drawdown(40.0, "strategy.cash", None);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 10.0));
    broker.place_pending_limit_long_entry("P".to_owned(), 1.0, 90.0, 1);
    broker.evaluate_max_drawdown(1, 20, 90.0);
    assert_eq!(broker.position_size, 0.0);
    assert!(broker.risk_state().blocked_order_placement);
    assert!(
        broker
            .risk_state()
            .tripped_rules
            .contains(&RiskRuleKind::MaxDrawdown)
    );
    assert_eq!(broker.pending_entry_count(), 0);
    assert!(!broker.entry_long("X".to_owned(), 2, 30, 90.0, 1.0));
    broker.place_pending_market_long_order("O".to_owned(), 1.0, 2);
    assert_eq!(broker.pending_entry_count(), 0);
}

#[test]
fn max_drawdown_percent_uses_peak_equity_and_stays_permanent() {
    let mut broker = BrokerState::new(1_000.0);
    broker.set_max_drawdown(4.0, "strategy.percent_of_equity", None);
    assert_eq!(
        broker.risk_rules().max_drawdown_type,
        Some(RiskDrawdownType::PercentOfEquity)
    );
    assert!(broker.entry_long("L".to_owned(), 0, 10, 10.0, 10.0));
    broker.evaluate_max_drawdown(1, 20, 5.0);
    assert_eq!(broker.position_size, 0.0);
    assert!(broker.risk_state().blocked_order_placement);
    broker.reset_intraday_risk_state(0, UTC_DAY_MS, INTRADAY_TF_SECONDS, 1_000.0);
    assert!(broker.risk_state().blocked_order_placement);
    assert!(!broker.entry_long("X".to_owned(), 2, 30, 5.0, 1.0));
}

#[test]
fn max_drawdown_below_threshold_does_not_flatten() {
    let mut broker = BrokerState::new(100_000.0);
    broker.set_max_drawdown(1_000.0, "strategy.cash", None);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 10.0));
    broker.evaluate_max_drawdown(1, 20, 90.0);
    assert_eq!(broker.position_size, 10.0);
    assert!(!broker.risk_state().blocked_order_placement);
}

#[test]
fn max_drawdown_snapshot_restores_tripped_flatten_state() {
    let mut broker = BrokerState::new(100_000.0);
    broker.set_max_drawdown(40.0, "strategy.cash", None);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 10.0));
    broker.evaluate_max_drawdown(1, 20, 90.0);
    let snapshot = broker.snapshot();
    assert!(snapshot.risk_state().blocked_order_placement);
    let mut restored = BrokerState::new(100_000.0);
    restored.restore(snapshot);
    assert!(restored.risk_state().blocked_order_placement);
    assert_eq!(restored.position_size, 0.0);
    assert!(!restored.entry_long("X".to_owned(), 2, 30, 90.0, 1.0));
}

#[test]
fn max_intraday_filled_orders_flattens_and_clears_on_window_reset() {
    let mut broker = BrokerState::new(100_000.0);
    broker.reset_intraday_risk_state(0, 0, INTRADAY_TF_SECONDS, 100_000.0);
    broker.set_max_intraday_filled_orders(1.0, None);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 1.0));
    broker.evaluate_risk_equity_stops(0, 10, 100.0);
    assert_eq!(broker.position_size, 0.0);
    assert!(
        broker
            .risk_state()
            .tripped_rules
            .contains(&RiskRuleKind::MaxIntradayFilledOrders)
    );
    assert!(!broker.entry_long("X".to_owned(), 1, 20, 100.0, 1.0));
    broker.place_pending_market_long_order("O".to_owned(), 1.0, 1);
    assert_eq!(broker.pending_entry_count(), 0);
    broker.reset_intraday_risk_state(0, UTC_DAY_MS, INTRADAY_TF_SECONDS, 100_000.0);
    assert_eq!(broker.check_risk_before_order(), RiskAdmission::Allow);
    assert!(broker.entry_long("N".to_owned(), 2, UTC_DAY_MS, 100.0, 1.0));
}

#[test]
fn max_intraday_filled_orders_counts_close_fills() {
    let mut broker = BrokerState::new(100_000.0);
    broker.reset_intraday_risk_state(0, 0, INTRADAY_TF_SECONDS, 100_000.0);
    broker.set_max_intraday_filled_orders(2.0, None);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 1.0));
    assert_eq!(broker.risk_state().intraday_filled_orders, 1);
    assert_eq!(broker.position_size, 1.0);
    broker.reduce_long_with_short_order(
        "R".to_owned(),
        1,
        20,
        110.0,
        1.0,
        super::types::StrategyOrderMetadata::default(),
    );
    assert_eq!(broker.risk_state().intraday_filled_orders, 2);
    assert!(
        broker
            .risk_state()
            .tripped_rules
            .contains(&RiskRuleKind::MaxIntradayFilledOrders)
    );
    assert_eq!(broker.check_risk_before_order(), RiskAdmission::Reject);
}

#[test]
fn max_intraday_filled_orders_rejects_non_positive_or_non_integer_count() {
    let mut broker = BrokerState::new(100_000.0);
    broker.set_max_intraday_filled_orders(0.0, None);
    broker.set_max_intraday_filled_orders(-1.0, None);
    broker.set_max_intraday_filled_orders(1.5, None);
    broker.set_max_intraday_filled_orders(f64::NAN, None);
    assert!(broker.risk_rules().max_intraday_filled_orders.is_none());
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 1.0));
    assert_eq!(broker.position_size, 1.0);
    assert!(!broker.risk_state().blocked_order_placement);
}

#[test]
fn max_intraday_loss_cash_flattens_from_window_max_and_resets() {
    let mut broker = BrokerState::new(100_000.0);
    broker.reset_intraday_risk_state(0, 0, INTRADAY_TF_SECONDS, 100_000.0);
    broker.set_max_intraday_loss(40.0, "strategy.cash", None);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 10.0));
    broker.place_pending_limit_long_entry("P".to_owned(), 1.0, 90.0, 1);
    broker.evaluate_max_intraday_loss(1, 20, 90.0);
    assert_eq!(broker.position_size, 0.0);
    assert!(
        broker
            .risk_state()
            .tripped_rules
            .contains(&RiskRuleKind::MaxIntradayLoss)
    );
    assert_eq!(broker.pending_entry_count(), 0);
    assert!(!broker.entry_long("X".to_owned(), 2, 30, 90.0, 1.0));
    broker.reset_intraday_risk_state(0, UTC_DAY_MS, INTRADAY_TF_SECONDS, 100_000.0);
    assert_eq!(broker.check_risk_before_order(), RiskAdmission::Allow);
    assert!(broker.entry_long("N".to_owned(), 3, UTC_DAY_MS, 100.0, 1.0));
}

#[test]
fn max_intraday_loss_percent_uses_window_max_equity() {
    let mut broker = BrokerState::new(1_000.0);
    broker.reset_intraday_risk_state(0, 0, INTRADAY_TF_SECONDS, 1_000.0);
    broker.set_max_intraday_loss(4.0, "strategy.percent_of_equity", None);
    assert_eq!(
        broker.risk_rules().max_intraday_loss_type,
        Some(RiskDrawdownType::PercentOfEquity)
    );
    assert!(broker.entry_long("L".to_owned(), 0, 10, 10.0, 10.0));
    broker.evaluate_max_intraday_loss(1, 20, 5.0);
    assert_eq!(broker.position_size, 0.0);
    assert!(broker.risk_state().blocked_order_placement);
    broker.reset_intraday_risk_state(0, UTC_DAY_MS, INTRADAY_TF_SECONDS, 1_000.0);
    assert!(!broker.risk_state().blocked_order_placement);
}

#[test]
fn max_intraday_loss_below_threshold_does_not_flatten() {
    let mut broker = BrokerState::new(100_000.0);
    broker.reset_intraday_risk_state(0, 0, INTRADAY_TF_SECONDS, 100_000.0);
    broker.set_max_intraday_loss(1_000.0, "strategy.cash", None);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 10.0));
    broker.evaluate_max_intraday_loss(1, 20, 90.0);
    assert_eq!(broker.position_size, 10.0);
    assert!(!broker.risk_state().blocked_order_placement);
}

#[test]
fn max_intraday_loss_unknown_type_or_invalid_value_is_ignored() {
    let mut broker = BrokerState::new(100_000.0);
    broker.set_max_intraday_loss(40.0, "strategy.percent", None);
    broker.set_max_intraday_loss(0.0, "strategy.cash", None);
    broker.set_max_intraday_loss(101.0, "strategy.percent_of_equity", None);
    assert!(broker.risk_rules().max_intraday_loss.is_none());
}

#[test]
fn max_cons_loss_days_trips_permanently_after_consecutive_loss_windows() {
    let mut broker = BrokerState::new(100_000.0);
    broker.set_max_cons_loss_days(2.0, None);
    broker.reset_intraday_risk_state(0, 0, INTRADAY_TF_SECONDS, 100_000.0);
    assert!(broker.entry_long("A".to_owned(), 0, 0, 100.0, 1.0));
    broker.close_all_position(0, 0, 90.0);
    assert!(broker.risk_state().window_realized_pnl < 0.0);
    broker.reset_intraday_risk_state(1, UTC_DAY_MS, INTRADAY_TF_SECONDS, 100_000.0);
    assert_eq!(broker.risk_state().consecutive_loss_days, 1);
    assert!(!broker.risk_state().blocked_order_placement);
    assert!(broker.entry_long("B".to_owned(), 1, UTC_DAY_MS, 100.0, 1.0));
    broker.close_all_position(1, UTC_DAY_MS, 90.0);
    broker.reset_intraday_risk_state(2, 2 * UTC_DAY_MS, INTRADAY_TF_SECONDS, 100_000.0);
    assert_eq!(broker.risk_state().consecutive_loss_days, 2);
    assert!(
        broker
            .risk_state()
            .tripped_rules
            .contains(&RiskRuleKind::MaxConsLossDays)
    );
    assert_eq!(broker.check_risk_before_order(), RiskAdmission::Reject);
    assert!(!broker.entry_long("X".to_owned(), 2, 2 * UTC_DAY_MS, 100.0, 1.0));
    broker.reset_intraday_risk_state(3, 3 * UTC_DAY_MS, INTRADAY_TF_SECONDS, 100_000.0);
    assert!(broker.risk_state().blocked_order_placement);
}

#[test]
fn max_cons_loss_days_profit_window_resets_streak() {
    let mut broker = BrokerState::new(100_000.0);
    broker.set_max_cons_loss_days(2.0, None);
    broker.reset_intraday_risk_state(0, 0, INTRADAY_TF_SECONDS, 100_000.0);
    assert!(broker.entry_long("A".to_owned(), 0, 0, 100.0, 1.0));
    broker.close_all_position(0, 0, 90.0);
    broker.reset_intraday_risk_state(1, UTC_DAY_MS, INTRADAY_TF_SECONDS, 100_000.0);
    assert_eq!(broker.risk_state().consecutive_loss_days, 1);
    assert!(broker.entry_long("B".to_owned(), 1, UTC_DAY_MS, 100.0, 1.0));
    broker.close_all_position(1, UTC_DAY_MS, 110.0);
    broker.reset_intraday_risk_state(2, 2 * UTC_DAY_MS, INTRADAY_TF_SECONDS, 100_000.0);
    assert_eq!(broker.risk_state().consecutive_loss_days, 0);
    assert!(!broker.risk_state().blocked_order_placement);
}

#[test]
fn max_cons_loss_days_no_trade_window_resets_streak() {
    let mut broker = BrokerState::new(100_000.0);
    broker.set_max_cons_loss_days(2.0, None);
    broker.reset_intraday_risk_state(0, 0, INTRADAY_TF_SECONDS, 100_000.0);
    assert!(broker.entry_long("A".to_owned(), 0, 0, 100.0, 1.0));
    broker.close_all_position(0, 0, 90.0);
    broker.reset_intraday_risk_state(1, UTC_DAY_MS, INTRADAY_TF_SECONDS, 100_000.0);
    assert_eq!(broker.risk_state().consecutive_loss_days, 1);
    broker.reset_intraday_risk_state(2, 2 * UTC_DAY_MS, INTRADAY_TF_SECONDS, 100_000.0);
    assert_eq!(broker.risk_state().consecutive_loss_days, 0);
    assert!(!broker.risk_state().blocked_order_placement);
}

#[test]
fn max_cons_loss_days_gap_keeps_observed_loss_streak() {
    let mut broker = BrokerState::new(100_000.0);
    broker.set_max_cons_loss_days(2.0, None);
    broker.reset_intraday_risk_state(0, 0, INTRADAY_TF_SECONDS, 100_000.0);
    assert!(broker.entry_long("A".to_owned(), 0, 0, 100.0, 1.0));
    broker.close_all_position(0, 0, 90.0);
    broker.reset_intraday_risk_state(1, 2 * UTC_DAY_MS, INTRADAY_TF_SECONDS, 100_000.0);
    assert_eq!(broker.risk_state().consecutive_loss_days, 1);
    assert!(broker.entry_long("B".to_owned(), 1, 2 * UTC_DAY_MS, 100.0, 1.0));
    broker.close_all_position(1, 2 * UTC_DAY_MS, 90.0);
    broker.reset_intraday_risk_state(2, 3 * UTC_DAY_MS, INTRADAY_TF_SECONDS, 100_000.0);
    assert!(
        broker
            .risk_state()
            .tripped_rules
            .contains(&RiskRuleKind::MaxConsLossDays)
    );
}

#[test]
fn max_cons_loss_days_flattens_open_position_at_mark_not_equity() {
    let mut broker = BrokerState::new(100_000.0);
    broker.set_max_cons_loss_days(1.0, None);
    broker.reset_intraday_risk_state(0, 0, INTRADAY_TF_SECONDS, 100_000.0);
    assert!(broker.entry_long("A".to_owned(), 0, 0, 100.0, 1.0));
    broker.close_all_position(0, 0, 90.0);
    assert!(broker.entry_long("B".to_owned(), 0, 0, 100.0, 1.0));
    assert_eq!(broker.position_size, 1.0);
    let mark = 80.0;
    let equity = broker.equity_value(mark);
    assert!(equity > 1_000.0);
    broker.reset_intraday_window(1, UTC_DAY_MS, INTRADAY_TF_SECONDS, equity, mark);
    assert_eq!(broker.position_size, 0.0);
    assert!(
        broker
            .risk_state()
            .tripped_rules
            .contains(&RiskRuleKind::MaxConsLossDays)
    );
    let result = broker.result();
    let leftover = result
        .trades
        .iter()
        .find(|trade| trade.id == "B")
        .expect("leftover position flattened");
    assert_eq!(leftover.exit_price, mark);
    assert_ne!(leftover.exit_price, equity);
}

#[test]
fn max_cons_loss_days_rejects_non_positive_or_non_integer_count() {
    let mut broker = BrokerState::new(100_000.0);
    broker.set_max_cons_loss_days(0.0, None);
    broker.set_max_cons_loss_days(-1.0, None);
    broker.set_max_cons_loss_days(1.5, None);
    broker.set_max_cons_loss_days(f64::NAN, None);
    assert!(broker.risk_rules().max_cons_loss_days.is_none());
}

fn host_ids(window: &str, day: &str) -> crate::SessionWindowIds {
    crate::SessionWindowIds {
        window_id: window.to_owned(),
        trading_day_id: day.to_owned(),
    }
}

#[test]
fn host_overnight_window_does_not_false_reset_across_utc_midnight() {
    let mut broker = BrokerState::new(100_000.0);
    broker.set_max_intraday_filled_orders(2.0, None);
    let eth = host_ids("eth", "d1");
    broker.reset_risk_windows(
        0,
        UTC_DAY_MS - 1_000,
        INTRADAY_TF_SECONDS,
        100_000.0,
        100.0,
        Some(&eth),
    );
    broker.check_risk_after_fill(0, UTC_DAY_MS - 1_000, 100.0);
    assert_eq!(broker.risk_state().intraday_filled_orders, 1);
    broker.reset_risk_windows(
        1,
        UTC_DAY_MS + 1_000,
        INTRADAY_TF_SECONDS,
        100_000.0,
        100.0,
        Some(&eth),
    );
    assert_eq!(broker.risk_state().intraday_filled_orders, 1);
    assert_eq!(
        broker.risk_state().window_key,
        Some(RiskWindowKey::Host("eth".into()))
    );
}

#[test]
fn utc_midnight_still_resets_when_no_host_window_is_supplied() {
    let mut broker = BrokerState::new(100_000.0);
    broker.reset_risk_windows(
        0,
        UTC_DAY_MS - 1_000,
        INTRADAY_TF_SECONDS,
        100_000.0,
        100.0,
        None,
    );
    broker.check_risk_after_fill(0, UTC_DAY_MS - 1_000, 100.0);
    broker.reset_risk_windows(
        1,
        UTC_DAY_MS + 1_000,
        INTRADAY_TF_SECONDS,
        100_000.0,
        100.0,
        None,
    );
    assert_eq!(broker.risk_state().intraday_filled_orders, 0);
}

#[test]
fn host_session_switch_resets_intraday_once_without_finalizing_trading_day() {
    let mut broker = BrokerState::new(100_000.0);
    broker.reset_risk_windows(
        0,
        10,
        INTRADAY_TF_SECONDS,
        100_000.0,
        100.0,
        Some(&host_ids("eth", "d1")),
    );
    broker.check_risk_after_fill(0, 10, 100.0);
    broker.record_window_realized_pnl(-5.0);
    broker.reset_risk_windows(
        1,
        20,
        INTRADAY_TF_SECONDS,
        99_000.0,
        99.0,
        Some(&host_ids("rth", "d1")),
    );
    assert_eq!(broker.risk_state().intraday_filled_orders, 0);
    assert_eq!(broker.risk_state().consecutive_loss_days, 0);
    assert_eq!(broker.risk_state().window_realized_pnl, -5.0);
    assert_eq!(
        broker.risk_state().window_key,
        Some(RiskWindowKey::Host("rth".into()))
    );
    assert_eq!(
        broker.risk_state().trading_day_key,
        Some(RiskWindowKey::Host("d1".into()))
    );
}

#[test]
fn host_trading_day_switch_finalizes_consecutive_loss_once() {
    let mut broker = BrokerState::new(100_000.0);
    broker.set_max_cons_loss_days(1.0, None);
    broker.reset_risk_windows(
        0,
        10,
        INTRADAY_TF_SECONDS,
        100_000.0,
        100.0,
        Some(&host_ids("rth", "d1")),
    );
    broker.record_window_realized_pnl(-5.0);
    broker.reset_risk_windows(
        1,
        UTC_DAY_MS + 10,
        INTRADAY_TF_SECONDS,
        90_000.0,
        90.0,
        Some(&host_ids("rth", "d2")),
    );
    assert_eq!(broker.risk_state().consecutive_loss_days, 1);
    assert!(
        broker
            .risk_state()
            .tripped_rules
            .contains(&RiskRuleKind::MaxConsLossDays)
    );
}
