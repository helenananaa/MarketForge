use exchange_core::{Command, OrderBook};
use serde_json::Value;

#[test]
fn mixed_commands_match_frozen_pre_refactor_events_and_depth() {
    // Generated from the frozen pre-refactor queue implementation, not from
    // the indexed-node implementation under test. Five independent seeds.
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/legacy_orderbook.json")).unwrap();
    let mut book = OrderBook::new();
    for case in cases {
        if case["seq"] == 1 {
            book = OrderBook::new();
        }
        let command: Command = serde_json::from_value(case["command"].clone()).unwrap();
        let events = book.apply(command);
        assert_eq!(
            serde_json::to_value(events).unwrap(),
            case["events"],
            "seed={} seq={}",
            case["seed"],
            case["seq"]
        );
        assert_eq!(
            serde_json::to_value(book.snapshot()).unwrap(),
            case["snapshot"]
        );
        if case["seq"].as_u64().unwrap() % 8 == 0 {
            book = serde_json::from_value(serde_json::to_value(book).unwrap()).unwrap();
        }
    }
}
