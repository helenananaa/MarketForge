use super::types::{OcaGroupKey, OcaMember, OcaType};
use super::*;

#[test]
fn oca_group_key_treats_same_name_different_types_as_distinct_groups() {
    let cancel = OcaGroupKey::new("g", OcaType::Cancel);
    let reduce = OcaGroupKey::new("g", OcaType::Reduce);
    let none = OcaGroupKey::new("g", OcaType::None);
    assert_ne!(cancel, reduce);
    assert_ne!(cancel, none);
    assert_ne!(reduce, none);
    assert_eq!(cancel, OcaGroupKey::new("g", OcaType::Cancel));
}

#[test]
fn oca_membership_survives_same_id_replacement_and_clone_rollback() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_order("O".to_owned(), 1.0, 90.0, 0);
    broker.assign_pending_entry_oca("O", OcaGroupKey::new("g", OcaType::Cancel));
    assert_eq!(
        broker.pending_entry_oca("O"),
        Some(&OcaGroupKey::new("g", OcaType::Cancel))
    );

    let snapshot = broker.clone();
    broker.place_pending_stop_long_order("O".to_owned(), 2.0, 110.0, 1);
    assert_eq!(
        broker.pending_entry_oca("O"),
        Some(&OcaGroupKey::new("g", OcaType::Cancel))
    );

    broker.assign_pending_entry_oca("O", OcaGroupKey::new("g", OcaType::Reduce));
    assert_eq!(
        broker.pending_entry_oca("O"),
        Some(&OcaGroupKey::new("g", OcaType::Reduce))
    );
    assert_eq!(
        snapshot.pending_entry_oca("O"),
        Some(&OcaGroupKey::new("g", OcaType::Cancel))
    );

    let mut restored = snapshot;
    restored.cancel_pending_order("O");
    assert!(restored.pending_entry_oca("O").is_none());
    assert!(restored.order_book.entries().find_by_id("O").is_none());
}

#[test]
fn oca_cancel_clears_entry_and_exit_membership_for_shared_public_id() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 1.0));
    broker.place_exit_stop("X".to_owned(), "L".to_owned(), 90.0, 1);
    broker.place_pending_limit_long_order("X".to_owned(), 1.0, 90.0, 1);
    broker.assign_pending_entry_oca("X", OcaGroupKey::new("g", OcaType::Cancel));
    broker.assign_pending_exit_oca("X", "L", OcaGroupKey::new("g", OcaType::Cancel));
    assert!(broker.pending_entry_oca("X").is_some());
    assert!(broker.pending_exit_oca("X", "L").is_some());

    broker.cancel_pending_order("X");

    assert!(broker.pending_entry_oca("X").is_none());
    assert!(broker.pending_exit_oca("X", "L").is_none());
    assert!(broker.order_book.entries().find_by_id("X").is_none());
    assert_eq!(broker.order_book.exits().count(), 0);
    assert_eq!(broker.position_size, 1.0);
}

#[test]
fn production_oca_cancel_fills_first_and_cancels_same_group_peer() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_order("A".to_owned(), 1.0, 90.0, 1);
    broker.place_pending_limit_long_order("B".to_owned(), 1.0, 90.0, 1);
    broker.place_pending_limit_long_order("C".to_owned(), 1.0, 90.0, 1);
    broker.assign_pending_order_oca_named("A", "g".to_owned(), Some("strategy.oca.cancel"));
    broker.assign_pending_order_oca_named("B", "g".to_owned(), Some("strategy.oca.cancel"));
    broker.assign_pending_order_oca_named("C", "other".to_owned(), Some("strategy.oca.cancel"));
    broker.place_pending_stop_limit_long_order("D".to_owned(), 1.0, 110.0, 100.0, 1);
    broker.assign_pending_order_oca_named("D", "g".to_owned(), Some("strategy.oca.cancel"));
    broker.fill_pending_limit_long_entries(2, 20, 89.0);
    assert_eq!(broker.position_size, 2.0);
    assert_eq!(broker.orders.len(), 2);
    assert!(broker.order_book.entries().find_by_id("B").is_none());
    assert!(broker.pending_entry_oca("B").is_none());
    assert!(broker.order_book.entries().find_by_id("D").is_none());
    assert!(broker.pending_entry_oca("D").is_none());
    broker.assert_ledger_aggregates();
}

#[test]
fn production_oca_none_grouped_limit_orders_fill_independently() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_order("A".to_owned(), 1.0, 90.0, 1);
    broker.place_pending_limit_long_order("B".to_owned(), 1.0, 90.0, 1);
    broker.assign_pending_order_oca_named("A", "g".to_owned(), Some("strategy.oca.none"));
    broker.assign_pending_order_oca_named("B", "g".to_owned(), Some("strategy.oca.none"));
    assert_eq!(
        broker.pending_entry_oca("A"),
        Some(&OcaGroupKey::new("g", OcaType::None))
    );
    assert_eq!(
        broker.pending_entry_oca("B"),
        Some(&OcaGroupKey::new("g", OcaType::None))
    );
    broker.fill_pending_limit_long_entries(2, 20, 89.0);
    assert_eq!(broker.position_size, 2.0);
    assert_eq!(broker.orders.len(), 2);
    broker.assert_ledger_aggregates();
}

#[test]
fn production_oca_reduce_cuts_peer_quantity_and_fills_remainder() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_order("A".to_owned(), 1.0, 90.0, 1);
    broker.place_pending_limit_long_order("B".to_owned(), 2.0, 90.0, 1);
    broker.assign_pending_order_oca_named("A", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.assign_pending_order_oca_named("B", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.fill_pending_limit_long_entries(2, 20, 89.0);
    assert_eq!(broker.position_size, 2.0);
    assert_eq!(broker.orders.len(), 2);
    assert_eq!(broker.orders[0].id, "A");
    assert_eq!(broker.orders[0].qty, 1.0);
    assert_eq!(broker.orders[1].id, "B");
    assert_eq!(broker.orders[1].qty, 1.0);
    assert!(broker.order_book.entries().find_by_id("B").is_none());
    broker.assert_ledger_aggregates();
}

#[test]
fn production_oca_reduce_removes_peer_reduced_to_zero() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_order("A".to_owned(), 2.0, 90.0, 1);
    broker.place_pending_limit_long_order("B".to_owned(), 1.0, 90.0, 1);
    broker.assign_pending_order_oca_named("A", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.assign_pending_order_oca_named("B", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.fill_pending_limit_long_entries(2, 20, 89.0);
    assert_eq!(broker.position_size, 2.0);
    assert_eq!(broker.orders.len(), 1);
    assert_eq!(broker.orders[0].id, "A");
    assert_eq!(broker.orders[0].qty, 2.0);
    assert!(broker.order_book.entries().find_by_id("B").is_none());
    assert!(broker.pending_entry_oca("B").is_none());
    broker.assert_ledger_aggregates();
}

#[test]
fn production_oca_reduce_multiple_peers_and_not_yet_eligible_stop_limit() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_order("A".to_owned(), 1.0, 90.0, 1);
    broker.place_pending_limit_long_order("B".to_owned(), 2.0, 90.0, 1);
    broker.place_pending_limit_long_order("C".to_owned(), 3.0, 90.0, 1);
    broker.assign_pending_order_oca_named("A", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.assign_pending_order_oca_named("B", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.assign_pending_order_oca_named("C", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.place_pending_stop_limit_long_order("D".to_owned(), 4.0, 110.0, 100.0, 1);
    broker.assign_pending_order_oca_named("D", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.place_pending_limit_long_order("E".to_owned(), 1.0, 90.0, 1);
    broker.assign_pending_order_oca_named("E", "other".to_owned(), Some("strategy.oca.reduce"));
    broker.fill_pending_limit_long_entries(2, 20, 89.0);
    assert_eq!(broker.position_size, 4.0);
    assert_eq!(broker.orders.len(), 4);
    let ids: Vec<_> = broker
        .orders
        .iter()
        .map(|order| order.id.as_str())
        .collect();
    assert_eq!(ids, ["A", "B", "C", "E"]);
    assert_eq!(broker.orders[0].qty, 1.0);
    assert_eq!(broker.orders[1].qty, 1.0);
    assert_eq!(broker.orders[2].qty, 1.0);
    assert_eq!(broker.orders[3].qty, 1.0);
    assert_eq!(broker.order_book.entries().quantity_for_id("D"), Some(1.0));
    assert_eq!(
        broker.pending_entry_oca("D"),
        Some(&OcaGroupKey::new("g", OcaType::Reduce))
    );
    broker.assert_ledger_aggregates();
}

#[test]
fn production_oca_reduce_cross_zero_uses_absolute_filled_quantity() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_short("S".to_owned(), 0, 10, 100.0, 2.0));
    broker.place_pending_limit_long_order("A".to_owned(), 3.0, 90.0, 1);
    broker.place_pending_limit_long_order("B".to_owned(), 2.0, 90.0, 1);
    broker.assign_pending_order_oca_named("A", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.assign_pending_order_oca_named("B", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.fill_pending_limit_long_entries(2, 20, 89.0);
    assert_eq!(broker.position_size, 1.0);
    assert_eq!(
        broker.orders.last().map(|order| order.id.as_str()),
        Some("A")
    );
    assert_eq!(broker.orders.last().map(|order| order.qty), Some(3.0));
    assert!(broker.order_book.entries().find_by_id("B").is_none());
    assert!(broker.pending_entry_oca("B").is_none());
    broker.assert_ledger_aggregates();
}

#[test]
fn production_oca_reduce_does_not_reduce_peers_on_margin_reject() {
    let mut broker = BrokerState::new_with_account_settings(
        100.0,
        None,
        0.0,
        0.0,
        pine_ir::StrategyMarginSetting::explicit(100.0),
        pine_ir::StrategyMarginSetting::default(),
    );
    broker.place_pending_limit_long_order("A".to_owned(), 3.0, 90.0, 1);
    broker.place_pending_limit_long_order("B".to_owned(), 1.0, 90.0, 1);
    broker.assign_pending_order_oca_named("A", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.assign_pending_order_oca_named("B", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.fill_pending_limit_long_entries(2, 20, 89.0);
    assert_eq!(broker.position_size, 1.0);
    assert_eq!(broker.orders.len(), 1);
    assert_eq!(broker.orders[0].id, "B");
    assert_eq!(broker.orders[0].qty, 1.0);
    broker.assert_ledger_aggregates();
}

#[test]
fn production_exit_oca_reduce_lets_full_stop_cover_partial_limit_peer() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    broker.set_next_exit_oca_name(Some("g".to_owned()));
    broker.place_exit_limit_qty("TP".to_owned(), "L".to_owned(), 110.0, 1.0, 1);
    broker.place_exit_stop("SL".to_owned(), "L".to_owned(), 90.0, 1);
    assert_eq!(
        broker.pending_exit_oca("TP", "L"),
        Some(&OcaGroupKey::new("g", OcaType::Reduce))
    );
    assert_eq!(
        broker.pending_exit_oca("SL", "L"),
        Some(&OcaGroupKey::new("g", OcaType::Reduce))
    );
    assert_eq!(
        broker
            .order_book
            .exits()
            .find_by_identity("SL", "L")
            .map(|pending| pending.reserved_quantity),
        Some(2.0)
    );
    broker.evaluate_pending_exits(2, 20, 100.0, 89.0);
    assert_eq!(broker.position_size, 0.0);
    assert_eq!(
        broker.orders.last().map(|order| order.id.as_str()),
        Some("SL")
    );
    assert_eq!(broker.orders.last().map(|order| order.qty), Some(2.0));
    assert!(
        broker
            .order_book
            .exits()
            .find_by_identity("TP", "L")
            .is_none()
    );
    broker.assert_ledger_aggregates();
}

#[test]
fn production_exit_without_oca_name_keeps_exclusive_reservation() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    broker.place_exit_limit_qty("TP".to_owned(), "L".to_owned(), 110.0, 1.0, 1);
    broker.place_exit_stop_qty("SL".to_owned(), "L".to_owned(), 90.0, 2.0, 1);
    assert_eq!(
        broker
            .order_book
            .exits()
            .find_by_identity("SL", "L")
            .map(|pending| pending.reserved_quantity),
        Some(1.0)
    );
    broker.evaluate_pending_exits(2, 20, 100.0, 89.0);
    assert_eq!(broker.position_size, 1.0);
    assert!(
        broker
            .order_book
            .exits()
            .find_by_identity("TP", "L")
            .is_some()
    );
    broker.assert_ledger_aggregates();
}

#[test]
fn production_exit_oca_reduce_updates_bracket_and_qty_peers() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    broker.set_next_exit_oca_name(Some("g".to_owned()));
    broker.place_exit_bracket("BR".to_owned(), "L".to_owned(), 80.0, 120.0, 1);
    broker.place_exit_stop_qty("X".to_owned(), "L".to_owned(), 90.0, 1.0, 1);
    broker.evaluate_pending_exits(2, 20, 100.0, 89.0);
    assert_eq!(broker.position_size, 1.0);
    assert_eq!(
        broker
            .order_book
            .exits()
            .find_by_identity("BR", "L")
            .map(|pending| pending.reserved_quantity),
        Some(1.0)
    );
    broker.assert_ledger_aggregates();
}

#[test]
fn production_exit_oca_reduce_percent_and_replacement() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    broker.set_next_exit_oca_name(Some("g".to_owned()));
    broker.place_exit_limit_qty_percent("TP".to_owned(), "L".to_owned(), 110.0, 50.0, 1);
    broker.place_exit_stop("SL".to_owned(), "L".to_owned(), 90.0, 1);
    broker.place_exit_limit_qty_percent("TP".to_owned(), "L".to_owned(), 111.0, 25.0, 1);
    assert_eq!(
        broker
            .order_book
            .exits()
            .find_by_identity("TP", "L")
            .map(|pending| pending.reserved_quantity),
        Some(0.5)
    );
    broker.evaluate_pending_exits(2, 20, 100.0, 89.0);
    assert_eq!(broker.position_size, 0.0);
    assert!(
        broker
            .order_book
            .exits()
            .find_by_identity("TP", "L")
            .is_none()
    );
    broker.assert_ledger_aggregates();
}

#[test]
fn production_exit_oca_reduce_trailing_peer_after_stop_fill() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    broker.set_next_exit_oca_name(Some("g".to_owned()));
    broker.place_exit_trail_price_qty(
        "T".to_owned(),
        "L".to_owned(),
        TrailPriceExitSpec {
            activation_price: 110.0,
            offset_ticks: 10.0,
            mintick: 1.0,
        },
        1.0,
        1,
    );
    broker.place_exit_stop("SL".to_owned(), "L".to_owned(), 90.0, 1);
    broker.evaluate_pending_exits(2, 20, 100.0, 89.0);
    assert_eq!(broker.position_size, 0.0);
    assert!(
        broker
            .order_book
            .exits()
            .find_by_identity("T", "L")
            .is_none()
    );
    broker.assert_ledger_aggregates();
}

#[test]
fn production_exit_oca_reduce_across_open_trade_keys() {
    let mut broker = BrokerState::new_with_account_settings_and_pyramiding(
        100_000.0,
        None,
        0.0,
        0.0,
        pine_ir::StrategyMarginSetting::default(),
        pine_ir::StrategyMarginSetting::default(),
        2,
    );
    assert!(broker.entry_long("A".to_owned(), 0, 10, 100.0, 1.0));
    assert!(broker.entry_long("B".to_owned(), 0, 10, 100.0, 1.0));
    broker.set_next_exit_oca_name(Some("g".to_owned()));
    broker.place_exit_stop("XA".to_owned(), "A".to_owned(), 90.0, 1);
    broker.place_exit_stop("XB".to_owned(), "B".to_owned(), 90.0, 1);
    broker.evaluate_pending_exits(2, 20, 100.0, 89.0);
    assert_eq!(broker.position_size, 1.0);
    assert!(
        broker
            .order_book
            .exits()
            .find_by_identity("XB", "B")
            .is_none()
    );
    broker.assert_ledger_aggregates();
}

#[test]
fn production_cancel_id_clears_all_pending_families_and_oca() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_entry("X".to_owned(), 1.0, 90.0, 0);
    broker.place_exit_stop("X".to_owned(), "X".to_owned(), 80.0, 0);
    broker.assign_pending_entry_oca("X", OcaGroupKey::new("g", OcaType::Reduce));
    broker.assign_pending_exit_oca("X", "X", OcaGroupKey::new("g", OcaType::Reduce));
    broker.place_pending_close(
        "X".to_owned(),
        PendingCloseQuantity::Full,
        0,
        StrategyOrderMetadata::default(),
    );

    broker.cancel_pending_order("X");

    assert!(broker.order_book.entries().find_by_id("X").is_none());
    assert!(
        broker
            .order_book
            .exits()
            .find_by_identity("X", "X")
            .is_none()
    );
    assert!(broker.order_book.closes().find_close_by_id("X").is_none());
    assert!(broker.pending_entry_oca("X").is_none());
    assert!(broker.pending_exit_oca("X", "X").is_none());
}

#[test]
fn production_cancel_all_clears_oca_reservations_deferred_and_activation() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_entry("L".to_owned(), 1.0, 90.0, 0);
    broker.place_exit_limit_qty("XL".to_owned(), "L".to_owned(), 110.0, 1.0, 0);
    broker.set_next_exit_oca_name(Some("g".to_owned()));
    broker.place_exit_stop_qty("XS".to_owned(), "L".to_owned(), 80.0, 1.0, 0);
    broker.place_pending_stop_limit_long_order("S".to_owned(), 1.0, 110.0, 100.0, 0);
    broker.assign_pending_order_oca_named("S", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.place_pending_close(
        "L".to_owned(),
        PendingCloseQuantity::Full,
        0,
        StrategyOrderMetadata::default(),
    );

    broker.cancel_all_pending_orders();

    assert_eq!(broker.order_book.entries().count(), 0);
    assert_eq!(broker.order_book.exits().count(), 0);
    assert_eq!(broker.order_book.exits().deferred_relative_count(), 0);
    assert_eq!(broker.order_book.closes().count(), 0);
    assert!(broker.pending_entry_oca("S").is_none());
    assert!(broker.pending_exit_oca("XL", "L").is_none());
    assert!(broker.pending_exit_oca("XS", "L").is_none());
}

#[test]
fn production_clear_exits_for_entry_prunes_oca_membership() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 1.0));
    broker.set_next_exit_oca_name(Some("g".to_owned()));
    broker.place_exit_stop("XL".to_owned(), "L".to_owned(), 90.0, 1);
    assert!(broker.pending_exit_oca("XL", "L").is_some());

    broker.cancel_exit_for_entry("L");

    assert_eq!(broker.order_book.exits().count(), 0);
    assert!(broker.pending_exit_oca("XL", "L").is_none());
}

#[test]
fn oca_same_name_different_types_do_not_share_peer_sets() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_order("A".to_owned(), 1.0, 90.0, 0);
    broker.place_pending_limit_long_order("B".to_owned(), 1.0, 91.0, 0);
    let cancel = OcaGroupKey::new("g", OcaType::Cancel);
    let reduce = OcaGroupKey::new("g", OcaType::Reduce);
    broker.assign_pending_entry_oca("A", cancel.clone());
    broker.assign_pending_entry_oca("B", reduce.clone());

    let cancel_peers = broker.order_book.oca_members_in_group(&cancel);
    let reduce_peers = broker.order_book.oca_members_in_group(&reduce);
    assert_eq!(cancel_peers.len(), 1);
    assert_eq!(reduce_peers.len(), 1);
    assert_ne!(cancel_peers, reduce_peers);
    match (&cancel_peers[0], &reduce_peers[0]) {
        (OcaMember::Order(left), OcaMember::Order(right)) => assert_ne!(left, right),
        other => panic!("expected order members, got {other:?}"),
    }
}

fn pyramiding_broker(limit: usize) -> BrokerState {
    BrokerState::new_with_account_settings_and_pyramiding(
        100_000.0,
        None,
        0.0,
        0.0,
        pine_ir::StrategyMarginSetting::default(),
        pine_ir::StrategyMarginSetting::default(),
        limit,
    )
}

#[test]
fn mixed_oca_entry_cancel_cancels_order_peer() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_entry("E".to_owned(), 1.0, 90.0, 1);
    broker.place_pending_limit_long_order("O".to_owned(), 1.0, 90.0, 1);
    broker.assign_pending_order_oca_named("E", "g".to_owned(), Some("strategy.oca.cancel"));
    broker.assign_pending_order_oca_named("O", "g".to_owned(), Some("strategy.oca.cancel"));
    broker.fill_pending_limit_long_entries(2, 20, 89.0);
    assert_eq!(broker.position_size, 1.0);
    assert_eq!(broker.orders.len(), 1);
    assert_eq!(broker.orders[0].id, "E");
    assert_eq!(broker.orders[0].qty, 1.0);
    assert!(broker.order_book.entries().find_by_id("O").is_none());
    assert!(broker.pending_entry_oca("O").is_none());
    broker.assert_ledger_aggregates();
}

#[test]
fn mixed_oca_order_cancel_cancels_entry_peer() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_order("O".to_owned(), 1.0, 90.0, 1);
    broker.place_pending_limit_long_entry("E".to_owned(), 1.0, 90.0, 1);
    broker.assign_pending_order_oca_named("O", "g".to_owned(), Some("strategy.oca.cancel"));
    broker.assign_pending_order_oca_named("E", "g".to_owned(), Some("strategy.oca.cancel"));
    broker.fill_pending_limit_long_entries(2, 20, 89.0);
    assert_eq!(broker.position_size, 1.0);
    assert_eq!(broker.orders.len(), 1);
    assert_eq!(broker.orders[0].id, "O");
    assert_eq!(broker.orders[0].qty, 1.0);
    assert!(broker.order_book.entries().find_by_id("E").is_none());
    assert!(broker.pending_entry_oca("E").is_none());
    broker.assert_ledger_aggregates();
}

#[test]
fn mixed_oca_none_entry_and_order_fill_independently() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_entry("E".to_owned(), 1.0, 90.0, 1);
    broker.place_pending_limit_long_order("O".to_owned(), 1.0, 90.0, 1);
    broker.assign_pending_order_oca_named("E", "g".to_owned(), Some("strategy.oca.none"));
    broker.assign_pending_order_oca_named("O", "g".to_owned(), Some("strategy.oca.none"));
    broker.fill_pending_limit_long_entries(2, 20, 89.0);
    assert_eq!(broker.position_size, 2.0);
    assert_eq!(broker.orders.len(), 2);
    broker.assert_ledger_aggregates();
}

#[test]
fn mixed_oca_entry_reduce_cuts_order_quantity() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_entry("E".to_owned(), 1.0, 90.0, 1);
    broker.place_pending_limit_long_order("O".to_owned(), 2.0, 90.0, 1);
    broker.assign_pending_order_oca_named("E", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.assign_pending_order_oca_named("O", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.fill_pending_limit_long_entries(2, 20, 89.0);
    assert_eq!(broker.position_size, 2.0);
    assert_eq!(broker.orders.len(), 2);
    assert_eq!(broker.orders[0].id, "E");
    assert_eq!(broker.orders[0].qty, 1.0);
    assert_eq!(broker.orders[1].id, "O");
    assert_eq!(broker.orders[1].qty, 1.0);
    assert!(broker.order_book.entries().find_by_id("O").is_none());
    broker.assert_ledger_aggregates();
}

#[test]
fn mixed_oca_order_reduce_removes_entry_peer_at_zero() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_order("O".to_owned(), 2.0, 90.0, 1);
    broker.place_pending_limit_long_entry("E".to_owned(), 1.0, 90.0, 1);
    broker.assign_pending_order_oca_named("O", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.assign_pending_order_oca_named("E", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.fill_pending_limit_long_entries(2, 20, 89.0);
    assert_eq!(broker.position_size, 2.0);
    assert_eq!(broker.orders.len(), 1);
    assert_eq!(broker.orders[0].id, "O");
    assert_eq!(broker.orders[0].qty, 2.0);
    assert!(broker.order_book.entries().find_by_id("E").is_none());
    assert!(broker.pending_entry_oca("E").is_none());
    broker.assert_ledger_aggregates();
}

#[test]
fn mixed_oca_order_fill_reduces_exit_reserved_quantity() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    broker.place_pending_limit_short_order("TP".to_owned(), 1.0, 110.0, 1);
    broker.assign_pending_order_oca_named("TP", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.set_next_exit_oca_name(Some("g".to_owned()));
    broker.place_exit_stop("SL".to_owned(), "L".to_owned(), 90.0, 1);
    assert_eq!(
        broker
            .order_book
            .exits()
            .find_by_identity("SL", "L")
            .map(|pending| pending.reserved_quantity),
        Some(2.0)
    );
    broker.fill_pending_limit_short_entries(2, 20, 111.0);
    assert_eq!(broker.position_size, 1.0);
    assert_eq!(
        broker.orders.last().map(|order| order.id.as_str()),
        Some("TP")
    );
    assert_eq!(broker.orders.last().map(|order| order.qty), Some(1.0));
    assert_eq!(
        broker
            .order_book
            .exits()
            .find_by_identity("SL", "L")
            .map(|pending| pending.reserved_quantity),
        Some(1.0)
    );
    assert_eq!(
        broker.pending_exit_oca("SL", "L"),
        Some(&OcaGroupKey::new("g", OcaType::Reduce))
    );
    broker.assert_ledger_aggregates();
}

#[test]
fn mixed_oca_exit_fill_reduces_order_peer_to_zero() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    broker.place_pending_limit_short_order("TP".to_owned(), 2.0, 110.0, 1);
    broker.assign_pending_order_oca_named("TP", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.set_next_exit_oca_name(Some("g".to_owned()));
    broker.place_exit_stop("SL".to_owned(), "L".to_owned(), 90.0, 1);
    broker.evaluate_pending_exits(2, 20, 100.0, 89.0);
    assert_eq!(broker.position_size, 0.0);
    assert_eq!(
        broker.orders.last().map(|order| order.id.as_str()),
        Some("SL")
    );
    assert_eq!(broker.orders.last().map(|order| order.qty), Some(2.0));
    assert!(broker.order_book.entries().find_by_id("TP").is_none());
    assert!(broker.pending_entry_oca("TP").is_none());
    broker.assert_ledger_aggregates();
}

#[test]
fn mixed_oca_entry_fill_reduces_live_exit_peer() {
    let mut broker = pyramiding_broker(2);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    broker.set_next_exit_oca_name(Some("g".to_owned()));
    broker.place_exit_stop("SL".to_owned(), "L".to_owned(), 90.0, 1);
    broker.place_pending_limit_long_entry("L2".to_owned(), 1.0, 90.0, 1);
    broker.assign_pending_order_oca_named("L2", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.fill_pending_limit_long_entries(2, 20, 89.0);
    assert_eq!(broker.position_size, 3.0);
    assert_eq!(
        broker
            .order_book
            .exits()
            .find_by_identity("SL", "L")
            .map(|pending| pending.reserved_quantity),
        Some(1.0)
    );
    broker.assert_ledger_aggregates();
}

#[test]
fn mixed_oca_exit_fill_reduces_pending_entry_peer() {
    let mut broker = pyramiding_broker(2);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    broker.place_pending_limit_long_entry("L2".to_owned(), 2.0, 90.0, 1);
    broker.assign_pending_order_oca_named("L2", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.set_next_exit_oca_name(Some("g".to_owned()));
    broker.place_exit_stop("SL".to_owned(), "L".to_owned(), 90.0, 1);
    broker.evaluate_pending_exits(2, 20, 100.0, 89.0);
    assert_eq!(broker.position_size, 0.0);
    assert!(broker.order_book.entries().find_by_id("L2").is_none());
    assert!(broker.pending_entry_oca("L2").is_none());
    broker.assert_ledger_aggregates();
}

#[test]
fn mixed_oca_same_name_different_types_do_not_cancel_or_reduce() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_entry("E".to_owned(), 1.0, 90.0, 1);
    broker.place_pending_limit_long_order("O".to_owned(), 1.0, 90.0, 1);
    broker.assign_pending_order_oca_named("E", "g".to_owned(), Some("strategy.oca.cancel"));
    broker.assign_pending_order_oca_named("O", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.fill_pending_limit_long_entries(2, 20, 89.0);
    assert_eq!(broker.position_size, 2.0);
    assert_eq!(broker.orders.len(), 2);
    broker.assert_ledger_aggregates();
}

#[test]
fn mixed_oca_empty_name_does_not_join_a_group() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_entry("E".to_owned(), 1.0, 90.0, 1);
    broker.place_pending_limit_long_order("O".to_owned(), 1.0, 90.0, 1);
    broker.assign_pending_order_oca_named("E", String::new(), Some("strategy.oca.cancel"));
    broker.assign_pending_order_oca_named("O", String::new(), Some("strategy.oca.cancel"));
    assert!(broker.pending_entry_oca("E").is_none());
    assert!(broker.pending_entry_oca("O").is_none());
    broker.fill_pending_limit_long_entries(2, 20, 89.0);
    assert_eq!(broker.position_size, 2.0);
    broker.assert_ledger_aggregates();
}

#[test]
fn mixed_oca_unrelated_group_is_unchanged() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 100.0, 2.0));
    broker.place_pending_limit_short_order("TP".to_owned(), 1.0, 110.0, 1);
    broker.assign_pending_order_oca_named("TP", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.set_next_exit_oca_name(Some("other".to_owned()));
    broker.place_exit_stop("SL".to_owned(), "L".to_owned(), 90.0, 1);
    broker.fill_pending_limit_short_entries(2, 20, 111.0);
    assert_eq!(broker.position_size, 1.0);
    assert_eq!(
        broker
            .order_book
            .exits()
            .find_by_identity("SL", "L")
            .map(|pending| pending.reserved_quantity),
        Some(2.0),
        "ungrouped exclusive exit must not be reduced by a different OCA group"
    );
    broker.assert_ledger_aggregates();
}

#[test]
fn mixed_oca_reduce_uses_decimal_filled_quantity() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_entry("E".to_owned(), 0.25, 90.0, 1);
    broker.place_pending_limit_long_order("O".to_owned(), 1.0, 90.0, 1);
    broker.assign_pending_order_oca_named("E", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.assign_pending_order_oca_named("O", "g".to_owned(), Some("strategy.oca.reduce"));
    broker.fill_pending_limit_long_entries(2, 20, 89.0);
    assert_eq!(broker.position_size, 1.0);
    assert_eq!(broker.orders[0].qty, 0.25);
    assert_eq!(broker.orders[1].qty, 0.75);
    broker.assert_ledger_aggregates();
}

#[test]
fn mixed_oca_stop_limit_peer_is_cancelled_before_activation() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_entry("E".to_owned(), 1.0, 90.0, 1);
    broker.place_pending_stop_limit_long_order("S".to_owned(), 1.0, 110.0, 100.0, 1);
    broker.assign_pending_order_oca_named("E", "g".to_owned(), Some("strategy.oca.cancel"));
    broker.assign_pending_order_oca_named("S", "g".to_owned(), Some("strategy.oca.cancel"));
    broker.fill_pending_limit_long_entries(2, 20, 89.0);
    assert_eq!(broker.position_size, 1.0);
    assert!(broker.order_book.entries().find_by_id("S").is_none());
    assert!(broker.pending_entry_oca("S").is_none());
    broker.assert_ledger_aggregates();
}
