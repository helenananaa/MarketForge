use std::{
    collections::{BTreeMap, BTreeSet},
    env, fmt, thread,
    time::{Duration, Instant},
};

use exchange_core::{
    ActorExecution, ActorExecutionResult, BookSnapshot, Command, Event, MarketConfig,
    MarketExecution, MarketKind, MarketStatus, RoomManager, ScenarioConfig, Side, Trade,
};
use exchange_server::{
    EventSummary, HttpTradingClient, HttpTradingError, RoomEventsResponse, RoomExecutionSummary,
};
use serde::Serialize;

use crate::error::AdapterError;

pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
pub const SUPPORTED_INTERVALS: &[&str] = &["1s", "1m", "5m", "15m", "1h"];
pub const MAX_CLOCK_STEPS_PER_CALL: u64 = 10_000;
pub const REMOTE_BACKEND_URL_ENV: &str = "MARKETFORGE_PLUGIN_BACKEND_URL";
pub const REMOTE_BACKEND_TOKEN_ENV: &str = "MARKETFORGE_PLUGIN_BACKEND_TOKEN";
pub const REMOTE_BACKEND_USER_ID_ENV: &str = "MARKETFORGE_PLUGIN_BACKEND_USER_ID";
pub const REMOTE_TRUSTED_OWNER_URLS_ENV: &str = "MARKETFORGE_PLUGIN_TRUSTED_OWNER_URLS";
const MAX_DEPTH_LEVELS: usize = 100;
const REMOTE_EVENT_PAGE_LIMIT: usize = 500;
const REMOTE_DIRECTORY_PAGE_LIMIT: usize = 500;
const REMOTE_POLL_INTERVAL_MS: u64 = 50;

#[derive(Clone)]
enum RemoteBackendAuth {
    None,
    UserId(String),
    BearerToken(String),
}

impl fmt::Debug for RemoteBackendAuth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => formatter.write_str("None"),
            Self::UserId(user_id) => formatter.debug_tuple("UserId").field(user_id).finish(),
            Self::BearerToken(_) => formatter.write_str("BearerToken([REDACTED])"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct RemoteBackendConfig {
    base_url: String,
    auth: RemoteBackendAuth,
    trusted_owner_urls: Vec<String>,
}

impl RemoteBackendConfig {
    pub fn new(base_url: impl Into<String>) -> Result<Self, AdapterError> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return Err(AdapterError::invalid_at(
                REMOTE_BACKEND_URL_ENV,
                "remote backend URL must not be empty",
            ));
        }
        Ok(Self {
            base_url,
            auth: RemoteBackendAuth::None,
            trusted_owner_urls: Vec::new(),
        })
    }

    pub fn with_user_id(mut self, user_id: impl Into<String>) -> Result<Self, AdapterError> {
        let user_id = user_id.into();
        if user_id.trim().is_empty() {
            return Err(AdapterError::invalid_at(
                REMOTE_BACKEND_USER_ID_ENV,
                "remote backend user ID must not be empty",
            ));
        }
        self.auth = RemoteBackendAuth::UserId(user_id);
        Ok(self)
    }

    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Result<Self, AdapterError> {
        let token = token.into();
        if token.trim().is_empty() {
            return Err(AdapterError::invalid_at(
                REMOTE_BACKEND_TOKEN_ENV,
                "remote backend bearer token must not be empty",
            ));
        }
        self.auth = RemoteBackendAuth::BearerToken(token);
        Ok(self)
    }

    pub fn with_trusted_owner_url(mut self, owner_url: impl Into<String>) -> Self {
        self.trusted_owner_urls.push(owner_url.into());
        self
    }

    pub fn from_env() -> Result<Option<Self>, AdapterError> {
        let base_url = optional_env(REMOTE_BACKEND_URL_ENV)?;
        let token = optional_env(REMOTE_BACKEND_TOKEN_ENV)?;
        let user_id = optional_env(REMOTE_BACKEND_USER_ID_ENV)?;
        let trusted_owner_urls = optional_env(REMOTE_TRUSTED_OWNER_URLS_ENV)?;
        let Some(base_url) = base_url else {
            if token.is_some() || user_id.is_some() || trusted_owner_urls.is_some() {
                return Err(AdapterError::invalid_at(
                    REMOTE_BACKEND_URL_ENV,
                    "remote backend credentials and owner URLs require a backend URL",
                ));
            }
            return Ok(None);
        };
        if token.is_some() && user_id.is_some() {
            return Err(AdapterError::invalid(
                "configure either a remote backend bearer token or user ID, not both",
            ));
        }

        let mut config = Self::new(base_url)?;
        if let Some(token) = token {
            config = config.with_bearer_token(token)?;
        } else if let Some(user_id) = user_id {
            config = config.with_user_id(user_id)?;
        }
        if let Some(urls) = trusted_owner_urls {
            for owner_url in urls.split(',').map(str::trim) {
                if owner_url.is_empty() {
                    return Err(AdapterError::invalid_at(
                        REMOTE_TRUSTED_OWNER_URLS_ENV,
                        "trusted owner URL list contains an empty entry",
                    ));
                }
                config = config.with_trusted_owner_url(owner_url);
            }
        }
        config.client()?;
        Ok(Some(config))
    }

    fn client(&self) -> Result<HttpTradingClient, AdapterError> {
        let mut client = match &self.auth {
            RemoteBackendAuth::None => HttpTradingClient::new(&self.base_url),
            RemoteBackendAuth::UserId(user_id) => {
                HttpTradingClient::with_user_id(&self.base_url, user_id)
            }
            RemoteBackendAuth::BearerToken(token) => {
                HttpTradingClient::with_bearer_token(&self.base_url, token)
            }
        };
        client.trust_owner_url(&self.base_url).map_err(|error| {
            AdapterError::invalid_at(
                REMOTE_BACKEND_URL_ENV,
                format!("invalid remote backend URL: {error}"),
            )
        })?;
        for owner_url in &self.trusted_owner_urls {
            client.trust_owner_url(owner_url).map_err(|error| {
                AdapterError::invalid_at(
                    REMOTE_TRUSTED_OWNER_URLS_ENV,
                    format!("invalid trusted owner URL: {error}"),
                )
            })?;
        }
        Ok(client)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentBinding {
    pub instrument_id: String,
    pub venue_id: String,
    pub symbol: String,
    pub base_asset: String,
    pub quote_asset: String,
    pub market_type: String,
    pub product_type: String,
    pub price_tick_size: String,
}

impl InstrumentBinding {
    fn from_market(config: &MarketConfig) -> Result<Self, AdapterError> {
        let instrument = config.instrument();
        validate_symbol(&instrument.symbol)?;
        validate_asset("baseAsset", &instrument.base_asset)?;
        validate_asset("quoteAsset", &instrument.quote_asset)?;
        let market_type = market_type(config.kind()).to_string();
        Ok(Self {
            instrument_id: instrument.instrument_id.clone(),
            venue_id: instrument.venue_id.clone(),
            symbol: instrument.symbol.clone(),
            base_asset: instrument.base_asset.clone(),
            quote_asset: instrument.quote_asset.clone(),
            market_type: market_type.clone(),
            product_type: market_type,
            price_tick_size: instrument.tick_size.to_string(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLoadResult {
    pub room_id: String,
    pub epoch_ms: u64,
    pub market_time_ms: u64,
    pub instruments: Vec<InstrumentBinding>,
    pub seed_execution_count: usize,
    pub seed_trade_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyCommandResult {
    pub room_id: String,
    pub instrument_id: String,
    pub command_seq: u64,
    pub accepted: bool,
    pub status: String,
    pub event_count: usize,
    pub trade_count: usize,
    pub market_time_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDescription {
    pub room_id: String,
    pub epoch_ms: u64,
    pub market_time_ms: u64,
    pub step: u64,
    pub step_duration_ms: u64,
    pub status: String,
    pub instruments: Vec<InstrumentBinding>,
}

#[derive(Clone, Debug, Default)]
pub struct MarketForgeAdapter {
    session: Option<MarketForgeSession>,
    remote_backend: Option<RemoteBackendConfig>,
}

impl MarketForgeAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_remote_backend(config: RemoteBackendConfig) -> Self {
        Self {
            session: None,
            remote_backend: Some(config),
        }
    }

    pub fn has_session(&self) -> bool {
        self.session.is_some()
    }

    pub fn remote_backend_configured(&self) -> bool {
        self.remote_backend.is_some()
    }

    pub fn session_mode(&self) -> Option<&'static str> {
        self.session.as_ref().map(MarketForgeSession::mode)
    }

    pub fn load_session(
        &mut self,
        scenario: ScenarioConfig,
        epoch_ms: u64,
    ) -> Result<SessionLoadResult, AdapterError> {
        let room_id = scenario.room_id.clone();
        let bindings = bindings_from_scenario(&scenario)?;

        let mut manager = RoomManager::new();
        let bootstrap = manager
            .create_room(scenario)
            .map_err(|error| AdapterError::core(format!("could not create room: {error:?}")))?;
        let clock = manager
            .clock(&room_id)
            .map_err(|error| AdapterError::core(format!("could not read room clock: {error:?}")))?;
        let market_time_ms = absolute_time(epoch_ms, clock.market_time_ms())?;
        let mut session = MarketForgeSession {
            backend: SessionBackend::Embedded {
                manager,
                projected_execution_count: 0,
            },
            room_id: room_id.clone(),
            epoch_ms,
            bindings,
            projection: Projection::new(market_time_ms),
        };
        let projected = session.sync_projection(market_time_ms, true)?;

        let result = SessionLoadResult {
            room_id,
            epoch_ms,
            market_time_ms,
            instruments: session.instruments(),
            seed_execution_count: bootstrap.seed_executions.len(),
            seed_trade_count: projected.trade_count,
        };
        self.session = Some(session);
        Ok(result)
    }

    pub fn attach_remote_session(
        &mut self,
        scenario: ScenarioConfig,
        epoch_ms: u64,
    ) -> Result<SessionLoadResult, AdapterError> {
        let config = self.remote_backend.as_ref().ok_or_else(|| {
            AdapterError::invalid_state(
                "REMOTE_BACKEND_NOT_CONFIGURED",
                format!("configure {REMOTE_BACKEND_URL_ENV} before attaching a remote room"),
            )
        })?;
        let client = config.client()?;
        let room_id = scenario.room_id.clone();
        let bindings = bindings_from_scenario(&scenario)?;
        ensure_remote_room_visible(&client, &room_id)?;

        let clock = client
            .room_clock(&room_id)
            .map_err(|error| remote_error("read room clock", error))?;
        if clock.room_id != room_id {
            return Err(AdapterError::internal(format!(
                "remote backend returned clock for room {}, expected {room_id}",
                clock.room_id
            )));
        }
        let market_time_ms = absolute_time(epoch_ms, clock.clock.market_time_ms())?;
        let mut session = MarketForgeSession {
            backend: SessionBackend::Remote {
                client,
                after_command_seq: None,
            },
            room_id: room_id.clone(),
            epoch_ms,
            bindings,
            projection: Projection::new(epoch_ms),
        };
        session.validate_remote_instruments()?;
        let projected = session.sync_remote_projection(true)?;

        let result = SessionLoadResult {
            room_id,
            epoch_ms,
            market_time_ms,
            instruments: session.instruments(),
            seed_execution_count: projected.execution_count,
            seed_trade_count: projected.trade_count,
        };
        self.session = Some(session);
        Ok(result)
    }

    pub fn unload_session(&mut self) -> bool {
        self.session.take().is_some()
    }

    pub fn describe_session(&self) -> Result<SessionDescription, AdapterError> {
        let session = self.session()?;
        match &session.backend {
            SessionBackend::Embedded { manager, .. } => {
                let clock = manager.clock(&session.room_id).map_err(|error| {
                    AdapterError::core(format!("could not read room clock: {error:?}"))
                })?;
                Ok(SessionDescription {
                    room_id: session.room_id.clone(),
                    epoch_ms: session.epoch_ms,
                    market_time_ms: absolute_time(session.epoch_ms, clock.market_time_ms())?,
                    step: clock.step(),
                    step_duration_ms: clock.step_duration_ms(),
                    status: status_name(manager.status(&session.room_id).map_err(|error| {
                        AdapterError::core(format!("could not read room status: {error:?}"))
                    })?)
                    .to_string(),
                    instruments: session.instruments(),
                })
            }
            SessionBackend::Remote { client, .. } => {
                let clock = client
                    .room_clock(&session.room_id)
                    .map_err(|error| remote_error("read room clock", error))?;
                let binding = session.bindings.values().next().ok_or_else(|| {
                    AdapterError::internal("remote session has no instrument bindings")
                })?;
                let view = client
                    .market_view_for(&session.room_id, &binding.instrument_id)
                    .map_err(|error| remote_error("read room status", error))?;
                Ok(SessionDescription {
                    room_id: session.room_id.clone(),
                    epoch_ms: session.epoch_ms,
                    market_time_ms: absolute_time(session.epoch_ms, clock.clock.market_time_ms())?,
                    step: clock.clock.step(),
                    step_duration_ms: clock.clock.step_duration_ms(),
                    status: status_name(view.status).to_string(),
                    instruments: session.instruments(),
                })
            }
        }
    }

    pub fn apply_command(
        &mut self,
        room_id: &str,
        instrument_id: &str,
        command: Command,
    ) -> Result<ApplyCommandResult, AdapterError> {
        let session = self.session_mut()?;
        session.require_room(room_id)?;
        session.binding_by_instrument(instrument_id)?;
        match &mut session.backend {
            SessionBackend::Embedded { manager, .. } => {
                let clock = manager.clock(room_id).map_err(|error| {
                    AdapterError::core(format!("could not read room clock: {error:?}"))
                })?;
                let market_time_ms = absolute_time(session.epoch_ms, clock.market_time_ms())?;
                let execution = manager
                    .apply_to_instrument(room_id, instrument_id, command)
                    .map_err(|error| {
                        AdapterError::core(format!("command routing failed: {error:?}"))
                    })?;
                let command_seq = execution.command_seq;
                let accepted = matches!(execution.result, ActorExecutionResult::Accepted(_));
                let status = execution.status;
                let projected = session.sync_projection(market_time_ms, false)?;
                Ok(ApplyCommandResult {
                    room_id: room_id.to_string(),
                    instrument_id: instrument_id.to_string(),
                    command_seq,
                    accepted,
                    status: status_name(status).to_string(),
                    event_count: projected.event_count,
                    trade_count: projected.trade_count,
                    market_time_ms,
                })
            }
            SessionBackend::Remote { client, .. } => {
                let exchange_core::Command::SetMarkPrice(mark) = command else {
                    return Err(AdapterError::invalid_state(
                        "REMOTE_COMMAND_UNSUPPORTED",
                        "remote command.apply currently supports SetMarkPrice only; submit orders through the backend trading API",
                    ));
                };
                let execution = client
                    .set_mark_price_for(room_id, instrument_id, mark.price_tick)
                    .map_err(|error| remote_error("set mark price", error))?;
                let market_time_ms =
                    absolute_time(session.epoch_ms, required_remote_market_time(&execution)?)?;
                let result = ApplyCommandResult {
                    room_id: room_id.to_string(),
                    instrument_id: instrument_id.to_string(),
                    command_seq: execution.command_seq,
                    accepted: execution.accepted,
                    status: status_name(execution.status).to_string(),
                    event_count: execution.events.len(),
                    trade_count: execution
                        .events
                        .iter()
                        .filter(|event| matches!(event, EventSummary::TradePrinted { .. }))
                        .count(),
                    market_time_ms,
                };
                session.sync_remote_projection(false)?;
                Ok(result)
            }
        }
    }

    pub fn advance_clock(&mut self, room_id: &str, steps: u64) -> Result<u64, AdapterError> {
        if !(1..=MAX_CLOCK_STEPS_PER_CALL).contains(&steps) {
            return Err(AdapterError::invalid_at(
                "steps",
                format!(
                    "clock advancement must be from 1 to {MAX_CLOCK_STEPS_PER_CALL} steps per call"
                ),
            ));
        }
        let session = self.session_mut()?;
        session.require_room(room_id)?;
        let market_time_ms = match &mut session.backend {
            SessionBackend::Embedded { manager, .. } => {
                manager.advance_clock(room_id, steps).map_err(|error| {
                    AdapterError::core(format!("could not advance room clock: {error:?}"))
                })?;
                let clock = manager.clock(room_id).map_err(|error| {
                    AdapterError::core(format!("could not read room clock: {error:?}"))
                })?;
                absolute_time(session.epoch_ms, clock.market_time_ms())?
            }
            SessionBackend::Remote { client, .. } => {
                let response = client
                    .advance_room_clock(room_id, steps)
                    .map_err(|error| remote_error("advance room clock", error))?;
                absolute_time(session.epoch_ms, response.clock.market_time_ms())?
            }
        };
        session.sync_projection(market_time_ms, false)?;
        Ok(market_time_ms)
    }

    pub fn pause(&mut self, room_id: &str) -> Result<(), AdapterError> {
        let session = self.session_mut()?;
        session.require_room(room_id)?;
        match &mut session.backend {
            SessionBackend::Embedded { manager, .. } => manager
                .pause_room(room_id)
                .map_err(|error| AdapterError::core(format!("could not pause room: {error:?}"))),
            SessionBackend::Remote { client, .. } => client
                .pause_room(room_id)
                .map(|_| ())
                .map_err(|error| remote_error("pause room", error)),
        }
    }

    pub fn resume(&mut self, room_id: &str) -> Result<(), AdapterError> {
        let session = self.session_mut()?;
        session.require_room(room_id)?;
        match &mut session.backend {
            SessionBackend::Embedded { manager, .. } => manager
                .resume_room(room_id)
                .map_err(|error| AdapterError::core(format!("could not resume room: {error:?}"))),
            SessionBackend::Remote { client, .. } => client
                .resume_room(room_id)
                .map(|_| ())
                .map_err(|error| remote_error("resume room", error)),
        }
    }

    pub fn close(&mut self, room_id: &str) -> Result<(), AdapterError> {
        let session = self.session_mut()?;
        session.require_room(room_id)?;
        match &mut session.backend {
            SessionBackend::Embedded { manager, .. } => manager
                .close_room(room_id)
                .map_err(|error| AdapterError::core(format!("could not close room: {error:?}"))),
            SessionBackend::Remote { client, .. } => client
                .close_room(room_id)
                .map(|_| ())
                .map_err(|error| remote_error("close room", error)),
        }
    }

    pub(crate) fn refresh_remote(&mut self, wait_ms: u64) -> Result<(), AdapterError> {
        let Some(session) = self.session.as_mut() else {
            return Ok(());
        };
        if !session.is_remote() {
            return Ok(());
        }

        let started = Instant::now();
        let wait = Duration::from_millis(wait_ms);
        loop {
            let before = session.projection.events.len();
            let sync = session.sync_remote_projection(false)?;
            if sync.execution_count > 0
                || session.projection.events.len() > before
                || started.elapsed() >= wait
            {
                return Ok(());
            }
            let remaining = wait.saturating_sub(started.elapsed());
            thread::sleep(remaining.min(Duration::from_millis(REMOTE_POLL_INTERVAL_MS)));
        }
    }

    pub(crate) fn instruments(&self) -> Vec<InstrumentBinding> {
        self.session
            .as_ref()
            .map(MarketForgeSession::instruments)
            .unwrap_or_default()
    }

    pub(crate) fn binding(
        &self,
        market_type: &str,
        symbol: &str,
    ) -> Result<&InstrumentBinding, AdapterError> {
        self.session()?
            .bindings
            .get(&(market_type.to_string(), symbol.to_string()))
            .ok_or_else(|| {
                AdapterError::not_found(
                    "SYMBOL_NOT_FOUND",
                    format!("MarketForge symbol {symbol} ({market_type}) is not loaded"),
                )
            })
    }

    pub(crate) fn final_bars(
        &self,
        market_type: &str,
        symbol: &str,
        interval: &str,
    ) -> Result<Vec<ProjectedBar>, AdapterError> {
        self.binding(market_type, symbol)?;
        let interval_ms = interval_ms(interval)?;
        Ok(self
            .session()?
            .projection
            .bars
            .get(&BarKey::new(market_type, symbol, interval_ms))
            .map(|bars| {
                bars.values()
                    .filter(|bar| bar.finality == BarFinality::Final)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    pub(crate) fn latest_forming_bar(
        &self,
        market_type: &str,
        symbol: &str,
        interval: &str,
    ) -> Result<Option<ProjectedBar>, AdapterError> {
        self.binding(market_type, symbol)?;
        let interval_ms = interval_ms(interval)?;
        Ok(self
            .session()?
            .projection
            .bars
            .get(&BarKey::new(market_type, symbol, interval_ms))
            .and_then(|bars| bars.values().next_back())
            .filter(|bar| bar.finality == BarFinality::Forming)
            .cloned())
    }

    pub(crate) fn current_book_snapshot(
        &self,
        market_type: &str,
        symbol: &str,
    ) -> Result<Option<BookSnapshotPayload>, AdapterError> {
        let binding = self.binding(market_type, symbol)?;
        Ok(self
            .session()?
            .projection
            .books
            .get(&binding.instrument_id)
            .and_then(BookProjection::snapshot_payload))
    }

    pub(crate) fn projection_len(&self) -> usize {
        self.session
            .as_ref()
            .map(|session| session.projection.events.len())
            .unwrap_or_default()
    }

    pub(crate) fn projection_event(&self, index: usize) -> Option<&ProjectionEvent> {
        self.session
            .as_ref()
            .and_then(|session| session.projection.events.get(index))
    }

    fn session(&self) -> Result<&MarketForgeSession, AdapterError> {
        self.session.as_ref().ok_or_else(|| {
            AdapterError::invalid_state(
                "SESSION_NOT_LOADED",
                "load a MarketForge scenario before requesting market data",
            )
        })
    }

    fn session_mut(&mut self) -> Result<&mut MarketForgeSession, AdapterError> {
        self.session.as_mut().ok_or_else(|| {
            AdapterError::invalid_state(
                "SESSION_NOT_LOADED",
                "load a MarketForge scenario before mutating the session",
            )
        })
    }
}

#[derive(Clone, Debug)]
struct MarketForgeSession {
    backend: SessionBackend,
    room_id: String,
    epoch_ms: u64,
    bindings: BTreeMap<(String, String), InstrumentBinding>,
    projection: Projection,
}

#[derive(Clone, Debug)]
enum SessionBackend {
    Embedded {
        manager: RoomManager,
        projected_execution_count: usize,
    },
    Remote {
        client: HttpTradingClient,
        after_command_seq: Option<u64>,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ProjectionSync {
    execution_count: usize,
    event_count: usize,
    trade_count: usize,
}

impl MarketForgeSession {
    fn mode(&self) -> &'static str {
        match self.backend {
            SessionBackend::Embedded { .. } => "embedded",
            SessionBackend::Remote { .. } => "remote",
        }
    }

    fn is_remote(&self) -> bool {
        matches!(self.backend, SessionBackend::Remote { .. })
    }

    fn instruments(&self) -> Vec<InstrumentBinding> {
        self.bindings.values().cloned().collect()
    }

    fn require_room(&self, room_id: &str) -> Result<(), AdapterError> {
        if room_id == self.room_id {
            Ok(())
        } else {
            Err(AdapterError::not_found(
                "ROOM_NOT_FOUND",
                format!("loaded room is {}, not {room_id}", self.room_id),
            ))
        }
    }

    fn binding_by_instrument(
        &self,
        instrument_id: &str,
    ) -> Result<&InstrumentBinding, AdapterError> {
        self.bindings
            .values()
            .find(|binding| binding.instrument_id == instrument_id)
            .ok_or_else(|| {
                AdapterError::not_found(
                    "INSTRUMENT_NOT_FOUND",
                    format!("instrument {instrument_id} is not exposed by this session"),
                )
            })
    }

    fn sync_projection(
        &mut self,
        market_time_ms: u64,
        initial: bool,
    ) -> Result<ProjectionSync, AdapterError> {
        if self.is_remote() {
            return self.sync_remote_projection(initial);
        }

        let (executions, history_len) = match &self.backend {
            SessionBackend::Embedded {
                manager,
                projected_execution_count,
            } => {
                let history = manager.execution_history(&self.room_id).map_err(|error| {
                    AdapterError::core(format!("could not read execution history: {error:?}"))
                })?;
                if history.len() < *projected_execution_count {
                    return Err(AdapterError::internal(
                        "MarketForge execution history regressed behind the projection cursor",
                    ));
                }
                (
                    history[*projected_execution_count..].to_vec(),
                    history.len(),
                )
            }
            SessionBackend::Remote { .. } => unreachable!("remote backend was handled above"),
        };
        let mut projection = self.projection.clone();
        projection.advance_time(market_time_ms);
        let mut sync = ProjectionSync {
            execution_count: executions.len(),
            ..ProjectionSync::default()
        };
        let mut affected_instruments = BTreeSet::new();
        for execution in &executions {
            let binding = self
                .binding_by_instrument(&execution.instrument_id)?
                .clone();
            affected_instruments.insert(binding.instrument_id.clone());
            if let Some(events) = execution_events(execution) {
                sync.event_count += events.len();
                for record in events {
                    if let Event::TradePrinted(trade) = &record.event {
                        projection.record_trade(&binding, trade, market_time_ms)?;
                        sync.trade_count += 1;
                    }
                }
            }
        }
        if initial {
            affected_instruments.extend(
                self.bindings
                    .values()
                    .map(|binding| binding.instrument_id.clone()),
            );
        }
        for instrument_id in affected_instruments {
            let binding = self.binding_by_instrument(&instrument_id)?.clone();
            let snapshot = match &self.backend {
                SessionBackend::Embedded { manager, .. } => manager
                    .book_snapshot_for(&self.room_id, &instrument_id)
                    .map_err(|error| {
                        AdapterError::core(format!("could not read order book: {error:?}"))
                    })?,
                SessionBackend::Remote { .. } => unreachable!("remote backend was handled above"),
            };
            projection.record_book(&binding, snapshot, market_time_ms, initial)?;
        }
        self.projection = projection;
        match &mut self.backend {
            SessionBackend::Embedded {
                projected_execution_count,
                ..
            } => *projected_execution_count = history_len,
            SessionBackend::Remote { .. } => unreachable!("remote backend was handled above"),
        }
        Ok(sync)
    }

    fn validate_remote_instruments(&self) -> Result<(), AdapterError> {
        let SessionBackend::Remote { client, .. } = &self.backend else {
            return Ok(());
        };
        for binding in self.bindings.values() {
            let view = client
                .market_view_for(&self.room_id, &binding.instrument_id)
                .map_err(|error| remote_error("validate remote instrument", error))?;
            if view.room_id != self.room_id
                || view.instrument_id != binding.instrument_id
                || view.venue_id != binding.venue_id
            {
                return Err(AdapterError::invalid_at(
                    "scenario",
                    format!(
                        "remote instrument {} does not match the scenario metadata",
                        binding.instrument_id
                    ),
                ));
            }
        }
        Ok(())
    }

    fn sync_remote_projection(&mut self, initial: bool) -> Result<ProjectionSync, AdapterError> {
        let (client, mut cursor) = match &self.backend {
            SessionBackend::Remote {
                client,
                after_command_seq,
            } => (client.clone(), *after_command_seq),
            SessionBackend::Embedded { .. } => return Ok(ProjectionSync::default()),
        };
        let mut projection = self.projection.clone();
        let mut sync = ProjectionSync::default();
        let mut affected_instruments = BTreeSet::new();
        if initial {
            affected_instruments.extend(
                self.bindings
                    .values()
                    .map(|binding| binding.instrument_id.clone()),
            );
        }

        let (market_time_ms, book_snapshots) = 'stabilize: loop {
            loop {
                let page = match cursor {
                    Some(after_command_seq) => client.room_events_after(
                        &self.room_id,
                        after_command_seq,
                        REMOTE_EVENT_PAGE_LIMIT,
                    ),
                    None => client.room_events_from_start(&self.room_id, REMOTE_EVENT_PAGE_LIMIT),
                }
                .map_err(|error| remote_error("read room events", error))?;
                validate_remote_event_page(&self.room_id, &page)?;

                for execution in &page.executions {
                    validate_remote_execution_cursor(&self.room_id, cursor, execution)?;
                    let market_time_ms =
                        absolute_time(self.epoch_ms, required_remote_market_time(execution)?)?;
                    projection.advance_time(market_time_ms);
                    sync.execution_count += 1;
                    sync.event_count += execution.events.len();
                    if let Some(instrument_id) = execution.instrument_id.as_deref() {
                        let binding = self.binding_by_instrument(instrument_id)?;
                        affected_instruments.insert(instrument_id.to_string());
                        for event in &execution.events {
                            if let Some(trade) = trade_from_summary(event) {
                                projection.record_trade(binding, &trade, market_time_ms)?;
                                sync.trade_count += 1;
                            }
                        }
                    } else if execution
                        .events
                        .iter()
                        .any(|event| matches!(event, EventSummary::TradePrinted { .. }))
                    {
                        return Err(AdapterError::internal(format!(
                            "remote execution {} contains a trade without an instrument",
                            execution.command_seq
                        )));
                    }
                    cursor = Some(execution.command_seq);
                }

                if !page.has_more {
                    break;
                }
                if page.next_after_command_seq != cursor {
                    return Err(AdapterError::internal(
                        "remote event page cursor does not match its final execution",
                    ));
                }
            }

            let clock = client
                .room_clock(&self.room_id)
                .map_err(|error| remote_error("read room clock", error))?;
            let market_time_ms = absolute_time(self.epoch_ms, clock.clock.market_time_ms())?;
            let mut book_snapshots = Vec::with_capacity(affected_instruments.len());
            for instrument_id in &affected_instruments {
                let binding = self.binding_by_instrument(instrument_id)?.clone();
                let view = client
                    .market_view_for(&self.room_id, instrument_id)
                    .map_err(|error| remote_error("read remote order book", error))?;
                book_snapshots.push((binding, view.book));
            }

            let confirmation = match cursor {
                Some(after_command_seq) => {
                    client.room_events_after(&self.room_id, after_command_seq, 1)
                }
                None => client.room_events_from_start(&self.room_id, 1),
            }
            .map_err(|error| remote_error("confirm remote event cursor", error))?;
            validate_remote_event_page(&self.room_id, &confirmation)?;
            if !confirmation.executions.is_empty() {
                continue 'stabilize;
            }
            if confirmation.latest_command_seq != cursor {
                return Err(AdapterError::internal(
                    "remote event timeline changed without returning the next execution",
                ));
            }
            break 'stabilize (market_time_ms, book_snapshots);
        };

        projection.advance_time(market_time_ms);
        for (binding, snapshot) in book_snapshots {
            projection.record_book(&binding, snapshot, market_time_ms, initial)?;
        }

        self.projection = projection;
        match &mut self.backend {
            SessionBackend::Remote {
                after_command_seq, ..
            } => *after_command_seq = cursor,
            SessionBackend::Embedded { .. } => unreachable!("remote backend was checked above"),
        }
        Ok(sync)
    }
}

fn execution_events(execution: &ActorExecution) -> Option<&[exchange_core::log::EventRecord]> {
    match &execution.result {
        ActorExecutionResult::Accepted(MarketExecution::Spot(result)) => Some(&result.events),
        ActorExecutionResult::Accepted(MarketExecution::Perp(result)) => Some(&result.events),
        ActorExecutionResult::Rejected(_) => None,
    }
}

fn bindings_from_scenario(
    scenario: &ScenarioConfig,
) -> Result<BTreeMap<(String, String), InstrumentBinding>, AdapterError> {
    let mut bindings = BTreeMap::new();
    for market in std::iter::once(&scenario.market).chain(scenario.extra_markets.iter()) {
        let binding = InstrumentBinding::from_market(market)?;
        let provider_key = (binding.market_type.clone(), binding.symbol.clone());
        if bindings.insert(provider_key, binding).is_some() {
            return Err(AdapterError::invalid_at(
                "scenario",
                "CandleScope requires symbols to be unique within each market type",
            ));
        }
    }
    Ok(bindings)
}

fn ensure_remote_room_visible(
    client: &HttpTradingClient,
    room_id: &str,
) -> Result<(), AdapterError> {
    let mut cursor: Option<String> = None;
    loop {
        let page = match cursor.as_deref() {
            Some(after_room_id) => {
                client.cluster_rooms_after(after_room_id, REMOTE_DIRECTORY_PAGE_LIMIT)
            }
            None => client.cluster_rooms(REMOTE_DIRECTORY_PAGE_LIMIT),
        }
        .map_err(|error| remote_error("read cluster room directory", error))?;
        if page.rooms.iter().any(|route| route.room_id == room_id) {
            return Ok(());
        }
        if !page.has_more {
            return Err(AdapterError::not_found(
                "ROOM_NOT_FOUND",
                format!("remote room {room_id} is not visible to the configured identity"),
            ));
        }
        let next = page.next_after_room_id.ok_or_else(|| {
            AdapterError::internal(
                "cluster room directory reported more data without a continuation cursor",
            )
        })?;
        if cursor
            .as_deref()
            .is_some_and(|current| next.as_str() <= current)
        {
            return Err(AdapterError::internal(
                "cluster room directory continuation cursor did not advance",
            ));
        }
        cursor = Some(next);
    }
}

fn validate_remote_execution_cursor(
    room_id: &str,
    cursor: Option<u64>,
    execution: &RoomExecutionSummary,
) -> Result<(), AdapterError> {
    if execution.room_id != room_id {
        return Err(AdapterError::internal(format!(
            "remote event for room {} appeared in the {room_id} stream",
            execution.room_id
        )));
    }
    let expected = match cursor {
        Some(cursor) => cursor.checked_add(1).ok_or_else(|| {
            AdapterError::invalid_state(
                "SEQUENCE_EXHAUSTED",
                "remote room command sequence is exhausted",
            )
        })?,
        None => 0,
    };
    if execution.command_seq != expected {
        return Err(AdapterError::invalid_state(
            "REMOTE_EVENT_GAP",
            format!(
                "remote room {room_id} expected command {expected}, received {}",
                execution.command_seq
            ),
        ));
    }
    Ok(())
}

fn validate_remote_event_page(
    room_id: &str,
    page: &RoomEventsResponse,
) -> Result<(), AdapterError> {
    if page.room_id != room_id {
        return Err(AdapterError::internal(format!(
            "remote backend returned events for room {}, expected {room_id}",
            page.room_id
        )));
    }
    if page.has_more && page.executions.is_empty() {
        return Err(AdapterError::internal(
            "remote event page reported more data without advancing the cursor",
        ));
    }
    Ok(())
}

fn required_remote_market_time(execution: &RoomExecutionSummary) -> Result<u64, AdapterError> {
    execution.market_time_ms.ok_or_else(|| {
        AdapterError::invalid_state(
            "REMOTE_EVENT_TIME_MISSING",
            format!(
                "remote execution {} has no authoritative market_time_ms",
                execution.command_seq
            ),
        )
    })
}

fn trade_from_summary(event: &EventSummary) -> Option<Trade> {
    let EventSummary::TradePrinted {
        trade_id,
        maker_order_id,
        maker_account_id,
        taker_order_id,
        taker_account_id,
        price_tick,
        qty,
        taker_side,
        ..
    } = event
    else {
        return None;
    };
    Some(Trade {
        maker_position_side: Default::default(),
        taker_position_side: Default::default(),
        trade_id: *trade_id,
        maker_order_id: *maker_order_id,
        maker_account_id: *maker_account_id,
        taker_order_id: *taker_order_id,
        taker_account_id: *taker_account_id,
        price_tick: *price_tick,
        qty: *qty,
        taker_side: *taker_side,
    })
}

fn remote_error(action: &str, error: HttpTradingError) -> AdapterError {
    match error {
        HttpTradingError::Api { status: 404, error } => AdapterError::not_found(
            "REMOTE_RESOURCE_NOT_FOUND",
            format!("could not {action}: {error}"),
        ),
        error => AdapterError::core(format!(
            "could not {action} through remote backend: {error}"
        )),
    }
}

fn optional_env(name: &str) -> Result<Option<String>, AdapterError> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(AdapterError::invalid_at(
            name,
            format!("environment value is not valid UTF-8: {error}"),
        )),
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct BarKey {
    market_type: String,
    symbol: String,
    interval_ms: u64,
}

impl BarKey {
    fn new(market_type: &str, symbol: &str, interval_ms: u64) -> Self {
        Self {
            market_type: market_type.to_string(),
            symbol: symbol.to_string(),
            interval_ms,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BarFinality {
    Forming,
    Final,
}

impl BarFinality {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Forming => "forming",
            Self::Final => "final",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectedBar {
    pub(crate) open_time_ms: u64,
    pub(crate) close_time_ms: u64,
    pub(crate) open: f64,
    pub(crate) high: f64,
    pub(crate) low: f64,
    pub(crate) close: f64,
    pub(crate) volume: f64,
    pub(crate) quote_volume: f64,
    pub(crate) trades: u64,
    pub(crate) taker_buy_base: f64,
    pub(crate) taker_buy_quote: f64,
    pub(crate) event_time_ms: u64,
    pub(crate) sequence: u64,
    #[serde(skip)]
    pub(crate) finality: BarFinality,
}

impl ProjectedBar {
    pub(crate) fn to_wire(&self) -> serde_json::Value {
        serde_json::json!({
            "openTimeMs": self.open_time_ms,
            "closeTimeMs": self.close_time_ms,
            "open": self.open,
            "high": self.high,
            "low": self.low,
            "close": self.close,
            "volume": self.volume,
            "quoteVolume": self.quote_volume,
            "trades": self.trades,
            "takerBuyBase": self.taker_buy_base,
            "takerBuyQuote": self.taker_buy_quote,
            "eventTimeMs": self.event_time_ms,
            "sequence": self.sequence,
            "finality": self.finality.as_str(),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
struct BookView {
    bids: Vec<(i64, u64)>,
    asks: Vec<(i64, u64)>,
}

impl BookView {
    fn from_snapshot(snapshot: BookSnapshot) -> Self {
        Self {
            bids: snapshot
                .bids
                .into_iter()
                .take(MAX_DEPTH_LEVELS)
                .map(|level| (level.price_tick, level.qty))
                .collect(),
            asks: snapshot
                .asks
                .into_iter()
                .take(MAX_DEPTH_LEVELS)
                .map(|level| (level.price_tick, level.qty))
                .collect(),
        }
    }

    fn is_two_sided(&self) -> bool {
        !self.bids.is_empty() && !self.asks.is_empty()
    }
}

#[derive(Clone, Debug)]
struct BookProjection {
    view: BookView,
    update_id: u64,
    event_time_ms: u64,
}

impl BookProjection {
    fn snapshot_payload(&self) -> Option<BookSnapshotPayload> {
        self.view.is_two_sided().then(|| BookSnapshotPayload {
            last_update_id: self.update_id,
            event_time_ms: self.event_time_ms,
            bids: wire_levels(&self.view.bids),
            asks: wire_levels(&self.view.asks),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BookSnapshotPayload {
    pub(crate) last_update_id: u64,
    pub(crate) event_time_ms: u64,
    pub(crate) bids: Vec<[f64; 2]>,
    pub(crate) asks: Vec<[f64; 2]>,
}

impl BookSnapshotPayload {
    pub(crate) fn to_wire(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": "snapshot",
            "lastUpdateId": self.last_update_id,
            "eventTimeMs": self.event_time_ms,
            "bids": self.bids,
            "asks": self.asks,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BookDeltaPayload {
    pub(crate) first_update_id: u64,
    pub(crate) final_update_id: u64,
    pub(crate) previous_final_update_id: u64,
    pub(crate) event_time_ms: u64,
    pub(crate) bids: Vec<[f64; 2]>,
    pub(crate) asks: Vec<[f64; 2]>,
}

impl BookDeltaPayload {
    pub(crate) fn to_wire(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": "delta",
            "firstUpdateId": self.first_update_id,
            "finalUpdateId": self.final_update_id,
            "previousFinalUpdateId": self.previous_final_update_id,
            "eventTimeMs": self.event_time_ms,
            "bids": self.bids,
            "asks": self.asks,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ProjectionPayload {
    Bar(ProjectedBar),
    BookDelta(BookDeltaPayload),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProjectionEvent {
    pub(crate) market_type: String,
    pub(crate) symbol: String,
    pub(crate) interval: Option<String>,
    pub(crate) event_type: &'static str,
    pub(crate) event_time_ms: u64,
    pub(crate) payload: ProjectionPayload,
}

impl ProjectionEvent {
    pub(crate) fn matches(
        &self,
        market_type: &str,
        symbol: &str,
        channel: &str,
        interval: Option<&str>,
    ) -> bool {
        self.market_type == market_type
            && self.symbol == symbol
            && match channel {
                "kline" => self.interval.as_deref() == interval,
                "full_depth" => self.interval.is_none(),
                _ => false,
            }
    }

    pub(crate) fn payload_wire(&self) -> serde_json::Value {
        match &self.payload {
            ProjectionPayload::Bar(bar) => bar.to_wire(),
            ProjectionPayload::BookDelta(book) => book.to_wire(),
        }
    }
}

#[derive(Clone, Debug)]
struct Projection {
    current_time_ms: u64,
    next_trade_sequence: u64,
    bars: BTreeMap<BarKey, BTreeMap<u64, ProjectedBar>>,
    books: BTreeMap<String, BookProjection>,
    events: Vec<ProjectionEvent>,
}

impl Projection {
    fn new(current_time_ms: u64) -> Self {
        Self {
            current_time_ms,
            next_trade_sequence: 1,
            bars: BTreeMap::new(),
            books: BTreeMap::new(),
            events: Vec::new(),
        }
    }

    fn advance_time(&mut self, market_time_ms: u64) {
        self.current_time_ms = self.current_time_ms.max(market_time_ms);
        let closing = self
            .bars
            .iter()
            .flat_map(|(key, bars)| {
                bars.iter()
                    .filter(|(_, bar)| {
                        bar.finality == BarFinality::Forming
                            && bar.close_time_ms < self.current_time_ms
                    })
                    .map(|(open_time_ms, _)| (key.clone(), *open_time_ms))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        for (key, open_time_ms) in closing {
            let bar = self
                .bars
                .get_mut(&key)
                .and_then(|bars| bars.get_mut(&open_time_ms))
                .expect("closing bar was selected from the projection");
            bar.finality = BarFinality::Final;
            bar.event_time_ms = self.current_time_ms;
            let closed = bar.clone();
            self.events.push(ProjectionEvent {
                market_type: key.market_type.clone(),
                symbol: key.symbol.clone(),
                interval: Some(interval_name(key.interval_ms).to_string()),
                event_type: "bar.closed",
                event_time_ms: self.current_time_ms,
                payload: ProjectionPayload::Bar(closed),
            });
        }
    }

    fn record_trade(
        &mut self,
        binding: &InstrumentBinding,
        trade: &Trade,
        market_time_ms: u64,
    ) -> Result<(), AdapterError> {
        let price = trade.price_tick as f64;
        let quantity = trade.qty as f64;
        if trade.price_tick <= 0
            || trade.price_tick.unsigned_abs() > MAX_SAFE_INTEGER
            || trade.qty > MAX_SAFE_INTEGER
            || !price.is_finite()
            || !quantity.is_finite()
        {
            return Err(AdapterError::invalid(
                "MarketForge trade cannot be represented by the CandleScope numeric contract",
            ));
        }
        let maximum_duration = interval_ms("1h")?;
        let maximum_open_time_ms = market_time_ms / maximum_duration * maximum_duration;
        if maximum_open_time_ms
            .checked_add(maximum_duration - 1)
            .is_none_or(|close_time_ms| close_time_ms > MAX_SAFE_INTEGER)
        {
            return Err(AdapterError::invalid(
                "bar close time exceeds the CandleScope safe-integer range",
            ));
        }
        if self.next_trade_sequence > MAX_SAFE_INTEGER {
            return Err(AdapterError::invalid_state(
                "SEQUENCE_EXHAUSTED",
                "CandleScope safe-integer trade sequence is exhausted",
            ));
        }
        let sequence = self.next_trade_sequence;
        self.advance_time(market_time_ms);
        self.next_trade_sequence += 1;

        for &interval in SUPPORTED_INTERVALS {
            let duration = interval_ms(interval)?;
            let open_time_ms = market_time_ms / duration * duration;
            let close_time_ms = open_time_ms
                .checked_add(duration - 1)
                .ok_or_else(|| AdapterError::invalid("bar close time overflowed"))?;
            if close_time_ms > MAX_SAFE_INTEGER {
                return Err(AdapterError::invalid(
                    "bar close time exceeds the CandleScope safe-integer range",
                ));
            }
            let key = BarKey::new(&binding.market_type, &binding.symbol, duration);
            let bars = self.bars.entry(key.clone()).or_default();
            let bar = bars.entry(open_time_ms).or_insert_with(|| ProjectedBar {
                open_time_ms,
                close_time_ms,
                open: price,
                high: price,
                low: price,
                close: price,
                volume: 0.0,
                quote_volume: 0.0,
                trades: 0,
                taker_buy_base: 0.0,
                taker_buy_quote: 0.0,
                event_time_ms: market_time_ms,
                sequence,
                finality: BarFinality::Forming,
            });
            bar.high = bar.high.max(price);
            bar.low = bar.low.min(price);
            bar.close = price;
            bar.volume += quantity;
            bar.quote_volume += quantity * price;
            bar.trades = bar
                .trades
                .checked_add(1)
                .ok_or_else(|| AdapterError::invalid("bar trade count overflowed"))?;
            if trade.taker_side == Side::Buy {
                bar.taker_buy_base += quantity;
                bar.taker_buy_quote += quantity * price;
            }
            if !bar.volume.is_finite()
                || !bar.quote_volume.is_finite()
                || !bar.taker_buy_base.is_finite()
                || !bar.taker_buy_quote.is_finite()
            {
                return Err(AdapterError::invalid(
                    "bar totals cannot be represented by the CandleScope numeric contract",
                ));
            }
            bar.event_time_ms = market_time_ms;
            bar.sequence = sequence;
            let updated = bar.clone();
            self.events.push(ProjectionEvent {
                market_type: key.market_type,
                symbol: key.symbol,
                interval: Some(interval.to_string()),
                event_type: "bar.updated",
                event_time_ms: market_time_ms,
                payload: ProjectionPayload::Bar(updated),
            });
        }
        Ok(())
    }

    fn record_book(
        &mut self,
        binding: &InstrumentBinding,
        snapshot: BookSnapshot,
        market_time_ms: u64,
        initial: bool,
    ) -> Result<(), AdapterError> {
        self.current_time_ms = self.current_time_ms.max(market_time_ms);
        let next_view = BookView::from_snapshot(snapshot);
        if initial || !self.books.contains_key(&binding.instrument_id) {
            self.books.insert(
                binding.instrument_id.clone(),
                BookProjection {
                    view: next_view,
                    update_id: 1,
                    event_time_ms: market_time_ms,
                },
            );
            return Ok(());
        }

        let current = self
            .books
            .get_mut(&binding.instrument_id)
            .expect("book existence was checked");
        let bids = changed_levels(&current.view.bids, &next_view.bids, true);
        let asks = changed_levels(&current.view.asks, &next_view.asks, false);
        if bids.is_empty() && asks.is_empty() {
            current.view = next_view;
            current.event_time_ms = market_time_ms;
            return Ok(());
        }
        let previous = current.update_id;
        let next_update_id = current.update_id.checked_add(1).ok_or_else(|| {
            AdapterError::invalid_state(
                "SEQUENCE_EXHAUSTED",
                "CandleScope order-book update sequence is exhausted",
            )
        })?;
        if next_update_id > MAX_SAFE_INTEGER {
            return Err(AdapterError::invalid_state(
                "SEQUENCE_EXHAUSTED",
                "CandleScope order-book update sequence is exhausted",
            ));
        }
        current.view = next_view;
        current.event_time_ms = market_time_ms;
        current.update_id = next_update_id;
        self.events.push(ProjectionEvent {
            market_type: binding.market_type.clone(),
            symbol: binding.symbol.clone(),
            interval: None,
            event_type: "orderbook.delta",
            event_time_ms: market_time_ms,
            payload: ProjectionPayload::BookDelta(BookDeltaPayload {
                first_update_id: current.update_id,
                final_update_id: current.update_id,
                previous_final_update_id: previous,
                event_time_ms: market_time_ms,
                bids,
                asks,
            }),
        });
        Ok(())
    }
}

fn changed_levels(
    previous: &[(i64, u64)],
    current: &[(i64, u64)],
    descending: bool,
) -> Vec<[f64; 2]> {
    let previous = previous.iter().copied().collect::<BTreeMap<_, _>>();
    let current = current.iter().copied().collect::<BTreeMap<_, _>>();
    let prices = previous
        .keys()
        .chain(current.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    let mut changed = prices
        .into_iter()
        .filter_map(|price| {
            let old_quantity = previous.get(&price).copied().unwrap_or_default();
            let new_quantity = current.get(&price).copied().unwrap_or_default();
            (old_quantity != new_quantity).then_some([price as f64, new_quantity as f64])
        })
        .collect::<Vec<_>>();
    if descending {
        changed.reverse();
    }
    changed
}

fn wire_levels(levels: &[(i64, u64)]) -> Vec<[f64; 2]> {
    levels
        .iter()
        .map(|(price, quantity)| [*price as f64, *quantity as f64])
        .collect()
}

pub(crate) fn interval_ms(interval: &str) -> Result<u64, AdapterError> {
    match interval {
        "1s" => Ok(1_000),
        "1m" => Ok(60_000),
        "5m" => Ok(300_000),
        "15m" => Ok(900_000),
        "1h" => Ok(3_600_000),
        _ => Err(AdapterError::invalid_at(
            "descriptor.interval",
            format!("unsupported MarketForge interval {interval}"),
        )),
    }
}

fn interval_name(interval_ms: u64) -> &'static str {
    match interval_ms {
        1_000 => "1s",
        60_000 => "1m",
        300_000 => "5m",
        900_000 => "15m",
        3_600_000 => "1h",
        _ => unreachable!("only supported intervals are projected"),
    }
}

fn absolute_time(epoch_ms: u64, market_time_ms: u64) -> Result<u64, AdapterError> {
    let value = epoch_ms
        .checked_add(market_time_ms)
        .ok_or_else(|| AdapterError::invalid("simulation time overflowed"))?;
    if value > MAX_SAFE_INTEGER {
        return Err(AdapterError::invalid(
            "simulation time exceeds the CandleScope safe-integer range",
        ));
    }
    Ok(value)
}

fn market_type(kind: MarketKind) -> &'static str {
    match kind {
        MarketKind::Spot => "spot",
        MarketKind::Perp => "perp",
    }
}

fn status_name(status: MarketStatus) -> &'static str {
    match status {
        MarketStatus::Running => "running",
        MarketStatus::Paused => "paused",
        MarketStatus::Closed => "closed",
    }
}

fn validate_symbol(symbol: &str) -> Result<(), AdapterError> {
    let mut characters = symbol.chars();
    let first = characters.next();
    let valid_first =
        first.is_some_and(|character| character.is_ascii_uppercase() || character.is_ascii_digit());
    let valid_rest = characters.all(|character| {
        character.is_ascii_uppercase()
            || character.is_ascii_digit()
            || matches!(character, '.' | '_' | ':' | '-')
    });
    if symbol.len() > 64 || !valid_first || !valid_rest {
        return Err(AdapterError::invalid_at(
            "scenario.market.instrument.symbol",
            format!("{symbol:?} is not a canonical CandleScope provider symbol"),
        ));
    }
    Ok(())
}

fn validate_asset(path: &str, asset: &str) -> Result<(), AdapterError> {
    if asset.is_empty() || asset.len() > 32 || asset != asset.to_ascii_uppercase() {
        return Err(AdapterError::invalid_at(
            path,
            format!("asset {asset:?} must be a non-empty uppercase identifier"),
        ));
    }
    Ok(())
}
