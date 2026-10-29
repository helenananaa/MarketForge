use super::*;
use crate::TradingApi;
use crate::{
    ActorExecutionResult, ActorRejectReason, GatewayExecution, GatewayRequest, OrderGateway,
    RoomManager, population,
};

fn modern(params: serde_json::Value) -> AgentTemplate {
    let mut t = template(
        "ExecutionTrader",
        json!({"target_qty":12,"horizon_ms":4000,"max_qty":10,"max_slippage_ticks":30,"liquidity_aware":true}),
    );
    if let AgentTemplate::Plugin(p) = &mut t {
        p.plugin_version = "1.1.0".into();
        p.config
            .as_object_mut()
            .unwrap()
            .extend(params.as_object().unwrap().clone());
    }
    t
}
fn data(b: &dyn ScheduledBot) -> serde_json::Value {
    let PersistedAgentKindState::Plugin { data, .. } = b.snapshot() else {
        panic!()
    };
    data.as_ref().clone()
}
fn thin(now: u64) -> ParticipantObservation {
    let mut v = view(now, 100, 10);
    v.book.asks[0].qty = 2;
    v.book.asks.push(BookLevel {
        price_tick: 102,
        qty: 100,
    });
    v
}
fn real_partial() -> (
    AgentTemplate,
    Box<dyn ScheduledBot>,
    OrderAction,
    GatewayExecution,
) {
    let mut spec: population::BackgroundMarket =
        serde_json::from_str(include_str!("../../scripts/fixtures/behavior_market.json")).unwrap();
    spec.scenario.seed_orders.clear();
    spec.scenario.seed_orders.push(serde_json::from_value(json!({"NewOrder":{"order_id":10002,"account_id":10,"side":"Sell","kind":{"Limit":{"price_tick":101}},"qty":1,"reduce_only":false}})).unwrap());
    let room = spec.scenario.room_id.clone();
    let mut rooms = RoomManager::new();
    rooms.create_room(spec.scenario).unwrap();
    let mut t = modern(json!({}));
    if let AgentTemplate::Plugin(p) = &mut t {
        p.participant.room_id = room.clone();
    }
    let mut b = BotRegistry::with_builtins()
        .create(&t, &t.initial_state())
        .unwrap();
    let action = b.decide(&thin(1000)).unwrap().remove(0);
    let mut gateway = OrderGateway::new_scheduler(&mut rooms, 100000);
    let mut e = gateway
        .submit_action(GatewayRequest {
            participant_id: "test".into(),
            room_id: room,
            instrument_id: Some("V-BTC-SPOT".into()),
            account_id: 20,
            action: action.clone(),
        })
        .unwrap();
    e.execution.market_time_ms = 1000;
    e.execution.command_seq = 20;
    (t, b, action, e)
}

#[test]
fn touch_parent_does_not_sweep_deeper_liquidity_or_mark_submission_as_fill() {
    let t = modern(json!({}));
    let mut b = BotRegistry::with_builtins()
        .create(&t, &t.initial_state())
        .unwrap();
    let actions = b.decide(&thin(1000)).unwrap();
    assert!(matches!(
        &actions[..],
        [OrderAction::PlaceProtected {
            side: Side::Buy,
            qty: 2,
            price_tick: 101,
            valid_until_market_time_ms: Some(5000),
            ..
        }]
    ));
    let s = data(b.as_ref());
    assert_eq!(s["completed_qty"], 0);
    assert_eq!(s["execution_ledger"]["remaining_lots"], 12);
    assert_eq!(s["execution_ledger"]["submitted_lots"], 2);
}

#[test]
fn in_flight_child_restores_and_position_changes_do_not_invent_fills() {
    let t = modern(json!({}));
    let r = BotRegistry::with_builtins();
    let mut b = r.create(&t, &t.initial_state()).unwrap();
    b.decide(&thin(1000)).unwrap();
    let saved = b.snapshot();
    let mut restored = r.create(&t, &saved).unwrap();
    let mut v = thin(2000);
    if let Some(AccountSnapshot::Spot(a)) = &mut v.own_account {
        a.position_qty = 90;
        a.available_position = 90;
    }
    assert!(b.decide(&v).unwrap().is_empty());
    assert!(restored.decide(&v).unwrap().is_empty());
    assert_eq!(b.snapshot(), restored.snapshot());
    assert_eq!(data(b.as_ref())["execution_ledger"]["filled_lots"], 0);
}

#[test]
fn authoritative_partial_fill_rejection_and_duplicates_preserve_parent_debt() {
    let (_, mut b, action, e) = real_partial();
    let mut foreign = e.clone();
    foreign.account_id = 999;
    b.execution_feedback(&action, &foreign);
    assert!(data(b.as_ref())["execution_ledger"]["pending"].is_object());
    b.execution_feedback(&action, &e);
    b.execution_feedback(&action, &e);
    assert_eq!(data(b.as_ref())["execution_ledger"]["filled_lots"], 1);
    assert_eq!(data(b.as_ref())["execution_ledger"]["remaining_lots"], 11);
    let next = b.decide(&thin(2000)).unwrap().remove(0);
    let mut rejected = e.clone();
    rejected.action = next.clone();
    rejected.execution.command_seq = 21;
    rejected.execution.market_time_ms = 2000;
    rejected.execution.result = ActorExecutionResult::Rejected(ActorRejectReason::MarketPaused);
    b.execution_feedback(&next, &rejected);
    assert_eq!(data(b.as_ref())["execution_ledger"]["remaining_lots"], 11);
    assert!(data(b.as_ref())["execution_ledger"]["pending"].is_null());
    let retry = b.decide(&thin(3000)).unwrap();
    assert!(!retry.is_empty());
    b.execution_feedback(&action, &e);
    assert_eq!(data(b.as_ref())["completed_qty"], 1);
}

#[test]
fn deadline_expires_only_residual_and_late_authoritative_ack_corrects_accounting() {
    let (t, mut b, action, e) = real_partial();
    assert!(b.decide(&thin(5000)).unwrap().is_empty());
    assert_eq!(data(b.as_ref())["execution_ledger"]["expired_lots"], 12);
    b.execution_feedback(&action, &e);
    b.execution_feedback(&action, &e);
    let s = data(b.as_ref());
    assert_eq!(s["execution_ledger"]["filled_lots"], 1);
    assert_eq!(s["execution_ledger"]["expired_lots"], 11);
    assert_eq!(s["execution_ledger"]["remaining_lots"], 0);
    assert!(b.decide(&thin(6000)).unwrap().is_empty());
    BotRegistry::with_builtins()
        .create(&t, &b.snapshot())
        .unwrap();
}

#[test]
fn sell_parent_remains_limited_by_real_spot_inventory_and_start_time() {
    let t = modern(json!({"side":"Sell","start_after_ms":2000}));
    let r = BotRegistry::with_builtins();
    let mut b = r.create(&t, &t.initial_state()).unwrap();
    assert!(b.decide(&thin(1000)).unwrap().is_empty());
    assert!(data(b.as_ref())["execution_ledger"].is_null());
    let mut v = view(2000, 100, 1);
    v.book.bids[0].qty = 2;
    let a = b.decide(&v).unwrap();
    assert!(matches!(
        &a[..],
        [OrderAction::PlaceProtected {
            side: Side::Sell,
            qty: 1,
            price_tick: 99,
            ..
        }]
    ));
}

#[test]
fn old_versions_and_invalid_parent_snapshots_fail_closed_without_changing_legacy_state() {
    let r = BotRegistry::with_builtins();
    for enabled in [false, true] {
        let t = template("ExecutionTrader", json!({"liquidity_aware":enabled}));
        assert!(r.create(&t, &t.initial_state()).is_err());
    }
    let old = template("ExecutionTrader", json!({}));
    let mut b = r.create(&old, &old.initial_state()).unwrap();
    b.decide(&view(1000, 100, 10)).unwrap();
    assert!(data(b.as_ref()).get("execution_ledger").is_none());
    let t = modern(json!({}));
    let mut b = r.create(&t, &t.initial_state()).unwrap();
    b.decide(&thin(1000)).unwrap();
    let mut bad = b.snapshot();
    if let PersistedAgentKindState::Plugin { data, .. } = &mut bad {
        std::sync::Arc::make_mut(data)["execution_ledger"]["remaining_lots"] = json!(13);
    }
    assert!(r.create(&t, &bad).is_err());
}

#[test]
fn expiry_is_clock_driven_even_when_account_or_liquidity_is_unavailable() {
    let t = modern(json!({}));
    let mut b = BotRegistry::with_builtins()
        .create(&t, &t.initial_state())
        .unwrap();
    b.decide(&thin(1000)).unwrap();
    let mut v = thin(5000);
    v.own_account = None;
    v.book.asks.clear();
    v.book.bids.clear();
    assert!(b.decide(&v).unwrap().is_empty());
    assert_eq!(data(b.as_ref())["execution_ledger"]["expired_lots"], 12);
    assert_eq!(data(b.as_ref())["execution_ledger"]["remaining_lots"], 0);
}

#[test]
fn clock_regression_cannot_reopen_an_expired_parent() {
    let t = modern(json!({}));
    let mut b = BotRegistry::with_builtins()
        .create(&t, &t.initial_state())
        .unwrap();
    b.decide(&thin(1000)).unwrap();
    b.decide(&thin(5000)).unwrap();
    let saved = b.snapshot();
    assert!(b.decide(&thin(3000)).is_err());
    assert_eq!(b.snapshot(), saved);
}

#[test]
fn feedback_can_revisit_debt_but_not_reuse_the_same_clock_with_a_new_step() {
    let (_, mut b, action, e) = real_partial();
    b.execution_feedback(&action, &e);
    let mut same = thin(1000);
    same.step += 100;
    assert!(b.decide(&same).unwrap().is_empty());
    let mut t = modern(json!({"decision_interval_ms":7000,"horizon_ms":20000}));
    if let AgentTemplate::Plugin(p) = &mut t {
        p.participant.room_id = e.room_id.clone();
    }
    let mut b = BotRegistry::with_builtins()
        .create(&t, &t.initial_state())
        .unwrap();
    let a = b.decide(&thin(1000)).unwrap().remove(0);
    let mut rejected = e.clone();
    rejected.action = a.clone();
    rejected.execution.result = ActorExecutionResult::Rejected(ActorRejectReason::MarketPaused);
    b.execution_feedback(&a, &rejected);
    assert!(!b.decide(&thin(2000)).unwrap().is_empty());
}
