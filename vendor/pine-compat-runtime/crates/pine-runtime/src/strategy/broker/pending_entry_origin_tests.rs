use super::pending_entries::{PendingEntry, PendingEntryDirection, PendingEntryKind};
use super::types::{InternalOrderKey, StrategyCommandOrigin};
use super::*;

fn pending(broker: &BrokerState, id: &str) -> PendingEntry {
    broker
        .order_book
        .entries()
        .find_by_id(id)
        .cloned()
        .unwrap_or_else(|| panic!("missing pending {id}"))
}

#[test]
fn pending_entry_records_entry_origin_and_stable_key() {
    let mut broker = BrokerState::new(100_000.0);

    broker.place_pending_market_long_entry("L".to_owned(), 2.0, 0);

    let recorded = pending(&broker, "L");
    assert_eq!(recorded.origin, StrategyCommandOrigin::Entry);
    assert_eq!(recorded.key, InternalOrderKey(0));
    assert_eq!(recorded.creation_sequence(), 0);
    assert!(recorded.enforce_pyramiding());
}

#[test]
fn pending_generic_order_records_order_origin_and_bypasses_pyramiding() {
    let mut broker = BrokerState::new(100_000.0);

    broker.place_pending_market_long_order("O".to_owned(), 2.0, 0);

    let recorded = pending(&broker, "O");
    assert_eq!(recorded.origin, StrategyCommandOrigin::Order);
    assert_eq!(recorded.key, InternalOrderKey(0));
    assert!(!recorded.enforce_pyramiding());
}

#[test]
fn pending_price_based_records_origin_for_entry_and_order_kinds() {
    let mut broker = BrokerState::new(100_000.0);

    broker.place_pending_limit_long_entry("LE".to_owned(), 1.0, 100.0, 0);
    broker.place_pending_stop_long_entry("SE".to_owned(), 1.0, 110.0, 0);
    broker.place_pending_stop_limit_long_entry("SLE".to_owned(), 1.0, 120.0, 115.0, 0);
    broker.place_pending_limit_long_order("LO".to_owned(), 1.0, 90.0, 0);
    broker.place_pending_stop_long_order("SO".to_owned(), 1.0, 130.0, 0);
    broker.place_pending_stop_limit_long_order("SLO".to_owned(), 1.0, 140.0, 135.0, 0);

    assert_eq!(pending(&broker, "LE").origin, StrategyCommandOrigin::Entry);
    assert_eq!(pending(&broker, "SE").origin, StrategyCommandOrigin::Entry);
    assert_eq!(pending(&broker, "SLE").origin, StrategyCommandOrigin::Entry);
    assert_eq!(pending(&broker, "LO").origin, StrategyCommandOrigin::Order);
    assert_eq!(pending(&broker, "SO").origin, StrategyCommandOrigin::Order);
    assert_eq!(pending(&broker, "SLO").origin, StrategyCommandOrigin::Order);
    assert_eq!(pending(&broker, "LE").key, InternalOrderKey(0));
    assert_eq!(pending(&broker, "SE").key, InternalOrderKey(1));
    assert_eq!(pending(&broker, "SLE").key, InternalOrderKey(2));
    assert_eq!(pending(&broker, "LO").key, InternalOrderKey(3));
    assert_eq!(pending(&broker, "SO").key, InternalOrderKey(4));
    assert_eq!(pending(&broker, "SLO").key, InternalOrderKey(5));
}

#[test]
fn same_id_replacement_preserves_creation_sequence_and_updates_bar() {
    let mut broker = BrokerState::new(100_000.0);

    broker.place_pending_market_long_entry("L".to_owned(), 1.0, 0);
    let original_key = pending(&broker, "L").key;
    broker.place_pending_market_long_entry("L".to_owned(), 3.0, 1);
    broker.place_pending_market_long_entry("M".to_owned(), 1.0, 1);

    let replaced = pending(&broker, "L");
    assert_eq!(replaced.key, original_key);
    assert_eq!(replaced.key, InternalOrderKey(0));
    assert_eq!(replaced.quantity, 3.0);
    assert_eq!(replaced.created_bar_index, 1);
    assert_eq!(replaced.origin, StrategyCommandOrigin::Entry);
    assert_eq!(pending(&broker, "M").key, InternalOrderKey(1));
}

#[test]
fn cancel_by_public_id_removes_matching_entry_and_keeps_other_keys() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_market_long_entry("A".to_owned(), 1.0, 0);
    broker.place_pending_market_long_order("B".to_owned(), 2.0, 0);

    broker.cancel_pending_order("A");

    assert!(broker.order_book.entries().find_by_id("A").is_none());
    let remaining = pending(&broker, "B");
    assert_eq!(remaining.origin, StrategyCommandOrigin::Order);
    assert_eq!(remaining.key, InternalOrderKey(1));
}

#[test]
fn cloned_pending_book_preserves_origin_keys_and_next_sequence() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_market_long_entry("A".to_owned(), 1.0, 0);
    let snapshot = broker.clone();
    broker.place_pending_market_long_entry("B".to_owned(), 1.0, 0);

    assert_eq!(pending(&broker, "A").key, InternalOrderKey(0));
    assert_eq!(pending(&broker, "B").key, InternalOrderKey(1));
    assert_eq!(pending(&snapshot, "A").key, InternalOrderKey(0));
    assert!(snapshot.order_book.entries().find_by_id("B").is_none());

    let mut restored = snapshot;
    restored.place_pending_market_long_entry("C".to_owned(), 1.0, 0);
    assert_eq!(pending(&restored, "C").key, InternalOrderKey(1));
    assert_eq!(pending(&restored, "C").origin, StrategyCommandOrigin::Entry);
}

#[test]
fn pending_entry_equality_includes_origin_and_key() {
    let left = PendingEntry {
        id: "L".to_owned(),
        key: InternalOrderKey(0),
        origin: StrategyCommandOrigin::Entry,
        direction: PendingEntryDirection::Long,
        kind: PendingEntryKind::Market,
        quantity: 1.0,
        same_bar_percent_of_equity: None,
        created_bar_index: 0,
        metadata: StrategyOrderMetadata::default(),
        enforce_pyramiding: true,
    };
    let mut different_key = left.clone();
    different_key.key = InternalOrderKey(1);
    let mut different_origin = left.clone();
    different_origin.origin = StrategyCommandOrigin::Order;
    different_origin.enforce_pyramiding = false;

    assert_eq!(left, left.clone());
    assert_ne!(left, different_key);
    assert_ne!(left, different_origin);
    assert!(!different_origin.enforce_pyramiding());
}
