use marketforge_candlescope_plugin::{JsonLineServer, RuntimeState};
use serde_json::{Value, json};

fn request(id: &str, method: &str, generation: u64, params: Value) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
        "generation": generation,
    }))
    .unwrap()
}

fn handshake(server: &mut JsonLineServer) {
    let response = server.handle_line(&request(
        "handshake-1",
        "handshake",
        0,
        json!({
            "protocols": ["candlescope.plugin/2"],
            "host": {"name": "CandleScope", "version": "0.4.0"},
            "entrypointId": "main",
            "hostApis": [],
            "transports": ["jsonl/1"],
        }),
    ));
    assert_eq!(response[0]["result"]["protocol"], "candlescope.plugin/2");
    assert_eq!(
        response[0]["result"]["descriptor"]["contributions"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

fn activate(server: &mut JsonLineServer, generation: u64) {
    let response = server.handle_line(&request(
        "activate-1",
        "activate",
        generation,
        json!({
            "instanceId": format!("marketforge-{generation}"),
            "generation": generation,
            "capabilities": [],
        }),
    ));
    assert_eq!(response[0]["result"]["ok"], true);
}

#[test]
fn lifecycle_negotiates_and_enforces_generation_ownership() {
    let mut server = JsonLineServer::new();
    let before_handshake = server.handle_line(&request("describe-early", "describe", 0, json!({})));
    assert_eq!(before_handshake[0]["error"]["code"], -32101);

    handshake(&mut server);
    assert_eq!(server.runtime().state(), RuntimeState::Handshaken);
    activate(&mut server, 1);
    assert_eq!(server.runtime().state(), RuntimeState::Active);

    let stale = server.handle_line(&request("health-stale", "healthCheck", 2, json!({})));
    assert_eq!(stale[0]["error"]["code"], -32104);
    assert_eq!(stale[0]["error"]["data"]["code"], "GENERATION_MISMATCH");

    let health = server.handle_line(&request("health-1", "healthCheck", 1, json!({})));
    assert_eq!(health[0]["result"]["status"], "ready");
    assert_eq!(health[0]["result"]["sessionLoaded"], false);

    let quiesce = server.handle_line(&request("upgrade-1", "prepareUpgrade", 1, json!({})));
    assert_eq!(quiesce[0]["result"]["ok"], true);
    assert_eq!(server.runtime().state(), RuntimeState::Quiescing);
    let invoke = server.handle_line(&request(
        "invoke-quiescing",
        "invoke",
        1,
        json!({
            "contributionId": "symbols",
            "input": {"operation": "symbols.list", "marketType": "spot", "limit": 10},
            "requestContext": {
                "contributionId": "symbols",
                "userAction": false,
                "generation": 1,
                "traceId": "quiescing-test",
            },
        }),
    ));
    assert_eq!(invoke[0]["error"]["data"]["code"], "PLUGIN_QUIESCING");

    let deactivated = server.handle_line(&request(
        "deactivate-1",
        "deactivate",
        1,
        json!({"reason": "test"}),
    ));
    assert_eq!(deactivated[0]["result"]["ok"], true);
    assert_eq!(server.runtime().state(), RuntimeState::Handshaken);

    activate(&mut server, 2);
    let shutdown = server.handle_line(&request("shutdown-1", "shutdown", 2, json!({})));
    assert_eq!(shutdown[0]["result"]["ok"], true);
    assert_eq!(server.runtime().state(), RuntimeState::Closed);
}

#[test]
fn invoke_checks_declared_contribution_and_request_context() {
    let mut server = JsonLineServer::new();
    handshake(&mut server);
    activate(&mut server, 1);

    let unknown = server.handle_line(&request(
        "unknown",
        "invoke",
        1,
        json!({
            "contributionId": "not-declared",
            "input": {},
            "requestContext": {
                "contributionId": "not-declared",
                "userAction": false,
                "generation": 1,
                "traceId": "unknown",
            },
        }),
    ));
    assert_eq!(
        unknown[0]["error"]["data"]["code"],
        "CONTRIBUTION_NOT_DECLARED"
    );

    let mismatch = server.handle_line(&request(
        "mismatch",
        "invoke",
        1,
        json!({
            "contributionId": "symbols",
            "input": {"operation": "symbols.list", "marketType": "spot", "limit": 10},
            "requestContext": {
                "contributionId": "market-data",
                "userAction": false,
                "generation": 1,
                "traceId": "mismatch",
            },
        }),
    ));
    assert_eq!(mismatch[0]["error"]["code"], -32602);

    let symbols = server.handle_line(&request(
        "symbols",
        "invoke",
        1,
        json!({
            "contributionId": "symbols",
            "input": {"operation": "symbols.list", "marketType": "spot", "limit": 10},
            "requestContext": {
                "contributionId": "symbols",
                "userAction": false,
                "generation": 1,
                "traceId": "symbols-empty",
            },
        }),
    ));
    assert_eq!(symbols[0]["result"]["symbols"], json!([]));
    assert_eq!(symbols[0]["result"]["exhausted"], true);
}

#[test]
fn duplicate_json_keys_are_rejected_before_dispatch() {
    let mut server = JsonLineServer::new();
    let response = server.handle_line(
        r#"{"jsonrpc":"2.0","id":"a","id":"b","method":"handshake","params":{},"generation":0}"#,
    );
    assert_eq!(response[0]["id"], Value::Null);
    assert_eq!(response[0]["error"]["code"], -32700);
    assert_eq!(response[0]["error"]["data"]["code"], "PARSE_ERROR");
}
