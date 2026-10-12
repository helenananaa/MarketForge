use super::pending_closes::PendingCloseQuantity;
use super::pending_exits::{PendingExit, PendingExitQuantity, PendingExitTrigger};
use super::types::{InternalOrderKey, StrategyCommandOrigin};
use super::*;
use crate::output::json::public_runtime_result_json;
use crate::output::model::RuntimeResult;
use crate::output::strategy::StrategyResult;

fn pending_entry(broker: &BrokerState, id: &str) -> super::pending_entries::PendingEntry {
    broker
        .order_book
        .entries()
        .find_by_id(id)
        .cloned()
        .unwrap_or_else(|| panic!("missing pending {id}"))
}

#[test]
fn alternating_entry_exit_close_share_one_increasing_sequence() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_market_long_entry("E".to_owned(), 1.0, 0);
    broker.place_pending_close(
        "E".to_owned(),
        PendingCloseQuantity::Full,
        0,
        StrategyOrderMetadata::default(),
    );
    broker.order_book.replace_or_append_exit(PendingExit {
        key: InternalOrderKey(0),
        id: "X".to_owned(),
        from_entry: "E".to_owned(),
        target_trade_key: None,
        trigger: PendingExitTrigger::Stop(90.0),
        quantity: PendingExitQuantity::Full,
        reserved_quantity: 1.0,
        multiple_reservation: false,
        last_update_bar_index: 0,
        metadata: StrategyExitMetadata::default(),
    });

    assert_eq!(pending_entry(&broker, "E").key, InternalOrderKey(0));
    assert_eq!(
        broker
            .order_book
            .closes()
            .find_close_by_id("E")
            .expect("close")
            .key,
        InternalOrderKey(1)
    );
    assert_eq!(
        broker.order_book.exits().current().expect("exit").key,
        InternalOrderKey(2)
    );
    assert_eq!(broker.order_book.next_order_sequence(), 3);
}

#[test]
fn same_id_replacement_keeps_key_across_families() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_market_long_entry("E".to_owned(), 1.0, 0);
    broker.place_pending_close(
        "E".to_owned(),
        PendingCloseQuantity::Full,
        0,
        StrategyOrderMetadata::default(),
    );
    broker.order_book.replace_or_append_exit(PendingExit {
        key: InternalOrderKey(0),
        id: "X".to_owned(),
        from_entry: "E".to_owned(),
        target_trade_key: None,
        trigger: PendingExitTrigger::Stop(90.0),
        quantity: PendingExitQuantity::Full,
        reserved_quantity: 1.0,
        multiple_reservation: false,
        last_update_bar_index: 0,
        metadata: StrategyExitMetadata::default(),
    });
    let entry_key = pending_entry(&broker, "E").key;
    let close_key = broker
        .order_book
        .closes()
        .find_close_by_id("E")
        .expect("close")
        .key;
    let exit_key = broker.order_book.exits().current().expect("exit").key;

    broker.place_pending_market_long_entry("E".to_owned(), 2.0, 1);
    broker.place_pending_close(
        "E".to_owned(),
        PendingCloseQuantity::Qty(1.0),
        1,
        StrategyOrderMetadata::default(),
    );
    broker.order_book.replace_or_append_exit(PendingExit {
        key: InternalOrderKey(0),
        id: "X".to_owned(),
        from_entry: "E".to_owned(),
        target_trade_key: None,
        trigger: PendingExitTrigger::Stop(85.0),
        quantity: PendingExitQuantity::Full,
        reserved_quantity: 2.0,
        multiple_reservation: false,
        last_update_bar_index: 1,
        metadata: StrategyExitMetadata::default(),
    });

    assert_eq!(pending_entry(&broker, "E").key, entry_key);
    assert_eq!(pending_entry(&broker, "E").quantity, 2.0);
    assert_eq!(
        broker
            .order_book
            .closes()
            .find_close_by_id("E")
            .expect("close")
            .key,
        close_key
    );
    assert_eq!(
        broker.order_book.exits().current().expect("exit").key,
        exit_key
    );
    assert_eq!(broker.order_book.next_order_sequence(), 3);
}

#[test]
fn cancel_then_new_placement_does_not_reuse_key() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_market_long_entry("A".to_owned(), 1.0, 0);
    broker.cancel_pending_order("A");
    broker.place_pending_market_long_entry("A".to_owned(), 1.0, 1);
    assert_eq!(pending_entry(&broker, "A").key, InternalOrderKey(1));
    assert_eq!(broker.order_book.next_order_sequence(), 2);
}

#[test]
fn snapshot_restore_continues_from_saved_next_key() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_market_long_entry("A".to_owned(), 1.0, 0);
    let snapshot = broker.snapshot();
    broker.place_pending_market_long_entry("B".to_owned(), 1.0, 0);
    assert_eq!(pending_entry(&broker, "B").key, InternalOrderKey(1));

    let mut restored = snapshot;
    restored.place_pending_close(
        "C".to_owned(),
        PendingCloseQuantity::Full,
        0,
        StrategyOrderMetadata::default(),
    );
    assert_eq!(
        restored
            .order_book
            .closes()
            .find_close_by_id("C")
            .expect("close")
            .key,
        InternalOrderKey(1)
    );
}

#[test]
fn forming_rollback_discards_abandoned_keys() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_market_long_entry("A".to_owned(), 1.0, 0);
    let confirmed = broker.snapshot();
    broker.place_pending_market_long_order("B".to_owned(), 1.0, 0);
    broker.place_pending_close_all(
        PendingCloseQuantity::Full,
        0,
        StrategyOrderMetadata::default(),
    );
    broker.restore(confirmed.clone());
    broker.place_pending_market_long_entry("C".to_owned(), 1.0, 1);
    assert!(broker.order_book.entries().find_by_id("B").is_none());
    assert_eq!(pending_entry(&broker, "C").key, InternalOrderKey(1));
    assert_eq!(
        pending_entry(&broker, "C").origin,
        StrategyCommandOrigin::Entry
    );
}

#[test]
fn expanded_exits_receive_ledger_ordered_keys() {
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
    broker.place_all_entry_exit_profit_ticks("X".to_owned(), 10.0, 1.0, 1);
    let keys: Vec<u64> = broker
        .order_book
        .exits()
        .iter()
        .map(|pending| pending.creation_sequence())
        .collect();
    assert_eq!(keys, vec![0, 1]);
    let from_entries: Vec<String> = broker
        .order_book
        .exits()
        .iter()
        .map(|pending| pending.from_entry.clone())
        .collect();
    assert_eq!(from_entries, vec!["A".to_owned(), "B".to_owned()]);
}

#[test]
fn public_strategy_json_does_not_contain_internal_order_key() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("E".to_owned(), 0, 10, 100.0, 1.0));
    let result = RuntimeResult {
        plots: Vec::new(),
        plot_chars: Vec::new(),
        plot_shapes: Vec::new(),
        plot_arrows: Vec::new(),
        plot_bars: Vec::new(),
        plot_candles: Vec::new(),
        bg_colors: Vec::new(),
        bar_colors: Vec::new(),
        hlines: Vec::new(),
        fills: Vec::new(),
        labels: Vec::new(),
        lines: Vec::new(),
        line_fills: Vec::new(),
        polylines: Vec::new(),
        boxes: Vec::new(),
        tables: Vec::new(),
        alerts: Vec::new(),
        strategy: Some(StrategyResult {
            orders: broker.orders.to_vec(),
            trades: broker.trades.to_vec(),
            position: broker.position.to_vec(),
            equity: broker.equity.to_vec(),
            alerts: Vec::new(),
            diagnostics: broker.diagnostics.clone(),
        }),
        diagnostics: Vec::new(),
    };
    let encoded = public_runtime_result_json(&result);
    assert!(encoded.contains("\"orders\""));
    assert!(!encoded.to_ascii_lowercase().contains("creation_sequence"));
    assert!(!encoded.contains("InternalOrderKey"));
    assert!(!encoded.contains("\"orderKey\""));
}
