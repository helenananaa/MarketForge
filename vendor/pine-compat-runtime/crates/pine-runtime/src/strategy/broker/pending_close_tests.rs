use super::pending_closes::{PendingCloseKind, PendingCloseQuantity};
use super::types::{InternalOrderKey, StrategyCommandOrigin};
use super::*;

#[test]
fn pending_close_stores_quantity_policy_without_filling() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));

    broker.place_pending_close(
        "L".to_owned(),
        PendingCloseQuantity::Qty(0.5),
        1,
        StrategyOrderMetadata {
            comment: Some("pending comment".to_owned()),
            alert_message: Some("pending alert".to_owned()),
            disable_alert: true,
        },
    );

    assert_eq!(broker.order_book.closes().count(), 1);
    let pending = broker
        .order_book
        .closes()
        .find_close_by_id("L")
        .expect("pending close");
    assert_eq!(pending.origin, StrategyCommandOrigin::Close);
    assert_eq!(pending.key, InternalOrderKey(0));
    assert_eq!(pending.quantity, PendingCloseQuantity::Qty(0.5));
    assert_eq!(pending.created_bar_index, 1);
    assert!(!pending.immediately);
    assert_eq!(broker.position_size, 2.0);
    assert!(broker.trades.is_empty());
}

#[test]
fn pending_close_percent_is_stored_unresolved_until_fill() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_close(
        "L".to_owned(),
        PendingCloseQuantity::QtyPercent(50.0),
        0,
        StrategyOrderMetadata::default(),
    );
    let pending = broker
        .order_book
        .closes()
        .find_close_by_id("L")
        .expect("pending close");
    assert_eq!(pending.quantity, PendingCloseQuantity::QtyPercent(50.0));
}

#[test]
fn same_id_pending_close_replacement_preserves_key() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_close(
        "L".to_owned(),
        PendingCloseQuantity::Full,
        0,
        StrategyOrderMetadata::default(),
    );
    broker.place_pending_close(
        "L".to_owned(),
        PendingCloseQuantity::Qty(1.0),
        1,
        StrategyOrderMetadata::default(),
    );
    broker.place_pending_close(
        "M".to_owned(),
        PendingCloseQuantity::Full,
        1,
        StrategyOrderMetadata::default(),
    );

    let replaced = broker
        .order_book
        .closes()
        .find_close_by_id("L")
        .expect("replaced");
    assert_eq!(replaced.key, InternalOrderKey(0));
    assert_eq!(replaced.quantity, PendingCloseQuantity::Qty(1.0));
    assert_eq!(replaced.created_bar_index, 1);
    assert_eq!(
        broker
            .order_book
            .closes()
            .find_close_by_id("M")
            .expect("second")
            .key,
        InternalOrderKey(1)
    );
}

#[test]
fn cancel_pending_close_by_public_id_and_rollback_sequence() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_close(
        "A".to_owned(),
        PendingCloseQuantity::Full,
        0,
        StrategyOrderMetadata::default(),
    );
    broker.place_pending_close_all(
        PendingCloseQuantity::Full,
        0,
        StrategyOrderMetadata::default(),
    );
    let snapshot = broker.clone();
    broker.cancel_pending_order("A");

    assert!(broker.order_book.closes().find_close_by_id("A").is_none());
    assert_eq!(broker.order_book.closes().count(), 1);
    assert!(matches!(
        broker
            .order_book
            .closes()
            .iter()
            .next()
            .map(|pending| &pending.kind),
        Some(PendingCloseKind::CloseAll)
    ));

    let mut restored = snapshot;
    restored.place_pending_close(
        "B".to_owned(),
        PendingCloseQuantity::Full,
        1,
        StrategyOrderMetadata::default(),
    );
    assert_eq!(
        restored
            .order_book
            .closes()
            .find_close_by_id("B")
            .expect("rollback next key")
            .key,
        InternalOrderKey(2)
    );
}

#[test]
fn pending_close_fills_at_next_bar_open_and_not_on_creation_bar() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    broker.place_pending_close(
        "L".to_owned(),
        PendingCloseQuantity::Full,
        1,
        StrategyOrderMetadata::default(),
    );

    broker.fill_pending_market_closes(1, 20, 110.0);
    assert_eq!(broker.position_size, 2.0);
    assert_eq!(broker.closed_trade_count(), 0);

    broker.fill_pending_market_closes(2, 30, 120.0);
    assert_eq!(broker.position_size, 0.0);
    assert_eq!(broker.closed_trade_count(), 1);
    assert_eq!(broker.trades[0].exit_price, 120.0);
    assert_eq!(broker.order_book.closes().count(), 0);
}

#[test]
fn pending_close_is_noop_if_position_already_flat() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    broker.place_pending_close(
        "L".to_owned(),
        PendingCloseQuantity::Full,
        1,
        StrategyOrderMetadata::default(),
    );
    broker.close_long("L".to_owned(), 1, 20, 110.0);
    broker.fill_pending_market_closes(2, 30, 120.0);

    assert_eq!(broker.closed_trade_count(), 1);
    assert_eq!(broker.trades[0].exit_price, 110.0);
    assert_eq!(broker.position_size, 0.0);
}

#[test]
fn production_close_still_fills_immediately_while_pending_close_is_storage_only() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    broker.close_long("L".to_owned(), 1, 20, 110.0);

    assert_eq!(broker.position_size, 0.0);
    assert_eq!(broker.closed_trade_count(), 1);
    assert_eq!(broker.order_book.closes().count(), 0);
}

#[test]
fn immediate_pending_close_fills_on_creation_bar_close_price() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    broker.place_pending_close_with_immediately(
        "L".to_owned(),
        PendingCloseQuantity::Full,
        1,
        StrategyOrderMetadata::default(),
        true,
    );

    broker.fill_pending_market_closes(1, 20, 105.0);
    assert_eq!(broker.position_size, 2.0);
    assert_eq!(broker.closed_trade_count(), 0);

    broker.fill_immediate_market_closes(1, 20, 110.0);
    assert_eq!(broker.position_size, 0.0);
    assert_eq!(broker.closed_trade_count(), 1);
    assert_eq!(broker.trades[0].exit_bar_index, 1);
    assert_eq!(broker.trades[0].exit_price, 110.0);
    assert_eq!(broker.order_book.closes().count(), 0);
}

#[test]
fn immediate_pending_close_all_fills_on_creation_bar() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    broker.place_pending_close_all_with_immediately(
        PendingCloseQuantity::Full,
        1,
        StrategyOrderMetadata::default(),
        true,
    );
    broker.fill_immediate_market_closes(1, 20, 110.0);
    assert_eq!(broker.position_size, 0.0);
    assert_eq!(broker.closed_trade_count(), 1);
}

#[test]
fn immediate_partial_qty_and_repeated_close_are_clamped_then_noop() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    broker.place_pending_close_with_immediately(
        "L".to_owned(),
        PendingCloseQuantity::Qty(0.5),
        1,
        StrategyOrderMetadata {
            comment: Some("partial".to_owned()),
            alert_message: Some("alert".to_owned()),
            disable_alert: false,
        },
        true,
    );
    broker.fill_immediate_market_closes(1, 20, 110.0);
    assert_eq!(broker.position_size, 1.5);
    assert_eq!(broker.closed_trade_count(), 1);
    assert_eq!(broker.trades[0].qty, 0.5);

    broker.place_pending_close_with_immediately(
        "L".to_owned(),
        PendingCloseQuantity::Full,
        1,
        StrategyOrderMetadata::default(),
        true,
    );
    broker.fill_immediate_market_closes(1, 20, 110.0);
    assert_eq!(broker.position_size, 0.0);
    broker.place_pending_close_with_immediately(
        "L".to_owned(),
        PendingCloseQuantity::Full,
        1,
        StrategyOrderMetadata::default(),
        true,
    );
    broker.fill_immediate_market_closes(1, 20, 110.0);
    assert_eq!(broker.closed_trade_count(), 2);
}

#[test]
fn immediate_close_cancels_matching_pending_exit() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    broker.place_exit_stop("XL".to_owned(), "L".to_owned(), 95.0, 0);
    assert!(broker.order_book.exits().count() > 0);
    broker.place_pending_close_with_immediately(
        "L".to_owned(),
        PendingCloseQuantity::Full,
        1,
        StrategyOrderMetadata::default(),
        true,
    );
    broker.fill_immediate_market_closes(1, 20, 110.0);
    assert_eq!(broker.position_size, 0.0);
    assert_eq!(broker.order_book.exits().count(), 0);
}
