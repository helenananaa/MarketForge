use super::*;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use tower::ServiceExt;

#[test]
fn viewer_bot_configuration_does_not_expose_nested_credentials() {
    let mut config = serde_json::json!({"Plugin":{"config":{"connection":{"api_key":"private","model":"model"},"access_token":"token","max_tokens":2048},"seed":1}});
    redact_credentials(&mut config);
    assert_eq!(
        config["Plugin"]["config"]["connection"]["api_key"],
        "[redacted]"
    );
    assert_eq!(config["Plugin"]["config"]["access_token"], "[redacted]");
    assert_eq!(config["Plugin"]["config"]["connection"]["model"], "model");
    assert_eq!(config["Plugin"]["config"]["max_tokens"], 2048);
}

async fn call(
    app: &Router,
    user: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .method(if body.is_some() { "POST" } else { "GET" })
                .header("authorization", format!("Bearer {user}-secret"))
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
    (
        status,
        serde_json::from_slice(&to_bytes(response.into_body(), 4_194_304).await.unwrap()).unwrap(),
    )
}
#[tokio::test]
async fn room_entry_enforces_roles_account_visibility_and_revocation() {
    let policy = AuthPolicy::from_token_json(r#"{"owner-secret":"owner","trader-secret":"trader","spectator-secret":"spectator","outsider-secret":"outsider","empty-secret":"empty"}"#).unwrap();
    let app = new_app_with_journal_and_auth_policy(
        "http://127.0.0.1",
        Box::new(journal::InMemoryJournalStore::new()),
        policy,
    );
    let mut recipe: serde_json::Value = serde_json::from_str(include_str!(
        "../../../scripts/fixtures/background_market.json"
    ))
    .unwrap();
    recipe["scenario"]["room_id"] = "portal-test".into();
    recipe["agents"] = serde_json::json!([]);
    recipe["autostart_agents"] = false.into();
    assert_eq!(
        call(&app, "owner", "/rooms", Some(recipe)).await.0,
        StatusCode::OK
    );
    for (user, role) in [
        ("trader", "trader"),
        ("spectator", "spectator"),
        ("empty", "trader"),
    ] {
        assert_eq!(
            call(
                &app,
                "owner",
                "/rooms/portal-test/members",
                Some(serde_json::json!({"user_id":user,"role":role}))
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    assert_eq!(
        call(
            &app,
            "owner",
            "/rooms/portal-test/accounts/20/owners",
            Some(serde_json::json!({"user_id":"trader"}))
        )
        .await
        .0,
        StatusCode::OK
    );
    let context = call(
        &app,
        "trader",
        "/rooms/portal-test/session",
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(context.0, StatusCode::OK);
    assert_eq!(context.1["visible_account_ids"], serde_json::json!([20]));
    assert_eq!(context.1["trade_account_ids"], serde_json::json!([20]));
    let view = call(&app, "trader", "/rooms/portal-test/workbench", None)
        .await
        .1;
    assert_eq!(
        view["markets"][0]["accounts"]["Spot"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(view["bots"].is_null());
    assert_eq!(
        call(
            &app,
            "trader",
            "/rooms/portal-test/observe?account_id=30",
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let spectator = call(&app, "spectator", "/rooms/portal-test/session", None)
        .await
        .1;
    assert!(spectator["visible_account_ids"].as_array().unwrap().len() > 1);
    assert_eq!(spectator["trade_account_ids"], serde_json::json!([]));
    assert_eq!(
        call(
            &app,
            "spectator",
            "/rooms/portal-test/observe?account_id=30",
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(
        !call(&app, "spectator", "/rooms/portal-test/workbench", None)
            .await
            .1["bots"]
            .is_null()
    );
    for path in ["pause", "resume", "agents/stop"] {
        assert_eq!(
            call(
                &app,
                "spectator",
                &format!("/rooms/portal-test/{path}"),
                Some(serde_json::json!({}))
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(call(&app,"spectator","/rooms/portal-test/orders",Some(serde_json::json!({"participant_id":"human","account_id":20,"action":{"PlaceMarket":{"side":"Buy","qty":1}}}))).await.0,StatusCode::FORBIDDEN);
    assert_eq!(
        call(&app, "spectator", "/rooms/portal-test/members", None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "outsider",
            "/rooms/portal-test/session",
            Some(serde_json::json!({}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let public = call(
        &app,
        "empty",
        "/rooms/portal-test/observe?account_id=0",
        None,
    )
    .await;
    assert_eq!(public.0, StatusCode::OK);
    assert!(public.1["observation"]["own_account"].is_null());
    assert_eq!(public.1["observation"]["own_orders"], serde_json::json!([]));
    assert_eq!(
        call(
            &app,
            "owner",
            "/rooms/portal-test/members/spectator",
            Some(serde_json::json!({}))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "spectator",
            "/rooms/portal-test/observe?account_id=30",
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "spectator", "/rooms/portal-test/workbench", None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}
