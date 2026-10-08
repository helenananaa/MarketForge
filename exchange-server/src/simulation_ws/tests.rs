use super::*;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message as ClientMessage, client::IntoClientRequest},
};
use tower::ServiceExt;

struct Server {
    app: Router,
    url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn setup() -> Server {
    let policy =
        AuthPolicy::from_token_json(r#"{"owner-secret":"owner","outsider-secret":"outsider"}"#)
            .unwrap();
    let app = new_app_with_journal_and_auth_policy(
        "http://127.0.0.1",
        Box::new(journal::InMemoryJournalStore::new()),
        policy,
    );
    let mut recipe: serde_json::Value = serde_json::from_str(include_str!(
        "../../../scripts/fixtures/background_market.json"
    ))
    .unwrap();
    recipe["scenario"]["room_id"] = "socket-test".into();
    recipe["agents"] = serde_json::json!([]);
    recipe["autostart_agents"] = false.into();
    assert_eq!(call(&app, "/rooms", Some(recipe)).await.0, StatusCode::OK);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/rooms/socket-test/ws?account_id=20",
        listener.local_addr().unwrap()
    );
    let copy = app.clone();
    let task = tokio::spawn(async move {
        axum::serve(listener, copy).await.unwrap();
    });
    Server { app, url, task }
}
async fn call(
    app: &Router,
    path: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let request = Request::builder()
        .uri(path)
        .method(if body.is_some() { "POST" } else { "GET" })
        .header("authorization", "Bearer owner-secret")
        .header("content-type", "application/json")
        .body(
            body.map(|body| Body::from(body.to_string()))
                .unwrap_or_else(Body::empty),
        )
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    (
        status,
        serde_json::from_slice(&to_bytes(response.into_body(), 1_048_576).await.unwrap()).unwrap(),
    )
}
type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
async fn frame(client: &mut Client) -> serde_json::Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(3), client.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let ClientMessage::Text(raw) = message {
            return serde_json::from_str(&raw).unwrap();
        }
    }
}
async fn authenticate(url: &str, token: &str) -> Client {
    let (mut client, _) = connect_async(url).await.unwrap();
    client
        .send(ClientMessage::Text(
            serde_json::json!({"kind":"authenticate","token":token})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    client
}

#[tokio::test]
async fn socket_origin_and_authentication_fail_before_private_data() {
    let server = setup().await;
    let mut request = server.url.clone().into_client_request().unwrap();
    request
        .headers_mut()
        .insert("origin", "https://untrusted.invalid".parse().unwrap());
    let error = connect_async(request).await.unwrap_err();
    assert!(
        matches!(error, tokio_tungstenite::tungstenite::Error::Http(response) if response.status() == StatusCode::FORBIDDEN)
    );
    for (token, expected) in [("wrong", 401), ("outsider-secret", 403)] {
        let mut client = authenticate(&server.url, token).await;
        let result = frame(&mut client).await;
        assert_eq!(result["kind"], "error");
        assert_eq!(result["status"], expected);
        assert!(result.get("data").is_none());
    }
    let (status, runtime) = call(&server.app, "/runtime", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(runtime["storage"]["durable"], false);
}

#[tokio::test]
async fn socket_pushes_authoritative_orders_pause_and_clock_without_read_advancement() {
    let server = setup().await;
    let mut client = authenticate(&server.url, "owner-secret").await;
    let first = frame(&mut client).await;
    assert_eq!(first["api_version"], VERSION);
    assert_eq!(first["sequence"], 1);
    assert_eq!(
        first["data"]["observation"]["observation"]["own_account"]["Spot"]["account_id"],
        20
    );
    assert_eq!(first["data"]["candles"]["market_time_ms"], 0);
    let order = serde_json::json!({"participant_id":"human-test","account_id":20,"action":{"PlaceLimit":{"side":"Buy","qty":2,"price_tick":1}}});
    assert_eq!(
        call(&server.app, "/rooms/socket-test/orders", Some(order))
            .await
            .0,
        StatusCode::OK
    );
    let second = frame(&mut client).await;
    assert_eq!(second["sequence"], 2);
    let observed = &second["data"]["observation"]["observation"];
    assert_eq!(observed["own_orders"].as_array().unwrap().len(), 1);
    assert_eq!(observed["own_account"]["Spot"]["reserved_cash"], 2);
    assert_eq!(
        call(
            &server.app,
            "/rooms/socket-test/pause",
            Some(serde_json::json!({}))
        )
        .await
        .0,
        StatusCode::OK
    );
    let paused = frame(&mut client).await;
    assert_eq!(
        paused["data"]["observation"]["observation"]["status"],
        "Paused"
    );
    assert_eq!(
        call(
            &server.app,
            "/rooms/socket-test/clock/advance",
            Some(serde_json::json!({"steps":1}))
        )
        .await
        .0,
        StatusCode::OK
    );
    let stepped = frame(&mut client).await;
    assert_eq!(
        stepped["data"]["observation"]["observation"]["market_time_ms"],
        1000
    );
    assert_eq!(stepped["data"]["candles"]["market_time_ms"], 1000);
    let (_, clock) = call(&server.app, "/rooms/socket-test/clock", None).await;
    assert_eq!(clock["clock"]["step"], 1);
    client.close(None).await.unwrap();
}

#[tokio::test]
async fn restore_auto_mode_preserves_pause_and_disabled_bots_without_reseeding() {
    let state = shared_state(AppState::new("http://localhost"));
    let mut recipe: CreateRoomRequest = serde_json::from_str(include_str!(
        "../../../scripts/fixtures/background_market.json"
    ))
    .unwrap();
    recipe.agents.clear();
    recipe.autostart_agents = Some(false);
    {
        let mut app = state.app.lock().await;
        app.rooms.create_room(recipe.scenario.clone()).unwrap();
        let room = recipe.scenario.room_id.clone();
        app.rooms.pause_room(&room).unwrap();
        let mut scheduler = exchange_core::SchedulerState::new(
            room.clone(),
            Vec::new(),
            exchange_core::SchedulerMode::Auto { interval_ms: 100 },
        );
        scheduler.bots_enabled = false;
        app.schedulers.insert(room, scheduler);
    }
    restore_auto_workers(&state).await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let app = state.app.lock().await;
    assert_eq!(app.agent_workers.len(), 1);
    assert_eq!(app.rooms.clock(&recipe.scenario.room_id).unwrap().step(), 0);
    assert!(
        !app.agent_workers[&recipe.scenario.room_id]
            .bots_enabled
            .load(Ordering::Acquire)
    );
}

#[tokio::test]
async fn socket_revocation_stops_an_idle_private_subscription() {
    let server = setup().await;
    assert_eq!(
        call(
            &server.app,
            "/rooms/socket-test/members",
            Some(serde_json::json!({"user_id":"outsider","role":"trader"}))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &server.app,
            "/rooms/socket-test/accounts/20/owners",
            Some(serde_json::json!({"user_id":"outsider"}))
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut client = authenticate(&server.url, "outsider-secret").await;
    assert_eq!(frame(&mut client).await["kind"], "snapshot");
    assert_eq!(
        call(
            &server.app,
            "/rooms/socket-test/members/outsider",
            Some(serde_json::json!({}))
        )
        .await
        .0,
        StatusCode::OK
    );
    let revoked = frame(&mut client).await;
    assert_eq!(revoked["kind"], "error");
    assert_eq!(revoked["status"], 403);
}

#[tokio::test]
async fn recovery_refuses_to_skip_unfinished_manual_actions() {
    let state = shared_state(AppState::new("http://localhost"));
    let recipe: CreateRoomRequest = serde_json::from_str(include_str!(
        "../../../scripts/fixtures/background_market.json"
    ))
    .unwrap();
    {
        let mut app = state.app.lock().await;
        app.rooms.create_room(recipe.scenario.clone()).unwrap();
        let mut scheduler = exchange_core::SchedulerState::new(
            recipe.scenario.room_id.clone(),
            vec![recipe.agents[0].clone()],
            exchange_core::SchedulerMode::Auto { interval_ms: 500 },
        );
        scheduler.agents[0]
            .unfinished_actions
            .push(OrderAction::PlaceMarket {
                side: exchange_core::Side::Buy,
                qty: 1,
            });
        app.schedulers.insert(recipe.scenario.room_id, scheduler);
    }
    assert!(
        restore_auto_workers(&state)
            .await
            .unwrap_err()
            .to_string()
            .contains("unfinished manual")
    );
    assert!(state.app.lock().await.agent_workers.is_empty());
}
