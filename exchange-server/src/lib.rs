use std::{
    collections::BTreeMap,
    io,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

pub mod journal;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    routing::{get, post},
};
use exchange_core::{
    AccountSnapshots, ActorExecution, ActorExecutionResult, ActorRejectReason, AgentTemplate,
    AssetLedgerEntry, BookSnapshot, Event, GatewayRequest, InstrumentId, MarketExecution,
    MarketStatus, MarketView, Money, OrderAction, OrderGateway, OrderId, Participant,
    ParticipantId, PortfolioAccountSnapshot, RoomId, RoomManager, RoomManagerError,
    RoomNetWorthSnapshot, ScenarioConfig, SimulationClock, SpotAccountSnapshot, SpotClearingEvent,
    TradingApi, VenueAccountSnapshot, VenueAccountVenueSnapshot, VenueToVenueTransfer,
    VenueTransfer,
    model::{AccountId, Command, OrderKind, SetMarkPrice},
    perp::{PerpAccountSnapshot, PerpClearingEvent},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tower_http::cors::CorsLayer;

use crate::journal::{
    AccountLedgerProjection, JournalError, JournalExecution, JournalRecovery, JournalSnapshot,
    JournalStore, JournalTransfer, MarketTickProjection, OrderProjection,
    PositionSnapshotProjection, TradeProjection, journal_store_from_env,
};

type SharedState = Arc<Mutex<AppState>>;
type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ErrorResponse>)>;
const SNAPSHOT_INTERVAL_COMMANDS: u64 = 100;
const USER_ID_HEADER: &str = "x-user-id";
const DEFAULT_USER_ID: &str = "local-user";
const SYSTEM_LIQUIDATION_ORDER_ID_BASE: OrderId = 9_000_000_000_000_000_000;

mod json_i128 {
    use std::fmt;

    use serde::{
        Deserializer, Serializer,
        de::{self, Visitor},
    };

    pub fn serialize<S>(value: &i128, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if let Ok(value) = i64::try_from(*value) {
            serializer.serialize_i64(value)
        } else if let Ok(value) = u64::try_from(*value) {
            serializer.serialize_u64(value)
        } else {
            serializer.serialize_str(&value.to_string())
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<i128, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct I128Visitor;

        impl Visitor<'_> for I128Visitor {
            type Value = i128;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an integer or a base-10 i128 string")
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(i128::from(value))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(i128::from(value))
            }

            fn visit_i128<E>(self, value: i128) -> Result<Self::Value, E> {
                Ok(value)
            }

            fn visit_u128<E>(self, value: u128) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                i128::try_from(value).map_err(E::custom)
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                value.parse().map_err(E::custom)
            }
        }

        deserializer.deserialize_any(I128Visitor)
    }
}

struct AppState {
    rooms: RoomManager,
    executions: BTreeMap<RoomId, Vec<RoomExecutionSummary>>,
    next_order_id: OrderId,
    base_url: String,
    agent_workers: BTreeMap<RoomId, AgentWorkerHandle>,
    journal: Box<dyn JournalStore>,
}

impl AppState {
    fn new(base_url: impl Into<String>) -> Self {
        Self::new_with_journal(base_url, Box::new(journal::InMemoryJournalStore::new()))
    }

    fn new_with_journal(base_url: impl Into<String>, journal: Box<dyn JournalStore>) -> Self {
        Self {
            rooms: RoomManager::new(),
            executions: BTreeMap::new(),
            next_order_id: 1,
            base_url: base_url.into(),
            agent_workers: BTreeMap::new(),
            journal,
        }
    }

    fn recover_with_journal(
        base_url: impl Into<String>,
        mut journal: Box<dyn JournalStore>,
    ) -> Result<Self, JournalError> {
        let recovery = journal.load_recovery()?;
        let next_order_id = next_order_id_from_recovery(&recovery)?;
        let rooms = recover_rooms(&recovery)?;
        let executions = execution_summaries_from_recovery(&recovery);

        Ok(Self {
            rooms,
            executions,
            next_order_id,
            base_url: base_url.into(),
            agent_workers: BTreeMap::new(),
            journal,
        })
    }
}

pub fn new_app() -> Router {
    new_app_with_base_url("http://127.0.0.1:57305")
}

pub fn new_app_with_base_url(base_url: impl Into<String>) -> Router {
    app(Arc::new(Mutex::new(AppState::new(base_url))))
}

pub fn new_app_with_journal(base_url: impl Into<String>, journal: Box<dyn JournalStore>) -> Router {
    app(Arc::new(Mutex::new(AppState::new_with_journal(
        base_url, journal,
    ))))
}

pub fn new_app_recovering_with_journal(
    base_url: impl Into<String>,
    journal: Box<dyn JournalStore>,
) -> Result<Router, JournalError> {
    Ok(app(Arc::new(Mutex::new(AppState::recover_with_journal(
        base_url, journal,
    )?))))
}

pub fn new_app_from_env_with_base_url(base_url: impl Into<String>) -> Result<Router, JournalError> {
    new_app_recovering_with_journal(base_url, journal_store_from_env()?)
}

pub async fn serve(addr: SocketAddr) -> Result<(), std::io::Error> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("exchange-server listening on http://{addr}");
    serve_listener(listener).await
}

pub async fn serve_listener(listener: tokio::net::TcpListener) -> Result<(), std::io::Error> {
    let addr = listener.local_addr()?;
    let app = new_app_from_env_with_base_url(format!("http://{addr}"))
        .map_err(|error| io::Error::other(error.to_string()))?;
    axum::serve(listener, app).await
}

fn app(state: SharedState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin("http://127.0.0.1:57304".parse::<HeaderValue>().unwrap())
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            HeaderName::from_static(USER_ID_HEADER),
        ]);

    Router::new()
        .route("/health", get(health))
        .route("/rooms", post(create_room).get(list_rooms))
        .route(
            "/rooms/{room_id}/agents",
            get(agent_status).post(start_agents),
        )
        .route("/rooms/{room_id}/agents/stop", post(stop_agents))
        .route("/rooms/{room_id}/events", get(room_events))
        .route("/rooms/{room_id}/view", get(market_view))
        .route("/rooms/{room_id}/book", get(book_snapshot))
        .route("/rooms/{room_id}/accounts", get(account_snapshots))
        .route(
            "/rooms/{room_id}/venue/accounts",
            get(venue_account_snapshots),
        )
        .route(
            "/rooms/{room_id}/venue/accounts/by-venue",
            get(venue_account_snapshots_by_venue),
        )
        .route("/rooms/{room_id}/portfolio", get(room_portfolios))
        .route("/rooms/{room_id}/assets/ledger", get(room_asset_ledger))
        .route("/rooms/{room_id}/net-worth", get(room_net_worth))
        .route("/rooms/{room_id}/clock", get(room_clock))
        .route("/rooms/{room_id}/clock/advance", post(advance_room_clock))
        .route("/rooms/{room_id}/transfers", get(room_transfers))
        .route("/rooms/{room_id}/transfers/deposit", post(submit_deposit))
        .route(
            "/rooms/{room_id}/transfers/withdraw",
            post(submit_withdrawal),
        )
        .route(
            "/rooms/{room_id}/transfers/venue-to-venue",
            post(submit_venue_to_venue_transfer),
        )
        .route(
            "/rooms/{room_id}/instruments/{instrument_id}/view",
            get(market_view_for_instrument),
        )
        .route(
            "/rooms/{room_id}/instruments/{instrument_id}/mark-price",
            post(set_mark_price_for_instrument),
        )
        .route(
            "/rooms/{room_id}/instruments/{instrument_id}/book",
            get(book_snapshot_for_instrument),
        )
        .route(
            "/rooms/{room_id}/instruments/{instrument_id}/accounts",
            get(account_snapshots_for_instrument),
        )
        .route(
            "/rooms/{room_id}/instruments/{instrument_id}/orders",
            get(room_orders_for_instrument).post(submit_order_for_instrument),
        )
        .route(
            "/rooms/{room_id}/instruments/{instrument_id}/trades",
            get(room_trades_for_instrument),
        )
        .route(
            "/rooms/{room_id}/instruments/{instrument_id}/ticks",
            get(room_ticks_for_instrument),
        )
        .route(
            "/rooms/{room_id}/instruments/{instrument_id}/ledger",
            get(room_ledger_for_instrument),
        )
        .route(
            "/rooms/{room_id}/instruments/{instrument_id}/positions",
            get(room_positions_for_instrument),
        )
        .route(
            "/rooms/{room_id}/orders",
            get(room_orders).post(submit_order),
        )
        .route("/rooms/{room_id}/trades", get(room_trades))
        .route("/rooms/{room_id}/ticks", get(room_ticks))
        .route("/rooms/{room_id}/ledger", get(room_ledger))
        .route("/rooms/{room_id}/positions", get(room_positions))
        .route("/rooms/{room_id}/pause", post(pause_room))
        .route("/rooms/{room_id}/resume", post(resume_room))
        .layer(cors)
        .with_state(state)
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { ok: true })
}

async fn create_room(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> ApiResult<CreateRoomResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    let request = parse_create_room_payload(payload)?;
    validate_agent_templates(&request.scenario.room_id, &request.agents)?;
    let mut candidate_rooms = state.rooms.clone();
    let seed_commands = request.scenario.seed_commands();
    let next_order_id = next_api_order_id_after_commands(state.next_order_id, &seed_commands)?;
    let account_ids = scenario_account_ids(&request.scenario);
    let bootstrap = candidate_rooms
        .create_room(request.scenario.clone())
        .map_err(api_error_from_room)?;
    let room_id = bootstrap.room_id.clone();
    let seed_execution_count = bootstrap.seed_executions.len();
    let seed_records = seed_commands
        .into_iter()
        .zip(bootstrap.seed_executions.iter().cloned())
        .map(|(command, execution)| JournalExecution::seed(command, execution))
        .collect::<Vec<_>>();
    let initial_snapshot = latest_room_snapshot(&candidate_rooms, &room_id, &seed_records);

    state
        .journal
        .create_room(
            &user_id,
            &request.scenario,
            &bootstrap,
            &account_ids,
            &seed_records,
            initial_snapshot.as_ref(),
        )
        .map_err(api_error_from_journal)?;
    state.executions.insert(
        room_id.clone(),
        seed_records
            .iter()
            .map(|record| record.execution.clone())
            .collect(),
    );
    state.rooms = candidate_rooms;
    state.next_order_id = next_order_id;

    let mut agent_status = AgentWorkerStatus::stopped(room_id.clone());

    if !request.agents.is_empty() && request.autostart_agents.unwrap_or(true) {
        agent_status = start_agent_worker_for_room(
            &mut state,
            room_id.clone(),
            user_id.clone(),
            StartAgentsRequest {
                agents: request.agents,
                interval_ms: request.agent_interval_ms,
            },
        )?;
    }

    Ok(Json(CreateRoomResponse {
        room_id,
        seed_execution_count,
        agent_worker: agent_status,
    }))
}

fn parse_create_room_payload(
    payload: serde_json::Value,
) -> Result<CreateRoomRequest, (StatusCode, Json<ErrorResponse>)> {
    if payload.get("scenario").is_some() {
        return serde_json::from_value(payload).map_err(api_error_from_json);
    }

    serde_json::from_value::<ScenarioConfig>(payload)
        .map(|scenario| CreateRoomRequest {
            scenario,
            agents: Vec::new(),
            agent_interval_ms: None,
            autostart_agents: None,
        })
        .map_err(api_error_from_json)
}

fn recover_rooms(recovery: &JournalRecovery) -> Result<RoomManager, JournalError> {
    let mut rooms = RoomManager::new();
    let executions_by_room = recovery.executions.iter().fold(
        BTreeMap::<&str, Vec<&JournalExecution>>::new(),
        |mut map, execution| {
            map.entry(execution.room_id.as_str())
                .or_default()
                .push(execution);
            map
        },
    );

    let snapshots_by_room = recovery.snapshots.iter().fold(
        BTreeMap::<&str, &JournalSnapshot>::new(),
        |mut map, snapshot| {
            map.insert(snapshot.room_id.as_str(), snapshot);
            map
        },
    );

    for room in &recovery.rooms {
        let replay_after_command_seq =
            if let Some(snapshot) = snapshots_by_room.get(room.room_id.as_str()) {
                rooms
                    .restore_simulation_room(snapshot.actor.clone(), Vec::new())
                    .map_err(|error| JournalError::Recovery(format!("{error:?}")))?;
                Some(snapshot.command_seq)
            } else {
                let bootstrap = rooms
                    .create_room(room.scenario.clone())
                    .map_err(|error| JournalError::Recovery(format!("{error:?}")))?;
                bootstrap
                    .seed_executions
                    .last()
                    .map(|execution| execution.command_seq)
            };

        let seed_count = room.scenario.seed_order_count();
        for record in executions_by_room
            .get(room.room_id.as_str())
            .into_iter()
            .flatten()
        {
            if replay_after_command_seq.is_some_and(|seq| record.command_seq <= seq) {
                continue;
            }
            if record.participant_id.is_none() && record.command_seq < seed_count as u64 {
                continue;
            }
            if record.participant_id.is_none() && is_system_liquidation_command(&record.command) {
                continue;
            }

            rooms
                .restore_room_status(&room.room_id, record.execution.status)
                .map_err(|error| JournalError::Recovery(format!("{error:?}")))?;
            let replayed = match record.execution.instrument_id.as_deref() {
                Some(instrument_id) => {
                    rooms.apply_to_instrument(&room.room_id, instrument_id, record.command.clone())
                }
                None => rooms.apply(&room.room_id, record.command.clone()),
            }
            .map_err(|error| JournalError::Recovery(format!("{error:?}")))?;
            let replayed_summary = RoomExecutionSummary::from_execution(replayed);
            if !execution_summary_matches(&record.execution, &replayed_summary) {
                return Err(JournalError::Recovery(format!(
                    "replayed execution diverged for room {} command_seq {}",
                    record.room_id, record.command_seq
                )));
            }
        }

        rooms
            .restore_room_status(&room.room_id, room.status)
            .map_err(|error| JournalError::Recovery(format!("{error:?}")))?;
    }

    Ok(rooms)
}

fn execution_summary_matches(
    stored: &RoomExecutionSummary,
    replayed: &RoomExecutionSummary,
) -> bool {
    stored.room_id == replayed.room_id
        && stored.command_seq == replayed.command_seq
        && (stored.instrument_id.is_none() || stored.instrument_id == replayed.instrument_id)
        && stored.status == replayed.status
        && stored.accepted == replayed.accepted
        && stored.reject_reason == replayed.reject_reason
        && stored.clearing_event_count == replayed.clearing_event_count
        && event_summaries_match(&stored.events, &replayed.events)
}

fn event_summaries_match(stored: &[EventSummary], replayed: &[EventSummary]) -> bool {
    stored.len() == replayed.len()
        && stored
            .iter()
            .zip(replayed)
            .all(|(stored, replayed)| event_summary_matches(stored, replayed))
}

fn event_summary_matches(stored: &EventSummary, replayed: &EventSummary) -> bool {
    if stored == replayed {
        return true;
    }

    match (stored, replayed) {
        (
            EventSummary::TradePrinted {
                seq,
                trade_id,
                maker_order_id,
                maker_account_id,
                taker_order_id,
                taker_account_id,
                price_tick,
                qty,
                ..
            },
            EventSummary::TradePrinted {
                seq: replayed_seq,
                trade_id: replayed_trade_id,
                price_tick: replayed_price_tick,
                qty: replayed_qty,
                ..
            },
        ) => {
            *seq == *replayed_seq
                && *trade_id == *replayed_trade_id
                && *price_tick == *replayed_price_tick
                && *qty == *replayed_qty
                && *maker_order_id == 0
                && *maker_account_id == 0
                && *taker_order_id == 0
                && *taker_account_id == 0
        }
        _ => false,
    }
}

fn execution_summaries_from_recovery(
    recovery: &JournalRecovery,
) -> BTreeMap<RoomId, Vec<RoomExecutionSummary>> {
    let mut executions = BTreeMap::<RoomId, Vec<RoomExecutionSummary>>::new();
    for record in &recovery.executions {
        executions
            .entry(record.room_id.clone())
            .or_default()
            .push(record.execution.clone());
    }
    executions
}

fn next_order_id_from_recovery(recovery: &JournalRecovery) -> Result<OrderId, JournalError> {
    let mut next_order_id = 1;
    for execution in &recovery.executions {
        let Command::NewOrder(order) = &execution.command else {
            continue;
        };
        if order.order_id >= SYSTEM_LIQUIDATION_ORDER_ID_BASE {
            if execution.participant_id.is_none()
                && is_system_liquidation_command(&execution.command)
            {
                continue;
            }
            return Err(JournalError::Recovery(format!(
                "order id {} in room {} uses the reserved system-order range",
                order.order_id, execution.room_id
            )));
        }
        let following_order_id = order.order_id.checked_add(1).ok_or_else(|| {
            JournalError::Recovery(format!(
                "order id {} in room {} cannot be incremented",
                order.order_id, execution.room_id
            ))
        })?;
        next_order_id = next_order_id.max(following_order_id);
    }
    Ok(next_order_id)
}

fn next_api_order_id_after_commands(
    current: OrderId,
    commands: &[Command],
) -> Result<OrderId, (StatusCode, Json<ErrorResponse>)> {
    let mut next_order_id = current;
    for command in commands {
        let Command::NewOrder(order) = command else {
            continue;
        };
        if order.order_id >= SYSTEM_LIQUIDATION_ORDER_ID_BASE {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                format!(
                    "seed order id {} uses the reserved system-order range",
                    order.order_id
                ),
            ));
        }
        let following_order_id = order.order_id.checked_add(1).ok_or_else(|| {
            api_error(
                StatusCode::BAD_REQUEST,
                format!("seed order id {} cannot be incremented", order.order_id),
            )
        })?;
        next_order_id = next_order_id.max(following_order_id);
    }
    Ok(next_order_id)
}

fn is_system_liquidation_command(command: &Command) -> bool {
    matches!(
        command,
        Command::NewOrder(order)
            if order.order_id >= SYSTEM_LIQUIDATION_ORDER_ID_BASE
                && order.reduce_only
                && matches!(
                    order.kind,
                    OrderKind::Market | OrderKind::FillOrKill { price_tick: None }
                )
    )
}

fn command_from_actor_execution(execution: &ActorExecution) -> Option<Command> {
    match &execution.result {
        ActorExecutionResult::Accepted(MarketExecution::Spot(execution)) => {
            Some(execution.command.command.clone())
        }
        ActorExecutionResult::Accepted(MarketExecution::Perp(execution)) => {
            Some(execution.command.command.clone())
        }
        ActorExecutionResult::Rejected(_) => None,
    }
}

fn journal_new_executions(
    state: &mut AppState,
    candidate_rooms: &RoomManager,
    room_id: &str,
    previous_history_len: usize,
    first_record: JournalExecution,
) -> Result<(), JournalError> {
    let new_history = candidate_rooms
        .execution_history(room_id)
        .map_err(|error| JournalError::Recovery(format!("{error:?}")))?
        .iter()
        .skip(previous_history_len)
        .cloned()
        .collect::<Vec<_>>();
    let mut journal_records = vec![first_record];
    for system_execution in new_history.into_iter().skip(1) {
        if let Some(command) = command_from_actor_execution(&system_execution) {
            journal_records.push(JournalExecution::system(command, system_execution));
        }
    }

    let snapshot = batch_snapshot_if_due(candidate_rooms, &journal_records);
    state
        .journal
        .append_executions(&journal_records, snapshot.as_ref())?;
    let timeline = state.executions.entry(room_id.to_string()).or_default();
    timeline.extend(journal_records.into_iter().map(|record| record.execution));

    Ok(())
}

fn batch_snapshot_if_due(
    rooms: &RoomManager,
    records: &[JournalExecution],
) -> Option<JournalSnapshot> {
    if !records.iter().any(|record| {
        record
            .command_seq
            .is_multiple_of(SNAPSHOT_INTERVAL_COMMANDS)
    }) {
        return None;
    }
    let final_record = records.last()?;
    Some(JournalSnapshot {
        room_id: final_record.room_id.clone(),
        command_seq: final_record.command_seq,
        actor: rooms.simulation_room(&final_record.room_id).ok()?.clone(),
    })
}

fn latest_room_snapshot(
    rooms: &RoomManager,
    room_id: &str,
    records: &[JournalExecution],
) -> Option<JournalSnapshot> {
    let command_seq = records.last()?.command_seq;
    Some(JournalSnapshot {
        room_id: room_id.to_string(),
        command_seq,
        actor: rooms.simulation_room(room_id).ok()?.clone(),
    })
}

fn current_room_snapshot(
    rooms: &RoomManager,
    room_id: &str,
    command_seq: u64,
) -> Option<JournalSnapshot> {
    Some(JournalSnapshot {
        room_id: room_id.to_string(),
        command_seq,
        actor: rooms.simulation_room(room_id).ok()?.clone(),
    })
}

fn last_persisted_command_seq(state: &AppState, room_id: &str) -> u64 {
    state
        .executions
        .get(room_id)
        .and_then(|executions| executions.last())
        .map(|execution| execution.command_seq)
        .unwrap_or(0)
}

async fn list_rooms(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> ApiResult<ListRoomsResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    let mut rooms = Vec::new();
    let room_ids = state
        .rooms
        .room_ids()
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    for room_id in room_ids {
        if state
            .journal
            .user_can_access_room(&user_id, &room_id)
            .map_err(api_error_from_journal)?
        {
            rooms.push(room_id);
        }
    }

    Ok(Json(ListRoomsResponse { rooms }))
}

async fn start_agents(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Json(request): Json<StartAgentsRequest>,
) -> ApiResult<AgentWorkerStatus> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_admin(&mut state, &user_id, &room_id)?;
    start_agent_worker_for_room(&mut state, room_id, user_id, request).map(Json)
}

async fn agent_status(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<AgentWorkerStatus> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_admin(&mut state, &user_id, &room_id)?;
    Ok(Json(agent_status_for_room(&state, &room_id)))
}

async fn stop_agents(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<AgentWorkerStatus> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_admin(&mut state, &user_id, &room_id)?;
    if let Some(worker) = state.agent_workers.remove(&room_id) {
        worker.stop();
    }
    Ok(Json(AgentWorkerStatus::stopped(room_id)))
}

async fn room_events(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Query(query): Query<RoomEventsQuery>,
) -> ApiResult<RoomEventsResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_admin(&mut state, &user_id, &room_id)?;
    let history = state
        .executions
        .get(&room_id)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let limit = query.limit.unwrap_or(100).min(500);
    let start = history.len().saturating_sub(limit);
    let executions = history[start..].to_vec();

    Ok(Json(RoomEventsResponse {
        room_id,
        executions,
    }))
}

async fn room_orders(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Query(query): Query<ProjectionQuery>,
) -> ApiResult<RoomOrdersResponse> {
    room_orders_response(state, headers, room_id, query.instrument_id.clone(), query).await
}

async fn room_orders_for_instrument(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, instrument_id)): Path<(String, String)>,
    Query(query): Query<ProjectionQuery>,
) -> ApiResult<RoomOrdersResponse> {
    room_orders_response(state, headers, room_id, Some(instrument_id), query).await
}

async fn room_orders_response(
    state: SharedState,
    headers: HeaderMap,
    room_id: String,
    instrument_id: Option<InstrumentId>,
    query: ProjectionQuery,
) -> ApiResult<RoomOrdersResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_projection_access(&mut state, &user_id, &room_id, query.account_id)?;
    let orders = state
        .journal
        .query_orders(
            &user_id,
            &room_id,
            instrument_id.as_deref(),
            query.account_id,
            query_limit(query.limit),
        )
        .map_err(api_error_from_journal)?;
    Ok(Json(RoomOrdersResponse { room_id, orders }))
}

async fn room_trades(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Query(query): Query<ProjectionQuery>,
) -> ApiResult<RoomTradesResponse> {
    room_trades_response(state, headers, room_id, query.instrument_id.clone(), query).await
}

async fn room_trades_for_instrument(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, instrument_id)): Path<(String, String)>,
    Query(query): Query<ProjectionQuery>,
) -> ApiResult<RoomTradesResponse> {
    room_trades_response(state, headers, room_id, Some(instrument_id), query).await
}

async fn room_trades_response(
    state: SharedState,
    headers: HeaderMap,
    room_id: String,
    instrument_id: Option<InstrumentId>,
    query: ProjectionQuery,
) -> ApiResult<RoomTradesResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_projection_access(&mut state, &user_id, &room_id, query.account_id)?;
    let trades = state
        .journal
        .query_trades(
            &user_id,
            &room_id,
            instrument_id.as_deref(),
            query.account_id,
            query_limit(query.limit),
        )
        .map_err(api_error_from_journal)?;
    Ok(Json(RoomTradesResponse { room_id, trades }))
}

async fn room_ticks(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Query(query): Query<ProjectionQuery>,
) -> ApiResult<RoomTicksResponse> {
    room_ticks_response(state, headers, room_id, query.instrument_id.clone(), query).await
}

async fn room_ticks_for_instrument(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, instrument_id)): Path<(String, String)>,
    Query(query): Query<ProjectionQuery>,
) -> ApiResult<RoomTicksResponse> {
    room_ticks_response(state, headers, room_id, Some(instrument_id), query).await
}

async fn room_ticks_response(
    state: SharedState,
    headers: HeaderMap,
    room_id: String,
    instrument_id: Option<InstrumentId>,
    query: ProjectionQuery,
) -> ApiResult<RoomTicksResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_access(&mut state, &user_id, &room_id)?;
    let ticks = state
        .journal
        .query_market_ticks(
            &user_id,
            &room_id,
            instrument_id.as_deref(),
            query_limit(query.limit),
        )
        .map_err(api_error_from_journal)?;
    Ok(Json(RoomTicksResponse { room_id, ticks }))
}

async fn room_ledger(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Query(query): Query<ProjectionQuery>,
) -> ApiResult<RoomLedgerResponse> {
    room_ledger_response(state, headers, room_id, query.instrument_id.clone(), query).await
}

async fn room_ledger_for_instrument(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, instrument_id)): Path<(String, String)>,
    Query(query): Query<ProjectionQuery>,
) -> ApiResult<RoomLedgerResponse> {
    room_ledger_response(state, headers, room_id, Some(instrument_id), query).await
}

async fn room_ledger_response(
    state: SharedState,
    headers: HeaderMap,
    room_id: String,
    instrument_id: Option<InstrumentId>,
    query: ProjectionQuery,
) -> ApiResult<RoomLedgerResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_projection_access(&mut state, &user_id, &room_id, query.account_id)?;
    let ledger = state
        .journal
        .query_account_ledger(
            &user_id,
            &room_id,
            instrument_id.as_deref(),
            query.account_id,
            query_limit(query.limit),
        )
        .map_err(api_error_from_journal)?;
    Ok(Json(RoomLedgerResponse { room_id, ledger }))
}

async fn room_positions(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Query(query): Query<ProjectionQuery>,
) -> ApiResult<RoomPositionsResponse> {
    room_positions_response(state, headers, room_id, query.instrument_id.clone(), query).await
}

async fn room_positions_for_instrument(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, instrument_id)): Path<(String, String)>,
    Query(query): Query<ProjectionQuery>,
) -> ApiResult<RoomPositionsResponse> {
    room_positions_response(state, headers, room_id, Some(instrument_id), query).await
}

async fn room_positions_response(
    state: SharedState,
    headers: HeaderMap,
    room_id: String,
    instrument_id: Option<InstrumentId>,
    query: ProjectionQuery,
) -> ApiResult<RoomPositionsResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_projection_access(&mut state, &user_id, &room_id, query.account_id)?;
    let positions = state
        .journal
        .query_position_snapshots(
            &user_id,
            &room_id,
            instrument_id.as_deref(),
            query.account_id,
            query_limit(query.limit),
        )
        .map_err(api_error_from_journal)?;
    Ok(Json(RoomPositionsResponse { room_id, positions }))
}

fn query_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(100).clamp(1, 500)
}

fn current_user_id(headers: &HeaderMap) -> Result<String, (StatusCode, Json<ErrorResponse>)> {
    match headers.get(USER_ID_HEADER) {
        Some(value) => {
            let user_id = value.to_str().map_err(|_| {
                api_error(
                    StatusCode::BAD_REQUEST,
                    format!("{USER_ID_HEADER} must be valid UTF-8"),
                )
            })?;
            let user_id = user_id.trim();
            if user_id.is_empty() {
                return Err(api_error(
                    StatusCode::BAD_REQUEST,
                    format!("{USER_ID_HEADER} must not be empty"),
                ));
            }
            Ok(user_id.to_string())
        }
        None => Ok(DEFAULT_USER_ID.to_string()),
    }
}

fn ensure_room_access(
    state: &mut AppState,
    user_id: &str,
    room_id: &str,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    state.rooms.status(room_id).map_err(api_error_from_room)?;
    if state
        .journal
        .user_can_access_room(user_id, room_id)
        .map_err(api_error_from_journal)?
    {
        return Ok(());
    }

    Err(api_error(
        StatusCode::FORBIDDEN,
        format!("user {user_id} cannot access room {room_id}"),
    ))
}

fn ensure_room_admin(
    state: &mut AppState,
    user_id: &str,
    room_id: &str,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    state.rooms.status(room_id).map_err(api_error_from_room)?;
    if state
        .journal
        .user_can_administer_room(user_id, room_id)
        .map_err(api_error_from_journal)?
    {
        return Ok(());
    }

    Err(api_error(
        StatusCode::FORBIDDEN,
        format!("user {user_id} cannot administer room {room_id}"),
    ))
}

fn ensure_projection_access(
    state: &mut AppState,
    user_id: &str,
    room_id: &str,
    account_id: Option<AccountId>,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    match account_id {
        Some(account_id) => ensure_account_access(state, user_id, room_id, account_id),
        None => ensure_room_admin(state, user_id, room_id),
    }
}

fn ensure_account_access(
    state: &mut AppState,
    user_id: &str,
    room_id: &str,
    account_id: AccountId,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    state.rooms.status(room_id).map_err(api_error_from_room)?;
    if state
        .journal
        .user_can_access_account(user_id, room_id, account_id)
        .map_err(api_error_from_journal)?
    {
        return Ok(());
    }

    Err(api_error(
        StatusCode::FORBIDDEN,
        format!("user {user_id} cannot access account {account_id} in room {room_id}"),
    ))
}

fn scenario_account_ids(scenario: &ScenarioConfig) -> Vec<AccountId> {
    scenario
        .accounts
        .iter()
        .map(|account| match account {
            exchange_core::ScenarioAccount::Basic { account_id, .. }
            | exchange_core::ScenarioAccount::Spot { account_id, .. } => *account_id,
        })
        .collect()
}

async fn market_view(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<MarketView> {
    market_view_response(state, headers, room_id, None).await
}

async fn market_view_for_instrument(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, instrument_id)): Path<(String, String)>,
) -> ApiResult<MarketView> {
    market_view_response(state, headers, room_id, Some(instrument_id)).await
}

async fn market_view_response(
    state: SharedState,
    headers: HeaderMap,
    room_id: String,
    instrument_id: Option<InstrumentId>,
) -> ApiResult<MarketView> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_admin(&mut state, &user_id, &room_id)?;
    let room = state.rooms.room(&room_id).map_err(api_error_from_room)?;
    let venue_id = room.venue_id().to_string();
    let instrument_id = instrument_id.unwrap_or_else(|| room.primary_instrument_id().to_string());
    Ok(Json(MarketView {
        room_id: room_id.clone(),
        venue_id,
        instrument_id: instrument_id.clone(),
        status: state.rooms.status(&room_id).map_err(api_error_from_room)?,
        book: state
            .rooms
            .book_snapshot_for(&room_id, &instrument_id)
            .map_err(api_error_from_room)?,
        accounts: state
            .rooms
            .account_snapshots_for(&room_id, &instrument_id)
            .map_err(api_error_from_room)?,
    }))
}

async fn book_snapshot(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<BookSnapshot> {
    book_snapshot_response(state, headers, room_id, None).await
}

async fn book_snapshot_for_instrument(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, instrument_id)): Path<(String, String)>,
) -> ApiResult<BookSnapshot> {
    book_snapshot_response(state, headers, room_id, Some(instrument_id)).await
}

async fn book_snapshot_response(
    state: SharedState,
    headers: HeaderMap,
    room_id: String,
    instrument_id: Option<InstrumentId>,
) -> ApiResult<BookSnapshot> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_access(&mut state, &user_id, &room_id)?;
    let instrument_id = match instrument_id {
        Some(instrument_id) => instrument_id,
        None => state
            .rooms
            .room(&room_id)
            .map(|room| room.primary_instrument_id().to_string())
            .map_err(api_error_from_room)?,
    };
    state
        .rooms
        .book_snapshot_for(&room_id, &instrument_id)
        .map(Json)
        .map_err(api_error_from_room)
}

async fn account_snapshots(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<AccountSnapshots> {
    account_snapshots_response(state, headers, room_id, None).await
}

async fn account_snapshots_for_instrument(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, instrument_id)): Path<(String, String)>,
) -> ApiResult<AccountSnapshots> {
    account_snapshots_response(state, headers, room_id, Some(instrument_id)).await
}

async fn account_snapshots_response(
    state: SharedState,
    headers: HeaderMap,
    room_id: String,
    instrument_id: Option<InstrumentId>,
) -> ApiResult<AccountSnapshots> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_admin(&mut state, &user_id, &room_id)?;
    let instrument_id = match instrument_id {
        Some(instrument_id) => instrument_id,
        None => state
            .rooms
            .room(&room_id)
            .map(|room| room.primary_instrument_id().to_string())
            .map_err(api_error_from_room)?,
    };
    state
        .rooms
        .account_snapshots_for(&room_id, &instrument_id)
        .map(Json)
        .map_err(api_error_from_room)
}

async fn venue_account_snapshots(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<RoomVenueAccountsResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_admin(&mut state, &user_id, &room_id)?;
    state
        .rooms
        .venue_account_snapshots(&room_id)
        .map(|accounts| {
            Json(RoomVenueAccountsResponse {
                room_id: room_id.clone(),
                accounts,
            })
        })
        .map_err(api_error_from_room)
}

async fn venue_account_snapshots_by_venue(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<RoomVenueAccountsByVenueResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_admin(&mut state, &user_id, &room_id)?;
    state
        .rooms
        .venue_account_snapshots_by_venue(&room_id)
        .map(|accounts| {
            Json(RoomVenueAccountsByVenueResponse {
                room_id: room_id.clone(),
                accounts,
            })
        })
        .map_err(api_error_from_room)
}

async fn room_portfolios(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<RoomPortfoliosResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_admin(&mut state, &user_id, &room_id)?;
    state
        .rooms
        .portfolio_snapshots(&room_id)
        .map(|accounts| {
            Json(RoomPortfoliosResponse {
                room_id: room_id.clone(),
                accounts,
            })
        })
        .map_err(api_error_from_room)
}

async fn room_asset_ledger(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<RoomAssetLedgerResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_admin(&mut state, &user_id, &room_id)?;
    state
        .rooms
        .asset_ledger(&room_id)
        .map(|ledger| {
            Json(RoomAssetLedgerResponse {
                room_id: room_id.clone(),
                ledger: ledger.to_vec(),
            })
        })
        .map_err(api_error_from_room)
}

async fn room_net_worth(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<RoomNetWorthSnapshot> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_admin(&mut state, &user_id, &room_id)?;
    state
        .rooms
        .net_worth_snapshot(&room_id)
        .map(Json)
        .map_err(api_error_from_room)
}

async fn room_clock(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<RoomClockResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_access(&mut state, &user_id, &room_id)?;
    state
        .rooms
        .clock(&room_id)
        .map(|clock| {
            Json(RoomClockResponse {
                room_id: room_id.clone(),
                clock,
            })
        })
        .map_err(api_error_from_room)
}

async fn advance_room_clock(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Json(request): Json<AdvanceClockRequest>,
) -> ApiResult<AdvanceClockResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_admin(&mut state, &user_id, &room_id)?;
    let mut candidate_rooms = state.rooms.clone();
    let completed_transfers = candidate_rooms
        .advance_clock(&room_id, request.steps)
        .map_err(api_error_from_room)?;
    let clock = candidate_rooms
        .clock(&room_id)
        .map_err(api_error_from_room)?;
    let snapshot = current_room_snapshot(
        &candidate_rooms,
        &room_id,
        last_persisted_command_seq(&state, &room_id),
    )
    .ok_or_else(|| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("room {room_id} disappeared while advancing its clock"),
        )
    })?;
    if completed_transfers.is_empty() {
        state
            .journal
            .append_snapshot(&snapshot)
            .map_err(api_error_from_journal)?;
    } else {
        let records = completed_transfers
            .iter()
            .cloned()
            .map(|transfer| JournalTransfer::recorded(room_id.clone(), transfer))
            .collect::<Vec<_>>();
        state
            .journal
            .append_transfers(&records, Some(&snapshot))
            .map_err(api_error_from_journal)?;
    }
    state.rooms = candidate_rooms;

    Ok(Json(AdvanceClockResponse {
        room_id,
        clock,
        completed_transfers,
    }))
}

async fn room_transfers(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<RoomTransfersResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_admin(&mut state, &user_id, &room_id)?;
    state
        .journal
        .query_transfers(&user_id, &room_id, None, 100)
        .map(|transfers| {
            Json(RoomTransfersResponse {
                room_id: room_id.clone(),
                transfers,
            })
        })
        .map_err(api_error_from_journal)
}

async fn submit_deposit(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Json(request): Json<TransferRequest>,
) -> ApiResult<TransferResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_account_access(&mut state, &user_id, &room_id, request.account_id)?;
    let mut candidate_rooms = state.rooms.clone();
    let transfer = candidate_rooms
        .submit_deposit(
            &room_id,
            request.venue_id.as_deref(),
            request.account_id,
            request.asset_id,
            request.amount,
        )
        .map_err(api_error_from_room)?;
    let snapshot = current_room_snapshot(
        &candidate_rooms,
        &room_id,
        last_persisted_command_seq(&state, &room_id),
    )
    .ok_or_else(|| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("room {room_id} disappeared while submitting a deposit"),
        )
    })?;
    let record = JournalTransfer::recorded(room_id.clone(), transfer.clone());
    state
        .journal
        .append_transfers(&[record], Some(&snapshot))
        .map_err(api_error_from_journal)?;
    state.rooms = candidate_rooms;

    Ok(Json(TransferResponse { room_id, transfer }))
}

async fn submit_withdrawal(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Json(request): Json<TransferRequest>,
) -> ApiResult<TransferResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_account_access(&mut state, &user_id, &room_id, request.account_id)?;
    let mut candidate_rooms = state.rooms.clone();
    let transfer = candidate_rooms
        .submit_withdrawal(
            &room_id,
            request.venue_id.as_deref(),
            request.account_id,
            request.asset_id,
            request.amount,
        )
        .map_err(api_error_from_room)?;
    let snapshot = current_room_snapshot(
        &candidate_rooms,
        &room_id,
        last_persisted_command_seq(&state, &room_id),
    )
    .ok_or_else(|| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("room {room_id} disappeared while submitting a withdrawal"),
        )
    })?;
    let record = JournalTransfer::recorded(room_id.clone(), transfer.clone());
    state
        .journal
        .append_transfers(&[record], Some(&snapshot))
        .map_err(api_error_from_journal)?;
    state.rooms = candidate_rooms;

    Ok(Json(TransferResponse { room_id, transfer }))
}

async fn submit_venue_to_venue_transfer(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Json(request): Json<VenueToVenueTransferRequest>,
) -> ApiResult<VenueToVenueTransferResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_account_access(&mut state, &user_id, &room_id, request.account_id)?;
    let mut candidate_rooms = state.rooms.clone();
    let transfer = candidate_rooms
        .submit_venue_to_venue_transfer(
            &room_id,
            &request.from_venue_id,
            &request.to_venue_id,
            request.account_id,
            request.asset_id,
            request.amount,
        )
        .map_err(api_error_from_room)?;
    let snapshot = current_room_snapshot(
        &candidate_rooms,
        &room_id,
        last_persisted_command_seq(&state, &room_id),
    )
    .ok_or_else(|| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("room {room_id} disappeared while submitting a venue transfer"),
        )
    })?;
    let mut records = vec![JournalTransfer::recorded(
        room_id.clone(),
        transfer.withdrawal.clone(),
    )];
    if let Some(deposit) = &transfer.deposit {
        records.push(JournalTransfer::recorded(room_id.clone(), deposit.clone()));
    }
    state
        .journal
        .append_transfers(&records, Some(&snapshot))
        .map_err(api_error_from_journal)?;
    state.rooms = candidate_rooms;

    Ok(Json(VenueToVenueTransferResponse { room_id, transfer }))
}

async fn submit_order(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Json(request): Json<SubmitOrderRequest>,
) -> ApiResult<OrderResponse> {
    submit_order_response(state, headers, room_id, None, request).await
}

async fn submit_order_for_instrument(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, instrument_id)): Path<(String, String)>,
    Json(request): Json<SubmitOrderRequest>,
) -> ApiResult<OrderResponse> {
    submit_order_response(state, headers, room_id, Some(instrument_id), request).await
}

async fn set_mark_price_for_instrument(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room_id, instrument_id)): Path<(String, String)>,
    Json(request): Json<SetMarkPriceRequest>,
) -> ApiResult<RoomExecutionSummary> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_admin(&mut state, &user_id, &room_id)?;
    let mut candidate_rooms = state.rooms.clone();
    let previous_history_len = candidate_rooms
        .execution_history(&room_id)
        .map_err(api_error_from_room)?
        .len();
    let command = Command::SetMarkPrice(SetMarkPrice {
        price_tick: request.price_tick,
    });
    let execution = candidate_rooms
        .apply_to_instrument(&room_id, &instrument_id, command.clone())
        .map_err(api_error_from_room)?;
    let response = RoomExecutionSummary::from_execution(execution.clone());
    let journal_record = JournalExecution::system(command, execution);
    let journal_room_id = journal_record.room_id.clone();
    journal_new_executions(
        &mut state,
        &candidate_rooms,
        &journal_room_id,
        previous_history_len,
        journal_record,
    )
    .map_err(api_error_from_journal)?;
    state.rooms = candidate_rooms;

    Ok(Json(response))
}

async fn submit_order_response(
    state: SharedState,
    headers: HeaderMap,
    room_id: String,
    instrument_id: Option<InstrumentId>,
    request: SubmitOrderRequest,
) -> ApiResult<OrderResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_account_access(&mut state, &user_id, &room_id, request.account_id)?;
    let first_order_id = state.next_order_id;
    if order_action_allocates_id(&request.action)
        && first_order_id >= SYSTEM_LIQUIDATION_ORDER_ID_BASE
    {
        return Err(api_error(
            StatusCode::CONFLICT,
            "API order-id range is exhausted",
        ));
    }
    let mut candidate_rooms = state.rooms.clone();
    let previous_history_len = candidate_rooms
        .execution_history(&room_id)
        .map_err(api_error_from_room)?
        .len();
    let mut gateway = OrderGateway::new(&mut candidate_rooms, first_order_id);
    let instrument_id = instrument_id.or_else(|| request.instrument_id.clone());
    let execution = gateway
        .submit_action(GatewayRequest {
            participant_id: request.participant_id.clone(),
            room_id,
            instrument_id,
            account_id: request.account_id,
            action: request.action.clone(),
        })
        .map_err(|error| api_error_from_room(error.into_room_error()))?;

    let next_order_id = gateway.next_order_id();
    let response = OrderResponse::from_gateway_execution(
        request.participant_id,
        request.account_id,
        request.action,
        execution.execution.clone(),
    );
    let journal_record = JournalExecution::submitted(
        execution.participant_id,
        execution.account_id,
        execution.command,
        execution.execution,
    );
    let journal_room_id = journal_record.room_id.clone();
    journal_new_executions(
        &mut state,
        &candidate_rooms,
        &journal_room_id,
        previous_history_len,
        journal_record,
    )
    .map_err(api_error_from_journal)?;
    state.rooms = candidate_rooms;
    state.next_order_id = next_order_id;

    Ok(Json(response))
}

fn order_action_allocates_id(action: &OrderAction) -> bool {
    !matches!(
        action,
        OrderAction::Cancel { .. } | OrderAction::Amend { .. }
    )
}

async fn pause_room(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<RoomStatusResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_admin(&mut state, &user_id, &room_id)?;
    let mut candidate_rooms = state.rooms.clone();
    candidate_rooms
        .pause_room(&room_id)
        .map_err(api_error_from_room)?;
    let status = candidate_rooms
        .status(&room_id)
        .map_err(api_error_from_room)?;
    state
        .journal
        .update_room_status(&room_id, status)
        .map_err(api_error_from_journal)?;
    state.rooms = candidate_rooms;
    Ok(Json(RoomStatusResponse { room_id, status }))
}

async fn resume_room(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<RoomStatusResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_admin(&mut state, &user_id, &room_id)?;
    let mut candidate_rooms = state.rooms.clone();
    candidate_rooms
        .resume_room(&room_id)
        .map_err(api_error_from_room)?;
    let status = candidate_rooms
        .status(&room_id)
        .map_err(api_error_from_room)?;
    state
        .journal
        .update_room_status(&room_id, status)
        .map_err(api_error_from_journal)?;
    state.rooms = candidate_rooms;
    Ok(Json(RoomStatusResponse { room_id, status }))
}

fn lock_state(
    state: &SharedState,
) -> Result<std::sync::MutexGuard<'_, AppState>, (StatusCode, Json<ErrorResponse>)> {
    state.lock().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "server state lock poisoned".to_string(),
            }),
        )
    })
}

fn start_agent_worker_for_room(
    state: &mut AppState,
    room_id: RoomId,
    user_id: String,
    request: StartAgentsRequest,
) -> Result<AgentWorkerStatus, (StatusCode, Json<ErrorResponse>)> {
    validate_agent_templates(&room_id, &request.agents)?;

    if let Some(worker) = state.agent_workers.remove(&room_id) {
        worker.stop();
    }
    if request.agents.is_empty() {
        return Ok(AgentWorkerStatus::stopped(room_id));
    }

    let interval_ms = request
        .interval_ms
        .unwrap_or(DEFAULT_AGENT_INTERVAL_MS)
        .max(1);
    let participant_ids = request
        .agents
        .iter()
        .map(|template| template.participant_id().to_string())
        .collect::<Vec<_>>();
    let worker = AgentWorkerHandle::spawn(
        state.base_url.clone(),
        room_id.clone(),
        user_id,
        request.agents,
        Duration::from_millis(interval_ms),
    )
    .map_err(|error| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: error.to_string(),
            }),
        )
    })?;
    state.agent_workers.insert(room_id.clone(), worker);

    Ok(AgentWorkerStatus {
        room_id,
        running: true,
        interval_ms,
        participants: participant_ids,
        last_error: None,
    })
}

fn validate_agent_templates(
    room_id: &str,
    templates: &[AgentTemplate],
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    for template in templates {
        let configured_room_id = match template {
            AgentTemplate::NoiseTrader(config) => &config.participant.room_id,
            AgentTemplate::DcaTrader(config) => &config.participant.room_id,
            AgentTemplate::GridTrader(config) => &config.participant.room_id,
        };
        if configured_room_id != room_id {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                format!(
                    "agent {} is configured for room {}, not path room {room_id}",
                    template.participant_id(),
                    configured_room_id
                ),
            ));
        }
    }
    Ok(())
}

fn agent_status_for_room(state: &AppState, room_id: &str) -> AgentWorkerStatus {
    state
        .agent_workers
        .get(room_id)
        .map(|worker| worker.status(room_id.to_string()))
        .unwrap_or_else(|| AgentWorkerStatus::stopped(room_id.to_string()))
}

fn api_error(status: StatusCode, error: impl Into<String>) -> (StatusCode, Json<ErrorResponse>) {
    (
        status,
        Json(ErrorResponse {
            error: error.into(),
        }),
    )
}

fn api_error_from_room(error: RoomManagerError) -> (StatusCode, Json<ErrorResponse>) {
    let status = match error {
        RoomManagerError::RoomNotFound { .. } => StatusCode::NOT_FOUND,
        RoomManagerError::OrderOwnershipMismatch { .. } => StatusCode::FORBIDDEN,
        RoomManagerError::RoomAlreadyExists { .. }
        | RoomManagerError::MarketConfig(_)
        | RoomManagerError::Actor(_)
        | RoomManagerError::Scenario(_)
        | RoomManagerError::Simulation(_)
        | RoomManagerError::SystemOrderIdOverflow => StatusCode::BAD_REQUEST,
    };

    (
        status,
        Json(ErrorResponse {
            error: format!("{error:?}"),
        }),
    )
}

fn api_error_from_json(error: serde_json::Error) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse {
            error: format!("invalid room request: {error}"),
        }),
    )
}

fn api_error_from_journal(error: JournalError) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            error: error.to_string(),
        }),
    )
}

trait IntoRoomError {
    fn into_room_error(self) -> RoomManagerError;
}

impl IntoRoomError for exchange_core::GatewayError {
    fn into_room_error(self) -> RoomManagerError {
        match self {
            Self::Room(error) => error,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HealthResponse {
    pub ok: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CreateRoomResponse {
    pub room_id: String,
    pub seed_execution_count: usize,
    pub agent_worker: AgentWorkerStatus,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ListRoomsResponse {
    pub rooms: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomEventsQuery {
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProjectionQuery {
    pub instrument_id: Option<InstrumentId>,
    pub account_id: Option<AccountId>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomEventsResponse {
    pub room_id: String,
    pub executions: Vec<RoomExecutionSummary>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomOrdersResponse {
    pub room_id: String,
    pub orders: Vec<OrderProjection>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomTradesResponse {
    pub room_id: String,
    pub trades: Vec<TradeProjection>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomTicksResponse {
    pub room_id: String,
    pub ticks: Vec<MarketTickProjection>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomLedgerResponse {
    pub room_id: String,
    pub ledger: Vec<AccountLedgerProjection>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomPositionsResponse {
    pub room_id: String,
    pub positions: Vec<PositionSnapshotProjection>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomVenueAccountsResponse {
    pub room_id: String,
    pub accounts: Vec<VenueAccountSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomVenueAccountsByVenueResponse {
    pub room_id: String,
    pub accounts: Vec<VenueAccountVenueSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomPortfoliosResponse {
    pub room_id: String,
    pub accounts: Vec<PortfolioAccountSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomAssetLedgerResponse {
    pub room_id: String,
    pub ledger: Vec<AssetLedgerEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomClockResponse {
    pub room_id: String,
    pub clock: SimulationClock,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AdvanceClockRequest {
    pub steps: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AdvanceClockResponse {
    pub room_id: String,
    pub clock: SimulationClock,
    pub completed_transfers: Vec<VenueTransfer>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TransferRequest {
    #[serde(default)]
    pub venue_id: Option<String>,
    pub account_id: AccountId,
    pub asset_id: String,
    pub amount: Money,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TransferResponse {
    pub room_id: String,
    pub transfer: VenueTransfer,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VenueToVenueTransferRequest {
    pub from_venue_id: String,
    pub to_venue_id: String,
    pub account_id: AccountId,
    pub asset_id: String,
    pub amount: Money,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VenueToVenueTransferResponse {
    pub room_id: String,
    pub transfer: VenueToVenueTransfer,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomTransfersResponse {
    pub room_id: String,
    pub transfers: Vec<VenueTransfer>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RoomExecutionSummary {
    pub room_id: String,
    #[serde(default)]
    pub instrument_id: Option<InstrumentId>,
    pub command_seq: u64,
    pub status: MarketStatus,
    pub accepted: bool,
    pub reject_reason: Option<String>,
    pub events: Vec<EventSummary>,
    #[serde(default)]
    pub clearing_events: Vec<ClearingEventSummary>,
    pub clearing_event_count: usize,
}

impl RoomExecutionSummary {
    pub(crate) fn from_execution(execution: ActorExecution) -> Self {
        let (accepted, reject_reason, events, clearing_events) = match execution.result {
            ActorExecutionResult::Accepted(market_execution) => {
                let (events, clearing_events) = summarize_market_execution(market_execution);
                (true, None, events, clearing_events)
            }
            ActorExecutionResult::Rejected(reason) => (
                false,
                Some(reject_reason_to_string(reason)),
                Vec::new(),
                Vec::new(),
            ),
        };
        let clearing_event_count = clearing_events.len();

        Self {
            room_id: execution.room_id,
            instrument_id: Some(execution.instrument_id),
            command_seq: execution.command_seq,
            status: execution.status,
            accepted,
            reject_reason,
            events,
            clearing_events,
            clearing_event_count,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SubmitOrderRequest {
    pub participant_id: ParticipantId,
    #[serde(default)]
    pub instrument_id: Option<InstrumentId>,
    pub account_id: AccountId,
    pub action: OrderAction,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SetMarkPriceRequest {
    pub price_tick: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CreateRoomRequest {
    pub scenario: ScenarioConfig,
    #[serde(default)]
    pub agents: Vec<AgentTemplate>,
    pub agent_interval_ms: Option<u64>,
    pub autostart_agents: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StartAgentsRequest {
    pub agents: Vec<AgentTemplate>,
    pub interval_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentWorkerStatus {
    pub room_id: String,
    pub running: bool,
    pub interval_ms: u64,
    pub participants: Vec<ParticipantId>,
    #[serde(default)]
    pub last_error: Option<String>,
}

impl AgentWorkerStatus {
    fn stopped(room_id: String) -> Self {
        Self {
            room_id,
            running: false,
            interval_ms: 0,
            participants: Vec::new(),
            last_error: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoomStatusResponse {
    pub room_id: String,
    pub status: MarketStatus,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OrderResponse {
    pub participant_id: ParticipantId,
    pub account_id: AccountId,
    pub action: OrderAction,
    pub room_id: String,
    pub instrument_id: Option<InstrumentId>,
    pub command_seq: u64,
    pub status: MarketStatus,
    pub accepted: bool,
    pub reject_reason: Option<String>,
    pub events: Vec<EventSummary>,
    pub clearing_events: Vec<ClearingEventSummary>,
    pub clearing_event_count: usize,
}

impl OrderResponse {
    fn from_gateway_execution(
        participant_id: ParticipantId,
        account_id: AccountId,
        action: OrderAction,
        execution: ActorExecution,
    ) -> Self {
        let summary = RoomExecutionSummary::from_execution(execution);

        Self {
            participant_id,
            account_id,
            action,
            room_id: summary.room_id,
            instrument_id: summary.instrument_id,
            command_seq: summary.command_seq,
            status: summary.status,
            accepted: summary.accepted,
            reject_reason: summary.reject_reason,
            events: summary.events,
            clearing_events: summary.clearing_events,
            clearing_event_count: summary.clearing_event_count,
        }
    }
}

fn summarize_market_execution(
    execution: MarketExecution,
) -> (Vec<EventSummary>, Vec<ClearingEventSummary>) {
    match execution {
        MarketExecution::Spot(execution) => (
            execution
                .events
                .into_iter()
                .map(|record| EventSummary::from_event(record.seq, record.event))
                .collect(),
            execution
                .clearing_events
                .into_iter()
                .map(ClearingEventSummary::from_spot)
                .collect(),
        ),
        MarketExecution::Perp(execution) => (
            execution
                .events
                .into_iter()
                .map(|record| EventSummary::from_event(record.seq, record.event))
                .collect(),
            execution
                .clearing_events
                .into_iter()
                .map(ClearingEventSummary::from_perp)
                .collect(),
        ),
    }
}

fn reject_reason_to_string(reason: ActorRejectReason) -> String {
    match reason {
        ActorRejectReason::MarketPaused => "market paused".to_string(),
        ActorRejectReason::MarketClosed => "market closed".to_string(),
        ActorRejectReason::InstrumentNotFound { instrument_id } => {
            format!("instrument not found: {instrument_id}")
        }
        ActorRejectReason::WrongMarketKind => "wrong market kind".to_string(),
        ActorRejectReason::VenueRule(reason) => format!("venue rule rejected: {reason:?}"),
        ActorRejectReason::Clearing(error) => format!("clearing error: {error:?}"),
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum EventSummary {
    OrderAccepted {
        seq: u64,
        order_id: OrderId,
    },
    OrderRejected {
        seq: u64,
        order_id: OrderId,
        reason: String,
    },
    RiskRejected {
        seq: u64,
        order_id: OrderId,
        reason: String,
    },
    TradePrinted {
        seq: u64,
        trade_id: u64,
        #[serde(default)]
        maker_order_id: OrderId,
        #[serde(default)]
        maker_account_id: AccountId,
        #[serde(default)]
        taker_order_id: OrderId,
        #[serde(default)]
        taker_account_id: AccountId,
        price_tick: i64,
        qty: u64,
        #[serde(default = "default_taker_side")]
        taker_side: exchange_core::Side,
    },
    OrderPartiallyFilled {
        seq: u64,
        order_id: OrderId,
        remaining_qty: u64,
    },
    OrderFilled {
        seq: u64,
        order_id: OrderId,
    },
    OrderRested {
        seq: u64,
        order_id: OrderId,
        price_tick: i64,
        remaining_qty: u64,
    },
    OrderExpired {
        seq: u64,
        order_id: OrderId,
        unfilled_qty: u64,
    },
    OrderCanceled {
        seq: u64,
        order_id: OrderId,
        remaining_qty: u64,
    },
    CancelRejected {
        seq: u64,
        order_id: OrderId,
        reason: String,
    },
    OrderAmended {
        seq: u64,
        order_id: OrderId,
        old_price_tick: i64,
        new_price_tick: i64,
        old_qty: u64,
        new_qty: u64,
    },
    AmendRejected {
        seq: u64,
        order_id: OrderId,
        reason: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum ClearingEventSummary {
    SpotTradeSettled {
        trade_id: u64,
        buyer_account_id: AccountId,
        seller_account_id: AccountId,
        price_tick: i64,
        qty: u64,
        #[serde(with = "json_i128")]
        notional: i128,
        #[serde(with = "json_i128")]
        buyer_fee: i128,
        #[serde(with = "json_i128")]
        seller_fee: i128,
        buyer: SpotAccountStateSummary,
        seller: SpotAccountStateSummary,
    },
    PerpTradeSettled {
        trade_id: u64,
        buyer_account_id: AccountId,
        seller_account_id: AccountId,
        price_tick: i64,
        qty: u64,
        #[serde(with = "json_i128")]
        notional: i128,
        #[serde(with = "json_i128")]
        buyer_fee: i128,
        #[serde(with = "json_i128")]
        seller_fee: i128,
        #[serde(with = "json_i128")]
        buyer_realized_pnl: i128,
        #[serde(with = "json_i128")]
        seller_realized_pnl: i128,
        buyer: PerpAccountStateSummary,
        seller: PerpAccountStateSummary,
    },
    PerpMarginStatusChanged {
        account_id: AccountId,
        previous_status: String,
        new_status: String,
        mark_price_tick: i64,
        account: PerpAccountStateSummary,
    },
    PerpLiquidationSettled {
        account_id: AccountId,
        order_id: u64,
        #[serde(with = "json_i128")]
        liquidation_notional: i128,
        #[serde(with = "json_i128")]
        liquidation_fee: i128,
        #[serde(with = "json_i128")]
        insurance_fund_payment: i128,
        #[serde(with = "json_i128")]
        auto_deleveraging_loss: i128,
        auto_deleveraging_allocations: Vec<PerpAutoDeleveragingAllocationSummary>,
        #[serde(with = "json_i128")]
        socialized_loss: i128,
        socialized_loss_allocations: Vec<PerpSocializedLossAllocationSummary>,
        #[serde(with = "json_i128")]
        bad_debt: i128,
        #[serde(with = "json_i128")]
        insurance_fund_balance: i128,
        account: PerpAccountStateSummary,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SpotAccountStateSummary {
    pub account_id: AccountId,
    #[serde(with = "json_i128")]
    pub cash_balance: i128,
    #[serde(with = "json_i128")]
    pub position_qty: i128,
    #[serde(with = "json_i128")]
    pub fees_paid: i128,
}

impl SpotAccountStateSummary {
    fn from_snapshot(snapshot: SpotAccountSnapshot) -> Self {
        Self {
            account_id: snapshot.account_id,
            cash_balance: snapshot.cash_balance,
            position_qty: snapshot.position_qty,
            fees_paid: snapshot.fees_paid,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerpAccountStateSummary {
    pub account_id: AccountId,
    #[serde(with = "json_i128")]
    pub cash_balance: i128,
    #[serde(with = "json_i128")]
    pub position_qty: i128,
    pub avg_entry_price_tick: i64,
    #[serde(with = "json_i128")]
    pub realized_pnl: i128,
    #[serde(with = "json_i128")]
    pub unrealized_pnl: i128,
    #[serde(with = "json_i128")]
    pub equity: i128,
    #[serde(with = "json_i128")]
    pub initial_margin: i128,
    #[serde(with = "json_i128")]
    pub maintenance_margin: i128,
    pub margin_status: String,
    #[serde(with = "json_i128")]
    pub reserved_margin: i128,
    #[serde(with = "json_i128")]
    pub available_cash: i128,
    #[serde(with = "json_i128")]
    pub fees_paid: i128,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerpSocializedLossAllocationSummary {
    pub account_id: AccountId,
    #[serde(with = "json_i128")]
    pub loss: i128,
    pub account: PerpAccountStateSummary,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerpAutoDeleveragingAllocationSummary {
    pub account_id: AccountId,
    #[serde(with = "json_i128")]
    pub position_delta: i128,
    pub price_tick: i64,
    pub qty: u64,
    #[serde(with = "json_i128")]
    pub realized_pnl: i128,
    #[serde(with = "json_i128")]
    pub loss: i128,
    pub account: PerpAccountStateSummary,
}

impl PerpAccountStateSummary {
    fn from_snapshot(snapshot: PerpAccountSnapshot) -> Self {
        Self {
            account_id: snapshot.account_id,
            cash_balance: snapshot.cash_balance,
            position_qty: snapshot.position_qty,
            avg_entry_price_tick: snapshot.avg_entry_price_tick,
            realized_pnl: snapshot.realized_pnl,
            unrealized_pnl: snapshot.unrealized_pnl,
            equity: snapshot.equity,
            initial_margin: snapshot.initial_margin,
            maintenance_margin: snapshot.maintenance_margin,
            margin_status: snapshot.margin_status.as_str().to_string(),
            reserved_margin: snapshot.reserved_margin,
            available_cash: snapshot.available_cash,
            fees_paid: snapshot.fees_paid,
        }
    }
}

impl ClearingEventSummary {
    fn from_spot(event: SpotClearingEvent) -> Self {
        match event {
            SpotClearingEvent::TradeSettled {
                trade_id,
                buyer_account_id,
                seller_account_id,
                price_tick,
                qty,
                notional,
                buyer_fee,
                seller_fee,
                buyer,
                seller,
            } => Self::SpotTradeSettled {
                trade_id,
                buyer_account_id,
                seller_account_id,
                price_tick,
                qty,
                notional,
                buyer_fee,
                seller_fee,
                buyer: SpotAccountStateSummary::from_snapshot(buyer),
                seller: SpotAccountStateSummary::from_snapshot(seller),
            },
        }
    }

    fn from_perp(event: PerpClearingEvent) -> Self {
        match event {
            PerpClearingEvent::TradeSettled {
                trade_id,
                buyer_account_id,
                seller_account_id,
                price_tick,
                qty,
                notional,
                buyer_fee,
                seller_fee,
                buyer_realized_pnl,
                seller_realized_pnl,
                buyer,
                seller,
            } => Self::PerpTradeSettled {
                trade_id,
                buyer_account_id,
                seller_account_id,
                price_tick,
                qty,
                notional,
                buyer_fee,
                seller_fee,
                buyer_realized_pnl,
                seller_realized_pnl,
                buyer: PerpAccountStateSummary::from_snapshot(buyer),
                seller: PerpAccountStateSummary::from_snapshot(seller),
            },
            PerpClearingEvent::MarginStatusChanged {
                account_id,
                previous_status,
                new_status,
                mark_price_tick,
                snapshot,
            } => Self::PerpMarginStatusChanged {
                account_id,
                previous_status: previous_status.as_str().to_string(),
                new_status: new_status.as_str().to_string(),
                mark_price_tick,
                account: PerpAccountStateSummary::from_snapshot(snapshot),
            },
            PerpClearingEvent::LiquidationSettled {
                account_id,
                order_id,
                liquidation_notional,
                liquidation_fee,
                insurance_fund_payment,
                auto_deleveraging_loss,
                auto_deleveraging_allocations,
                socialized_loss,
                socialized_loss_allocations,
                bad_debt,
                insurance_fund_balance,
                snapshot,
            } => Self::PerpLiquidationSettled {
                account_id,
                order_id,
                liquidation_notional,
                liquidation_fee,
                insurance_fund_payment,
                auto_deleveraging_loss,
                auto_deleveraging_allocations: auto_deleveraging_allocations
                    .into_iter()
                    .map(|allocation| PerpAutoDeleveragingAllocationSummary {
                        account_id: allocation.account_id,
                        position_delta: allocation.position_delta,
                        price_tick: allocation.price_tick,
                        qty: allocation.qty,
                        realized_pnl: allocation.realized_pnl,
                        loss: allocation.loss,
                        account: PerpAccountStateSummary::from_snapshot(allocation.snapshot),
                    })
                    .collect(),
                socialized_loss,
                socialized_loss_allocations: socialized_loss_allocations
                    .into_iter()
                    .map(|allocation| PerpSocializedLossAllocationSummary {
                        account_id: allocation.account_id,
                        loss: allocation.loss,
                        account: PerpAccountStateSummary::from_snapshot(allocation.snapshot),
                    })
                    .collect(),
                bad_debt,
                insurance_fund_balance,
                account: PerpAccountStateSummary::from_snapshot(snapshot),
            },
        }
    }
}

impl EventSummary {
    fn from_event(seq: u64, event: Event) -> Self {
        match event {
            Event::OrderAccepted { order_id } => Self::OrderAccepted { seq, order_id },
            Event::OrderRejected { order_id, reason } => Self::OrderRejected {
                seq,
                order_id,
                reason: format!("{reason:?}"),
            },
            Event::RiskRejected { order_id, reason } => Self::RiskRejected {
                seq,
                order_id,
                reason: format!("{reason:?}"),
            },
            Event::TradePrinted(trade) => Self::TradePrinted {
                seq,
                trade_id: trade.trade_id,
                maker_order_id: trade.maker_order_id,
                maker_account_id: trade.maker_account_id,
                taker_order_id: trade.taker_order_id,
                taker_account_id: trade.taker_account_id,
                price_tick: trade.price_tick,
                qty: trade.qty,
                taker_side: trade.taker_side,
            },
            Event::OrderPartiallyFilled {
                order_id,
                remaining_qty,
            } => Self::OrderPartiallyFilled {
                seq,
                order_id,
                remaining_qty,
            },
            Event::OrderFilled { order_id } => Self::OrderFilled { seq, order_id },
            Event::OrderRested {
                order_id,
                price_tick,
                remaining_qty,
            } => Self::OrderRested {
                seq,
                order_id,
                price_tick,
                remaining_qty,
            },
            Event::OrderExpired {
                order_id,
                unfilled_qty,
            } => Self::OrderExpired {
                seq,
                order_id,
                unfilled_qty,
            },
            Event::OrderCanceled {
                order_id,
                remaining_qty,
            } => Self::OrderCanceled {
                seq,
                order_id,
                remaining_qty,
            },
            Event::CancelRejected { order_id, reason } => Self::CancelRejected {
                seq,
                order_id,
                reason: format!("{reason:?}"),
            },
            Event::OrderAmended {
                order_id,
                old_price_tick,
                new_price_tick,
                old_qty,
                new_qty,
            } => Self::OrderAmended {
                seq,
                order_id,
                old_price_tick,
                new_price_tick,
                old_qty,
                new_qty,
            },
            Event::AmendRejected { order_id, reason } => Self::AmendRejected {
                seq,
                order_id,
                reason: format!("{reason:?}"),
            },
        }
    }
}

fn default_taker_side() -> exchange_core::Side {
    exchange_core::Side::Buy
}

#[derive(Clone, Debug)]
pub struct HttpTradingClient {
    base_url: String,
    client: reqwest::blocking::Client,
    user_id: Option<String>,
}

impl HttpTradingClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client: reqwest::blocking::Client::new(),
            user_id: None,
        }
    }

    pub fn with_user_id(base_url: impl Into<String>, user_id: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client: reqwest::blocking::Client::new(),
            user_id: Some(user_id.into()),
        }
    }

    pub fn create_room(
        &self,
        scenario: &ScenarioConfig,
    ) -> Result<CreateRoomResponse, HttpTradingError> {
        self.post_json("/rooms", scenario)
    }

    pub fn create_room_with_agents(
        &self,
        request: &CreateRoomRequest,
    ) -> Result<CreateRoomResponse, HttpTradingError> {
        self.post_json("/rooms", request)
    }

    pub fn list_rooms(&self) -> Result<ListRoomsResponse, HttpTradingError> {
        self.get_json("/rooms")
    }

    pub fn market_view(&self, room_id: &str) -> Result<MarketView, HttpTradingError> {
        self.get_json(&format!("/rooms/{room_id}/view"))
    }

    pub fn market_view_for(
        &self,
        room_id: &str,
        instrument_id: &str,
    ) -> Result<MarketView, HttpTradingError> {
        self.get_json(&format!(
            "/rooms/{room_id}/instruments/{instrument_id}/view"
        ))
    }

    pub fn room_events(
        &self,
        room_id: &str,
        limit: usize,
    ) -> Result<RoomEventsResponse, HttpTradingError> {
        self.get_json(&format!("/rooms/{room_id}/events?limit={limit}"))
    }

    pub fn submit_order(
        &self,
        room_id: &str,
        request: &SubmitOrderRequest,
    ) -> Result<OrderResponse, HttpTradingError> {
        self.post_json(&format!("/rooms/{room_id}/orders"), request)
    }

    pub fn submit_order_for(
        &self,
        room_id: &str,
        instrument_id: &str,
        request: &SubmitOrderRequest,
    ) -> Result<OrderResponse, HttpTradingError> {
        self.post_json(
            &format!("/rooms/{room_id}/instruments/{instrument_id}/orders"),
            request,
        )
    }

    pub fn pause_room(&self, room_id: &str) -> Result<RoomStatusResponse, HttpTradingError> {
        self.post_json(&format!("/rooms/{room_id}/pause"), &())
    }

    pub fn resume_room(&self, room_id: &str) -> Result<RoomStatusResponse, HttpTradingError> {
        self.post_json(&format!("/rooms/{room_id}/resume"), &())
    }

    pub fn start_agents(
        &self,
        room_id: &str,
        request: &StartAgentsRequest,
    ) -> Result<AgentWorkerStatus, HttpTradingError> {
        self.post_json(&format!("/rooms/{room_id}/agents"), request)
    }

    pub fn agent_status(&self, room_id: &str) -> Result<AgentWorkerStatus, HttpTradingError> {
        self.get_json(&format!("/rooms/{room_id}/agents"))
    }

    pub fn stop_agents(&self, room_id: &str) -> Result<AgentWorkerStatus, HttpTradingError> {
        self.post_json(&format!("/rooms/{room_id}/agents/stop"), &())
    }

    fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, HttpTradingError> {
        let mut request = self.client.get(format!("{}{}", self.base_url, path));
        if let Some(user_id) = &self.user_id {
            request = request.header(USER_ID_HEADER, user_id);
        }
        let response = request.send().map_err(HttpTradingError::Http)?;
        decode_response(response)
    }

    fn post_json<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, HttpTradingError> {
        let mut request = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .json(body);
        if let Some(user_id) = &self.user_id {
            request = request.header(USER_ID_HEADER, user_id);
        }
        let response = request.send().map_err(HttpTradingError::Http)?;
        decode_response(response)
    }
}

fn decode_response<T: DeserializeOwned>(
    response: reqwest::blocking::Response,
) -> Result<T, HttpTradingError> {
    let status = response.status();
    if status.is_success() {
        return response.json().map_err(HttpTradingError::Http);
    }

    let fallback = ErrorResponse {
        error: format!("HTTP {status}"),
    };
    let error = response.json::<ErrorResponse>().unwrap_or(fallback);
    Err(HttpTradingError::Api {
        status: status.as_u16(),
        error: error.error,
    })
}

#[derive(Debug)]
pub enum HttpTradingError {
    Http(reqwest::Error),
    Api { status: u16, error: String },
}

impl std::fmt::Display for HttpTradingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(error) => write!(formatter, "{error}"),
            Self::Api { status, error } => write!(formatter, "api error {status}: {error}"),
        }
    }
}

impl std::error::Error for HttpTradingError {}

pub fn run_remote_participant_once<P: Participant + ?Sized>(
    client: &HttpTradingClient,
    participant: &mut P,
) -> Result<Vec<OrderResponse>, HttpTradingError> {
    let config = participant.config().clone();
    let view = client.market_view(&config.room_id)?;
    participant.observe(&view);

    participant
        .decide()
        .into_iter()
        .map(|action| {
            client.submit_order(
                &config.room_id,
                &SubmitOrderRequest {
                    participant_id: config.participant_id.clone(),
                    instrument_id: None,
                    account_id: config.account_id,
                    action,
                },
            )
        })
        .collect()
}

const DEFAULT_AGENT_INTERVAL_MS: u64 = 1_000;

struct AgentWorkerHandle {
    stop: Arc<AtomicBool>,
    last_error: Arc<Mutex<Option<String>>>,
    interval_ms: u64,
    participants: Vec<ParticipantId>,
}

impl AgentWorkerHandle {
    fn spawn(
        base_url: String,
        room_id: RoomId,
        user_id: String,
        templates: Vec<AgentTemplate>,
        interval: Duration,
    ) -> Result<Self, AgentWorkerError> {
        if templates.is_empty() {
            return Err(AgentWorkerError::NoAgents);
        }

        let interval_ms = interval.as_millis().try_into().unwrap_or(u64::MAX);
        let participants = templates
            .iter()
            .map(|template| template.participant_id().to_string())
            .collect::<Vec<_>>();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let last_error = Arc::new(Mutex::new(None));
        let worker_last_error = Arc::clone(&last_error);
        thread::Builder::new()
            .name(format!("marketforge-agents-{room_id}"))
            .spawn(move || {
                let client = HttpTradingClient::with_user_id(base_url, user_id);
                let mut participants = templates
                    .into_iter()
                    .map(AgentTemplate::into_participant)
                    .collect::<Vec<_>>();

                while !worker_stop.load(Ordering::Relaxed) {
                    for participant in &mut participants {
                        if worker_stop.load(Ordering::Relaxed) {
                            break;
                        }
                        if let Err(error) =
                            run_remote_participant_once(&client, participant.as_mut())
                        {
                            if let Ok(mut last_error) = worker_last_error.lock() {
                                *last_error = Some(error.to_string());
                            }
                            worker_stop.store(true, Ordering::Relaxed);
                            return;
                        }
                    }
                    sleep_until_next_step(interval, &worker_stop);
                }
            })
            .map_err(AgentWorkerError::Spawn)?;

        Ok(Self {
            stop,
            last_error,
            interval_ms,
            participants,
        })
    }

    fn status(&self, room_id: String) -> AgentWorkerStatus {
        AgentWorkerStatus {
            room_id,
            running: !self.stop.load(Ordering::Relaxed),
            interval_ms: self.interval_ms,
            participants: self.participants.clone(),
            last_error: self
                .last_error
                .lock()
                .ok()
                .and_then(|last_error| last_error.clone()),
        }
    }

    fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for AgentWorkerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn sleep_until_next_step(interval: Duration, stop: &AtomicBool) {
    let mut slept = Duration::ZERO;
    while slept < interval && !stop.load(Ordering::Relaxed) {
        let remaining = interval - slept;
        let chunk = remaining.min(Duration::from_millis(50));
        thread::sleep(chunk);
        slept += chunk;
    }
}

#[derive(Debug)]
enum AgentWorkerError {
    NoAgents,
    Spawn(std::io::Error),
}

impl std::fmt::Display for AgentWorkerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAgents => formatter.write_str("agent worker needs at least one agent"),
            Self::Spawn(error) => write!(formatter, "failed to spawn agent worker: {error}"),
        }
    }
}

impl std::error::Error for AgentWorkerError {}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Method, Request},
    };
    use exchange_core::{
        AgentTemplate, DcaTrader, DcaTraderConfig, GatewayRequest, InstrumentConfig, MarketConfig,
        NewOrder, OrderKind, ParticipantConfig, ParticipantKind, RoomBootstrap, Side,
        SpotClearingConfig, SpotMarketConfig, SpotRiskConfig,
        scenario::{ScenarioAccount, ScenarioPortfolio},
    };
    use tower::ServiceExt;

    use crate::journal::{JournalError, JournalExecution, JournalStore, PostgresJournalStore};

    fn spot_scenario(room_id: &str) -> ScenarioConfig {
        ScenarioConfig {
            room_id: room_id.to_string(),
            venue_preset: None,
            venue_rules: exchange_core::VenueRuleConfig::default(),
            venue_asset_policy: exchange_core::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: MarketConfig::Spot(SpotMarketConfig {
                instrument: InstrumentConfig::new("V-BTC-SPOT", 1, 1).unwrap(),
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
                    cash_balance: 1_000,
                    position_qty: 10,
                },
                ScenarioAccount::Basic {
                    account_id: 20,
                    cash_balance: 1_000,
                },
            ],
            seed_orders: vec![],
            routed_seed_orders: Vec::new(),
        }
    }

    fn seeded_spot_scenario(room_id: &str) -> ScenarioConfig {
        let mut scenario = spot_scenario(room_id);
        scenario.seed_orders = vec![Command::NewOrder(NewOrder {
            order_id: 10_000,
            account_id: 10,
            side: Side::Sell,
            kind: OrderKind::Limit { price_tick: 104 },
            qty: 8,
            reduce_only: false,
        })];
        scenario
    }

    fn spot_perp_scenario(room_id: &str) -> ScenarioConfig {
        ScenarioConfig {
            room_id: room_id.to_string(),
            venue_preset: None,
            venue_rules: exchange_core::VenueRuleConfig::default(),
            venue_asset_policy: exchange_core::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: MarketConfig::Spot(SpotMarketConfig {
                instrument: InstrumentConfig::new("V-BTC-SPOT", 1, 1).unwrap(),
                clearing: SpotClearingConfig::default(),
                risk: SpotRiskConfig::default(),
            }),
            extra_markets: vec![MarketConfig::Perp(exchange_core::PerpMarketConfig {
                instrument: InstrumentConfig::new("V-BTC-PERP", 1, 1).unwrap(),
                clearing: exchange_core::PerpClearingConfig {
                    leverage: 10,
                    ..exchange_core::PerpClearingConfig::default()
                },
                risk: exchange_core::PerpRiskConfig::default(),
                initial_mark_price_tick: 100,
            })],
            initial_portfolios: Vec::new(),
            initial_allocations: Vec::new(),
            routed_initial_allocations: Vec::new(),
            accounts: vec![
                ScenarioAccount::Spot {
                    account_id: 10,
                    cash_balance: 1_000,
                    position_qty: 10,
                },
                ScenarioAccount::Basic {
                    account_id: 20,
                    cash_balance: 1_000,
                },
            ],
            seed_orders: Vec::new(),
            routed_seed_orders: vec![exchange_core::ScenarioSeedOrder {
                instrument_id: Some("V-BTC-PERP".to_string()),
                command: Command::NewOrder(NewOrder {
                    order_id: 10_000,
                    account_id: 20,
                    side: Side::Sell,
                    kind: OrderKind::Limit { price_tick: 100 },
                    qty: 5,
                    reduce_only: false,
                }),
            }],
        }
    }

    fn perp_liquidation_scenario(room_id: &str) -> ScenarioConfig {
        perp_liquidation_scenario_with_mark(room_id, 80)
    }

    fn perp_liquidation_scenario_with_mark(
        room_id: &str,
        initial_mark_price_tick: i64,
    ) -> ScenarioConfig {
        ScenarioConfig {
            room_id: room_id.to_string(),
            venue_preset: None,
            venue_rules: exchange_core::VenueRuleConfig::default(),
            venue_asset_policy: exchange_core::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: MarketConfig::Perp(exchange_core::PerpMarketConfig {
                instrument: InstrumentConfig::new("V-BTC-PERP", 1, 1).unwrap(),
                clearing: exchange_core::PerpClearingConfig {
                    leverage: 10,
                    maintenance_margin_ppm: 50_000,
                    liquidation_fee_ppm: 10_000,
                    ..exchange_core::PerpClearingConfig::default()
                },
                risk: exchange_core::PerpRiskConfig::default(),
                initial_mark_price_tick,
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

    fn dca_template(room_id: &str, participant_id: &str, account_id: AccountId) -> AgentTemplate {
        AgentTemplate::DcaTrader(DcaTraderConfig {
            participant: ParticipantConfig {
                participant_id: participant_id.to_string(),
                kind: ParticipantKind::RuleAgent,
                room_id: room_id.to_string(),
                account_id,
            },
            interval_steps: 1,
            order_qty: 2,
            use_market_order: false,
            limit_offset_ticks: 0,
            fallback_price_tick: 100,
            side: Side::Buy,
        })
    }

    #[tokio::test]
    async fn cors_preflight_allows_user_identity_header() {
        let response = new_app()
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/rooms")
                    .header("origin", "http://127.0.0.1:57304")
                    .header("access-control-request-method", "POST")
                    .header("access-control-request-headers", "content-type, x-user-id")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let allowed_headers = response
            .headers()
            .get("access-control-allow-headers")
            .unwrap()
            .to_str()
            .unwrap()
            .to_ascii_lowercase();
        assert!(allowed_headers.contains("x-user-id"));
    }

    #[test]
    fn account_summaries_preserve_i128_values() {
        let summary = SpotAccountStateSummary::from_snapshot(SpotAccountSnapshot {
            account_id: 7,
            cash_balance: i128::MAX,
            position_qty: i128::MIN + 1,
            reserved_cash: 0,
            reserved_position: 0,
            available_cash: i128::MAX,
            available_position: i128::MIN + 1,
            fees_paid: i128::MAX - 1,
        });
        assert_eq!(summary.cash_balance, i128::MAX);
        assert_eq!(summary.position_qty, i128::MIN + 1);
        assert_eq!(summary.fees_paid, i128::MAX - 1);
        let json = serde_json::to_string(&summary).unwrap();
        let round_trip: SpotAccountStateSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(round_trip, summary);

        let normal = SpotAccountStateSummary::from_snapshot(SpotAccountSnapshot {
            account_id: 8,
            cash_balance: 42,
            position_qty: -3,
            reserved_cash: 0,
            reserved_position: 0,
            available_cash: 42,
            available_position: -3,
            fees_paid: 1,
        });
        let normal_json = serde_json::to_value(normal).unwrap();
        assert_eq!(normal_json["cash_balance"], 42);

        let execution = RoomExecutionSummary {
            room_id: "i128-room".to_string(),
            instrument_id: Some("V-BTC-SPOT".to_string()),
            command_seq: 3,
            status: MarketStatus::Running,
            accepted: true,
            reject_reason: None,
            events: Vec::new(),
            clearing_events: vec![ClearingEventSummary::SpotTradeSettled {
                trade_id: 1,
                buyer_account_id: 7,
                seller_account_id: 8,
                price_tick: 100,
                qty: 1,
                notional: i128::MAX,
                buyer_fee: 42,
                seller_fee: i128::MIN,
                buyer: summary.clone(),
                seller: summary,
            }],
            clearing_event_count: 1,
        };
        let execution_json = serde_json::to_value(&execution).unwrap();
        assert_eq!(execution_json["clearing_events"][0]["buyer_fee"], 42);
        assert_eq!(
            execution_json["clearing_events"][0]["notional"],
            i128::MAX.to_string()
        );
        let round_trip: RoomExecutionSummary = serde_json::from_value(execution_json).unwrap();
        match &round_trip.clearing_events[0] {
            ClearingEventSummary::SpotTradeSettled {
                notional,
                buyer_fee,
                seller_fee,
                buyer,
                ..
            } => {
                assert_eq!(*notional, i128::MAX);
                assert_eq!(*buyer_fee, 42);
                assert_eq!(*seller_fee, i128::MIN);
                assert_eq!(buyer.cash_balance, i128::MAX);
            }
            other => panic!("unexpected clearing event: {other:?}"),
        }
    }

    #[test]
    fn reserved_and_max_order_ids_are_rejected_without_overflow() {
        let command_with_id = |order_id| {
            Command::NewOrder(NewOrder {
                order_id,
                account_id: 10,
                side: Side::Sell,
                kind: OrderKind::Limit { price_tick: 100 },
                qty: 1,
                reduce_only: false,
            })
        };
        for order_id in [SYSTEM_LIQUIDATION_ORDER_ID_BASE, OrderId::MAX] {
            assert!(next_api_order_id_after_commands(1, &[command_with_id(order_id)]).is_err());
        }

        let scenario = spot_scenario("invalid-recovery-id");
        let mut rooms = RoomManager::new();
        rooms.create_room(scenario).unwrap();
        let execution = rooms
            .apply("invalid-recovery-id", command_with_id(1))
            .unwrap();
        let mut record =
            JournalExecution::submitted("api".to_string(), 10, command_with_id(1), execution);
        let mut system_record = record.clone();
        system_record.participant_id = None;
        system_record.command = Command::NewOrder(NewOrder {
            order_id: SYSTEM_LIQUIDATION_ORDER_ID_BASE,
            account_id: 10,
            side: Side::Sell,
            kind: OrderKind::Market,
            qty: 1,
            reduce_only: true,
        });
        let system_recovery = JournalRecovery {
            rooms: Vec::new(),
            executions: vec![system_record],
            snapshots: Vec::new(),
        };
        assert_eq!(next_order_id_from_recovery(&system_recovery).unwrap(), 1);

        record.command = command_with_id(OrderId::MAX);
        let recovery = JournalRecovery {
            rooms: Vec::new(),
            executions: vec![record],
            snapshots: Vec::new(),
        };
        assert!(next_order_id_from_recovery(&recovery).is_err());
    }

    #[test]
    fn recovery_skips_atomic_system_liquidation_records() {
        let room_id = "recovered-liquidation-room";
        let scenario = perp_liquidation_scenario(room_id);
        let mut rooms = RoomManager::new();
        rooms.create_room(scenario.clone()).unwrap();

        let commands = [
            Command::NewOrder(NewOrder {
                order_id: 1,
                account_id: 30,
                side: Side::Buy,
                kind: OrderKind::Limit { price_tick: 80 },
                qty: 10,
                reduce_only: false,
            }),
            Command::NewOrder(NewOrder {
                order_id: 2,
                account_id: 10,
                side: Side::Sell,
                kind: OrderKind::Limit { price_tick: 100 },
                qty: 10,
                reduce_only: false,
            }),
            Command::NewOrder(NewOrder {
                order_id: 3,
                account_id: 20,
                side: Side::Buy,
                kind: OrderKind::Market,
                qty: 10,
                reduce_only: false,
            }),
        ];
        let participants = [("bidder", 30), ("seller", 10), ("distressed", 20)];
        let mut records = Vec::new();
        for (command, (participant_id, account_id)) in commands.into_iter().zip(participants) {
            let execution = rooms.apply(room_id, command.clone()).unwrap();
            records.push(JournalExecution::submitted(
                participant_id.to_string(),
                account_id,
                command,
                execution,
            ));
        }

        let history = rooms.execution_history(room_id).unwrap();
        assert_eq!(history.len(), 4);
        let liquidation = history.last().unwrap().clone();
        let liquidation_command = command_from_actor_execution(&liquidation).unwrap();
        assert!(is_system_liquidation_command(&liquidation_command));
        assert!(matches!(
            liquidation_command,
            Command::NewOrder(NewOrder {
                kind: OrderKind::FillOrKill { price_tick: None },
                ..
            })
        ));
        records.push(JournalExecution::system(liquidation_command, liquidation));
        for record in &mut records {
            record.execution.instrument_id = None;
        }

        let recovery = JournalRecovery {
            rooms: vec![journal::JournalRoom {
                room_id: room_id.to_string(),
                scenario,
                status: MarketStatus::Running,
            }],
            executions: records,
            snapshots: Vec::new(),
        };
        assert_eq!(next_order_id_from_recovery(&recovery).unwrap(), 4);

        let recovered = recover_rooms(&recovery).unwrap();
        assert_eq!(recovered.execution_history(room_id).unwrap().len(), 4);
        let AccountSnapshots::Perp(accounts) = recovered
            .account_snapshots_for(room_id, "V-BTC-PERP")
            .unwrap()
        else {
            panic!("expected perp account snapshots");
        };
        let distressed = accounts
            .iter()
            .find(|account| account.account_id == 20)
            .unwrap();
        assert_eq!(distressed.position_qty, 0);
    }

    #[tokio::test]
    async fn creates_room_and_accepts_order_over_http_shape() {
        let app = new_app();
        let scenario = serde_json::to_string(&spot_scenario("room-1")).unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let order = serde_json::json!({
            "participant_id": "human-1",
            "account_id": 20,
            "action": {
                "PlaceLimit": {
                    "side": "Buy",
                    "price_tick": 100,
                    "qty": 2
                }
            }
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/room-1/orders")
                    .header("content-type", "application/json")
                    .body(Body::from(order.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn api_order_ids_start_after_seed_order_ids() {
        let app = new_app();
        let scenario = serde_json::to_string(&seeded_spot_scenario("seed-id-room")).unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let order = serde_json::json!({
            "participant_id": "human-1",
            "account_id": 20,
            "action": {
                "PlaceLimit": {
                    "side": "Buy",
                    "price_tick": 99,
                    "qty": 1
                }
            }
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/seed-id-room/orders")
                    .header("content-type", "application/json")
                    .body(Body::from(order.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let order: OrderResponse = serde_json::from_slice(&body).unwrap();
        assert!(order.events.iter().any(|event| matches!(
            event,
            EventSummary::OrderAccepted {
                order_id: 10_001,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn account_cannot_cancel_or_amend_another_accounts_order() {
        let app = new_app();
        let scenario = serde_json::to_string(&spot_scenario("order-owner-room")).unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let owner_order = serde_json::json!({
            "participant_id": "owner-10",
            "account_id": 10,
            "action": {"PlaceLimit": {"side": "Sell", "price_tick": 105, "qty": 2}}
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/order-owner-room/orders")
                    .header("content-type", "application/json")
                    .body(Body::from(owner_order.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let order: OrderResponse = serde_json::from_slice(&body).unwrap();
        let order_id = order
            .events
            .iter()
            .find_map(|event| match event {
                EventSummary::OrderAccepted { order_id, .. } => Some(*order_id),
                _ => None,
            })
            .unwrap();

        for action in [
            serde_json::json!({"Cancel": {"order_id": order_id}}),
            serde_json::json!({"Amend": {
                "order_id": order_id,
                "price_tick": 104,
                "qty": 1
            }}),
        ] {
            let request = serde_json::json!({
                "participant_id": "attacker-20",
                "account_id": 20,
                "action": action
            });
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/rooms/order-owner-room/orders")
                        .header("content-type", "application/json")
                        .body(Body::from(request.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/order-owner-room/book")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let book: BookSnapshot = serde_json::from_slice(&body).unwrap();
        assert_eq!(book.asks[0].qty, 2);
    }

    #[tokio::test]
    async fn create_room_can_configure_venue_price_limits() {
        let app = new_app();
        let mut scenario = spot_scenario("venue-rules-room");
        scenario.venue_rules = exchange_core::VenueRuleConfig {
            price_limits: vec![exchange_core::PriceLimitRuleConfig {
                instrument_id: "V-BTC-SPOT".to_string(),
                reference_price_tick: 100,
                limit_up_ppm: 100_000,
                limit_down_ppm: 100_000,
            }],
            ..exchange_core::VenueRuleConfig::default()
        };
        let scenario = serde_json::to_string(&scenario).unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let order = serde_json::json!({
            "participant_id": "human-1",
            "account_id": 20,
            "action": {
                "PlaceLimit": {
                    "side": "Buy",
                    "price_tick": 111,
                    "qty": 1
                }
            }
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/venue-rules-room/orders")
                    .header("content-type", "application/json")
                    .body(Body::from(order.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let order_response: OrderResponse = serde_json::from_slice(&body).unwrap();
        assert!(!order_response.accepted);
        assert!(
            order_response
                .reject_reason
                .as_deref()
                .unwrap_or_default()
                .contains("PriceLimitExceeded")
        );
    }

    #[tokio::test]
    async fn venue_accounts_endpoint_reports_cross_asset_balances() {
        let app = new_app();
        let scenario = serde_json::to_string(&seeded_spot_scenario("venue-room")).unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let order = serde_json::json!({
            "participant_id": "human-1",
            "account_id": 20,
            "action": {
                "PlaceLimit": {
                    "side": "Buy",
                    "price_tick": 104,
                    "qty": 2
                }
            }
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/venue-room/orders")
                    .header("content-type", "application/json")
                    .body(Body::from(order.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/venue-room/venue/accounts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let accounts: RoomVenueAccountsResponse = serde_json::from_slice(&body).unwrap();

        let balance_total = |account_id, asset_id: &str| {
            accounts
                .accounts
                .iter()
                .find(|account| account.account_id == account_id)
                .and_then(|account| {
                    account
                        .balances
                        .iter()
                        .find(|balance| balance.asset_id == asset_id)
                })
                .map(|balance| balance.total)
                .expect("venue balance should exist")
        };
        assert_eq!(balance_total(10, "V"), 8);
        assert_eq!(balance_total(10, "BTC"), 1_208);
        assert_eq!(balance_total(20, "V"), 2);
        assert_eq!(balance_total(20, "BTC"), 792);
    }

    #[tokio::test]
    async fn transfer_routes_apply_deposit_and_withdrawal_after_clock_delay() {
        let app = new_app();
        let mut scenario = spot_scenario("transfer-room");
        scenario.venue_rules = exchange_core::VenueRuleConfig {
            transfers: exchange_core::TransferPolicyConfig {
                deposit_delay_steps: 2,
                withdrawal_delay_steps: 1,
            },
            ..exchange_core::VenueRuleConfig::default()
        };
        let mut wallet_balances = BTreeMap::new();
        wallet_balances.insert("BTC".to_string(), 500);
        scenario.initial_portfolios = vec![ScenarioPortfolio {
            account_id: 20,
            balances: wallet_balances,
        }];
        let scenario = serde_json::to_string(&scenario).unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let transfer = serde_json::json!({
            "account_id": 20,
            "asset_id": "BTC",
            "amount": 250
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/transfer-room/transfers/deposit")
                    .header("content-type", "application/json")
                    .body(Body::from(transfer.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let transfer_response: TransferResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            transfer_response.transfer.status,
            exchange_core::VenueTransferStatus::Pending
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/transfer-room/portfolio")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let portfolios: RoomPortfoliosResponse = serde_json::from_slice(&body).unwrap();
        let btc_wallet = portfolios
            .accounts
            .iter()
            .find(|account| account.account_id == 20)
            .and_then(|account| {
                account
                    .balances
                    .iter()
                    .find(|balance| balance.asset_id == "BTC")
            })
            .unwrap();
        assert_eq!(btc_wallet.total, 500);
        assert_eq!(btc_wallet.reserved, 250);

        let advance = serde_json::json!({ "steps": 1 });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/transfer-room/clock/advance")
                    .header("content-type", "application/json")
                    .body(Body::from(advance.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let advance_response: AdvanceClockResponse = serde_json::from_slice(&body).unwrap();
        assert!(advance_response.completed_transfers.is_empty());

        let advance = serde_json::json!({ "steps": 1 });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/transfer-room/clock/advance")
                    .header("content-type", "application/json")
                    .body(Body::from(advance.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let advance_response: AdvanceClockResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(advance_response.completed_transfers.len(), 1);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/transfer-room/portfolio")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let portfolios: RoomPortfoliosResponse = serde_json::from_slice(&body).unwrap();
        let btc_wallet = portfolios
            .accounts
            .iter()
            .find(|account| account.account_id == 20)
            .and_then(|account| {
                account
                    .balances
                    .iter()
                    .find(|balance| balance.asset_id == "BTC")
            })
            .unwrap();
        assert_eq!(btc_wallet.total, 250);
        assert_eq!(btc_wallet.reserved, 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/transfer-room/transfers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let transfers: RoomTransfersResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(transfers.transfers.len(), 1);
        assert_eq!(
            transfers.transfers[0].status,
            exchange_core::VenueTransferStatus::Completed
        );

        let transfer = serde_json::json!({
            "account_id": 20,
            "asset_id": "BTC",
            "amount": 100
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/transfer-room/transfers/withdraw")
                    .header("content-type", "application/json")
                    .body(Body::from(transfer.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/transfer-room/venue/accounts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let accounts: RoomVenueAccountsResponse = serde_json::from_slice(&body).unwrap();
        let btc = accounts
            .accounts
            .iter()
            .find(|account| account.account_id == 20)
            .and_then(|account| {
                account
                    .balances
                    .iter()
                    .find(|balance| balance.asset_id == "BTC")
            })
            .unwrap();
        assert_eq!(btc.total, 1_250);
        assert_eq!(btc.reserved, 100);

        let advance = serde_json::json!({ "steps": 1 });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/transfer-room/clock/advance")
                    .header("content-type", "application/json")
                    .body(Body::from(advance.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/transfer-room/venue/accounts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let accounts: RoomVenueAccountsResponse = serde_json::from_slice(&body).unwrap();
        let btc = accounts
            .accounts
            .iter()
            .find(|account| account.account_id == 20)
            .and_then(|account| {
                account
                    .balances
                    .iter()
                    .find(|balance| balance.asset_id == "BTC")
            })
            .unwrap();
        assert_eq!(btc.total, 1_150);
        assert_eq!(btc.reserved, 0);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/transfer-room/portfolio")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let portfolios: RoomPortfoliosResponse = serde_json::from_slice(&body).unwrap();
        let btc_wallet = portfolios
            .accounts
            .iter()
            .find(|account| account.account_id == 20)
            .and_then(|account| {
                account
                    .balances
                    .iter()
                    .find(|balance| balance.asset_id == "BTC")
            })
            .unwrap();
        assert_eq!(btc_wallet.total, 350);
        assert_eq!(btc_wallet.reserved, 0);
    }

    #[tokio::test]
    async fn instrument_routes_target_non_primary_market() {
        let app = new_app();
        let scenario = serde_json::to_string(&spot_perp_scenario("multi-room")).unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/multi-room/instruments/V-BTC-PERP/view")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let perp_view: MarketView = serde_json::from_slice(&body).unwrap();
        assert_eq!(perp_view.instrument_id, "V-BTC-PERP");
        assert_eq!(perp_view.book.asks[0].price_tick, 100);

        let order = serde_json::json!({
            "participant_id": "human-1",
            "account_id": 10,
            "action": {
                "PlaceLimit": {
                    "side": "Buy",
                    "price_tick": 100,
                    "qty": 2
                }
            }
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/multi-room/instruments/V-BTC-PERP/orders")
                    .header("content-type", "application/json")
                    .body(Body::from(order.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let order_response: OrderResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(order_response.instrument_id.as_deref(), Some("V-BTC-PERP"));
        assert!(order_response.accepted);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/multi-room/view")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let spot_view: MarketView = serde_json::from_slice(&body).unwrap();
        assert_eq!(spot_view.instrument_id, "V-BTC-SPOT");
        assert!(spot_view.book.bids.is_empty());
        assert!(spot_view.book.asks.is_empty());
    }

    #[tokio::test]
    async fn room_events_include_submitted_orders() {
        let app = new_app();
        let scenario = serde_json::to_string(&spot_scenario("events-room")).unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let order = serde_json::json!({
            "participant_id": "human-1",
            "account_id": 20,
            "action": {
                "PlaceLimit": {
                    "side": "Buy",
                    "price_tick": 100,
                    "qty": 2
                }
            }
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/events-room/orders")
                    .header("content-type", "application/json")
                    .body(Body::from(order.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/events-room/events?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let events: RoomEventsResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(events.room_id, "events-room");
        assert_eq!(events.executions.len(), 1);
        assert!(events.executions[0].accepted);
        assert!(
            events.executions[0]
                .events
                .iter()
                .any(|event| matches!(event, EventSummary::OrderRested { .. }))
        );
    }

    #[tokio::test]
    async fn room_events_include_auto_liquidation_execution() {
        let app = new_app();
        let scenario =
            serde_json::to_string(&perp_liquidation_scenario("auto-liquidation-room")).unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let orders = [
            serde_json::json!({
                "participant_id": "bidder",
                "account_id": 30,
                "action": {
                    "PlaceLimit": {
                        "side": "Buy",
                        "price_tick": 80,
                        "qty": 10
                    }
                }
            }),
            serde_json::json!({
                "participant_id": "seller",
                "account_id": 10,
                "action": {
                    "PlaceLimit": {
                        "side": "Sell",
                        "price_tick": 100,
                        "qty": 10
                    }
                }
            }),
            serde_json::json!({
                "participant_id": "distressed",
                "account_id": 20,
                "action": {
                    "PlaceMarket": {
                        "side": "Buy",
                        "qty": 10
                    }
                }
            }),
        ];

        for order in orders {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/rooms/auto-liquidation-room/orders")
                        .header("content-type", "application/json")
                        .body(Body::from(order.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/auto-liquidation-room/events?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let events: RoomEventsResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(events.executions.len(), 4);
        let liquidation = events.executions.last().unwrap();
        assert!(liquidation.accepted);
        assert!(liquidation.clearing_events.iter().any(
            |event| matches!(event, ClearingEventSummary::PerpMarginStatusChanged {
                    account_id: 20,
                    new_status,
                    ..
                } if new_status == "flat")
        ));
        assert!(liquidation.clearing_events.iter().any(|event| matches!(
            event,
            ClearingEventSummary::PerpLiquidationSettled {
                account_id: 20,
                liquidation_notional: 800,
                liquidation_fee: 8,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn mark_price_update_can_trigger_auto_liquidation() {
        let app = new_app();
        let scenario = serde_json::to_string(&perp_liquidation_scenario_with_mark(
            "mark-liquidation-room",
            100,
        ))
        .unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let orders = [
            serde_json::json!({
                "participant_id": "bidder",
                "account_id": 30,
                "action": {
                    "PlaceLimit": {
                        "side": "Buy",
                        "price_tick": 80,
                        "qty": 10
                    }
                }
            }),
            serde_json::json!({
                "participant_id": "seller",
                "account_id": 10,
                "action": {
                    "PlaceLimit": {
                        "side": "Sell",
                        "price_tick": 100,
                        "qty": 10
                    }
                }
            }),
            serde_json::json!({
                "participant_id": "buyer",
                "account_id": 20,
                "action": {
                    "PlaceMarket": {
                        "side": "Buy",
                        "qty": 10
                    }
                }
            }),
        ];

        for order in orders {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/rooms/mark-liquidation-room/orders")
                        .header("content-type", "application/json")
                        .body(Body::from(order.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/mark-liquidation-room/instruments/V-BTC-PERP/mark-price")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"price_tick":80}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let mark_execution: RoomExecutionSummary = serde_json::from_slice(&body).unwrap();
        assert!(mark_execution.clearing_events.iter().any(
            |event| matches!(event, ClearingEventSummary::PerpMarginStatusChanged {
                    account_id: 20,
                    new_status,
                    ..
                } if new_status == "liquidatable")
        ));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/mark-liquidation-room/events?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let events: RoomEventsResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(events.executions.len(), 5);
        assert!(
            events
                .executions
                .last()
                .unwrap()
                .clearing_events
                .iter()
                .any(
                    |event| matches!(event, ClearingEventSummary::PerpMarginStatusChanged {
                    account_id: 20,
                    new_status,
                    ..
                } if new_status == "flat")
                )
        );
        assert!(
            events
                .executions
                .last()
                .unwrap()
                .clearing_events
                .iter()
                .any(|event| matches!(
                    event,
                    ClearingEventSummary::PerpLiquidationSettled {
                        account_id: 20,
                        liquidation_notional: 800,
                        liquidation_fee: 8,
                        ..
                    }
                ))
        );
    }

    #[tokio::test]
    async fn projection_query_routes_are_available_without_postgres() {
        let app = new_app();
        let scenario = serde_json::to_string(&spot_scenario("query-room")).unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        for uri in [
            "/rooms/query-room/orders",
            "/rooms/query-room/orders?instrument_id=V-BTC-SPOT",
            "/rooms/query-room/trades",
            "/rooms/query-room/ticks",
            "/rooms/query-room/ledger",
            "/rooms/query-room/positions",
            "/rooms/query-room/instruments/V-BTC-SPOT/orders",
            "/rooms/query-room/instruments/V-BTC-SPOT/trades",
            "/rooms/query-room/instruments/V-BTC-SPOT/ticks",
            "/rooms/query-room/instruments/V-BTC-SPOT/ledger",
            "/rooms/query-room/instruments/V-BTC-SPOT/positions",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::GET)
                        .uri(uri)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn user_header_scopes_room_visibility_and_account_actions() {
        let app = new_app();
        let scenario = serde_json::to_string(&spot_scenario("tenant-room")).unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .header(USER_ID_HEADER, "alice")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms")
                    .header(USER_ID_HEADER, "bob")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let rooms: ListRoomsResponse = serde_json::from_slice(&body).unwrap();
        assert!(rooms.rooms.is_empty());

        let order = serde_json::json!({
            "participant_id": "bob-human",
            "account_id": 20,
            "action": {
                "PlaceLimit": {
                    "side": "Buy",
                    "price_tick": 100,
                    "qty": 2
                }
            }
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/tenant-room/orders")
                    .header("content-type", "application/json")
                    .header(USER_ID_HEADER, "bob")
                    .body(Body::from(order.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn ordinary_room_members_cannot_access_admin_surfaces() {
        #[derive(Default)]
        struct RoleJournal {
            roles: BTreeMap<(String, String), String>,
            account_owners: BTreeMap<(String, AccountId), String>,
        }

        impl JournalStore for RoleJournal {
            fn load_recovery(&mut self) -> Result<JournalRecovery, JournalError> {
                Ok(JournalRecovery::default())
            }

            fn create_room(
                &mut self,
                owner_user_id: &str,
                _scenario: &ScenarioConfig,
                bootstrap: &RoomBootstrap,
                account_ids: &[AccountId],
                _seed_records: &[JournalExecution],
                _initial_snapshot: Option<&JournalSnapshot>,
            ) -> Result<(), JournalError> {
                let room_id = bootstrap.room_id.clone();
                self.roles.insert(
                    (room_id.clone(), owner_user_id.to_string()),
                    "owner".to_string(),
                );
                self.roles.insert(
                    (room_id.clone(), "member".to_string()),
                    "member".to_string(),
                );
                self.roles
                    .insert((room_id.clone(), "admin".to_string()), "admin".to_string());
                for account_id in account_ids {
                    self.account_owners
                        .insert((room_id.clone(), *account_id), owner_user_id.to_string());
                }
                Ok(())
            }

            fn append_executions(
                &mut self,
                _records: &[JournalExecution],
                _snapshot: Option<&JournalSnapshot>,
            ) -> Result<(), JournalError> {
                Ok(())
            }

            fn update_room_status(
                &mut self,
                _room_id: &str,
                _status: MarketStatus,
            ) -> Result<(), JournalError> {
                Ok(())
            }

            fn user_can_access_room(
                &mut self,
                user_id: &str,
                room_id: &str,
            ) -> Result<bool, JournalError> {
                Ok(self
                    .roles
                    .contains_key(&(room_id.to_string(), user_id.to_string())))
            }

            fn user_can_administer_room(
                &mut self,
                user_id: &str,
                room_id: &str,
            ) -> Result<bool, JournalError> {
                Ok(self
                    .roles
                    .get(&(room_id.to_string(), user_id.to_string()))
                    .is_some_and(|role| role == "owner" || role == "admin"))
            }

            fn user_can_access_account(
                &mut self,
                user_id: &str,
                room_id: &str,
                account_id: AccountId,
            ) -> Result<bool, JournalError> {
                if self.user_can_administer_room(user_id, room_id)? {
                    return Ok(true);
                }
                Ok(self
                    .account_owners
                    .get(&(room_id.to_string(), account_id))
                    .is_some_and(|owner| owner == user_id))
            }
        }

        let app = new_app_with_journal("http://127.0.0.1:57305", Box::new(RoleJournal::default()));
        let scenario = serde_json::to_string(&spot_scenario("role-room")).unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .header(USER_ID_HEADER, "owner")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/role-room/book")
                    .header(USER_ID_HEADER, "member")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        for path in [
            "/rooms/role-room/events",
            "/rooms/role-room/view",
            "/rooms/role-room/accounts",
            "/rooms/role-room/venue/accounts",
            "/rooms/role-room/venue/accounts/by-venue",
            "/rooms/role-room/portfolio",
            "/rooms/role-room/assets/ledger",
            "/rooms/role-room/net-worth",
            "/rooms/role-room/transfers",
            "/rooms/role-room/orders",
            "/rooms/role-room/trades",
            "/rooms/role-room/ledger",
            "/rooms/role-room/positions",
            "/rooms/role-room/agents",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::GET)
                        .uri(path)
                        .header(USER_ID_HEADER, "member")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "path {path}");
        }

        let mutations = [
            (
                "/rooms/role-room/clock/advance",
                serde_json::json!({"steps": 1}),
            ),
            ("/rooms/role-room/pause", serde_json::json!(null)),
            ("/rooms/role-room/resume", serde_json::json!(null)),
            (
                "/rooms/role-room/instruments/V-BTC-SPOT/mark-price",
                serde_json::json!({"price_tick": 90}),
            ),
            (
                "/rooms/role-room/agents",
                serde_json::json!({"agents": [], "interval_ms": 10}),
            ),
            ("/rooms/role-room/agents/stop", serde_json::json!(null)),
        ];
        for (path, body) in mutations {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(path)
                        .header("content-type", "application/json")
                        .header(USER_ID_HEADER, "member")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "path {path}");
        }

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/role-room/accounts")
                    .header(USER_ID_HEADER, "admin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn failed_journal_append_does_not_commit_order_state() {
        struct FailingAppendJournal;

        impl JournalStore for FailingAppendJournal {
            fn load_recovery(&mut self) -> Result<JournalRecovery, JournalError> {
                Ok(JournalRecovery::default())
            }

            fn create_room(
                &mut self,
                _owner_user_id: &str,
                _scenario: &ScenarioConfig,
                _bootstrap: &RoomBootstrap,
                _account_ids: &[AccountId],
                _seed_records: &[JournalExecution],
                _initial_snapshot: Option<&JournalSnapshot>,
            ) -> Result<(), JournalError> {
                Ok(())
            }

            fn append_executions(
                &mut self,
                _records: &[JournalExecution],
                _snapshot: Option<&JournalSnapshot>,
            ) -> Result<(), JournalError> {
                Err(JournalError::CountOutOfRange(0))
            }

            fn user_can_administer_room(
                &mut self,
                _user_id: &str,
                _room_id: &str,
            ) -> Result<bool, JournalError> {
                Ok(true)
            }

            fn update_room_status(
                &mut self,
                _room_id: &str,
                _status: MarketStatus,
            ) -> Result<(), JournalError> {
                Ok(())
            }
        }

        let app = new_app_with_journal("http://127.0.0.1:57305", Box::new(FailingAppendJournal));
        let scenario = serde_json::to_string(&spot_scenario("journal-failure")).unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let order = serde_json::json!({
            "participant_id": "human-1",
            "account_id": 20,
            "action": {
                "PlaceLimit": {
                    "side": "Buy",
                    "price_tick": 100,
                    "qty": 2
                }
            }
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/journal-failure/orders")
                    .header("content-type", "application/json")
                    .body(Body::from(order.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/journal-failure/view")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let view: MarketView = serde_json::from_slice(&body).unwrap();
        assert!(view.book.bids.is_empty());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/journal-failure/events?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let events: RoomEventsResponse = serde_json::from_slice(&body).unwrap();
        assert!(events.executions.is_empty());
    }

    #[tokio::test]
    async fn failed_multi_execution_batch_does_not_commit_any_timeline_entry() {
        struct RejectMultiExecutionJournal;

        impl JournalStore for RejectMultiExecutionJournal {
            fn load_recovery(&mut self) -> Result<JournalRecovery, JournalError> {
                Ok(JournalRecovery::default())
            }

            fn create_room(
                &mut self,
                _owner_user_id: &str,
                _scenario: &ScenarioConfig,
                _bootstrap: &RoomBootstrap,
                _account_ids: &[AccountId],
                _seed_records: &[JournalExecution],
                _initial_snapshot: Option<&JournalSnapshot>,
            ) -> Result<(), JournalError> {
                Ok(())
            }

            fn append_executions(
                &mut self,
                records: &[JournalExecution],
                _snapshot: Option<&JournalSnapshot>,
            ) -> Result<(), JournalError> {
                if records.len() > 1 {
                    return Err(JournalError::CountOutOfRange(records.len()));
                }
                Ok(())
            }

            fn update_room_status(
                &mut self,
                _room_id: &str,
                _status: MarketStatus,
            ) -> Result<(), JournalError> {
                Ok(())
            }

            fn user_can_administer_room(
                &mut self,
                _user_id: &str,
                _room_id: &str,
            ) -> Result<bool, JournalError> {
                Ok(true)
            }
        }

        let app = new_app_with_journal(
            "http://127.0.0.1:57305",
            Box::new(RejectMultiExecutionJournal),
        );
        let scenario =
            serde_json::to_string(&perp_liquidation_scenario("batch-failure-room")).unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let orders = [
            serde_json::json!({
                "participant_id": "bidder",
                "account_id": 30,
                "action": {"PlaceLimit": {"side": "Buy", "price_tick": 80, "qty": 10}}
            }),
            serde_json::json!({
                "participant_id": "seller",
                "account_id": 10,
                "action": {"PlaceLimit": {"side": "Sell", "price_tick": 100, "qty": 10}}
            }),
            serde_json::json!({
                "participant_id": "distressed",
                "account_id": 20,
                "action": {"PlaceMarket": {"side": "Buy", "qty": 10}}
            }),
        ];

        for (index, order) in orders.into_iter().enumerate() {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/rooms/batch-failure-room/orders")
                        .header("content-type", "application/json")
                        .body(Body::from(order.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            let expected = if index < 2 {
                StatusCode::OK
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            assert_eq!(response.status(), expected);
        }

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/batch-failure-room/events?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let events: RoomEventsResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(events.executions.len(), 2);
    }

    #[tokio::test]
    async fn recovery_replays_rooms_and_restores_next_order_id() {
        #[derive(Clone)]
        struct RecoveryJournal {
            recovery: JournalRecovery,
        }

        impl JournalStore for RecoveryJournal {
            fn load_recovery(&mut self) -> Result<JournalRecovery, JournalError> {
                Ok(self.recovery.clone())
            }

            fn create_room(
                &mut self,
                _owner_user_id: &str,
                _scenario: &ScenarioConfig,
                _bootstrap: &RoomBootstrap,
                _account_ids: &[AccountId],
                _seed_records: &[JournalExecution],
                _initial_snapshot: Option<&JournalSnapshot>,
            ) -> Result<(), JournalError> {
                Ok(())
            }

            fn append_executions(
                &mut self,
                _records: &[JournalExecution],
                _snapshot: Option<&JournalSnapshot>,
            ) -> Result<(), JournalError> {
                Ok(())
            }

            fn user_can_administer_room(
                &mut self,
                _user_id: &str,
                _room_id: &str,
            ) -> Result<bool, JournalError> {
                Ok(true)
            }

            fn update_room_status(
                &mut self,
                _room_id: &str,
                _status: MarketStatus,
            ) -> Result<(), JournalError> {
                Ok(())
            }
        }

        let scenario = seeded_spot_scenario("recovered-room");
        let mut rooms = RoomManager::new();
        let bootstrap = rooms.create_room(scenario.clone()).unwrap();
        let seed_records = scenario
            .seed_commands()
            .into_iter()
            .zip(bootstrap.seed_executions.iter().cloned())
            .map(|(command, execution)| JournalExecution::seed(command, execution))
            .collect::<Vec<_>>();
        let mut gateway = OrderGateway::new(&mut rooms, 1);
        let submitted = gateway
            .submit_action(GatewayRequest {
                participant_id: "human-1".to_string(),
                room_id: "recovered-room".to_string(),
                instrument_id: None,
                account_id: 20,
                action: OrderAction::PlaceLimit {
                    side: Side::Buy,
                    price_tick: 104,
                    qty: 2,
                },
            })
            .unwrap();
        let submitted_record = JournalExecution::submitted(
            submitted.participant_id,
            submitted.account_id,
            submitted.command,
            submitted.execution,
        );
        let snapshot = JournalSnapshot {
            room_id: "recovered-room".to_string(),
            command_seq: submitted_record.command_seq,
            actor: rooms.simulation_room("recovered-room").unwrap().clone(),
        };

        let mut executions = seed_records;
        executions.push(submitted_record);
        let recovery = JournalRecovery {
            rooms: vec![journal::JournalRoom {
                room_id: "recovered-room".to_string(),
                scenario,
                status: MarketStatus::Running,
            }],
            executions,
            snapshots: vec![snapshot],
        };
        let app = new_app_recovering_with_journal(
            "http://127.0.0.1:57305",
            Box::new(RecoveryJournal { recovery }),
        )
        .unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/recovered-room/view")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let view: MarketView = serde_json::from_slice(&body).unwrap();
        assert_eq!(view.book.asks[0].qty, 6);

        let order = serde_json::json!({
            "participant_id": "human-1",
            "account_id": 20,
            "action": {
                "PlaceLimit": {
                    "side": "Buy",
                    "price_tick": 99,
                    "qty": 1
                }
            }
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/recovered-room/orders")
                    .header("content-type", "application/json")
                    .body(Body::from(order.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let order: OrderResponse = serde_json::from_slice(&body).unwrap();
        assert!(order.events.iter().any(|event| matches!(
            event,
            EventSummary::OrderAccepted {
                order_id: 10_001,
                ..
            }
        )));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/recovered-room/events?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let events: RoomEventsResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(events.executions.len(), 3);
        assert_eq!(events.executions[0].command_seq, 0);
        assert_eq!(events.executions[1].command_seq, 1);
    }

    #[tokio::test]
    async fn recovered_clock_snapshot_keeps_last_command_sequence_across_second_restart() {
        #[derive(Clone)]
        struct SharedRecoveryJournal {
            recovery: Arc<Mutex<JournalRecovery>>,
        }

        impl SharedRecoveryJournal {
            fn store_snapshot(recovery: &mut JournalRecovery, snapshot: &JournalSnapshot) {
                if let Some(existing) = recovery
                    .snapshots
                    .iter_mut()
                    .find(|existing| existing.room_id == snapshot.room_id)
                {
                    if snapshot.command_seq >= existing.command_seq {
                        *existing = snapshot.clone();
                    }
                } else {
                    recovery.snapshots.push(snapshot.clone());
                }
            }
        }

        impl JournalStore for SharedRecoveryJournal {
            fn load_recovery(&mut self) -> Result<JournalRecovery, JournalError> {
                self.recovery
                    .lock()
                    .map(|recovery| recovery.clone())
                    .map_err(|_| JournalError::Recovery("shared recovery lock poisoned".into()))
            }

            fn create_room(
                &mut self,
                _owner_user_id: &str,
                _scenario: &ScenarioConfig,
                _bootstrap: &RoomBootstrap,
                _account_ids: &[AccountId],
                _seed_records: &[JournalExecution],
                _initial_snapshot: Option<&JournalSnapshot>,
            ) -> Result<(), JournalError> {
                Ok(())
            }

            fn append_executions(
                &mut self,
                records: &[JournalExecution],
                snapshot: Option<&JournalSnapshot>,
            ) -> Result<(), JournalError> {
                let mut recovery = self
                    .recovery
                    .lock()
                    .map_err(|_| JournalError::Recovery("shared recovery lock poisoned".into()))?;
                recovery.executions.extend_from_slice(records);
                if let Some(snapshot) = snapshot {
                    Self::store_snapshot(&mut recovery, snapshot);
                }
                Ok(())
            }

            fn append_snapshot(&mut self, snapshot: &JournalSnapshot) -> Result<(), JournalError> {
                let mut recovery = self
                    .recovery
                    .lock()
                    .map_err(|_| JournalError::Recovery("shared recovery lock poisoned".into()))?;
                Self::store_snapshot(&mut recovery, snapshot);
                Ok(())
            }

            fn update_room_status(
                &mut self,
                _room_id: &str,
                _status: MarketStatus,
            ) -> Result<(), JournalError> {
                Ok(())
            }

            fn user_can_administer_room(
                &mut self,
                _user_id: &str,
                _room_id: &str,
            ) -> Result<bool, JournalError> {
                Ok(true)
            }
        }

        let scenario = seeded_spot_scenario("snapshot-seq-room");
        let mut rooms = RoomManager::new();
        let bootstrap = rooms.create_room(scenario.clone()).unwrap();
        let mut executions = scenario
            .seed_commands()
            .into_iter()
            .zip(bootstrap.seed_executions.iter().cloned())
            .map(|(command, execution)| JournalExecution::seed(command, execution))
            .collect::<Vec<_>>();
        let mut gateway = OrderGateway::new(&mut rooms, 10_001);
        let submitted = gateway
            .submit_action(GatewayRequest {
                participant_id: "human-1".to_string(),
                room_id: "snapshot-seq-room".to_string(),
                instrument_id: None,
                account_id: 20,
                action: OrderAction::PlaceLimit {
                    side: Side::Buy,
                    price_tick: 99,
                    qty: 1,
                },
            })
            .unwrap();
        let submitted_record = JournalExecution::submitted(
            submitted.participant_id,
            submitted.account_id,
            submitted.command,
            submitted.execution,
        );
        let snapshot = JournalSnapshot {
            room_id: "snapshot-seq-room".to_string(),
            command_seq: submitted_record.command_seq,
            actor: rooms.simulation_room("snapshot-seq-room").unwrap().clone(),
        };
        assert_eq!(snapshot.command_seq, 1);
        executions.push(submitted_record);

        let recovery = Arc::new(Mutex::new(JournalRecovery {
            rooms: vec![journal::JournalRoom {
                room_id: "snapshot-seq-room".to_string(),
                scenario,
                status: MarketStatus::Running,
            }],
            executions,
            snapshots: vec![snapshot],
        }));

        let first_app = new_app_recovering_with_journal(
            "http://127.0.0.1:57305",
            Box::new(SharedRecoveryJournal {
                recovery: Arc::clone(&recovery),
            }),
        )
        .unwrap();
        let response = first_app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/snapshot-seq-room/clock/advance")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"steps":7}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(recovery.lock().unwrap().snapshots[0].command_seq, 1);

        let second_app = new_app_recovering_with_journal(
            "http://127.0.0.1:57305",
            Box::new(SharedRecoveryJournal { recovery }),
        )
        .unwrap();
        let response = second_app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/rooms/snapshot-seq-room/clock")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let clock: RoomClockResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(clock.clock.step(), 7);
    }

    #[tokio::test]
    async fn postgres_journal_persists_and_recovers_room_when_configured() {
        let Ok(database_url) = std::env::var("MARKETFORGE_TEST_DATABASE_URL") else {
            return;
        };
        let room_id = format!(
            "pg-recovery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let mut scenario = seeded_spot_scenario(&room_id);
        scenario.initial_portfolios = vec![ScenarioPortfolio {
            account_id: 20,
            balances: BTreeMap::from([("BTC".to_string(), 25)]),
        }];

        cleanup_postgres_room(&database_url, &room_id);
        let app = new_app_recovering_with_journal(
            "http://127.0.0.1:57305",
            Box::new(PostgresJournalStore::connect_migrated(&database_url).unwrap()),
        )
        .unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&scenario).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let order = serde_json::json!({
            "participant_id": "human-pg",
            "account_id": 20,
            "action": {
                "PlaceLimit": {
                    "side": "Buy",
                    "price_tick": 104,
                    "qty": 2
                }
            }
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/rooms/{room_id}/orders"))
                    .header("content-type", "application/json")
                    .body(Body::from(order.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let submitted: OrderResponse = serde_json::from_slice(&body).unwrap();
        let submitted_order_id = submitted
            .events
            .iter()
            .find_map(|event| match event {
                EventSummary::OrderAccepted { order_id, .. } => Some(*order_id),
                _ => None,
            })
            .unwrap();
        let transfer = serde_json::json!({
            "account_id": 20,
            "asset_id": "BTC",
            "amount": 25
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/rooms/{room_id}/transfers/deposit"))
                    .header("content-type", "application/json")
                    .body(Body::from(transfer.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_postgres_trade_projection(&database_url, &room_id, submitted_order_id);
        assert_postgres_transfer_projection(&database_url, &room_id);

        let recovered = new_app_recovering_with_journal(
            "http://127.0.0.1:57305",
            Box::new(PostgresJournalStore::connect_migrated(&database_url).unwrap()),
        )
        .unwrap();
        let response = recovered
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/rooms/{room_id}/view"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let view: MarketView = serde_json::from_slice(&body).unwrap();
        assert_eq!(view.book.asks[0].qty, 6);

        let response = recovered
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/rooms/{room_id}/trades?account_id=20&limit=10"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let trades: RoomTradesResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(trades.trades.len(), 1);
        assert_eq!(trades.trades[0].taker_account_id, 20);

        cleanup_postgres_room(&database_url, &room_id);
    }

    fn assert_postgres_trade_projection(
        database_url: &str,
        room_id: &str,
        taker_order_id: OrderId,
    ) {
        let database_url = database_url.to_string();
        let room_id = room_id.to_string();
        std::thread::spawn(move || {
            let mut client = postgres::Client::connect(&database_url, postgres::NoTls).unwrap();
            let taker_order_id = i64::try_from(taker_order_id).unwrap();

            let migration_name: String = client
                .query_one(
                    "SELECT name FROM marketforge_schema_migrations WHERE version = 1",
                    &[],
                )
                .unwrap()
                .get(0);
            assert_eq!(migration_name, "initial_schema");
            let access_migration_name: String = client
                .query_one(
                    "SELECT name FROM marketforge_schema_migrations WHERE version = 2",
                    &[],
                )
                .unwrap()
                .get(0);
            assert_eq!(access_migration_name, "access_control");
            let instrument_migration_name: String = client
                .query_one(
                    "SELECT name FROM marketforge_schema_migrations WHERE version = 3",
                    &[],
                )
                .unwrap()
                .get(0);
            assert_eq!(instrument_migration_name, "instrument_projection_scope");
            let transfer_migration_name: String = client
                .query_one(
                    "SELECT name FROM marketforge_schema_migrations WHERE version = 4",
                    &[],
                )
                .unwrap()
                .get(0);
            assert_eq!(transfer_migration_name, "transfer_journal");

            let order_count: i64 = client
                .query_one(
                    "SELECT count(*) FROM marketforge_orders WHERE room_id = $1",
                    &[&room_id],
                )
                .unwrap()
                .get(0);
            assert_eq!(order_count, 2);

            let seed_order = client
                .query_one(
                    r#"
                    SELECT instrument_id, status, remaining_qty
                    FROM marketforge_orders
                    WHERE room_id = $1 AND order_id = 10000
                    "#,
                    &[&room_id],
                )
                .unwrap();
            let seed_instrument_id: String = seed_order.get("instrument_id");
            let seed_status: String = seed_order.get("status");
            let seed_remaining_qty: i64 = seed_order.get("remaining_qty");
            assert_eq!(seed_instrument_id, "V-BTC-SPOT");
            assert_eq!(seed_status, "partially_filled");
            assert_eq!(seed_remaining_qty, 6);

            let taker_order = client
                .query_one(
                    r#"
                    SELECT instrument_id, status, remaining_qty, account_id, participant_id
                    FROM marketforge_orders
                    WHERE room_id = $1 AND order_id = $2
                    "#,
                    &[&room_id, &taker_order_id],
                )
                .unwrap();
            let taker_instrument_id: String = taker_order.get("instrument_id");
            let taker_status: String = taker_order.get("status");
            let taker_remaining_qty: i64 = taker_order.get("remaining_qty");
            let taker_account_id: i64 = taker_order.get("account_id");
            let participant_id: Option<String> = taker_order.get("participant_id");
            assert_eq!(taker_instrument_id, "V-BTC-SPOT");
            assert_eq!(taker_status, "filled");
            assert_eq!(taker_remaining_qty, 0);
            assert_eq!(taker_account_id, 20);
            assert_eq!(participant_id.as_deref(), Some("human-pg"));

            let trade = client
                .query_one(
                    r#"
                    SELECT instrument_id, maker_account_id, taker_account_id, price_tick, qty, taker_side
                    FROM marketforge_trades
                    WHERE room_id = $1
                    "#,
                    &[&room_id],
                )
                .unwrap();
            let trade_instrument_id: String = trade.get("instrument_id");
            let maker_account_id: i64 = trade.get("maker_account_id");
            let taker_account_id: i64 = trade.get("taker_account_id");
            let price_tick: i64 = trade.get("price_tick");
            let qty: i64 = trade.get("qty");
            let taker_side: String = trade.get("taker_side");
            assert_eq!(trade_instrument_id, "V-BTC-SPOT");
            assert_eq!(maker_account_id, 10);
            assert_eq!(taker_account_id, 20);
            assert_eq!(price_tick, 104);
            assert_eq!(qty, 2);
            assert_eq!(taker_side, "buy");

            let tick_count: i64 = client
                .query_one(
                    "SELECT count(*) FROM marketforge_market_ticks WHERE room_id = $1",
                    &[&room_id],
                )
                .unwrap()
                .get(0);
            assert_eq!(tick_count, 1);

            let ledger_count: i64 = client
                .query_one(
                    "SELECT count(*) FROM marketforge_account_ledger WHERE room_id = $1",
                    &[&room_id],
                )
                .unwrap()
                .get(0);
            assert_eq!(ledger_count, 2);

            let buyer_ledger = client
                .query_one(
                    r#"
                    SELECT instrument_id, account_side, cash_delta, position_delta, cash_balance, position_qty
                    FROM marketforge_account_ledger
                    WHERE room_id = $1 AND account_id = 20
                    "#,
                    &[&room_id],
                )
                .unwrap();
            let ledger_instrument_id: String = buyer_ledger.get("instrument_id");
            let account_side: String = buyer_ledger.get("account_side");
            let cash_delta: i64 = buyer_ledger.get("cash_delta");
            let position_delta: i64 = buyer_ledger.get("position_delta");
            let cash_balance: i64 = buyer_ledger.get("cash_balance");
            let position_qty: i64 = buyer_ledger.get("position_qty");
            assert_eq!(ledger_instrument_id, "V-BTC-SPOT");
            assert_eq!(account_side, "buy");
            assert_eq!(cash_delta, -208);
            assert_eq!(position_delta, 2);
            assert_eq!(cash_balance, 792);
            assert_eq!(position_qty, 2);

            let position_count: i64 = client
                .query_one(
                    "SELECT count(*) FROM marketforge_position_snapshots WHERE room_id = $1",
                    &[&room_id],
                )
                .unwrap()
                .get(0);
            assert_eq!(position_count, 2);
        })
        .join()
        .unwrap();
    }

    fn assert_postgres_transfer_projection(database_url: &str, room_id: &str) {
        let database_url = database_url.to_string();
        let room_id = room_id.to_string();
        std::thread::spawn(move || {
            let mut client = postgres::Client::connect(&database_url, postgres::NoTls).unwrap();
            let transfer = client
                .query_one(
                    r#"
                    SELECT kind, account_id, asset_id, amount, status
                    FROM marketforge_transfers
                    WHERE room_id = $1
                    "#,
                    &[&room_id],
                )
                .unwrap();
            let kind: String = transfer.get("kind");
            let account_id: i64 = transfer.get("account_id");
            let asset_id: String = transfer.get("asset_id");
            let amount: i64 = transfer.get("amount");
            let status: String = transfer.get("status");
            assert_eq!(kind, "deposit");
            assert_eq!(account_id, 20);
            assert_eq!(asset_id, "BTC");
            assert_eq!(amount, 25);
            assert_eq!(status, "completed");

            let event_count: i64 = client
                .query_one(
                    "SELECT count(*) FROM marketforge_transfer_events WHERE room_id = $1",
                    &[&room_id],
                )
                .unwrap()
                .get(0);
            assert_eq!(event_count, 1);
        })
        .join()
        .unwrap();
    }

    fn cleanup_postgres_room(database_url: &str, room_id: &str) {
        let database_url = database_url.to_string();
        let room_id = room_id.to_string();
        let _ = std::thread::spawn(move || {
            if let Ok(mut client) = postgres::Client::connect(&database_url, postgres::NoTls) {
                let _ = client.execute(
                    "DELETE FROM marketforge_rooms WHERE room_id = $1",
                    &[&room_id],
                );
            }
        })
        .join();
    }

    #[tokio::test]
    async fn agent_templates_cannot_target_a_different_room() {
        let app = new_app();
        let scenario = serde_json::to_string(&spot_scenario("agent-path-room")).unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms")
                    .header("content-type", "application/json")
                    .body(Body::from(scenario))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let request = StartAgentsRequest {
            agents: vec![dca_template("other-room", "cross-room-agent", 20)],
            interval_ms: Some(10),
        };
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/rooms/agent-path-room/agents")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&request).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn agent_worker_stops_and_reports_http_errors() {
        let worker = AgentWorkerHandle::spawn(
            "http://127.0.0.1:0".to_string(),
            "agent-error-room".to_string(),
            DEFAULT_USER_ID.to_string(),
            vec![dca_template("agent-error-room", "failing-agent", 20)],
            Duration::from_millis(1),
        )
        .unwrap();

        let mut status = worker.status("agent-error-room".to_string());
        for _ in 0..50 {
            if !status.running {
                break;
            }
            thread::sleep(Duration::from_millis(10));
            status = worker.status("agent-error-room".to_string());
        }
        assert!(!status.running);
        assert!(status.last_error.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rule_agent_can_trade_through_real_http_client() {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to bind test listener: {error}"),
        };
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{addr}");
        let app = new_app_with_base_url(base_url.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let responses = tokio::task::spawn_blocking(move || {
            let client = HttpTradingClient::with_user_id(base_url, "alice");
            client.create_room(&spot_scenario("remote-ai")).unwrap();
            let mut participant = DcaTrader::new(DcaTraderConfig {
                participant: ParticipantConfig {
                    participant_id: "dca-http".to_string(),
                    kind: ParticipantKind::RuleAgent,
                    room_id: "remote-ai".to_string(),
                    account_id: 20,
                },
                interval_steps: 1,
                order_qty: 2,
                use_market_order: false,
                limit_offset_ticks: 0,
                fallback_price_tick: 100,
                side: Side::Buy,
            });

            run_remote_participant_once(&client, &mut participant)
        })
        .await
        .unwrap()
        .unwrap();

        server.abort();
        assert_eq!(responses.len(), 1);
        assert!(responses[0].accepted);
        assert!(
            responses[0]
                .events
                .iter()
                .any(|event| matches!(event, EventSummary::OrderRested { .. }))
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn room_can_autostart_agent_worker_over_http() {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to bind test listener: {error}"),
        };
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{addr}");
        let app = new_app_with_base_url(base_url.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let view = tokio::task::spawn_blocking(move || {
            let client = HttpTradingClient::with_user_id(base_url, "alice");
            let room = CreateRoomRequest {
                scenario: spot_scenario("worker-ai"),
                agents: vec![dca_template("worker-ai", "dca-worker", 20)],
                agent_interval_ms: Some(20),
                autostart_agents: Some(true),
            };
            let created = client.create_room_with_agents(&room).unwrap();
            assert!(created.agent_worker.running);
            assert_eq!(created.agent_worker.participants, vec!["dca-worker"]);

            let mut view = client.market_view("worker-ai").unwrap();
            for _ in 0..20 {
                if !view.book.bids.is_empty() {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
                view = client.market_view("worker-ai").unwrap();
            }

            let status = client.agent_status("worker-ai").unwrap();
            assert!(status.running);
            client.stop_agents("worker-ai").unwrap();
            view
        })
        .await
        .unwrap();

        server.abort();
        assert_eq!(view.book.bids[0].price_tick, 100);
        assert!(view.book.bids[0].qty >= 2);
    }
}
