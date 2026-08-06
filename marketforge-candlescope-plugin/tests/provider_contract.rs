use exchange_core::{
    Command, InstrumentConfig, MarketConfig, NewOrder, OrderKind, PerpClearingConfig,
    PerpMarketConfig, PerpRiskConfig, ScenarioConfig, SetMarkPrice, Side, SpotClearingConfig,
    SpotMarketConfig, SpotRiskConfig, VenueAssetPolicyConfig, VenueRuleConfig,
    scenario::ScenarioAccount,
};
use marketforge_candlescope_plugin::{
    MARKET_DATA_CONTRIBUTION_ID, MARKETFORGE_CONTROL_CONTRIBUTION_ID, PluginService,
    RemoteBackendConfig, SYMBOLS_CONTRIBUTION_ID,
};
use serde_json::{Value, json};

const EPOCH_MS: u64 = 1_700_000_040_000;

fn scenario() -> ScenarioConfig {
    ScenarioConfig {
        room_id: "candlescope-room".to_string(),
        venue_preset: None,
        venue_rules: VenueRuleConfig::default(),
        venue_asset_policy: VenueAssetPolicyConfig::default(),
        assets: Vec::new(),
        market: MarketConfig::Spot(SpotMarketConfig {
            instrument: InstrumentConfig::new_for_venue(
                "marketforge",
                "V-BTC-SPOT",
                "BTC",
                "USD",
                "V-BTC-SPOT",
                1,
                1,
            )
            .unwrap(),
            clearing: SpotClearingConfig::default(),
            risk: SpotRiskConfig::default(),
        }),
        extra_markets: Vec::new(),
        initial_portfolios: Vec::new(),
        initial_allocations: Vec::new(),
        routed_initial_allocations: Vec::new(),
        accounts: vec![
            ScenarioAccount::Spot {
                account_id: 10,
                cash_balance: 10_000,
                position_qty: 100,
            },
            ScenarioAccount::Basic {
                account_id: 20,
                cash_balance: 100_000,
            },
        ],
        seed_orders: vec![
            Command::NewOrder(NewOrder {
                order_id: 1,
                account_id: 10,
                side: Side::Sell,
                kind: OrderKind::Limit { price_tick: 105 },
                qty: 10,
                reduce_only: false,
            }),
            Command::NewOrder(NewOrder {
                order_id: 2,
                account_id: 20,
                side: Side::Buy,
                kind: OrderKind::Limit { price_tick: 95 },
                qty: 10,
                reduce_only: false,
            }),
        ],
        routed_seed_orders: Vec::new(),
    }
}

fn perp_liquidation_scenario() -> ScenarioConfig {
    ScenarioConfig {
        room_id: "liquidation-room".to_string(),
        venue_preset: None,
        venue_rules: VenueRuleConfig::default(),
        venue_asset_policy: VenueAssetPolicyConfig::default(),
        assets: Vec::new(),
        market: MarketConfig::Perp(PerpMarketConfig {
            instrument: InstrumentConfig::new_for_venue(
                "marketforge",
                "V-BTC-PERP",
                "BTC",
                "USD",
                "V-BTC-PERP",
                1,
                1,
            )
            .unwrap(),
            clearing: PerpClearingConfig {
                leverage: 10,
                maintenance_margin_ppm: 50_000,
                ..PerpClearingConfig::default()
            },
            risk: PerpRiskConfig::default(),
            initial_mark_price_tick: 100,
        }),
        extra_markets: Vec::new(),
        initial_portfolios: Vec::new(),
        initial_allocations: Vec::new(),
        routed_initial_allocations: Vec::new(),
        accounts: vec![
            ScenarioAccount::Basic {
                account_id: 10,
                cash_balance: 10_000,
            },
            ScenarioAccount::Basic {
                account_id: 20,
                cash_balance: 200,
            },
            ScenarioAccount::Basic {
                account_id: 30,
                cash_balance: 10_000,
            },
        ],
        seed_orders: Vec::new(),
        routed_seed_orders: Vec::new(),
    }
}

fn descriptor(channel: &str, interval: Option<&str>) -> Value {
    let mut value = json!({
        "exchange": "marketforge",
        "marketType": "spot",
        "channel": channel,
        "symbol": "V-BTC-SPOT",
    });
    if let Some(interval) = interval {
        value["interval"] = Value::String(interval.to_string());
    }
    value
}

fn apply_crossing_order(service: &mut PluginService) {
    let result = service
        .adapter_mut()
        .apply_command(
            "candlescope-room",
            "V-BTC-SPOT",
            Command::NewOrder(NewOrder {
                order_id: 3,
                account_id: 20,
                side: Side::Buy,
                kind: OrderKind::Limit { price_tick: 105 },
                qty: 3,
                reduce_only: false,
            }),
        )
        .unwrap();
    assert!(result.accepted);
    assert_eq!(result.trade_count, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attaches_to_remote_backend_and_projects_incremental_market_data() {
    use exchange_core::OrderAction;
    use exchange_server::{HttpTradingClient, SubmitOrderRequest};
    use tokio::sync::oneshot;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let base_url = format!("http://{address}");
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server = tokio::spawn(exchange_server::serve_listener_with_shutdown(
        listener,
        async move {
            let _ = shutdown_rx.await;
        },
    ));
    tokio::task::yield_now().await;

    let result = tokio::task::spawn_blocking(move || {
        let scenario = scenario();
        let client = HttpTradingClient::new(&base_url);
        client.create_room(&scenario).unwrap();

        let config = RemoteBackendConfig::new(&base_url).unwrap();
        let mut service = PluginService::with_remote_backend(config);
        service.activate(7);
        let attached = service
            .invoke(
                MARKETFORGE_CONTROL_CONTRIBUTION_ID,
                &json!({
                    "operation": "session.attach",
                    "scenario": scenario,
                    "epochMs": EPOCH_MS,
                }),
                true,
            )
            .unwrap();
        assert_eq!(attached["attached"]["roomId"], "candlescope-room");
        assert_eq!(attached["attached"]["seedExecutionCount"], 2);
        assert_eq!(service.health_check()["sessionMode"], "remote");

        let opened = service
            .invoke(
                MARKET_DATA_CONTRIBUTION_ID,
                &json!({
                    "operation": "stream.open",
                    "hostStreamId": "remote-bars",
                    "descriptor": descriptor("kline", Some("1m")),
                    "batchLimit": 16,
                    "resync": false,
                }),
                false,
            )
            .unwrap();
        let stream_id = opened["providerStreamId"].as_str().unwrap();

        client
            .submit_order_for(
                "candlescope-room",
                "V-BTC-SPOT",
                &SubmitOrderRequest {
                    participant_id: "candlescope-test".to_string(),
                    instrument_id: None,
                    account_id: 20,
                    action: OrderAction::PlaceLimit {
                        side: Side::Buy,
                        price_tick: 105,
                        qty: 3,
                    },
                },
            )
            .unwrap();

        let update = service
            .invoke(
                MARKET_DATA_CONTRIBUTION_ID,
                &json!({
                    "operation": "stream.poll",
                    "providerStreamId": stream_id,
                    "afterSequence": 0,
                    "batchLimit": 16,
                    "waitMs": 250,
                }),
                false,
            )
            .unwrap();
        assert_eq!(update["events"][0]["eventType"], "bar.updated");
        assert_eq!(update["events"][0]["payload"]["volume"], 3.0);
        let after_update = update["nextSequence"].as_u64().unwrap() - 1;

        service
            .invoke(
                MARKETFORGE_CONTROL_CONTRIBUTION_ID,
                &json!({
                    "operation": "clock.advance",
                    "roomId": "candlescope-room",
                    "steps": 60,
                }),
                true,
            )
            .unwrap();
        let closed_bar = service
            .invoke(
                MARKET_DATA_CONTRIBUTION_ID,
                &json!({
                    "operation": "stream.poll",
                    "providerStreamId": stream_id,
                    "afterSequence": after_update,
                    "batchLimit": 16,
                    "waitMs": 0,
                }),
                false,
            )
            .unwrap();
        assert!(
            closed_bar["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event["eventType"] == "bar.closed")
        );

        service
            .invoke(
                MARKETFORGE_CONTROL_CONTRIBUTION_ID,
                &json!({"operation": "room.pause", "roomId": "candlescope-room"}),
                true,
            )
            .unwrap();
        service
            .invoke(
                MARKETFORGE_CONTROL_CONTRIBUTION_ID,
                &json!({"operation": "room.resume", "roomId": "candlescope-room"}),
                true,
            )
            .unwrap();
        service
            .invoke(
                MARKETFORGE_CONTROL_CONTRIBUTION_ID,
                &json!({"operation": "room.close", "roomId": "candlescope-room"}),
                true,
            )
            .unwrap();
        let described = service
            .invoke(
                MARKETFORGE_CONTROL_CONTRIBUTION_ID,
                &json!({"operation": "session.describe"}),
                false,
            )
            .unwrap();
        assert_eq!(described["session"]["status"], "closed");
    })
    .await;

    let _ = shutdown_tx.send(());
    result.unwrap();
    server.await.unwrap().unwrap();
}

#[test]
fn exposes_symbols_and_projects_authoritative_kline_history() {
    let mut service = PluginService::new();
    service.activate(1);
    service
        .adapter_mut()
        .load_session(scenario(), EPOCH_MS)
        .unwrap();

    let symbols = service
        .invoke(
            SYMBOLS_CONTRIBUTION_ID,
            &json!({"operation": "symbols.list", "marketType": "spot", "limit": 100}),
            false,
        )
        .unwrap();
    assert_eq!(
        symbols["schemaVersion"],
        "candlescope.provider-symbols-page/1"
    );
    assert_eq!(symbols["symbols"][0]["symbol"], "V-BTC-SPOT");
    assert_eq!(symbols["symbols"][0]["baseAsset"], "BTC");
    assert_eq!(symbols["sourceQuality"]["quality"], "authoritative");

    let opened = service
        .invoke(
            MARKET_DATA_CONTRIBUTION_ID,
            &json!({
                "operation": "stream.open",
                "hostStreamId": "bars-1",
                "descriptor": descriptor("kline", Some("1m")),
                "batchLimit": 16,
                "resync": false,
            }),
            false,
        )
        .unwrap();
    let stream_id = opened["providerStreamId"].as_str().unwrap().to_string();

    apply_crossing_order(&mut service);
    let update = service
        .invoke(
            MARKET_DATA_CONTRIBUTION_ID,
            &json!({
                "operation": "stream.poll",
                "providerStreamId": stream_id,
                "afterSequence": 0,
                "batchLimit": 16,
                "waitMs": 0,
            }),
            false,
        )
        .unwrap();
    assert_eq!(update["events"][0]["eventType"], "bar.updated");
    assert_eq!(update["events"][0]["payload"]["open"], 105.0);
    assert_eq!(update["events"][0]["payload"]["volume"], 3.0);
    assert_eq!(update["events"][0]["payload"]["finality"], "forming");

    service
        .adapter_mut()
        .advance_clock("candlescope-room", 60)
        .unwrap();
    let closed = service
        .invoke(
            MARKET_DATA_CONTRIBUTION_ID,
            &json!({
                "operation": "stream.poll",
                "providerStreamId": stream_id,
                "afterSequence": 1,
                "batchLimit": 16,
                "waitMs": 0,
            }),
            false,
        )
        .unwrap();
    assert_eq!(closed["events"][0]["sequence"], 2);
    assert_eq!(closed["events"][0]["eventType"], "bar.closed");
    assert_eq!(closed["events"][0]["payload"]["finality"], "final");

    let history = service
        .invoke(
            MARKET_DATA_CONTRIBUTION_ID,
            &json!({
                "operation": "history.read",
                "descriptor": descriptor("kline", Some("1m")),
                "startMs": null,
                "endMs": null,
                "limit": 500,
            }),
            false,
        )
        .unwrap();
    assert_eq!(
        history["schemaVersion"],
        "candlescope.provider-history-page/1"
    );
    assert_eq!(history["rows"].as_array().unwrap().len(), 1);
    assert_eq!(history["rows"][0]["openTimeMs"], EPOCH_MS);
    assert_eq!(history["rows"][0]["finality"], "final");
}

#[test]
fn full_depth_starts_with_snapshot_then_emits_linked_delta() {
    let mut service = PluginService::new();
    service.activate(7);
    service
        .adapter_mut()
        .load_session(scenario(), EPOCH_MS)
        .unwrap();
    let opened = service
        .invoke(
            MARKET_DATA_CONTRIBUTION_ID,
            &json!({
                "operation": "stream.open",
                "hostStreamId": "depth-1",
                "descriptor": descriptor("full_depth", None),
                "batchLimit": 8,
                "resync": false,
            }),
            false,
        )
        .unwrap();
    let stream_id = opened["providerStreamId"].as_str().unwrap().to_string();
    let snapshot = service
        .invoke(
            MARKET_DATA_CONTRIBUTION_ID,
            &json!({
                "operation": "stream.poll",
                "providerStreamId": stream_id,
                "afterSequence": 0,
                "batchLimit": 8,
                "waitMs": 0,
            }),
            false,
        )
        .unwrap();
    assert_eq!(snapshot["events"][0]["eventType"], "orderbook.snapshot");
    assert_eq!(snapshot["events"][0]["payload"]["lastUpdateId"], 1);
    assert_eq!(snapshot["events"][0]["payload"]["bids"][0][0], 95.0);
    assert_eq!(snapshot["events"][0]["payload"]["asks"][0][0], 105.0);

    apply_crossing_order(&mut service);
    let delta = service
        .invoke(
            MARKET_DATA_CONTRIBUTION_ID,
            &json!({
                "operation": "stream.poll",
                "providerStreamId": stream_id,
                "afterSequence": 1,
                "batchLimit": 8,
                "waitMs": 0,
            }),
            false,
        )
        .unwrap();
    assert_eq!(delta["events"][0]["eventType"], "orderbook.delta");
    assert_eq!(delta["events"][0]["payload"]["previousFinalUpdateId"], 1);
    assert_eq!(delta["events"][0]["payload"]["finalUpdateId"], 2);
    assert_eq!(delta["events"][0]["payload"]["asks"][0][0], 105.0);
    assert_eq!(delta["events"][0]["payload"]["asks"][0][1], 7.0);
}

#[test]
fn stream_cursor_must_be_contiguous() {
    let mut service = PluginService::new();
    service.activate(1);
    service
        .adapter_mut()
        .load_session(scenario(), EPOCH_MS)
        .unwrap();
    let opened = service
        .invoke(
            MARKET_DATA_CONTRIBUTION_ID,
            &json!({
                "operation": "stream.open",
                "hostStreamId": "depth-cursor",
                "descriptor": descriptor("full_depth", None),
                "batchLimit": 8,
                "resync": false,
            }),
            false,
        )
        .unwrap();
    let stream_id = opened["providerStreamId"].as_str().unwrap();
    let error = service
        .invoke(
            MARKET_DATA_CONTRIBUTION_ID,
            &json!({
                "operation": "stream.poll",
                "providerStreamId": stream_id,
                "afterSequence": 2,
                "batchLimit": 8,
                "waitMs": 0,
            }),
            false,
        )
        .unwrap_err();
    assert_eq!(error.code(), "INVALID_CONTRACT");
    assert_eq!(error.path(), Some("afterSequence"));
}

#[test]
fn control_load_requires_user_action_and_uses_core_serde_contract() {
    let mut service = PluginService::new();
    service.activate(3);
    let input = json!({
        "operation": "session.load",
        "scenario": serde_json::to_value(scenario()).unwrap(),
        "epochMs": EPOCH_MS,
    });
    let denied = service
        .invoke(MARKETFORGE_CONTROL_CONTRIBUTION_ID, &input, false)
        .unwrap_err();
    assert_eq!(denied.code(), "USER_ACTION_REQUIRED");

    let loaded = service
        .invoke(MARKETFORGE_CONTROL_CONTRIBUTION_ID, &input, true)
        .unwrap();
    assert_eq!(loaded["loaded"]["roomId"], "candlescope-room");
    assert_eq!(loaded["loaded"]["instruments"][0]["symbol"], "V-BTC-SPOT");

    let described = service
        .invoke(
            MARKETFORGE_CONTROL_CONTRIBUTION_ID,
            &json!({"operation": "session.describe"}),
            false,
        )
        .unwrap();
    assert_eq!(described["session"]["marketTimeMs"], EPOCH_MS);
}

#[test]
fn projects_automatic_liquidation_executions_from_room_history() {
    let mut service = PluginService::new();
    service.activate(11);
    service
        .adapter_mut()
        .load_session(perp_liquidation_scenario(), EPOCH_MS)
        .unwrap();
    service
        .adapter_mut()
        .apply_command(
            "liquidation-room",
            "V-BTC-PERP",
            Command::NewOrder(NewOrder {
                order_id: 1,
                account_id: 10,
                side: Side::Sell,
                kind: OrderKind::Limit { price_tick: 100 },
                qty: 10,
                reduce_only: false,
            }),
        )
        .unwrap();
    service
        .adapter_mut()
        .apply_command(
            "liquidation-room",
            "V-BTC-PERP",
            Command::NewOrder(NewOrder {
                order_id: 2,
                account_id: 20,
                side: Side::Buy,
                kind: OrderKind::Market,
                qty: 10,
                reduce_only: false,
            }),
        )
        .unwrap();
    service
        .adapter_mut()
        .apply_command(
            "liquidation-room",
            "V-BTC-PERP",
            Command::SetMarkPrice(SetMarkPrice { price_tick: 80 }),
        )
        .unwrap();

    let opened = service
        .invoke(
            MARKET_DATA_CONTRIBUTION_ID,
            &json!({
                "operation": "stream.open",
                "hostStreamId": "liquidation-bars",
                "descriptor": {
                    "exchange": "marketforge",
                    "marketType": "perp",
                    "channel": "kline",
                    "symbol": "V-BTC-PERP",
                    "interval": "1m"
                },
                "batchLimit": 8,
                "resync": false,
            }),
            false,
        )
        .unwrap();
    let stream_id = opened["providerStreamId"].as_str().unwrap().to_string();
    let initial = service
        .invoke(
            MARKET_DATA_CONTRIBUTION_ID,
            &json!({
                "operation": "stream.poll",
                "providerStreamId": stream_id,
                "afterSequence": 0,
                "batchLimit": 8,
                "waitMs": 0,
            }),
            false,
        )
        .unwrap();
    assert_eq!(initial["events"][0]["eventType"], "bar.updated");

    let liquidity = service
        .adapter_mut()
        .apply_command(
            "liquidation-room",
            "V-BTC-PERP",
            Command::NewOrder(NewOrder {
                order_id: 3,
                account_id: 30,
                side: Side::Buy,
                kind: OrderKind::Limit { price_tick: 80 },
                qty: 10,
                reduce_only: false,
            }),
        )
        .unwrap();
    assert_eq!(
        liquidity.trade_count, 1,
        "the returned limit order only rests; this trade comes from the auto-liquidation execution"
    );

    let liquidation = service
        .invoke(
            MARKET_DATA_CONTRIBUTION_ID,
            &json!({
                "operation": "stream.poll",
                "providerStreamId": stream_id,
                "afterSequence": 1,
                "batchLimit": 8,
                "waitMs": 0,
            }),
            false,
        )
        .unwrap();
    assert_eq!(liquidation["events"][0]["eventType"], "bar.updated");
    assert_eq!(liquidation["events"][0]["payload"]["close"], 80.0);
}
