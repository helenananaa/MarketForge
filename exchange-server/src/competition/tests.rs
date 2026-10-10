use super::*;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn call(app: &Router, token: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .method(if body.is_some() { "POST" } else { "GET" })
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .header("x-user-id", "local-user")
                .header("content-type", "application/json")
                .body(
                    body.map(|v| Body::from(v.to_string()))
                        .unwrap_or_else(Body::empty),
                )
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let raw = to_bytes(response.into_body(), 8_388_608).await.unwrap();
    (
        status,
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| json!({"raw":String::from_utf8_lossy(&raw)})),
    )
}
async fn account(app: &Router, name: &str) -> (String, String) {
    let response = call(
        app,
        "",
        "/auth/register",
        Some(json!({"username":name,"password":"Match-Test-Password-2026","display_name":name})),
    )
    .await;
    assert_eq!(response.0, StatusCode::OK, "{}", response.1);
    (
        response.1["token"].as_str().unwrap().into(),
        response.1["user"]["user_id"].as_str().unwrap().into(),
    )
}
async fn create(app: &Router, owner: &str, room: &str) {
    let mut recipe: Value = serde_json::from_str(include_str!(
        "../../../scripts/fixtures/background_market.json"
    ))
    .unwrap();
    recipe["scenario"]["room_id"] = room.into();
    recipe["agents"] = json!([]);
    recipe["autostart_agents"] = false.into();
    let response = call(app, owner, "/rooms", Some(recipe)).await;
    assert_eq!(response.0, StatusCode::OK, "{}", response.1);
    assert_eq!(
        call(app, owner, &format!("/rooms/{room}/pause"), Some(json!({})))
            .await
            .0,
        StatusCode::OK
    );
}
async fn invitation(app: &Router, owner: &str, room: &str, role: &str, id: Option<u64>) -> String {
    let value = call(
        app,
        owner,
        &format!("/rooms/{room}/invitations"),
        Some(json!({"role":role,"account_id":id})),
    )
    .await;
    assert_eq!(value.0, StatusCode::OK, "{}", value.1);
    value.1["code"].as_str().unwrap().into()
}

#[tokio::test]
async fn formal_identity_password_login_logout_and_expiration() {
    let state = shared_state(AppState::new_with_journal_and_auth_policy(
        "http://localhost",
        Box::new(journal::InMemoryJournalStore::new()),
        AuthPolicy::accounts(),
    ));
    let router = app(state.clone());
    let (token, user) = account(&router, "auth-user-20261009").await;
    assert_ne!(user, "local-user");
    assert_eq!(
        call(&router, &token, "/identity", None).await.1["user_id"],
        user
    );
    assert_eq!(
        call(&router, "", "/identity", None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let duplicate = call(
        &router,
        "",
        "/auth/register",
        Some(json!({"username":"AUTH-USER-20261009","password":"Match-Test-Password-2026"})),
    )
    .await;
    assert_eq!(duplicate.0, StatusCode::CONFLICT);
    assert_eq!(
        call(
            &router,
            "",
            "/auth/login",
            Some(json!({"username":"auth-user-20261009","password":"incorrect"}))
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let login = call(
        &router,
        "",
        "/auth/login",
        Some(json!({"username":"auth-user-20261009","password":"Match-Test-Password-2026"})),
    )
    .await;
    assert_eq!(login.0, StatusCode::OK);
    let second = login.1["token"].as_str().unwrap();
    assert_eq!(
        call(&router, &token, "/auth/logout", Some(json!({})))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&router, &token, "/identity", None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&router, second, "/identity", None).await.0,
        StatusCode::OK
    );
    {
        let mut app = state.app.lock().await;
        let mut candidate = app.platform.clone();
        candidate
            .sessions
            .get_mut(&platform::digest(second))
            .unwrap()
            .expires_at_ms = now_ms() - 1;
        platform::commit(&mut app, candidate, vec![]).await.unwrap();
        assert!(!app.platform.sessions.contains_key(&token));
        assert!(
            app.platform.users[&user]
                .password_hash
                .starts_with("$argon2id$")
        );
    }
    assert_eq!(
        call(&router, second, "/identity", None).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn invitation_redemption_is_atomic_one_use_and_role_bound() {
    let state = shared_state(AppState::new_with_journal_and_auth_policy(
        "http://localhost",
        Box::new(journal::InMemoryJournalStore::new()),
        AuthPolicy::accounts(),
    ));
    let router = app(state.clone());
    let (owner, _) = account(&router, "invite-host").await;
    let (alice, _) = account(&router, "invite-alice").await;
    let (bob, _) = account(&router, "invite-bob").await;
    create(&router, &owner, "invite-room").await;
    let code = invitation(&router, &owner, "invite-room", "trader", Some(20)).await;
    let (a, b) = tokio::join!(
        call(
            &router,
            &alice,
            "/invitations/redeem",
            Some(json!({"code":code,"role":"admin","account_id":30}))
        ),
        call(
            &router,
            &bob,
            "/invitations/redeem",
            Some(json!({"code":code}))
        )
    );
    assert_eq!(
        [a.0, b.0].iter().filter(|s| **s == StatusCode::OK).count(),
        1
    );
    assert_eq!(
        [a.0, b.0]
            .iter()
            .filter(|s| **s == StatusCode::CONFLICT)
            .count(),
        1
    );
    let winner = if a.0 == StatusCode::OK { &alice } else { &bob };
    let context = call(&router, winner, "/rooms/invite-room/session", None)
        .await
        .1;
    assert_eq!(context["role"], "trader");
    assert_eq!(context["trade_account_ids"], json!([20]));
    assert_eq!(
        call(
            &router,
            winner,
            "/invitations/redeem",
            Some(json!({"code":code}))
        )
        .await
        .0,
        StatusCode::OK
    );
    let app = state.app.lock().await;
    assert!(!app.platform.invitations.contains_key(&code));
}

#[tokio::test]
async fn expired_invites_do_not_grant_access_and_prejoined_players_keep_seats() {
    let state = shared_state(AppState::new_with_journal_and_auth_policy(
        "http://localhost",
        Box::new(journal::InMemoryJournalStore::new()),
        AuthPolicy::accounts(),
    ));
    let router = app(state.clone());
    let (host, _) = account(&router, "prejoined-host").await;
    let (player, user) = account(&router, "prejoined-player").await;
    create(&router, &host, "prejoined-room").await;
    let expired = invitation(&router, &host, "prejoined-room", "trader", Some(20)).await;
    {
        let mut app = state.app.lock().await;
        let mut candidate = app.platform.clone();
        candidate
            .invitations
            .get_mut(&platform::digest(&expired))
            .unwrap()
            .expires_at_ms = now_ms() - 1;
        platform::commit(&mut app, candidate, vec![]).await.unwrap();
    }
    assert_eq!(
        call(
            &router,
            &player,
            "/invitations/redeem",
            Some(json!({"code":expired}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(&router, &player, "/rooms/prejoined-room/session", None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let code = invitation(&router, &host, "prejoined-room", "trader", Some(20)).await;
    assert_eq!(
        call(
            &router,
            &player,
            "/invitations/redeem",
            Some(json!({"code":code}))
        )
        .await
        .0,
        StatusCode::OK
    );
    let result = call(
        &router,
        &host,
        "/rooms/prejoined-room/competition",
        Some(json!({"title":"prejoined","seats":[20,30],"duration_seconds":10})),
    )
    .await;
    assert_eq!(result.0, StatusCode::OK, "{}", result.1);
    assert_eq!(result.1["players"][&user]["account_id"], 20);
    assert_eq!(result.1["players"][&user]["ready"], false);
}

#[tokio::test]
async fn multiplayer_ready_countdown_deadline_archive_and_recovery() {
    let store = journal::SharedInMemoryJournalStore::new();
    let state = shared_state(AppState::new_with_journal_and_auth_policy(
        "http://localhost",
        Box::new(store.clone()),
        AuthPolicy::accounts(),
    ));
    let router = app(state.clone());
    let (owner, _) = account(&router, "match-host").await;
    let (alice, alice_id) = account(&router, "match-alice").await;
    let (bob, bob_id) = account(&router, "match-bob").await;
    let (viewer, _) = account(&router, "match-viewer").await;
    create(&router, &owner, "match-room").await;
    let setup = call(
        &router,
        &owner,
        "/rooms/match-room/competition",
        Some(json!({"title":"test","seats":[20,30],"duration_seconds":10,"countdown_seconds":3})),
    )
    .await;
    assert_eq!(setup.0, StatusCode::OK, "{}", setup.1);
    for (token, role, id) in [
        (&alice, "trader", Some(20)),
        (&bob, "trader", Some(30)),
        (&viewer, "spectator", None),
    ] {
        let code = invitation(&router, &owner, "match-room", role, id).await;
        assert_eq!(
            call(
                &router,
                token,
                "/invitations/redeem",
                Some(json!({"code":code}))
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    assert_eq!(
        call(
            &router,
            &viewer,
            "/rooms/match-room/observe?account_id=20",
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&router, &viewer, "/rooms/match-room/session", None)
            .await
            .1["visible_account_ids"],
        json!([])
    );
    let order = json!({"participant_id":"human","account_id":20,"action":{"PlaceMarket":{"side":"Buy","qty":1}}});
    assert_eq!(
        call(
            &router,
            &alice,
            "/rooms/match-room/orders",
            Some(order.clone())
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &router,
            &owner,
            "/rooms/match-room/competition/start",
            Some(json!({}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    for token in [&alice, &bob] {
        assert_eq!(
            call(
                &router,
                token,
                "/rooms/match-room/competition/ready",
                Some(json!({"ready":true}))
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    assert_eq!(
        call(
            &router,
            &owner,
            "/rooms/match-room/competition/start",
            Some(json!({}))
        )
        .await
        .0,
        StatusCode::OK
    );
    for path in [
        "resume",
        "clock/step",
        "agents/stop",
        "accounts/20/owners",
        "members",
    ] {
        assert_eq!(
            call(
                &router,
                &owner,
                &format!("/rooms/match-room/{path}"),
                Some(json!({"user_id":bob_id,"role":"admin"}))
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
    }
    {
        let mut app = state.app.lock().await;
        let mut candidate = app.platform.clone();
        candidate
            .competitions
            .get_mut("match-room")
            .unwrap()
            .starts_at_ms = Some(now_ms() - 1);
        platform::commit(&mut app, candidate, vec![]).await.unwrap();
    }
    tick(&state).await.unwrap();
    assert_eq!(
        call(&router, &alice, "/rooms/match-room/competition", None)
            .await
            .1["phase"],
        "Running"
    );
    assert_eq!(
        call(
            &router,
            &owner,
            "/rooms/match-room/orders",
            Some(order.clone())
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &router,
            &alice,
            "/rooms/match-room/observe?account_id=30",
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let fill = call(
        &router,
        &alice,
        "/rooms/match-room/orders",
        Some(order.clone()),
    )
    .await;
    assert_eq!(fill.0, StatusCode::OK, "{}", fill.1);
    {
        let mut app = state.app.lock().await;
        let mut candidate = app.platform.clone();
        candidate
            .competitions
            .get_mut("match-room")
            .unwrap()
            .ends_at_ms = Some(now_ms() - 1);
        platform::commit(&mut app, candidate, vec![]).await.unwrap();
    }
    assert_eq!(
        call(&router, &alice, "/rooms/match-room/orders", Some(order))
            .await
            .0,
        StatusCode::CONFLICT
    );
    tick(&state).await.unwrap();
    let result = call(&router, &alice, "/rooms/match-room/competition", None)
        .await
        .1;
    assert_eq!(result["phase"], "Finished");
    assert_eq!(result["results"].as_array().unwrap().len(), 2);
    assert_eq!(result["results"][0]["user_id"], bob_id);
    assert_eq!(result["results"][1]["user_id"], alice_id);
    assert_eq!(
        call(
            &router,
            &viewer,
            "/rooms/match-room/observe?account_id=20",
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &router,
            &owner,
            "/rooms/match-room/competition/abort",
            Some(json!({}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let restored = AppState::recover_with_journal_bundle_and_auth_policy(
        "http://localhost",
        JournalStoreBundle::single(Box::new(store)),
        AuthPolicy::accounts(),
        None,
    )
    .unwrap();
    assert_eq!(
        restored.platform.competitions["match-room"].phase,
        Phase::Finished
    );
    assert_eq!(
        serde_json::to_value(&restored.platform.competitions["match-room"].results).unwrap(),
        result["results"]
    );
    assert_eq!(
        restored.rooms.status("match-room"),
        Ok(MarketStatus::Closed)
    );
    let recovered = app(shared_state(restored));
    assert_eq!(
        call(&recovered, &alice, "/identity", None).await.0,
        StatusCode::OK
    );
}
