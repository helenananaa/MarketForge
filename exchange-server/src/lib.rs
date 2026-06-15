use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use exchange_core::{
    AccountSnapshots, ActorExecution, ActorExecutionResult, ActorRejectReason, BookSnapshot, Event,
    GatewayRequest, MarketExecution, MarketStatus, MarketView, OrderAction, OrderGateway, OrderId,
    Participant, ParticipantId, RoomManager, RoomManagerError, ScenarioConfig, TradingApi,
    model::AccountId,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

type SharedState = Arc<Mutex<AppState>>;
type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ErrorResponse>)>;

#[derive(Debug)]
struct AppState {
    rooms: RoomManager,
    next_order_id: OrderId,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            rooms: RoomManager::new(),
            next_order_id: 1,
        }
    }
}

pub fn new_app() -> Router {
    app(Arc::new(Mutex::new(AppState::default())))
}

pub async fn serve(addr: SocketAddr) -> Result<(), std::io::Error> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("exchange-server listening on http://{addr}");
    serve_listener(listener).await
}

pub async fn serve_listener(listener: tokio::net::TcpListener) -> Result<(), std::io::Error> {
    axum::serve(listener, new_app()).await
}

fn app(state: SharedState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/rooms", post(create_room).get(list_rooms))
        .route("/rooms/{room_id}/view", get(market_view))
        .route("/rooms/{room_id}/book", get(book_snapshot))
        .route("/rooms/{room_id}/accounts", get(account_snapshots))
        .route("/rooms/{room_id}/orders", post(submit_order))
        .route("/rooms/{room_id}/pause", post(pause_room))
        .route("/rooms/{room_id}/resume", post(resume_room))
        .with_state(state)
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { ok: true })
}

async fn create_room(
    State(state): State<SharedState>,
    Json(scenario): Json<ScenarioConfig>,
) -> ApiResult<CreateRoomResponse> {
    let mut state = lock_state(&state)?;
    let bootstrap = state
        .rooms
        .create_room(scenario)
        .map_err(api_error_from_room)?;

    Ok(Json(CreateRoomResponse {
        room_id: bootstrap.room_id,
        seed_execution_count: bootstrap.seed_executions.len(),
    }))
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Method, Request},
    };
    use exchange_core::{
        DcaTrader, DcaTraderConfig, InstrumentConfig, MarketConfig, ParticipantConfig,
        ParticipantKind, Side, SpotClearingConfig, SpotMarketConfig, SpotRiskConfig,
        scenario::ScenarioAccount,
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
}
