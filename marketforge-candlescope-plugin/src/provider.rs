use std::collections::{BTreeMap, BTreeSet, VecDeque};

use exchange_core::{Command, ScenarioConfig};
use serde_json::{Map, Value, json};

use crate::{
    adapter::{
        BookSnapshotPayload, MarketForgeAdapter, ProjectedBar, ProjectionEvent, ProjectionPayload,
        RemoteBackendConfig,
    },
    error::AdapterError,
};

pub use crate::adapter::SUPPORTED_INTERVALS;

pub const EXCHANGE_ID: &str = "marketforge";
pub const SYMBOLS_CONTRIBUTION_ID: &str = "symbols";
pub const MARKET_DATA_CONTRIBUTION_ID: &str = "market-data";
pub const MARKETFORGE_CONTROL_CONTRIBUTION_ID: &str = "marketforge-control";

const SYMBOLS_SCHEMA: &str = "candlescope.provider-symbols-page/1";
const HISTORY_SCHEMA: &str = "candlescope.provider-history-page/1";
const STREAM_OPEN_SCHEMA: &str = "candlescope.provider-stream-open/1";
const STREAM_BATCH_SCHEMA: &str = "candlescope.provider-stream-batch/1";
const STREAM_CLOSE_SCHEMA: &str = "candlescope.provider-stream-close/1";
const MAX_STREAMS: usize = 8;
const MAX_SYMBOL_PAGE: u64 = 100;
const MAX_HISTORY_PAGE: u64 = 500;
const MAX_STREAM_BATCH: u64 = 32;

fn source_quality() -> Value {
    json!({
        "quality": "authoritative",
        "finality": "explicit",
        "timestamp": "exchange",
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StreamDescriptor {
    exchange: String,
    market_type: String,
    channel: String,
    symbol: String,
    interval: Option<String>,
}

impl StreamDescriptor {
    fn from_wire(value: &Value) -> Result<Self, AdapterError> {
        let object = exact_object(
            value,
            "descriptor",
            &["exchange", "marketType", "channel", "symbol"],
            &["interval"],
        )?;
        let descriptor = Self {
            exchange: string_field(object, "exchange", "descriptor.exchange")?.to_string(),
            market_type: string_field(object, "marketType", "descriptor.marketType")?.to_string(),
            channel: string_field(object, "channel", "descriptor.channel")?.to_string(),
            symbol: string_field(object, "symbol", "descriptor.symbol")?.to_string(),
            interval: optional_string_field(object, "interval", "descriptor.interval")?
                .map(str::to_string),
        };
        if descriptor.exchange != EXCHANGE_ID {
            return Err(AdapterError::invalid_at(
                "descriptor.exchange",
                "descriptor does not target the MarketForge provider",
            ));
        }
        if !matches!(descriptor.market_type.as_str(), "spot" | "perp") {
            return Err(AdapterError::invalid_at(
                "descriptor.marketType",
                "MarketForge supports spot and perp provider markets",
            ));
        }
        match descriptor.channel.as_str() {
            "kline" => {
                let interval = descriptor.interval.as_deref().ok_or_else(|| {
                    AdapterError::invalid_at(
                        "descriptor.interval",
                        "kline descriptors require an interval",
                    )
                })?;
                if !SUPPORTED_INTERVALS.contains(&interval) {
                    return Err(AdapterError::invalid_at(
                        "descriptor.interval",
                        format!("unsupported MarketForge interval {interval}"),
                    ));
                }
            }
            "full_depth" => {
                if descriptor.interval.is_some() {
                    return Err(AdapterError::invalid_at(
                        "descriptor.interval",
                        "full_depth descriptors must not carry an interval",
                    ));
                }
            }
            _ => {
                return Err(AdapterError::invalid_at(
                    "descriptor.channel",
                    "MarketForge supports kline and full_depth channels",
                ));
            }
        }
        Ok(descriptor)
    }

    fn to_wire(&self) -> Value {
        let mut value = json!({
            "exchange": self.exchange,
            "marketType": self.market_type,
            "channel": self.channel,
            "symbol": self.symbol,
        });
        if let Some(interval) = &self.interval {
            value
                .as_object_mut()
                .expect("descriptor is an object")
                .insert("interval".to_string(), Value::String(interval.clone()));
        }
        value
    }

    fn matches(&self, event: &ProjectionEvent) -> bool {
        event.matches(
            &self.market_type,
            &self.symbol,
            &self.channel,
            self.interval.as_deref(),
        )
    }
}

#[derive(Clone, Debug)]
enum PendingPayload {
    Bar(ProjectedBar),
    BookSnapshot(BookSnapshotPayload),
}

#[derive(Clone, Debug)]
struct PendingEvent {
    event_type: &'static str,
    event_time_ms: u64,
    payload: PendingPayload,
}

impl PendingEvent {
    fn to_wire(&self, sequence: u64, descriptor: &StreamDescriptor) -> Value {
        let payload = match &self.payload {
            PendingPayload::Bar(bar) => bar.to_wire(),
            PendingPayload::BookSnapshot(book) => book.to_wire(),
        };
        json!({
            "sequence": sequence,
            "eventType": self.event_type,
            "descriptor": descriptor.to_wire(),
            "eventTimeMs": self.event_time_ms,
            "payload": payload,
        })
    }
}

#[derive(Clone, Debug)]
struct StreamState {
    descriptor: StreamDescriptor,
    generation: u64,
    batch_limit: usize,
    next_sequence: u64,
    projection_cursor: usize,
    pending: VecDeque<PendingEvent>,
    depth_initialized: bool,
}

#[derive(Clone, Debug, Default)]
pub struct PluginService {
    adapter: MarketForgeAdapter,
    generation: u64,
    next_stream_id: u64,
    streams: BTreeMap<String, StreamState>,
}

impl PluginService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_remote_backend(config: RemoteBackendConfig) -> Self {
        Self {
            adapter: MarketForgeAdapter::with_remote_backend(config),
            ..Self::default()
        }
    }

    pub fn adapter(&self) -> &MarketForgeAdapter {
        &self.adapter
    }

    pub fn adapter_mut(&mut self) -> &mut MarketForgeAdapter {
        &mut self.adapter
    }

    pub fn activate(&mut self, generation: u64) {
        self.generation = generation;
        self.streams.clear();
    }

    pub fn deactivate(&mut self) {
        self.generation = 0;
        self.streams.clear();
    }

    pub fn prepare_upgrade(&mut self) {
        self.streams.clear();
    }

    pub fn health_check(&self) -> Value {
        json!({
            "status": "ready",
            "provider": EXCHANGE_ID,
            "sessionLoaded": self.adapter.has_session(),
            "sessionMode": self.adapter.session_mode(),
            "remoteBackendConfigured": self.adapter.remote_backend_configured(),
            "openStreams": self.streams.len(),
        })
    }

    pub fn invoke(
        &mut self,
        contribution_id: &str,
        input: &Value,
        user_action: bool,
    ) -> Result<Value, AdapterError> {
        match contribution_id {
            SYMBOLS_CONTRIBUTION_ID => self.symbols(input),
            MARKET_DATA_CONTRIBUTION_ID => self.market_data(input),
            MARKETFORGE_CONTROL_CONTRIBUTION_ID => self.control(input, user_action),
            _ => Err(AdapterError::invalid_at(
                "contributionId",
                format!("unknown contribution {contribution_id}"),
            )),
        }
    }

    fn symbols(&self, input: &Value) -> Result<Value, AdapterError> {
        let object = exact_object(
            input,
            "provider.symbols.input",
            &["operation", "marketType", "limit"],
            &["quoteAsset", "search", "cursor"],
        )?;
        require_operation(object, "symbols.list")?;
        let market_type = string_field(object, "marketType", "marketType")?;
        if !matches!(market_type, "spot" | "perp") {
            return Err(AdapterError::invalid_at(
                "marketType",
                "marketType must be spot or perp",
            ));
        }
        let limit = integer_field(object, "limit", 1, MAX_SYMBOL_PAGE, "limit")? as usize;
        let quote_asset = optional_string_field(object, "quoteAsset", "quoteAsset")?;
        if quote_asset.is_some_and(|asset| asset != asset.to_ascii_uppercase()) {
            return Err(AdapterError::invalid_at(
                "quoteAsset",
                "quoteAsset must be uppercase",
            ));
        }
        let search = optional_string_field(object, "search", "search")?
            .map(|value| value.to_ascii_uppercase());
        let cursor = optional_string_field(object, "cursor", "cursor")?;

        let rows = self
            .adapter
            .instruments()
            .into_iter()
            .filter(|binding| binding.market_type == market_type)
            .filter(|binding| quote_asset.is_none_or(|asset| binding.quote_asset.as_str() == asset))
            .filter(|binding| {
                search
                    .as_deref()
                    .is_none_or(|needle| binding.symbol.contains(needle))
            })
            .filter(|binding| cursor.is_none_or(|cursor| binding.symbol.as_str() > cursor))
            .collect::<Vec<_>>();
        let exhausted = rows.len() <= limit;
        let page = rows.into_iter().take(limit).collect::<Vec<_>>();
        let next_cursor = (!exhausted)
            .then(|| page.last().map(|binding| binding.symbol.clone()))
            .flatten();
        let symbols = page
            .into_iter()
            .map(|binding| {
                json!({
                    "symbol": binding.symbol,
                    "baseAsset": binding.base_asset,
                    "quoteAsset": binding.quote_asset,
                    "status": "active",
                    "exchange": EXCHANGE_ID,
                    "marketType": binding.market_type,
                    "productType": binding.product_type,
                    "priceTickSize": binding.price_tick_size,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "schemaVersion": SYMBOLS_SCHEMA,
            "exchange": EXCHANGE_ID,
            "marketType": market_type,
            "symbols": symbols,
            "nextCursor": next_cursor,
            "exhausted": exhausted,
            "sourceQuality": source_quality(),
        }))
    }

    fn market_data(&mut self, input: &Value) -> Result<Value, AdapterError> {
        let object = object(input, "provider.input")?;
        match string_field(object, "operation", "operation")? {
            "history.read" => self.history(input),
            "stream.open" => self.open_stream(input),
            "stream.poll" => self.poll_stream(input),
            "stream.close" => self.close_stream(input),
            operation => Err(AdapterError::invalid_at(
                "operation",
                format!("unsupported market-data operation {operation}"),
            )),
        }
    }

    fn history(&mut self, input: &Value) -> Result<Value, AdapterError> {
        let object = exact_object(
            input,
            "provider.history.input",
            &["operation", "descriptor", "startMs", "endMs", "limit"],
            &[],
        )?;
        require_operation(object, "history.read")?;
        let descriptor = StreamDescriptor::from_wire(
            object
                .get("descriptor")
                .expect("exact object requires descriptor"),
        )?;
        if descriptor.channel != "kline" {
            return Err(AdapterError::invalid_at(
                "descriptor.channel",
                "history is available for kline descriptors only",
            ));
        }
        let start_ms = nullable_integer_field(object, "startMs", 0, "startMs")?;
        let end_ms = nullable_integer_field(object, "endMs", 0, "endMs")?;
        if start_ms.zip(end_ms).is_some_and(|(start, end)| start > end) {
            return Err(AdapterError::invalid_at(
                "startMs",
                "startMs must not exceed endMs",
            ));
        }
        let limit = integer_field(object, "limit", 1, MAX_HISTORY_PAGE, "limit")? as usize;
        self.adapter.refresh_remote(0)?;
        let mut eligible = self.adapter.final_bars(
            &descriptor.market_type,
            &descriptor.symbol,
            descriptor
                .interval
                .as_deref()
                .expect("kline descriptor has an interval"),
        )?;
        eligible.retain(|bar| {
            start_ms.is_none_or(|start| bar.open_time_ms >= start)
                && end_ms.is_none_or(|end| bar.open_time_ms <= end)
        });
        let exhausted = eligible.len() <= limit;
        let split_at = eligible.len().saturating_sub(limit);
        let page = eligible.split_off(split_at);
        let next_before_ms = (!exhausted)
            .then(|| page.first().map(|bar| bar.open_time_ms - 1))
            .flatten();
        let rows = page
            .into_iter()
            .map(|bar| bar.to_wire())
            .collect::<Vec<_>>();
        Ok(json!({
            "schemaVersion": HISTORY_SCHEMA,
            "descriptor": descriptor.to_wire(),
            "rows": rows,
            "nextBeforeMs": next_before_ms,
            "exhausted": exhausted,
            "sourceQuality": source_quality(),
        }))
    }

    fn open_stream(&mut self, input: &Value) -> Result<Value, AdapterError> {
        let object = exact_object(
            input,
            "provider.stream.open.input",
            &[
                "operation",
                "hostStreamId",
                "descriptor",
                "batchLimit",
                "resync",
            ],
            &[],
        )?;
        require_operation(object, "stream.open")?;
        self.adapter.refresh_remote(0)?;
        if self.streams.len() >= MAX_STREAMS {
            return Err(AdapterError::invalid_state(
                "STREAM_LIMIT_REACHED",
                format!("MarketForge allows at most {MAX_STREAMS} provider streams"),
            ));
        }
        let host_stream_id = opaque_id_field(object, "hostStreamId", "hostStreamId")?;
        let descriptor = StreamDescriptor::from_wire(
            object
                .get("descriptor")
                .expect("exact object requires descriptor"),
        )?;
        self.adapter
            .binding(&descriptor.market_type, &descriptor.symbol)?;
        let batch_limit =
            integer_field(object, "batchLimit", 1, MAX_STREAM_BATCH, "batchLimit")? as usize;
        let _resync = bool_field(object, "resync", "resync")?;
        self.next_stream_id = self.next_stream_id.checked_add(1).ok_or_else(|| {
            AdapterError::invalid_state(
                "SEQUENCE_EXHAUSTED",
                "CandleScope provider stream ID sequence is exhausted",
            )
        })?;
        if self.next_stream_id > crate::adapter::MAX_SAFE_INTEGER {
            return Err(AdapterError::invalid_state(
                "SEQUENCE_EXHAUSTED",
                "CandleScope provider stream ID sequence is exhausted",
            ));
        }
        let provider_stream_id = format!("mf-stream-{}-{}", self.generation, self.next_stream_id);
        let mut pending = VecDeque::new();
        let mut depth_initialized = false;
        if descriptor.channel == "kline" {
            if let Some(bar) = self.adapter.latest_forming_bar(
                &descriptor.market_type,
                &descriptor.symbol,
                descriptor
                    .interval
                    .as_deref()
                    .expect("kline descriptor has an interval"),
            )? {
                pending.push_back(PendingEvent {
                    event_type: "bar.updated",
                    event_time_ms: bar.event_time_ms,
                    payload: PendingPayload::Bar(bar),
                });
            }
        } else if let Some(snapshot) = self
            .adapter
            .current_book_snapshot(&descriptor.market_type, &descriptor.symbol)?
        {
            pending.push_back(PendingEvent {
                event_type: "orderbook.snapshot",
                event_time_ms: snapshot.event_time_ms,
                payload: PendingPayload::BookSnapshot(snapshot),
            });
            depth_initialized = true;
        }
        let projection_cursor = self.adapter.projection_len();
        self.streams.insert(
            provider_stream_id.clone(),
            StreamState {
                descriptor,
                generation: self.generation,
                batch_limit,
                next_sequence: 1,
                projection_cursor,
                pending,
                depth_initialized,
            },
        );
        Ok(json!({
            "schemaVersion": STREAM_OPEN_SCHEMA,
            "hostStreamId": host_stream_id,
            "providerStreamId": provider_stream_id,
            "generation": self.generation,
            "nextSequence": 1,
            "sourceQuality": source_quality(),
        }))
    }

    fn poll_stream(&mut self, input: &Value) -> Result<Value, AdapterError> {
        let object = exact_object(
            input,
            "provider.stream.poll.input",
            &[
                "operation",
                "providerStreamId",
                "afterSequence",
                "batchLimit",
                "waitMs",
            ],
            &[],
        )?;
        require_operation(object, "stream.poll")?;
        let provider_stream_id =
            opaque_id_field(object, "providerStreamId", "providerStreamId")?.to_string();
        let after_sequence = integer_field(
            object,
            "afterSequence",
            0,
            crate::adapter::MAX_SAFE_INTEGER,
            "afterSequence",
        )?;
        let requested_limit =
            integer_field(object, "batchLimit", 1, MAX_STREAM_BATCH, "batchLimit")? as usize;
        let wait_ms = integer_field(object, "waitMs", 0, 5_000, "waitMs")?;

        let state = self.streams.get(&provider_stream_id).ok_or_else(|| {
            AdapterError::not_found(
                "STREAM_NOT_FOUND",
                format!("provider stream {provider_stream_id} is not open"),
            )
        })?;
        if state.generation != self.generation {
            return Err(AdapterError::invalid_state(
                "STALE_STREAM",
                "provider stream belongs to a stale plugin generation",
            ));
        }
        if after_sequence != state.next_sequence - 1 {
            return Err(AdapterError::invalid_at(
                "afterSequence",
                format!(
                    "expected {}, received {after_sequence}",
                    state.next_sequence - 1
                ),
            ));
        }
        let has_buffered_events =
            !state.pending.is_empty() || state.projection_cursor < self.adapter.projection_len();

        self.adapter
            .refresh_remote(if has_buffered_events { 0 } else { wait_ms })?;

        let adapter = &self.adapter;
        let state = self
            .streams
            .get_mut(&provider_stream_id)
            .expect("provider stream existence was checked before remote polling");
        let limit = requested_limit.min(state.batch_limit);
        if state.next_sequence > crate::adapter::MAX_SAFE_INTEGER
            || limit as u64 > crate::adapter::MAX_SAFE_INTEGER.saturating_sub(state.next_sequence)
        {
            return Err(AdapterError::invalid_state(
                "SEQUENCE_EXHAUSTED",
                "CandleScope stream event sequence is exhausted",
            ));
        }

        if state.descriptor.channel == "full_depth" && !state.depth_initialized {
            if let Some(snapshot) = adapter
                .current_book_snapshot(&state.descriptor.market_type, &state.descriptor.symbol)?
            {
                state.pending.push_back(PendingEvent {
                    event_type: "orderbook.snapshot",
                    event_time_ms: snapshot.event_time_ms,
                    payload: PendingPayload::BookSnapshot(snapshot),
                });
                state.depth_initialized = true;
            }
            state.projection_cursor = adapter.projection_len();
        }

        let first_sequence = state.next_sequence;
        let mut events = Vec::new();
        while events.len() < limit {
            let Some(event) = state.pending.pop_front() else {
                break;
            };
            events.push(event.to_wire(state.next_sequence, &state.descriptor));
            state.next_sequence += 1;
        }
        while events.len() < limit && state.projection_cursor < adapter.projection_len() {
            let event = adapter
                .projection_event(state.projection_cursor)
                .expect("projection cursor is within bounds");
            state.projection_cursor += 1;
            if !state.descriptor.matches(event) {
                continue;
            }
            events.push(projection_event_wire(
                event,
                state.next_sequence,
                &state.descriptor,
            ));
            state.next_sequence += 1;
        }
        let heartbeat = events.is_empty();
        Ok(json!({
            "schemaVersion": STREAM_BATCH_SCHEMA,
            "providerStreamId": provider_stream_id,
            "generation": state.generation,
            "firstSequence": first_sequence,
            "nextSequence": state.next_sequence,
            "events": events,
            "heartbeat": heartbeat,
            "sourceQuality": source_quality(),
        }))
    }

    fn close_stream(&mut self, input: &Value) -> Result<Value, AdapterError> {
        let object = exact_object(
            input,
            "provider.stream.close.input",
            &["operation", "providerStreamId"],
            &[],
        )?;
        require_operation(object, "stream.close")?;
        let provider_stream_id =
            opaque_id_field(object, "providerStreamId", "providerStreamId")?.to_string();
        let closed = self.streams.remove(&provider_stream_id).is_some();
        Ok(json!({
            "schemaVersion": STREAM_CLOSE_SCHEMA,
            "providerStreamId": provider_stream_id,
            "closed": closed,
        }))
    }

    fn control(&mut self, input: &Value, user_action: bool) -> Result<Value, AdapterError> {
        let object = object(input, "marketforge.control.input")?;
        let operation = string_field(object, "operation", "operation")?;
        match operation {
            "session.describe" => {
                exact_keys(object, "marketforge.control.input", &["operation"], &[])?;
                let session = self.adapter.describe_session()?;
                Ok(json!({"operation": operation, "session": session}))
            }
            "session.load" => {
                require_user_action(user_action)?;
                exact_keys(
                    object,
                    "marketforge.control.input",
                    &["operation", "scenario"],
                    &["epochMs"],
                )?;
                let scenario: ScenarioConfig = serde_json::from_value(
                    object
                        .get("scenario")
                        .expect("exact keys require scenario")
                        .clone(),
                )
                .map_err(|error| {
                    AdapterError::invalid_at(
                        "scenario",
                        format!("invalid MarketForge scenario: {error}"),
                    )
                })?;
                let epoch_ms = object
                    .get("epochMs")
                    .map(|_| {
                        integer_field(
                            object,
                            "epochMs",
                            0,
                            crate::adapter::MAX_SAFE_INTEGER,
                            "epochMs",
                        )
                    })
                    .transpose()?
                    .unwrap_or_default();
                let loaded = self.adapter.load_session(scenario, epoch_ms)?;
                self.streams.clear();
                Ok(json!({"operation": operation, "loaded": loaded}))
            }
            "session.attach" => {
                require_user_action(user_action)?;
                exact_keys(
                    object,
                    "marketforge.control.input",
                    &["operation", "scenario"],
                    &["epochMs"],
                )?;
                let scenario: ScenarioConfig = serde_json::from_value(
                    object
                        .get("scenario")
                        .expect("exact keys require scenario")
                        .clone(),
                )
                .map_err(|error| {
                    AdapterError::invalid_at(
                        "scenario",
                        format!("invalid MarketForge scenario: {error}"),
                    )
                })?;
                let epoch_ms = object
                    .get("epochMs")
                    .map(|_| {
                        integer_field(
                            object,
                            "epochMs",
                            0,
                            crate::adapter::MAX_SAFE_INTEGER,
                            "epochMs",
                        )
                    })
                    .transpose()?
                    .unwrap_or_default();
                let attached = self.adapter.attach_remote_session(scenario, epoch_ms)?;
                self.streams.clear();
                Ok(json!({"operation": operation, "attached": attached}))
            }
            "session.unload" => {
                require_user_action(user_action)?;
                exact_keys(object, "marketforge.control.input", &["operation"], &[])?;
                let unloaded = self.adapter.unload_session();
                self.streams.clear();
                Ok(json!({"operation": operation, "unloaded": unloaded}))
            }
            "command.apply" => {
                require_user_action(user_action)?;
                exact_keys(
                    object,
                    "marketforge.control.input",
                    &["operation", "roomId", "instrumentId", "command"],
                    &[],
                )?;
                let room_id = string_field(object, "roomId", "roomId")?;
                let instrument_id = string_field(object, "instrumentId", "instrumentId")?;
                let command: Command = serde_json::from_value(
                    object
                        .get("command")
                        .expect("exact keys require command")
                        .clone(),
                )
                .map_err(|error| {
                    AdapterError::invalid_at(
                        "command",
                        format!("invalid MarketForge command: {error}"),
                    )
                })?;
                let result = self
                    .adapter
                    .apply_command(room_id, instrument_id, command)?;
                Ok(json!({"operation": operation, "execution": result}))
            }
            "clock.advance" => {
                require_user_action(user_action)?;
                exact_keys(
                    object,
                    "marketforge.control.input",
                    &["operation", "roomId", "steps"],
                    &[],
                )?;
                let room_id = string_field(object, "roomId", "roomId")?;
                let steps = integer_field(
                    object,
                    "steps",
                    1,
                    crate::adapter::MAX_CLOCK_STEPS_PER_CALL,
                    "steps",
                )?;
                let market_time_ms = self.adapter.advance_clock(room_id, steps)?;
                Ok(json!({
                    "operation": operation,
                    "roomId": room_id,
                    "steps": steps,
                    "marketTimeMs": market_time_ms,
                }))
            }
            "room.pause" | "room.resume" | "room.close" => {
                require_user_action(user_action)?;
                exact_keys(
                    object,
                    "marketforge.control.input",
                    &["operation", "roomId"],
                    &[],
                )?;
                let room_id = string_field(object, "roomId", "roomId")?;
                match operation {
                    "room.pause" => self.adapter.pause(room_id)?,
                    "room.resume" => self.adapter.resume(room_id)?,
                    "room.close" => self.adapter.close(room_id)?,
                    _ => unreachable!(),
                }
                Ok(json!({"operation": operation, "roomId": room_id, "ok": true}))
            }
            _ => Err(AdapterError::invalid_at(
                "operation",
                format!("unsupported MarketForge control operation {operation}"),
            )),
        }
    }
}

fn projection_event_wire(
    event: &ProjectionEvent,
    sequence: u64,
    descriptor: &StreamDescriptor,
) -> Value {
    debug_assert!(descriptor.matches(event));
    let payload = match &event.payload {
        ProjectionPayload::Bar(_) | ProjectionPayload::BookDelta(_) => event.payload_wire(),
    };
    json!({
        "sequence": sequence,
        "eventType": event.event_type,
        "descriptor": descriptor.to_wire(),
        "eventTimeMs": event.event_time_ms,
        "payload": payload,
    })
}

fn require_user_action(user_action: bool) -> Result<(), AdapterError> {
    if user_action {
        Ok(())
    } else {
        Err(AdapterError::invalid_state(
            "USER_ACTION_REQUIRED",
            "MarketForge mutations require a Host-issued user action",
        ))
    }
}

fn object<'a>(value: &'a Value, path: &str) -> Result<&'a Map<String, Value>, AdapterError> {
    value
        .as_object()
        .ok_or_else(|| AdapterError::invalid_at(path, format!("{path} must be an object")))
}

fn exact_object<'a>(
    value: &'a Value,
    path: &str,
    required: &[&str],
    optional: &[&str],
) -> Result<&'a Map<String, Value>, AdapterError> {
    let object = object(value, path)?;
    exact_keys(object, path, required, optional)?;
    Ok(object)
}

fn exact_keys(
    object: &Map<String, Value>,
    path: &str,
    required: &[&str],
    optional: &[&str],
) -> Result<(), AdapterError> {
    let actual = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let required = required.iter().copied().collect::<BTreeSet<_>>();
    let allowed = required
        .iter()
        .copied()
        .chain(optional.iter().copied())
        .collect::<BTreeSet<_>>();
    let missing = required.difference(&actual).copied().collect::<Vec<_>>();
    let unknown = actual.difference(&allowed).copied().collect::<Vec<_>>();
    if !missing.is_empty() || !unknown.is_empty() {
        return Err(AdapterError::invalid_at(
            path,
            format!("invalid shape; missing={missing:?}, unknown={unknown:?}"),
        ));
    }
    Ok(())
}

fn require_operation(object: &Map<String, Value>, expected: &str) -> Result<(), AdapterError> {
    let actual = string_field(object, "operation", "operation")?;
    if actual == expected {
        Ok(())
    } else {
        Err(AdapterError::invalid_at(
            "operation",
            format!("expected operation {expected}, received {actual}"),
        ))
    }
}

fn string_field<'a>(
    object: &'a Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<&'a str, AdapterError> {
    let value = object
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| AdapterError::invalid_at(path, format!("{path} must be a string")))?;
    if value.is_empty() || value != value.trim() || value.len() > 256 {
        return Err(AdapterError::invalid_at(
            path,
            format!("{path} must be a bounded canonical string"),
        ));
    }
    Ok(value)
}

fn optional_string_field<'a>(
    object: &'a Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<Option<&'a str>, AdapterError> {
    match object.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => string_field(object, name, path).map(Some),
    }
}

fn opaque_id_field<'a>(
    object: &'a Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<&'a str, AdapterError> {
    let value = string_field(object, name, path)?;
    if value.len() > 128
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | ':' | '-')
        })
    {
        return Err(AdapterError::invalid_at(
            path,
            format!("{path} must be an opaque provider identifier"),
        ));
    }
    Ok(value)
}

fn integer_field(
    object: &Map<String, Value>,
    name: &str,
    minimum: u64,
    maximum: u64,
    path: &str,
) -> Result<u64, AdapterError> {
    let value = object.get(name).and_then(Value::as_u64).ok_or_else(|| {
        AdapterError::invalid_at(path, format!("{path} must be a non-negative integer"))
    })?;
    if !(minimum..=maximum).contains(&value) {
        return Err(AdapterError::invalid_at(
            path,
            format!("{path} must be from {minimum} to {maximum}"),
        ));
    }
    Ok(value)
}

fn nullable_integer_field(
    object: &Map<String, Value>,
    name: &str,
    minimum: u64,
    path: &str,
) -> Result<Option<u64>, AdapterError> {
    match object.get(name) {
        Some(Value::Null) => Ok(None),
        Some(_) => integer_field(
            object,
            name,
            minimum,
            crate::adapter::MAX_SAFE_INTEGER,
            path,
        )
        .map(Some),
        None => Err(AdapterError::invalid_at(
            path,
            format!("{path} is required"),
        )),
    }
}

fn bool_field(object: &Map<String, Value>, name: &str, path: &str) -> Result<bool, AdapterError> {
    object
        .get(name)
        .and_then(Value::as_bool)
        .ok_or_else(|| AdapterError::invalid_at(path, format!("{path} must be boolean")))
}
