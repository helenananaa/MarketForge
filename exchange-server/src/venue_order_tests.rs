use super::*;
use axum::{body::Body, http::Request};
use tower::ServiceExt;

pub(super) fn scenario() -> ScenarioConfig {
    let market = |venue: &str, instrument: &str| {
        serde_json::json!({"Spot": {
            "instrument": {"instrument_id": instrument, "venue_id": venue,
                "symbol": instrument, "base_asset": "BASE", "quote_asset": "QUOTE",
                "tick_size": 1, "lot_size": 1},
            "clearing": {"maker_fee_ppm": 0, "taker_fee_ppm": 0},
            "risk": {"allow_short": false}
        }})
    };
    serde_json::from_value(serde_json::json!({
        "room_id": "venue-test", "market": market("A", "A-0"),
        "extra_markets": [market("A", "A-1"), market("B", "B-0")],
        "accounts": [{"Spot": {"account_id": 10, "cash_balance": 1000, "position_qty": 10}},
            {"Spot": {"account_id": 20, "cash_balance": 1000, "position_qty": 10}}],
        "seed_orders": []
    }))
    .unwrap()
}

fn buy(instrument: &str) -> GatewayRequest {
    GatewayRequest {
        participant_id: "human".into(),
        room_id: "venue-test".into(),
        instrument_id: Some(instrument.into()),
        account_id: 20,
        action: OrderAction::PlaceLimit {
            side: exchange_core::Side::Buy,
            price_tick: 100,
            qty: 2,
        },
    }
}

#[test]
fn venue_merge_preserves_other_commits_and_matches_serial_replay() {
    let mut original = RoomManager::new();
    original.create_room(scenario()).unwrap();
    // Preserve a non-empty committed history while rebasing a local candidate
    // log. Cross a history chunk boundary before preparing either venue.
    for id in 1..=300 {
        original
            .apply_to_instrument(
                "venue-test",
                "A-0",
                Command::CancelOrder(CancelOrder { order_id: id }),
            )
            .unwrap();
    }
    let predecessor = original.clone();
    let mut a = original
        .fork_venue_order("venue-test", "A-0")
        .unwrap()
        .unwrap();
    let mut b = original
        .fork_venue_order("venue-test", "B-0")
        .unwrap()
        .unwrap();
    let mut ar = OrderGateway::new(&mut a, 100)
        .submit_action(buy("A-0"))
        .unwrap();
    let mut br = OrderGateway::new(&mut b, 101)
        .submit_action(buy("B-0"))
        .unwrap();
    assert!(
        original
            .merge_venue_order(&predecessor, b, &mut br.execution)
            .unwrap()
    );
    assert!(
        original
            .merge_venue_order(&predecessor, a, &mut ar.execution)
            .unwrap()
    );
    assert_eq!(br.execution.command_seq, 300);
    assert_eq!(ar.execution.command_seq, 301);
    let mut serial = predecessor.clone();
    serial
        .apply_to_instrument("venue-test", "B-0", br.command)
        .unwrap();
    serial
        .apply_to_instrument("venue-test", "A-0", ar.command)
        .unwrap();
    assert_eq!(
        serde_json::to_value(original.simulation_room("venue-test").unwrap()).unwrap(),
        serde_json::to_value(serial.simulation_room("venue-test").unwrap()).unwrap()
    );
    assert_eq!(
        original.execution_history("venue-test").unwrap(),
        serial.execution_history("venue-test").unwrap()
    );
    // The predecessor remains an unchanged committed snapshot.
    assert!(
        predecessor
            .book_snapshot_for("venue-test", "A-0")
            .unwrap()
            .bids
            .is_empty()
    );
    let snapshot = serde_json::to_value(original.simulation_room("venue-test").unwrap()).unwrap();
    let restored: exchange_core::SimulationRoom = serde_json::from_value(snapshot.clone()).unwrap();
    assert_eq!(serde_json::to_value(restored).unwrap(), snapshot);
}

#[test]
fn stale_collateral_and_multi_execution_candidates_cannot_merge() {
    let mut original = RoomManager::new();
    original.create_room(scenario()).unwrap();
    let predecessor = original.clone();
    let mut a = original
        .fork_venue_order("venue-test", "A-0")
        .unwrap()
        .unwrap();
    let mut stale = original
        .fork_venue_order("venue-test", "A-1")
        .unwrap()
        .unwrap();
    let mut ar = OrderGateway::new(&mut a, 100)
        .submit_action(buy("A-0"))
        .unwrap();
    let mut sr = OrderGateway::new(&mut stale, 101)
        .submit_action(buy("A-1"))
        .unwrap();
    assert!(
        original
            .merge_venue_order(&predecessor, a, &mut ar.execution)
            .unwrap()
    );
    let committed = serde_json::to_value(original.simulation_room("venue-test").unwrap()).unwrap();
    assert!(
        !original
            .merge_venue_order(&predecessor, stale, &mut sr.execution)
            .unwrap()
    );
    assert_eq!(
        serde_json::to_value(original.simulation_room("venue-test").unwrap()).unwrap(),
        committed
    );
    let mut extra = predecessor
        .fork_venue_order("venue-test", "B-0")
        .unwrap()
        .unwrap();
    let mut gateway = OrderGateway::new(&mut extra, 102);
    let mut first = gateway.submit_action(buy("B-0")).unwrap();
    gateway.submit_action(buy("B-0")).unwrap();
    assert!(
        !original
            .merge_venue_order(&predecessor, extra, &mut first.execution)
            .unwrap()
    );
    assert_eq!(original.execution_history_len("venue-test").unwrap(), 1);
}

#[tokio::test]
async fn venue_lanes_allow_independent_prepare_and_fence_coordinated_writers() {
    let mut app = AppState::new("http://localhost");
    app.rooms.create_room(scenario()).unwrap();
    let store = Arc::new(room_runtime::StateStore::new(app));
    let a = store.venue_order("venue-test", Some("A-0")).await.unwrap();
    let b = tokio::time::timeout(
        Duration::from_secs(1),
        store.venue_order("venue-test", Some("B-0")),
    )
    .await
    .unwrap()
    .unwrap();
    let same_store = store.clone();
    let same = tokio::spawn(async move { same_store.venue_order("venue-test", Some("A-1")).await });
    tokio::task::yield_now().await;
    assert!(!same.is_finished());
    let writer_store = store.clone();
    let writer = tokio::spawn(async move { writer_store.lock_room("venue-test").await });
    tokio::task::yield_now().await;
    assert!(!writer.is_finished());
    // A queued writer must not deadlock a current venue's commit through a
    // recursive directory/coordination acquisition.
    drop(
        tokio::time::timeout(Duration::from_secs(1), b.commit_lock())
            .await
            .unwrap(),
    );
    drop(a);
    let same = tokio::time::timeout(Duration::from_secs(1), same)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!writer.is_finished());
    drop(same);
    drop(b);
    drop(
        tokio::time::timeout(Duration::from_secs(1), writer)
            .await
            .unwrap()
            .unwrap(),
    );
}

async fn request(
    app: &Router,
    path: &str,
    payload: serde_json::Value,
    key: Option<&str>,
) -> Response {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    if let Some(key) = key {
        builder = builder.header(IDEMPOTENCY_KEY_HEADER, key);
    }
    app.clone()
        .oneshot(builder.body(Body::from(payload.to_string())).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn venue_http_replays_cancel_and_conflicting_cross_venue_keys_are_atomic() {
    let app = new_app();
    assert_eq!(
        request(
            &app,
            "/rooms",
            serde_json::to_value(scenario()).unwrap(),
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    let order = |instrument: &str| {
        serde_json::to_value(SubmitOrderRequest {
            participant_id: "human".into(),
            account_id: 20,
            instrument_id: Some(instrument.into()),
            action: buy(instrument).action,
        })
        .unwrap()
    };
    let (a, b) = tokio::join!(
        request(
            &app,
            "/rooms/venue-test/orders",
            order("A-0"),
            Some("shared-key")
        ),
        request(
            &app,
            "/rooms/venue-test/orders",
            order("B-0"),
            Some("shared-key")
        )
    );
    assert!(
        (a.status() == StatusCode::OK && b.status() == StatusCode::CONFLICT)
            || (a.status() == StatusCode::CONFLICT && b.status() == StatusCode::OK)
    );
    let winner = if a.status() == StatusCode::OK { a } else { b };
    let body = axum::body::to_bytes(winner.into_body(), usize::MAX)
        .await
        .unwrap();
    let response: OrderResponse = serde_json::from_slice(&body).unwrap();
    let instrument = response.instrument_id.unwrap();
    let id = response
        .events
        .iter()
        .find_map(|event| match event {
            EventSummary::OrderAccepted { order_id, .. } => Some(*order_id),
            _ => None,
        })
        .unwrap();
    let cancel = serde_json::json!({"participant_id": "human", "account_id": 20,
        "instrument_id": instrument, "action": {"Cancel": {"order_id": id}}});
    let first = request(
        &app,
        "/rooms/venue-test/orders",
        cancel.clone(),
        Some("cancel-key"),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    let first = axum::body::to_bytes(first.into_body(), usize::MAX)
        .await
        .unwrap();
    let retry = request(&app, "/rooms/venue-test/orders", cancel, Some("cancel-key")).await;
    assert_eq!(retry.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(retry.into_body(), usize::MAX)
            .await
            .unwrap(),
        first
    );
}

#[derive(Clone)]
struct ControlledJournal {
    inner: Arc<Mutex<journal::InMemoryJournalStore>>,
    fail: Arc<AtomicBool>,
    block: Arc<AtomicBool>,
    entered: Arc<Notify>,
    release: Arc<(Mutex<bool>, std::sync::Condvar)>,
    admin: bool,
}

impl ControlledJournal {
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(journal::InMemoryJournalStore::new())),
            fail: Arc::new(AtomicBool::new(false)),
            block: Arc::new(AtomicBool::new(false)),
            entered: Arc::new(Notify::new()),
            release: Arc::new((Mutex::new(false), std::sync::Condvar::new())),
            admin: true,
        }
    }
}

impl JournalStore for ControlledJournal {
    fn load_recovery(&mut self) -> Result<JournalRecovery, JournalError> {
        self.inner.lock().unwrap().load_recovery()
    }
    fn create_room(
        &mut self,
        user: &str,
        scenario: &ScenarioConfig,
        bootstrap: &exchange_core::RoomBootstrap,
        accounts: &[AccountId],
        seeds: &[JournalExecution],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        self.inner
            .lock()
            .unwrap()
            .create_room(user, scenario, bootstrap, accounts, seeds, snapshot)
    }
    fn append_executions(
        &mut self,
        records: &[JournalExecution],
        snapshot: Option<&JournalSnapshot>,
    ) -> Result<(), JournalError> {
        if self.fail.swap(false, Ordering::SeqCst) {
            return Err(JournalError::Recovery("injected failure".into()));
        }
        if self.block.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            let (released, wake) = &*self.release;
            let mut released = released.lock().unwrap();
            while !*released {
                released = wake.wait(released).unwrap();
            }
        }
        self.inner
            .lock()
            .unwrap()
            .append_executions(records, snapshot)
    }
    fn find_idempotent_execution(
        &mut self,
        user: &str,
        room: &str,
        key: &str,
    ) -> Result<Option<JournalExecution>, JournalError> {
        self.inner
            .lock()
            .unwrap()
            .find_idempotent_execution(user, room, key)
    }
    fn external_action_count(
        &mut self,
        user: &str,
        room: &str,
        step: u64,
    ) -> Result<u32, JournalError> {
        self.inner
            .lock()
            .unwrap()
            .external_action_count(user, room, step)
    }
    fn user_can_administer_room(&mut self, _user: &str, _room: &str) -> Result<bool, JournalError> {
        Ok(self.admin)
    }
    fn update_room_status(&mut self, room: &str, status: MarketStatus) -> Result<(), JournalError> {
        self.inner.lock().unwrap().update_room_status(room, status)
    }
}

#[tokio::test]
async fn venue_failed_journal_preserves_state_and_interleaved_commits_recover() {
    let mut journal = ControlledJournal::new();
    let shared = shared_state(AppState::new_with_journal(
        "http://localhost",
        Box::new(journal.clone()),
    ));
    let router = app(shared.clone());
    assert_eq!(
        request(
            &router,
            "/rooms",
            serde_json::to_value(scenario()).unwrap(),
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    let before = shared.app.read_room("venue-test").await.rooms;
    journal.fail.store(true, Ordering::SeqCst);
    let payload = |instrument: &str| {
        serde_json::json!({"participant_id": "human", "account_id": 20,
        "instrument_id": instrument, "action": {"PlaceLimit": {"side": "Buy", "price_tick": 100, "qty": 1}}})
    };
    assert_eq!(
        request(&router, "/rooms/venue-test/orders", payload("A-0"), None)
            .await
            .status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        serde_json::to_value(
            shared
                .app
                .read_room("venue-test")
                .await
                .rooms
                .simulation_room("venue-test")
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(before.simulation_room("venue-test").unwrap()).unwrap()
    );
    assert!(journal.load_recovery().unwrap().executions.is_empty());
    for _ in 0..12 {
        let (a, b) = tokio::join!(
            request(&router, "/rooms/venue-test/orders", payload("A-0"), None),
            request(&router, "/rooms/venue-test/orders", payload("B-0"), None)
        );
        assert_eq!((a.status(), b.status()), (StatusCode::OK, StatusCode::OK));
    }
    let recovered = AppState::recover_with_journal_bundle_and_auth_policy(
        "http://localhost",
        JournalStoreBundle::single(Box::new(journal.clone())),
        AuthPolicy::local_development(),
        None,
    )
    .unwrap();
    let current = shared.app.read_room("venue-test").await;
    assert_eq!(
        serde_json::to_value(current.rooms.simulation_room("venue-test").unwrap()).unwrap(),
        serde_json::to_value(recovered.rooms.simulation_room("venue-test").unwrap()).unwrap()
    );
    let recovery = journal.load_recovery().unwrap();
    assert_eq!(recovery.executions.len(), 24);
    assert_eq!(
        recovery
            .executions
            .iter()
            .map(|e| e.command_seq)
            .collect::<Vec<_>>(),
        (0..24).collect::<Vec<_>>()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn venue_cancelled_request_keeps_uncommitted_snapshot_private_and_finishes_install() {
    let journal = ControlledJournal::new();
    let shared = shared_state(AppState::new_with_journal(
        "http://localhost",
        Box::new(journal.clone()),
    ));
    let router = app(shared.clone());
    assert_eq!(
        request(
            &router,
            "/rooms",
            serde_json::to_value(scenario()).unwrap(),
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    let before = shared.app.read_room("venue-test").await.rooms;
    journal.block.store(true, Ordering::SeqCst);
    let task_router = router.clone();
    let pending =
        tokio::spawn(async move {
            request(&task_router, "/rooms/venue-test/orders",
        serde_json::json!({"participant_id": "human", "account_id": 20, "instrument_id": "B-0",
            "action": {"PlaceLimit": {"side": "Buy", "price_tick": 100, "qty": 1}}}), None).await
        });
    tokio::time::timeout(Duration::from_secs(2), journal.entered.notified())
        .await
        .unwrap();
    let during =
        tokio::time::timeout(Duration::from_secs(2), shared.app.read_room("venue-test")).await;
    pending.abort();
    let _ = pending.await;
    let (released, wake) = &*journal.release;
    *released.lock().unwrap() = true;
    wake.notify_all();
    assert_eq!(
        serde_json::to_value(during.unwrap().rooms.simulation_room("venue-test").unwrap()).unwrap(),
        serde_json::to_value(before.simulation_room("venue-test").unwrap()).unwrap()
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if !shared
                .app
                .read_room("venue-test")
                .await
                .rooms
                .book_snapshot_for("venue-test", "B-0")
                .unwrap()
                .bids
                .is_empty()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        journal
            .inner
            .lock()
            .unwrap()
            .load_recovery()
            .unwrap()
            .executions
            .len(),
        1
    );
}

#[tokio::test]
async fn venue_quota_is_shared_across_concurrent_venues() {
    let mut journal = ControlledJournal::new();
    journal.admin = false;
    let router = new_app_with_journal("http://localhost", Box::new(journal.clone()));
    assert_eq!(
        request(
            &router,
            "/rooms",
            serde_json::to_value(scenario()).unwrap(),
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    let payload = |instrument: &str| {
        serde_json::json!({"participant_id": "human", "account_id": 20,
        "instrument_id": instrument, "action": {"PlaceLimit": {"side": "Buy", "price_tick": 1, "qty": 1}}})
    };
    for _ in 0..EXTERNAL_ACTIONS_PER_STEP / 2 {
        let (a, b) = tokio::join!(
            request(&router, "/rooms/venue-test/orders", payload("A-0"), None),
            request(&router, "/rooms/venue-test/orders", payload("B-0"), None)
        );
        assert_eq!((a.status(), b.status()), (StatusCode::OK, StatusCode::OK));
    }
    let (a, b) = tokio::join!(
        request(&router, "/rooms/venue-test/orders", payload("A-0"), None),
        request(&router, "/rooms/venue-test/orders", payload("B-0"), None)
    );
    assert_eq!(
        (a.status(), b.status()),
        (StatusCode::TOO_MANY_REQUESTS, StatusCode::TOO_MANY_REQUESTS)
    );
    assert_eq!(
        journal.load_recovery().unwrap().executions.len(),
        EXTERNAL_ACTIONS_PER_STEP as usize
    );
}

#[tokio::test]
async fn venue_shared_collateral_cannot_be_reserved_twice_by_different_products() {
    let shared = shared_state(AppState::new("http://localhost"));
    let router = app(shared.clone());
    assert_eq!(
        request(
            &router,
            "/rooms",
            serde_json::to_value(scenario()).unwrap(),
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    let payload = |instrument: &str| {
        serde_json::json!({"participant_id": "human", "account_id": 20,
        "instrument_id": instrument, "action": {"PlaceLimit": {"side": "Buy", "price_tick": 100, "qty": 10}}})
    };
    let (a, b) = tokio::join!(
        request(&router, "/rooms/venue-test/orders", payload("A-0"), None),
        request(&router, "/rooms/venue-test/orders", payload("A-1"), None)
    );
    assert_eq!((a.status(), b.status()), (StatusCode::OK, StatusCode::OK));
    let current = shared.app.read_room("venue-test").await;
    let a = current
        .rooms
        .book_snapshot_for("venue-test", "A-0")
        .unwrap();
    let b = current
        .rooms
        .book_snapshot_for("venue-test", "A-1")
        .unwrap();
    assert_eq!(a.bids.len() + b.bids.len(), 1);
    let account = current
        .rooms
        .simulation_room("venue-test")
        .unwrap()
        .exchange("A")
        .unwrap()
        .venue_account_snapshot(20);
    let quote = account
        .balances
        .iter()
        .find(|a| a.asset_id == "QUOTE")
        .unwrap();
    assert_eq!(quote.total, 1000);
    assert_eq!(quote.reserved, 1000);
}

#[test]
fn venue_dormant_conditional_order_requires_coordinated_path() {
    let mut config = serde_json::to_value(scenario()).unwrap();
    let market = |venue: &str, instrument: &str| {
        serde_json::json!({"Perp": {
        "instrument": {"instrument_id": instrument, "venue_id": venue, "symbol": instrument,
            "base_asset": "BASE", "quote_asset": "QUOTE", "tick_size": 1, "lot_size": 1},
        "clearing": {"maker_fee_ppm": 0, "taker_fee_ppm": 0, "leverage": 5},
        "risk": {}, "initial_mark_price_tick": 100 }})
    };
    config["market"] = market("A", "A-0");
    config["extra_markets"] = serde_json::json!([market("B", "B-0")]);
    config["accounts"] = serde_json::json!([{"Basic": {"account_id": 20, "cash_balance": 1000}}]);
    let mut rooms = RoomManager::new();
    rooms
        .create_room(serde_json::from_value(config).unwrap())
        .unwrap();
    let execution = rooms
        .apply_to_instrument(
            "venue-test",
            "B-0",
            Command::SetConditionalOrder {
                account_id: 20,
                key: "dormant".into(),
                spec: Some(Box::new(
                    exchange_core::conditional_orders::ConditionalOrderSpec {
                        side: exchange_core::Side::Buy,
                        position_side: Default::default(),
                        qty: 1,
                        trigger_price_tick: 200,
                        above: true,
                        trigger: Default::default(),
                        limit_price_tick: Some(100),
                        protection: None,
                    },
                )),
            },
        )
        .unwrap();
    assert!(matches!(
        execution.result,
        ActorExecutionResult::Accepted(_)
    ));
    assert!(
        rooms
            .fork_venue_order("venue-test", "A-0")
            .unwrap()
            .is_none()
    );
}
