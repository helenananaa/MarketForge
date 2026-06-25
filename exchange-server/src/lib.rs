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
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    routing::{get, post},
};
use exchange_core::{
    AccountSnapshots, ActorExecution, ActorExecutionResult, ActorRejectReason, AgentTemplate,
    BookSnapshot, Event, GatewayRequest, MarketExecution, MarketStatus, MarketView, OrderAction,
    OrderGateway, OrderId, Participant, ParticipantId, RoomId, RoomManager, RoomManagerError,
    ScenarioConfig, SpotAccountSnapshot, SpotClearingEvent, TradingApi,
    model::{AccountId, Command},
    perp::{PerpAccountSnapshot, PerpClearingEvent},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tower_http::cors::CorsLayer;

use crate::journal::{
    AccountLedgerProjection, JournalError, JournalExecution, JournalRecovery, JournalSnapshot,
    JournalStore, MarketTickProjection, OrderProjection, PositionSnapshotProjection,
    TradeProjection, journal_store_from_env,
};

type SharedState = Arc<Mutex<AppState>>;
type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ErrorResponse>)>;
const SNAPSHOT_INTERVAL_COMMANDS: u64 = 100;
const USER_ID_HEADER: &str = "x-user-id";
const DEFAULT_USER_ID: &str = "local-user";

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
        let next_order_id = next_order_id_from_recovery(&recovery);
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
        .allow_headers([axum::http::header::CONTENT_TYPE]);

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
    let mut candidate_rooms = state.rooms.clone();
    let seed_commands = request.scenario.seed_orders.clone();
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

    let mut agent_status = AgentWorkerStatus::stopped(room_id.clone());

    if !request.agents.is_empty() && request.autostart_agents.unwrap_or(true) {
        agent_status = start_agent_worker_for_room(
            &mut state,
            room_id.clone(),
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
                    .restore_room(snapshot.actor.clone(), Vec::new())
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

        let seed_count = room.scenario.seed_orders.len();
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

            rooms
                .restore_room_status(&room.room_id, record.execution.status)
                .map_err(|error| JournalError::Recovery(format!("{error:?}")))?;
            let replayed = rooms
                .apply(&room.room_id, record.command.clone())
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

fn next_order_id_from_recovery(recovery: &JournalRecovery) -> OrderId {
    recovery
        .executions
        .iter()
        .filter(|execution| execution.participant_id.is_some())
        .filter_map(|execution| match &execution.command {
            Command::NewOrder(order) => Some(order.order_id),
            Command::CancelOrder(_) => None,
        })
        .max()
        .and_then(|order_id| order_id.checked_add(1))
        .unwrap_or(1)
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
        actor: rooms.room(room_id).ok()?.clone(),
    })
}

fn room_snapshot_if_due(rooms: &RoomManager, record: &JournalExecution) -> Option<JournalSnapshot> {
    if record.command_seq % SNAPSHOT_INTERVAL_COMMANDS != 0 {
        return None;
    }
    Some(JournalSnapshot {
        room_id: record.room_id.clone(),
        command_seq: record.command_seq,
        actor: rooms.room(&record.room_id).ok()?.clone(),
    })
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
    ensure_room_access(&mut state, &user_id, &room_id)?;
    start_agent_worker_for_room(&mut state, room_id, request).map(Json)
}

async fn agent_status(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<AgentWorkerStatus> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_access(&mut state, &user_id, &room_id)?;
    Ok(Json(agent_status_for_room(&state, &room_id)))
}

async fn stop_agents(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<AgentWorkerStatus> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_access(&mut state, &user_id, &room_id)?;
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
    ensure_room_access(&mut state, &user_id, &room_id)?;
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
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_access(&mut state, &user_id, &room_id)?;
    let orders = state
        .journal
        .query_orders(
            &user_id,
            &room_id,
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
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_access(&mut state, &user_id, &room_id)?;
    let trades = state
        .journal
        .query_trades(
            &user_id,
            &room_id,
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
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_access(&mut state, &user_id, &room_id)?;
    let ticks = state
        .journal
        .query_market_ticks(&user_id, &room_id, query_limit(query.limit))
        .map_err(api_error_from_journal)?;
    Ok(Json(RoomTicksResponse { room_id, ticks }))
}

async fn room_ledger(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Query(query): Query<ProjectionQuery>,
) -> ApiResult<RoomLedgerResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_access(&mut state, &user_id, &room_id)?;
    let ledger = state
        .journal
        .query_account_ledger(
            &user_id,
            &room_id,
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
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_access(&mut state, &user_id, &room_id)?;
    let positions = state
        .journal
        .query_position_snapshots(
            &user_id,
            &room_id,
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
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_access(&mut state, &user_id, &room_id)?;
    Ok(Json(MarketView {
        room_id: room_id.clone(),
        status: state.rooms.status(&room_id).map_err(api_error_from_room)?,
        book: state
            .rooms
            .book_snapshot(&room_id)
            .map_err(api_error_from_room)?,
        accounts: state
            .rooms
            .account_snapshots(&room_id)
            .map_err(api_error_from_room)?,
    }))
}

async fn book_snapshot(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<BookSnapshot> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_access(&mut state, &user_id, &room_id)?;
    state
        .rooms
        .book_snapshot(&room_id)
        .map(Json)
        .map_err(api_error_from_room)
}

async fn account_snapshots(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<AccountSnapshots> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_access(&mut state, &user_id, &room_id)?;
    state
        .rooms
        .account_snapshots(&room_id)
        .map(Json)
        .map_err(api_error_from_room)
}

async fn submit_order(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Json(request): Json<SubmitOrderRequest>,
) -> ApiResult<OrderResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_account_access(&mut state, &user_id, &room_id, request.account_id)?;
    let first_order_id = state.next_order_id;
    let mut candidate_rooms = state.rooms.clone();
    let mut gateway = OrderGateway::new(&mut candidate_rooms, first_order_id);
    let execution = gateway
        .submit_action(GatewayRequest {
            participant_id: request.participant_id.clone(),
            room_id,
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
    let snapshot = room_snapshot_if_due(&candidate_rooms, &journal_record);

    state
        .journal
        .append_execution(&journal_record, snapshot.as_ref())
        .map_err(api_error_from_journal)?;
    state
        .executions
        .entry(journal_record.room_id.clone())
        .or_default()
        .push(journal_record.execution.clone());
    state.rooms = candidate_rooms;
    state.next_order_id = next_order_id;

    Ok(Json(response))
}

async fn pause_room(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> ApiResult<RoomStatusResponse> {
    let mut state = lock_state(&state)?;
    let user_id = current_user_id(&headers)?;
    ensure_room_access(&mut state, &user_id, &room_id)?;
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
    ensure_room_access(&mut state, &user_id, &room_id)?;
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
    request: StartAgentsRequest,
) -> Result<AgentWorkerStatus, (StatusCode, Json<ErrorResponse>)> {
    if request.agents.is_empty() {
        return Ok(AgentWorkerStatus::stopped(room_id));
    }

    if let Some(worker) = state.agent_workers.remove(&room_id) {
        worker.stop();
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
    })
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
        RoomManagerError::RoomAlreadyExists { .. } | RoomManagerError::Scenario(_) => {
            StatusCode::BAD_REQUEST
        }
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RoomExecutionSummary {
    pub room_id: String,
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
    pub account_id: AccountId,
    pub action: OrderAction,
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
}

impl AgentWorkerStatus {
    fn stopped(room_id: String) -> Self {
        Self {
            room_id,
            running: false,
            interval_ms: 0,
            participants: Vec::new(),
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
        ActorRejectReason::WrongMarketKind => "wrong market kind".to_string(),
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
        notional: i64,
        buyer_fee: i64,
        seller_fee: i64,
        buyer: SpotAccountStateSummary,
        seller: SpotAccountStateSummary,
    },
    PerpTradeSettled {
        trade_id: u64,
        buyer_account_id: AccountId,
        seller_account_id: AccountId,
        price_tick: i64,
        qty: u64,
        notional: i64,
        buyer_fee: i64,
        seller_fee: i64,
        buyer_realized_pnl: i64,
        seller_realized_pnl: i64,
        buyer: PerpAccountStateSummary,
        seller: PerpAccountStateSummary,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SpotAccountStateSummary {
    pub account_id: AccountId,
    pub cash_balance: i64,
    pub position_qty: i64,
    pub fees_paid: i64,
}

impl SpotAccountStateSummary {
    fn from_snapshot(snapshot: SpotAccountSnapshot) -> Self {
        Self {
            account_id: snapshot.account_id,
            cash_balance: amount_to_i64(snapshot.cash_balance),
            position_qty: amount_to_i64(snapshot.position_qty),
            fees_paid: amount_to_i64(snapshot.fees_paid),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerpAccountStateSummary {
    pub account_id: AccountId,
    pub cash_balance: i64,
    pub position_qty: i64,
    pub avg_entry_price_tick: i64,
    pub realized_pnl: i64,
    pub unrealized_pnl: i64,
    pub equity: i64,
    pub initial_margin: i64,
    pub fees_paid: i64,
}

impl PerpAccountStateSummary {
    fn from_snapshot(snapshot: PerpAccountSnapshot) -> Self {
        Self {
            account_id: snapshot.account_id,
            cash_balance: amount_to_i64(snapshot.cash_balance),
            position_qty: amount_to_i64(snapshot.position_qty),
            avg_entry_price_tick: snapshot.avg_entry_price_tick,
            realized_pnl: amount_to_i64(snapshot.realized_pnl),
            unrealized_pnl: amount_to_i64(snapshot.unrealized_pnl),
            equity: amount_to_i64(snapshot.equity),
            initial_margin: amount_to_i64(snapshot.initial_margin),
            fees_paid: amount_to_i64(snapshot.fees_paid),
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
                notional: amount_to_i64(notional),
                buyer_fee: amount_to_i64(buyer_fee),
                seller_fee: amount_to_i64(seller_fee),
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
                notional: amount_to_i64(notional),
                buyer_fee: amount_to_i64(buyer_fee),
                seller_fee: amount_to_i64(seller_fee),
                buyer_realized_pnl: amount_to_i64(buyer_realized_pnl),
                seller_realized_pnl: amount_to_i64(seller_realized_pnl),
                buyer: PerpAccountStateSummary::from_snapshot(buyer),
                seller: PerpAccountStateSummary::from_snapshot(seller),
            },
        }
    }
}

fn amount_to_i64(value: i128) -> i64 {
    i64::try_from(value).expect("clearing amount should fit in API summary")
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
}

impl HttpTradingClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client: reqwest::blocking::Client::new(),
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
        let response = self
            .client
            .get(format!("{}{}", self.base_url, path))
            .send()
            .map_err(HttpTradingError::Http)?;
        decode_response(response)
    }

    fn post_json<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, HttpTradingError> {
        let response = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .json(body)
            .send()
            .map_err(HttpTradingError::Http)?;
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
    interval_ms: u64,
    participants: Vec<ParticipantId>,
}

impl AgentWorkerHandle {
    fn spawn(
        base_url: String,
        room_id: RoomId,
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
        thread::Builder::new()
            .name(format!("marketforge-agents-{room_id}"))
            .spawn(move || {
                let client = HttpTradingClient::new(base_url);
                let mut participants = templates
                    .into_iter()
                    .map(AgentTemplate::into_participant)
                    .collect::<Vec<_>>();

                while !worker_stop.load(Ordering::Relaxed) {
                    for participant in &mut participants {
                        if worker_stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let _ = run_remote_participant_once(&client, participant.as_mut());
                    }
                    sleep_until_next_step(interval, &worker_stop);
                }
            })
            .map_err(AgentWorkerError::Spawn)?;

        Ok(Self {
            stop,
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
        SpotClearingConfig, SpotMarketConfig, SpotRiskConfig, scenario::ScenarioAccount,
    };
    use tower::ServiceExt;

    use crate::journal::{JournalError, JournalExecution, JournalStore, PostgresJournalStore};

    fn spot_scenario(room_id: &str) -> ScenarioConfig {
        ScenarioConfig {
            room_id: room_id.to_string(),
            market: MarketConfig::Spot(SpotMarketConfig {
                instrument: InstrumentConfig::new("V-BTC-SPOT", 1, 1).unwrap(),
                clearing: SpotClearingConfig::default(),
                risk: SpotRiskConfig::default(),
            }),
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
        })];
        scenario
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
            "/rooms/query-room/trades",
            "/rooms/query-room/ticks",
            "/rooms/query-room/ledger",
            "/rooms/query-room/positions",
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

            fn append_execution(
                &mut self,
                _record: &JournalExecution,
                _snapshot: Option<&JournalSnapshot>,
            ) -> Result<(), JournalError> {
                Err(JournalError::CountOutOfRange(0))
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

            fn append_execution(
                &mut self,
                _record: &JournalExecution,
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
        }

        let scenario = seeded_spot_scenario("recovered-room");
        let mut rooms = RoomManager::new();
        let bootstrap = rooms.create_room(scenario.clone()).unwrap();
        let seed_records = scenario
            .seed_orders
            .iter()
            .cloned()
            .zip(bootstrap.seed_executions.iter().cloned())
            .map(|(command, execution)| JournalExecution::seed(command, execution))
            .collect::<Vec<_>>();
        let mut gateway = OrderGateway::new(&mut rooms, 1);
        let submitted = gateway
            .submit_action(GatewayRequest {
                participant_id: "human-1".to_string(),
                room_id: "recovered-room".to_string(),
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
            actor: rooms.room("recovered-room").unwrap().clone(),
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
        assert!(
            order
                .events
                .iter()
                .any(|event| matches!(event, EventSummary::OrderAccepted { order_id: 2, .. }))
        );

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
        let scenario = seeded_spot_scenario(&room_id);

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
        assert_postgres_trade_projection(&database_url, &room_id, submitted_order_id);

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
                    SELECT status, remaining_qty
                    FROM marketforge_orders
                    WHERE room_id = $1 AND order_id = 10000
                    "#,
                    &[&room_id],
                )
                .unwrap();
            let seed_status: String = seed_order.get("status");
            let seed_remaining_qty: i64 = seed_order.get("remaining_qty");
            assert_eq!(seed_status, "partially_filled");
            assert_eq!(seed_remaining_qty, 6);

            let taker_order = client
                .query_one(
                    r#"
                    SELECT status, remaining_qty, account_id, participant_id
                    FROM marketforge_orders
                    WHERE room_id = $1 AND order_id = $2
                    "#,
                    &[&room_id, &taker_order_id],
                )
                .unwrap();
            let taker_status: String = taker_order.get("status");
            let taker_remaining_qty: i64 = taker_order.get("remaining_qty");
            let taker_account_id: i64 = taker_order.get("account_id");
            let participant_id: Option<String> = taker_order.get("participant_id");
            assert_eq!(taker_status, "filled");
            assert_eq!(taker_remaining_qty, 0);
            assert_eq!(taker_account_id, 20);
            assert_eq!(participant_id.as_deref(), Some("human-pg"));

            let trade = client
                .query_one(
                    r#"
                    SELECT maker_account_id, taker_account_id, price_tick, qty, taker_side
                    FROM marketforge_trades
                    WHERE room_id = $1
                    "#,
                    &[&room_id],
                )
                .unwrap();
            let maker_account_id: i64 = trade.get("maker_account_id");
            let taker_account_id: i64 = trade.get("taker_account_id");
            let price_tick: i64 = trade.get("price_tick");
            let qty: i64 = trade.get("qty");
            let taker_side: String = trade.get("taker_side");
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
                    SELECT account_side, cash_delta, position_delta, cash_balance, position_qty
                    FROM marketforge_account_ledger
                    WHERE room_id = $1 AND account_id = 20
                    "#,
                    &[&room_id],
                )
                .unwrap();
            let account_side: String = buyer_ledger.get("account_side");
            let cash_delta: i64 = buyer_ledger.get("cash_delta");
            let position_delta: i64 = buyer_ledger.get("position_delta");
            let cash_balance: i64 = buyer_ledger.get("cash_balance");
            let position_qty: i64 = buyer_ledger.get("position_qty");
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
            let client = HttpTradingClient::new(base_url);
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
            let client = HttpTradingClient::new(base_url);
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
