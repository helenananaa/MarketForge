use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderValue, Method, StatusCode},
    routing::{get, post},
};
use exchange_core::{
    AccountSnapshots, ActorExecution, ActorExecutionResult, ActorRejectReason, AgentTemplate,
    BookSnapshot, Event, GatewayRequest, MarketExecution, MarketStatus, MarketView, OrderAction,
    OrderGateway, OrderId, Participant, ParticipantId, RoomId, RoomManager, RoomManagerError,
    ScenarioConfig, TradingApi, model::AccountId,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tower_http::cors::CorsLayer;

type SharedState = Arc<Mutex<AppState>>;
type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ErrorResponse>)>;

struct AppState {
    rooms: RoomManager,
    next_order_id: OrderId,
    base_url: String,
    agent_workers: BTreeMap<RoomId, AgentWorkerHandle>,
}

impl AppState {
    fn new(base_url: impl Into<String>) -> Self {
        Self {
            rooms: RoomManager::new(),
            next_order_id: 1,
            base_url: base_url.into(),
            agent_workers: BTreeMap::new(),
        }
    }
}

pub fn new_app() -> Router {
    new_app_with_base_url("http://127.0.0.1:3000")
}

pub fn new_app_with_base_url(base_url: impl Into<String>) -> Router {
    app(Arc::new(Mutex::new(AppState::new(base_url))))
}

pub async fn serve(addr: SocketAddr) -> Result<(), std::io::Error> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("exchange-server listening on http://{addr}");
    serve_listener(listener).await
}

pub async fn serve_listener(listener: tokio::net::TcpListener) -> Result<(), std::io::Error> {
    let addr = listener.local_addr()?;
    axum::serve(listener, new_app_with_base_url(format!("http://{addr}"))).await
}

fn app(state: SharedState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin("http://127.0.0.1:5173".parse::<HeaderValue>().unwrap())
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
        .route("/rooms/{room_id}/view", get(market_view))
        .route("/rooms/{room_id}/book", get(book_snapshot))
        .route("/rooms/{room_id}/accounts", get(account_snapshots))
        .route("/rooms/{room_id}/orders", post(submit_order))
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
    Json(payload): Json<serde_json::Value>,
) -> ApiResult<CreateRoomResponse> {
    let mut state = lock_state(&state)?;
    let request = parse_create_room_payload(payload)?;
    let bootstrap = state
        .rooms
        .create_room(request.scenario)
        .map_err(api_error_from_room)?;
    let room_id = bootstrap.room_id;
    let seed_execution_count = bootstrap.seed_executions.len();
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

async fn list_rooms(State(state): State<SharedState>) -> ApiResult<ListRoomsResponse> {
    let state = lock_state(&state)?;

    Ok(Json(ListRoomsResponse {
        rooms: state
            .rooms
            .room_ids()
            .into_iter()
            .map(str::to_string)
            .collect(),
    }))
}

async fn start_agents(
    State(state): State<SharedState>,
    Path(room_id): Path<String>,
    Json(request): Json<StartAgentsRequest>,
) -> ApiResult<AgentWorkerStatus> {
    let mut state = lock_state(&state)?;
    state.rooms.status(&room_id).map_err(api_error_from_room)?;
    start_agent_worker_for_room(&mut state, room_id, request).map(Json)
}

async fn agent_status(
    State(state): State<SharedState>,
    Path(room_id): Path<String>,
) -> ApiResult<AgentWorkerStatus> {
    let state = lock_state(&state)?;
    state.rooms.status(&room_id).map_err(api_error_from_room)?;
    Ok(Json(agent_status_for_room(&state, &room_id)))
}

async fn stop_agents(
    State(state): State<SharedState>,
    Path(room_id): Path<String>,
) -> ApiResult<AgentWorkerStatus> {
    let mut state = lock_state(&state)?;
    state.rooms.status(&room_id).map_err(api_error_from_room)?;
    if let Some(worker) = state.agent_workers.remove(&room_id) {
        worker.stop();
    }
    Ok(Json(AgentWorkerStatus::stopped(room_id)))
}

async fn market_view(
    State(state): State<SharedState>,
    Path(room_id): Path<String>,
) -> ApiResult<MarketView> {
    let state = lock_state(&state)?;
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
    Path(room_id): Path<String>,
) -> ApiResult<BookSnapshot> {
    let state = lock_state(&state)?;
    state
        .rooms
        .book_snapshot(&room_id)
        .map(Json)
        .map_err(api_error_from_room)
}

async fn account_snapshots(
    State(state): State<SharedState>,
    Path(room_id): Path<String>,
) -> ApiResult<AccountSnapshots> {
    let state = lock_state(&state)?;
    state
        .rooms
        .account_snapshots(&room_id)
        .map(Json)
        .map_err(api_error_from_room)
}

async fn submit_order(
    State(state): State<SharedState>,
    Path(room_id): Path<String>,
    Json(request): Json<SubmitOrderRequest>,
) -> ApiResult<OrderResponse> {
    let mut state = lock_state(&state)?;
    let first_order_id = state.next_order_id;
    let mut gateway = OrderGateway::new(&mut state.rooms, first_order_id);
    let execution = gateway
        .submit_action(GatewayRequest {
            participant_id: request.participant_id.clone(),
            room_id,
            account_id: request.account_id,
            action: request.action.clone(),
        })
        .map_err(|error| api_error_from_room(error.into_room_error()))?;

    state.next_order_id = gateway.next_order_id();
    Ok(Json(OrderResponse::from_gateway_execution(
        request.participant_id,
        request.account_id,
        request.action,
        execution.execution,
    )))
}

async fn pause_room(
    State(state): State<SharedState>,
    Path(room_id): Path<String>,
) -> ApiResult<RoomStatusResponse> {
    let mut state = lock_state(&state)?;
    state
        .rooms
        .pause_room(&room_id)
        .map_err(api_error_from_room)?;
    room_status(&state.rooms, &room_id)
}

async fn resume_room(
    State(state): State<SharedState>,
    Path(room_id): Path<String>,
) -> ApiResult<RoomStatusResponse> {
    let mut state = lock_state(&state)?;
    state
        .rooms
        .resume_room(&room_id)
        .map_err(api_error_from_room)?;
    room_status(&state.rooms, &room_id)
}

fn room_status(rooms: &RoomManager, room_id: &str) -> ApiResult<RoomStatusResponse> {
    rooms
        .status(room_id)
        .map(|status| {
            Json(RoomStatusResponse {
                room_id: room_id.to_string(),
                status,
            })
        })
        .map_err(api_error_from_room)
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
    pub clearing_event_count: usize,
}

impl OrderResponse {
    fn from_gateway_execution(
        participant_id: ParticipantId,
        account_id: AccountId,
        action: OrderAction,
        execution: ActorExecution,
    ) -> Self {
        let (accepted, reject_reason, events, clearing_event_count) = match execution.result {
            ActorExecutionResult::Accepted(market_execution) => {
                let (events, clearing_event_count) = summarize_market_execution(market_execution);
                (true, None, events, clearing_event_count)
            }
            ActorExecutionResult::Rejected(reason) => {
                (false, Some(reject_reason_to_string(reason)), Vec::new(), 0)
            }
        };

        Self {
            participant_id,
            account_id,
            action,
            room_id: execution.room_id,
            command_seq: execution.command_seq,
            status: execution.status,
            accepted,
            reject_reason,
            events,
            clearing_event_count,
        }
    }
}

fn summarize_market_execution(execution: MarketExecution) -> (Vec<EventSummary>, usize) {
    match execution {
        MarketExecution::Spot(execution) => (
            execution
                .events
                .into_iter()
                .map(|record| EventSummary::from_event(record.seq, record.event))
                .collect(),
            execution.clearing_events.len(),
        ),
        MarketExecution::Perp(execution) => (
            execution
                .events
                .into_iter()
                .map(|record| EventSummary::from_event(record.seq, record.event))
                .collect(),
            execution.clearing_events.len(),
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

#[derive(Clone, Debug, Deserialize, Serialize)]
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
        price_tick: i64,
        qty: u64,
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
                price_tick: trade.price_tick,
                qty: trade.qty,
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
        AgentTemplate, DcaTrader, DcaTraderConfig, InstrumentConfig, MarketConfig,
        ParticipantConfig, ParticipantKind, Side, SpotClearingConfig, SpotMarketConfig,
        SpotRiskConfig, scenario::ScenarioAccount,
    };
    use tower::ServiceExt;

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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rule_agent_can_trade_through_real_http_client() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            serve_listener(listener).await.unwrap();
        });
        let base_url = format!("http://{addr}");

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
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            serve_listener(listener).await.unwrap();
        });
        let base_url = format!("http://{addr}");

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
