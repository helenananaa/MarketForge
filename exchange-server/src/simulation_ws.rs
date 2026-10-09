//! Bounded participant snapshots. Trading writes retain the durable HTTP gateway.
use super::*;
use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::Response,
};

const VERSION: &str = "simulation.ws.v1";
const CADENCE: Duration = Duration::from_millis(250);

#[derive(Clone, Deserialize)]
pub(super) struct Selection {
    account_id: AccountId,
    instrument_id: Option<String>,
    #[serde(default = "default_interval")]
    interval_ms: u64,
}
fn default_interval() -> u64 {
    1_000
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credentials {
    kind: String,
    token: Option<String>,
    user_id: Option<String>,
}

#[derive(Serialize)]
struct Snapshot {
    observation: ObservationResponse,
    candles: CandleResponse,
}

pub(super) async fn runtime_info(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> ApiResult<serde_json::Value> {
    let app = lock_state(&state).await?;
    if !app.auth_policy.is_accounts() {
        current_user_id(&headers, &app.auth_policy)?;
    }
    let kind = app.journal.storage_kind();
    Ok(Json(
        serde_json::json!({"api_version":"http.v1", "storage":{"kind":kind,"durable":kind == "postgresql"},
        "websocket":{"version":VERSION,"snapshot_interval_ms":CADENCE.as_millis()},"room_portal":{"version":"room.portal.v1"},"competition_platform":{"version":"competition.v1","auth_mode":app.auth_policy.mode()}}),
    ))
}

pub(super) async fn upgrade(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Query(selection): Query<Selection>,
    ws: WebSocketUpgrade,
    origins: Arc<Vec<HeaderValue>>,
) -> Result<Response, ApiError> {
    if headers
        .get(axum::http::header::ORIGIN)
        .is_some_and(|origin| !origins.contains(origin))
    {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "WebSocket origin is not allowed",
        ));
    }
    if !(1_000..=2_678_400_000).contains(&selection.interval_ms)
        || selection.interval_ms % 1_000 != 0
    {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Invalid WebSocket account or candle interval",
        ));
    }
    Ok(ws
        .max_message_size(16_384)
        .max_frame_size(16_384)
        .max_write_buffer_size(1_048_576)
        .on_upgrade(move |socket| session(socket, state, room_id, selection)))
}

fn credential_headers(raw: &str) -> Result<HeaderMap, ApiError> {
    let hello: Credentials = serde_json::from_str(raw)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Expected authenticate message"))?;
    if hello.kind != "authenticate" || (hello.token.is_some() && hello.user_id.is_some()) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Use either token or local user identity",
        ));
    }
    let mut headers = HeaderMap::new();
    if let Some(token) = hello.token {
        headers.insert(
            AUTHORIZATION,
            format!("Bearer {token}")
                .parse()
                .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Invalid credentials"))?,
        );
    } else if let Some(user) = hello.user_id {
        headers.insert(
            USER_ID_HEADER,
            user.parse()
                .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Invalid user identity"))?,
        );
    }
    Ok(headers)
}

async fn send(socket: &mut WebSocket, text: String) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_secs(5),
            socket.send(Message::Text(text.into()))
        )
        .await,
        Ok(Ok(()))
    )
}

async fn reject(socket: &mut WebSocket, error: ApiError) {
    let frame = serde_json::json!({"api_version":VERSION,"kind":"error","status":error.0.as_u16(),"error":error.1.0.error});
    let _ = send(socket, frame.to_string()).await;
    let _ = tokio::time::timeout(Duration::from_secs(1), socket.send(Message::Close(None))).await;
}

async fn session(mut socket: WebSocket, state: SharedState, room_id: String, selection: Selection) {
    let hello = tokio::time::timeout(Duration::from_secs(5), socket.recv()).await;
    let headers = match hello {
        Ok(Some(Ok(Message::Text(raw)))) => credential_headers(&raw),
        _ => Err(api_error(
            StatusCode::UNAUTHORIZED,
            "Authentication timed out or was malformed",
        )),
    };
    let headers = match headers {
        Ok(headers) => headers,
        Err(error) => {
            reject(&mut socket, error).await;
            return;
        }
    };
    let mut ticks = tokio::time::interval(CADENCE);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut shutdown = state.lifecycle.subscribe_shutdown();
    let mut previous = String::new();
    let mut sequence = 0_u64;
    let mut last_sent = Instant::now();
    loop {
        tokio::select! {
            _ = shutdown.changed() => { let _ = tokio::time::timeout(Duration::from_secs(1), socket.send(Message::Close(None))).await; return; }
            message = socket.recv() => match message {
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {},
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                _ => { reject(&mut socket, api_error(StatusCode::BAD_REQUEST, "This socket is a read-only subscription")).await; return; }
            },
            _ = ticks.tick() => {
                match snapshot(&state, &headers, &room_id, &selection).await {
                    Ok(Some(payload)) => {
                        let data = match serde_json::to_string(&payload) { Ok(data) => data, Err(_) => return };
                        if data != previous {
                            sequence += 1;
                            let frame = format!(r#"{{"api_version":"{VERSION}","kind":"snapshot","sequence":{sequence},"data":{data}}}"#);
                            if !send(&mut socket, frame).await { return; }
                            previous = data;
                            last_sent = Instant::now();
                        } else if last_sent.elapsed() >= Duration::from_secs(5) {
                            if !send(&mut socket, format!(r#"{{"api_version":"{VERSION}","kind":"heartbeat"}}"#)).await { return; }
                            last_sent = Instant::now();
                        }
                    }
                    Ok(None) => {}, // A concurrent commit crossed the read; sample again.
                    Err(error) => { reject(&mut socket, error).await; return; }
                }
            }
        }
    }
}

async fn snapshot(
    state: &SharedState,
    headers: &HeaderMap,
    room: &str,
    selection: &Selection,
) -> Result<Option<Snapshot>, ApiError> {
    let authorization = authorize_room_read(
        state,
        headers,
        room,
        RoomReadAccess::ViewAccount(selection.account_id),
    )
    .await?;
    let (observation, cursor, fallback) = {
        let app = lock_state(state).await?;
        let instrument = selection.instrument_id.clone().unwrap_or_else(|| {
            app.rooms
                .room(room)
                .map(|room| room.primary_instrument_id().to_string())
                .unwrap_or_default()
        });
        let mut observation = app
            .rooms
            .participant_observation(room, &instrument, selection.account_id)
            .map_err(api_error_from_room)?;
        room_portal::public_observation(&mut observation, selection.account_id);
        let cursor = app
            .rooms
            .simulation_room(room)
            .map_err(api_error_from_room)?
            .next_command_seq();
        // Only in-memory journals need this fallback. PostgreSQL retains full trade history.
        let fallback = if authorization.journal.storage_kind() == "memory" {
            Some(
                app.rooms
                    .candles(room, &instrument, selection.interval_ms)
                    .map_err(api_error_from_room)?,
            )
        } else {
            None
        };
        (observation, cursor, fallback)
    };
    let window = (observation.market_time_ms / selection.interval_ms).saturating_sub(499)
        * selection.interval_ms;
    let after = window.checked_sub(1);
    let mut candles = match authorization
        .journal
        .query_candles(
            &authorization.user_id,
            room,
            &observation.instrument_id,
            selection.interval_ms,
            observation.market_time_ms,
            after,
        )
        .await
        .map_err(api_error_from_journal)?
    {
        Some(candles) => candles,
        None => fallback.unwrap_or_default(),
    };
    {
        // Never hold the matching writer lock across database I/O. A changed cursor or
        // account/book/clock rejects the mixed sample instead of publishing it.
        let app = lock_state(state).await?;
        let mut now = app
            .rooms
            .participant_observation(room, &observation.instrument_id, selection.account_id)
            .map_err(api_error_from_room)?;
        room_portal::public_observation(&mut now, selection.account_id);
        if now != observation
            || app
                .rooms
                .simulation_room(room)
                .map_err(api_error_from_room)?
                .next_command_seq()
                != cursor
        {
            return Ok(None);
        }
    }
    candles.retain(|candle| candle.open_time_ms >= window);
    let response = CandleResponse {
        api_version: "http.v1".into(),
        room_id: room.into(),
        instrument_id: observation.instrument_id.clone(),
        interval_ms: selection.interval_ms,
        market_time_ms: observation.market_time_ms,
        next_after_open_time_ms: candles.last().map(|candle| candle.open_time_ms),
        candles,
    };
    Ok(Some(Snapshot {
        observation: ObservationResponse {
            api_version: STRATEGY_PROTOCOL_VERSION.into(),
            observation,
        },
        candles: response,
    }))
}

/// Restore auto mode from the committed scheduler, including stopped bots and paused rooms.
/// Embedded recovery constructors intentionally do not start background workers.
pub(super) async fn restore_auto_workers(state: &SharedState) -> Result<(), io::Error> {
    let mut app = state.app.lock().await;
    let schedulers: Vec<_> = app.schedulers.values().cloned().collect();
    for scheduler in &schedulers {
        if let exchange_core::SchedulerMode::Auto { interval_ms } = scheduler.mode
            && app.rooms.status(&scheduler.room_id) != Ok(MarketStatus::Closed)
            && !app.agent_workers.contains_key(&scheduler.room_id)
        {
            if interval_ms == 0 {
                return Err(io::Error::other("Recovered scheduler has a zero interval"));
            }
            if scheduler
                .agents
                .iter()
                .any(|agent| !agent.unfinished_actions.is_empty())
            {
                return Err(io::Error::other(
                    "Recovered auto scheduler has unfinished manual actions; complete manual recovery before enabling automatic mode",
                ));
            }
        }
    }
    for scheduler in schedulers {
        let exchange_core::SchedulerMode::Auto { interval_ms } = scheduler.mode else {
            continue;
        };
        if app.rooms.status(&scheduler.room_id) == Ok(MarketStatus::Closed)
            || app.agent_workers.contains_key(&scheduler.room_id)
        {
            continue;
        }
        let worker = AgentWorkerHandle::spawn(
            state.clone(),
            scheduler.room_id.clone(),
            scheduler
                .agents
                .iter()
                .map(|agent| agent.template.clone())
                .collect(),
            Duration::from_millis(interval_ms),
        )
        .map_err(|error| io::Error::other(error.to_string()))?;
        worker
            .bots_enabled
            .store(scheduler.bots_enabled, Ordering::Release);
        app.agent_workers.insert(scheduler.room_id, worker);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
