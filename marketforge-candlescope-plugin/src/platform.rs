use std::collections::BTreeSet;
use std::io::{self, BufRead, Write};

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Number, Value, json};

use crate::{
    error::{AdapterError, ErrorKind},
    provider::{
        MARKET_DATA_CONTRIBUTION_ID, MARKETFORGE_CONTROL_CONTRIBUTION_ID, PluginService,
        SYMBOLS_CONTRIBUTION_ID,
    },
};

const PROTOCOL: &str = "candlescope.plugin/2";
const TRANSPORT: &str = "jsonl/1";
const ENTRYPOINT_ID: &str = "main";
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_JSON_DEPTH: usize = 32;
const MAX_CONTAINER_ITEMS: usize = 10_000;
const MAX_STRING_BYTES: usize = 256 * 1024;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

const RPC_PARSE_ERROR: i64 = -32700;
const RPC_INVALID_REQUEST: i64 = -32600;
const RPC_METHOD_NOT_FOUND: i64 = -32601;
const RPC_INVALID_PARAMS: i64 = -32602;
const RPC_INTERNAL_ERROR: i64 = -32603;
const RPC_HANDSHAKE_REQUIRED: i64 = -32101;
const RPC_PROTOCOL_UNSUPPORTED: i64 = -32102;
const RPC_INVALID_STATE: i64 = -32103;
const RPC_GENERATION_MISMATCH: i64 = -32104;
const RPC_CAPABILITY_INVALID: i64 = -32106;
const RPC_CONTRACT_VIOLATION: i64 = -32107;

pub fn descriptor() -> Value {
    json!({
        "protocol": PROTOCOL,
        "plugin": {
            "id": "marketforge.candlescope-adapter",
            "name": "MarketForge",
            "version": env!("CARGO_PKG_VERSION"),
            "publisher": "marketforge",
        },
        "entrypointId": ENTRYPOINT_ID,
        "contributions": [
            {
                "id": SYMBOLS_CONTRIBUTION_ID,
                "kind": "symbol-provider/1",
                "title": "MarketForge Symbols",
                "entrypoint": ENTRYPOINT_ID,
            },
            {
                "id": MARKET_DATA_CONTRIBUTION_ID,
                "kind": "market-data-provider/1",
                "title": "MarketForge Market Data",
                "entrypoint": ENTRYPOINT_ID,
            },
            {
                "id": MARKETFORGE_CONTROL_CONTRIBUTION_ID,
                "kind": "command/1",
                "title": "Control MarketForge Session",
                "entrypoint": ENTRYPOINT_ID,
            },
        ],
        "permissions": {"required": [], "optional": []},
        "hostApis": {"required": [], "optional": []},
        "features": [],
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeState {
    Created,
    Handshaken,
    Active,
    Quiescing,
    Closed,
}

#[derive(Clone, Debug)]
pub struct PlatformRuntime {
    state: RuntimeState,
    generation: u64,
    highest_generation: u64,
    service: PluginService,
}

impl Default for PlatformRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformRuntime {
    pub fn new() -> Self {
        Self {
            state: RuntimeState::Created,
            generation: 0,
            highest_generation: 0,
            service: PluginService::new(),
        }
    }

    pub fn with_service(service: PluginService) -> Self {
        Self {
            state: RuntimeState::Created,
            generation: 0,
            highest_generation: 0,
            service,
        }
    }

    pub fn state(&self) -> RuntimeState {
        self.state
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn service(&self) -> &PluginService {
        &self.service
    }

    pub fn service_mut(&mut self) -> &mut PluginService {
        &mut self.service
    }

    pub fn shutdown_requested(&self) -> bool {
        self.state == RuntimeState::Closed
    }

    pub fn handle_value(&mut self, value: Value) -> Vec<Value> {
        let (request_id, generation) = best_effort_identity(&value);
        match self.handle_request(&value) {
            Ok((result, response_generation)) => {
                vec![success(request_id, response_generation, result)]
            }
            Err(error) => vec![failure(request_id, generation, error)],
        }
    }

    fn handle_request(&mut self, value: &Value) -> Result<(Value, u64), ProtocolError> {
        let request = value.as_object().ok_or_else(|| {
            ProtocolError::invalid_request("INVALID_CONTRACT", "request must be an object")
        })?;
        if let Err(error) = exact_keys(
            request,
            "request",
            &["jsonrpc", "id", "method", "params", "generation"],
            &[],
        ) {
            return Err(ProtocolError::invalid_request(
                "INVALID_CONTRACT",
                error.message,
            ));
        }
        if request.get("jsonrpc") != Some(&Value::String("2.0".to_string())) {
            return Err(ProtocolError::invalid_request(
                "INVALID_CONTRACT",
                "jsonrpc must be 2.0",
            ));
        }
        validate_request_id(request.get("id").expect("request id is required"))?;
        let method = bounded_string(request, "method", "method", 128)?;
        let params = request
            .get("params")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                ProtocolError::invalid_request("INVALID_CONTRACT", "params must be an object")
            })?;
        let generation = integer(request, "generation", "generation", 0, MAX_SAFE_INTEGER)?;

        if self.state == RuntimeState::Closed {
            return Err(ProtocolError::new(
                RPC_INVALID_STATE,
                "SESSION_CLOSED",
                "The plugin session is already closed.",
            ));
        }
        if method == "handshake" {
            return self.handshake(params, generation);
        }
        if self.state == RuntimeState::Created {
            return Err(ProtocolError::new(
                RPC_HANDSHAKE_REQUIRED,
                "HANDSHAKE_REQUIRED",
                "handshake must complete before other methods",
            ));
        }

        match method {
            "describe" => {
                exact_keys(params, "describe", &[], &[])?;
                self.require_control_generation(generation, true)?;
                Ok((descriptor(), generation))
            }
            "activate" => self.activate(params, generation),
            "invoke" => self.invoke(params, generation),
            "eventBatch" => self.event_batch(params, generation),
            "healthCheck" => {
                exact_keys(params, "healthCheck", &[], &[])?;
                self.require_current_generation(generation)?;
                Ok((self.service.health_check(), generation))
            }
            "cancel" => self.cancel(params, generation),
            "prepareUpgrade" => self.prepare_upgrade(params, generation),
            "deactivate" => self.deactivate(params, generation),
            "shutdown" => self.shutdown(params, generation),
            _ => Err(ProtocolError::new(
                RPC_METHOD_NOT_FOUND,
                "METHOD_NOT_FOUND",
                format!("Unknown Plugin Platform method: {method}"),
            )
            .with_data("method", Value::String(method.to_string()))),
        }
    }

    fn handshake(
        &mut self,
        params: &Map<String, Value>,
        generation: u64,
    ) -> Result<(Value, u64), ProtocolError> {
        if self.state != RuntimeState::Created {
            return Err(ProtocolError::new(
                RPC_INVALID_STATE,
                "HANDSHAKE_ALREADY_COMPLETED",
                "handshake may only be completed once per process session",
            ));
        }
        if generation != 0 {
            return Err(generation_error("handshake must use generation 0"));
        }
        exact_keys(
            params,
            "handshake",
            &[
                "protocols",
                "host",
                "entrypointId",
                "hostApis",
                "transports",
            ],
            &[],
        )?;
        let protocols = string_array(params, "protocols", "handshake.protocols", false)?;
        let transports = string_array(params, "transports", "handshake.transports", false)?;
        let _host_apis = string_array(params, "hostApis", "handshake.hostApis", true)?;
        let host = params
            .get("host")
            .and_then(Value::as_object)
            .ok_or_else(|| ProtocolError::invalid_params("handshake.host must be an object"))?;
        exact_keys(host, "handshake.host", &["name", "version"], &[])?;
        bounded_string(host, "name", "handshake.host.name", 256)?;
        bounded_string(host, "version", "handshake.host.version", 64)?;
        let entrypoint = bounded_string(params, "entrypointId", "handshake.entrypointId", 128)?;
        if entrypoint != ENTRYPOINT_ID {
            return Err(ProtocolError::new(
                RPC_CONTRACT_VIOLATION,
                "ENTRYPOINT_MISMATCH",
                "Host requested an entrypoint not owned by this process.",
            )
            .with_data("entrypointId", Value::String(entrypoint.to_string())));
        }
        if !protocols.iter().any(|value| value == PROTOCOL) {
            return Err(ProtocolError::new(
                RPC_PROTOCOL_UNSUPPORTED,
                "PROTOCOL_UNSUPPORTED",
                format!("Host did not offer required protocol {PROTOCOL}."),
            ));
        }
        if !transports.iter().any(|value| value == TRANSPORT) {
            return Err(ProtocolError::new(
                RPC_PROTOCOL_UNSUPPORTED,
                "TRANSPORT_UNSUPPORTED",
                format!("Host did not offer required transport {TRANSPORT}."),
            ));
        }
        self.state = RuntimeState::Handshaken;
        Ok((
            json!({
                "protocol": PROTOCOL,
                "transport": TRANSPORT,
                "descriptor": descriptor(),
                "negotiatedHostApis": [],
            }),
            0,
        ))
    }

    fn activate(
        &mut self,
        params: &Map<String, Value>,
        generation: u64,
    ) -> Result<(Value, u64), ProtocolError> {
        if self.state != RuntimeState::Handshaken {
            return Err(ProtocolError::new(
                RPC_INVALID_STATE,
                "ACTIVATION_STATE_INVALID",
                "activate requires a handshaken inactive session",
            ));
        }
        exact_keys(
            params,
            "activate",
            &["instanceId", "generation", "capabilities"],
            &[],
        )?;
        let instance_id = bounded_string(params, "instanceId", "activate.instanceId", 128)?;
        let params_generation = integer(
            params,
            "generation",
            "activate.generation",
            1,
            MAX_SAFE_INTEGER,
        )?;
        if generation != params_generation {
            return Err(generation_error(
                "activate envelope and params generations differ",
            ));
        }
        if generation <= self.highest_generation {
            return Err(ProtocolError::new(
                RPC_GENERATION_MISMATCH,
                "STALE_GENERATION",
                "activation generation must increase monotonically",
            )
            .with_data("highestGeneration", json!(self.highest_generation)));
        }
        let capabilities = params
            .get("capabilities")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ProtocolError::invalid_params("activate.capabilities must be an array")
            })?;
        if !capabilities.is_empty() {
            return Err(ProtocolError::new(
                RPC_CAPABILITY_INVALID,
                "CAPABILITY_GRANTS_INVALID",
                "MarketForge declares no Host capability grants.",
            ));
        }
        self.generation = generation;
        self.highest_generation = generation;
        self.state = RuntimeState::Active;
        self.service.activate(generation);
        Ok((
            json!({
                "ok": true,
                "instanceId": instance_id,
                "generation": generation,
            }),
            generation,
        ))
    }

    fn invoke(
        &mut self,
        params: &Map<String, Value>,
        generation: u64,
    ) -> Result<(Value, u64), ProtocolError> {
        self.require_active(generation)?;
        if self.state == RuntimeState::Quiescing {
            return Err(ProtocolError::new(
                RPC_INVALID_STATE,
                "PLUGIN_QUIESCING",
                "New invocations are rejected while preparing an upgrade.",
            ));
        }
        exact_keys(
            params,
            "invoke",
            &["contributionId", "input", "requestContext"],
            &[],
        )?;
        let contribution_id =
            bounded_string(params, "contributionId", "invoke.contributionId", 128)?;
        if !declared_contributions().contains(contribution_id) {
            return Err(ProtocolError::new(
                RPC_CONTRACT_VIOLATION,
                "CONTRIBUTION_NOT_DECLARED",
                "invoke references a contribution absent from the descriptor",
            )
            .with_data("contributionId", Value::String(contribution_id.to_string())));
        }
        let input = params
            .get("input")
            .filter(|value| value.is_object())
            .ok_or_else(|| ProtocolError::invalid_params("invoke.input must be an object"))?;
        let context = params
            .get("requestContext")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                ProtocolError::invalid_params("invoke.requestContext must be an object")
            })?;
        exact_keys(
            context,
            "requestContext",
            &["contributionId", "userAction", "generation", "traceId"],
            &[],
        )?;
        let context_contribution = bounded_string(
            context,
            "contributionId",
            "requestContext.contributionId",
            128,
        )?;
        if context_contribution != contribution_id {
            return Err(ProtocolError::invalid_params(
                "invoke contribution does not match requestContext",
            ));
        }
        let context_generation = integer(
            context,
            "generation",
            "requestContext.generation",
            1,
            MAX_SAFE_INTEGER,
        )?;
        if context_generation != generation {
            return Err(generation_error(
                "requestContext generation does not match the envelope",
            ));
        }
        let user_action = context
            .get("userAction")
            .and_then(Value::as_bool)
            .ok_or_else(|| {
                ProtocolError::invalid_params("requestContext.userAction must be boolean")
            })?;
        validate_trace_id(bounded_string(
            context,
            "traceId",
            "requestContext.traceId",
            128,
        )?)?;
        let result = self
            .service
            .invoke(contribution_id, input, user_action)
            .map_err(ProtocolError::from_adapter)?;
        Ok((result, generation))
    }

    fn event_batch(
        &mut self,
        params: &Map<String, Value>,
        generation: u64,
    ) -> Result<(Value, u64), ProtocolError> {
        self.require_active(generation)?;
        exact_keys(params, "eventBatch", &["events", "delivery"], &[])?;
        let events = params
            .get("events")
            .and_then(Value::as_array)
            .ok_or_else(|| ProtocolError::invalid_params("eventBatch.events must be an array"))?;
        if !events.iter().all(Value::is_object) {
            return Err(ProtocolError::invalid_params(
                "eventBatch events must be objects",
            ));
        }
        let delivery = params
            .get("delivery")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                ProtocolError::invalid_params("eventBatch.delivery must be an object")
            })?;
        if let Some(context) = delivery.get("requestContext") {
            let context = context.as_object().ok_or_else(|| {
                ProtocolError::invalid_params(
                    "eventBatch delivery requestContext must be an object",
                )
            })?;
            exact_keys(
                context,
                "requestContext",
                &["contributionId", "userAction", "generation", "traceId"],
                &[],
            )?;
            let contribution_id = bounded_string(
                context,
                "contributionId",
                "requestContext.contributionId",
                128,
            )?;
            if !declared_contributions().contains(contribution_id) {
                return Err(ProtocolError::new(
                    RPC_CONTRACT_VIOLATION,
                    "CONTRIBUTION_NOT_DECLARED",
                    "eventBatch requestContext references an undeclared contribution",
                ));
            }
            let context_generation = integer(
                context,
                "generation",
                "requestContext.generation",
                1,
                MAX_SAFE_INTEGER,
            )?;
            if context_generation != generation {
                return Err(generation_error(
                    "eventBatch requestContext generation does not match the envelope",
                ));
            }
            if context.get("userAction") != Some(&Value::Bool(false)) {
                return Err(ProtocolError::new(
                    RPC_CONTRACT_VIOLATION,
                    "EVENT_BATCH_USER_ACTION_INVALID",
                    "eventBatch requestContext cannot carry user-action authority",
                ));
            }
            validate_trace_id(bounded_string(
                context,
                "traceId",
                "requestContext.traceId",
                128,
            )?)?;
        }
        Ok((json!({"accepted": events.len()}), generation))
    }

    fn cancel(
        &self,
        params: &Map<String, Value>,
        generation: u64,
    ) -> Result<(Value, u64), ProtocolError> {
        self.require_current_generation(generation)?;
        exact_keys(params, "cancel", &["requestId"], &[])?;
        let request_id = params
            .get("requestId")
            .expect("exact keys require requestId");
        validate_request_id(request_id)?;
        Ok((
            json!({"cancelled": false, "requestId": request_id}),
            generation,
        ))
    }

    fn prepare_upgrade(
        &mut self,
        params: &Map<String, Value>,
        generation: u64,
    ) -> Result<(Value, u64), ProtocolError> {
        self.require_active(generation)?;
        exact_keys(params, "prepareUpgrade", &[], &[])?;
        self.state = RuntimeState::Quiescing;
        self.service.prepare_upgrade();
        Ok((json!({"ok": true}), generation))
    }

    fn deactivate(
        &mut self,
        params: &Map<String, Value>,
        generation: u64,
    ) -> Result<(Value, u64), ProtocolError> {
        self.require_active(generation)?;
        exact_keys(params, "deactivate", &["reason"], &[])?;
        bounded_string(params, "reason", "deactivate.reason", 256)?;
        self.service.deactivate();
        self.state = RuntimeState::Handshaken;
        self.generation = 0;
        Ok((json!({"ok": true}), generation))
    }

    fn shutdown(
        &mut self,
        params: &Map<String, Value>,
        generation: u64,
    ) -> Result<(Value, u64), ProtocolError> {
        exact_keys(params, "shutdown", &[], &[])?;
        match self.state {
            RuntimeState::Active | RuntimeState::Quiescing => {
                self.require_current_generation(generation)?;
            }
            RuntimeState::Handshaken if generation == 0 => {}
            RuntimeState::Handshaken => {
                return Err(generation_error("inactive shutdown must use generation 0"));
            }
            RuntimeState::Created | RuntimeState::Closed => unreachable!(),
        }
        self.service.deactivate();
        self.state = RuntimeState::Closed;
        self.generation = 0;
        Ok((json!({"ok": true}), generation))
    }

    fn require_active(&self, generation: u64) -> Result<(), ProtocolError> {
        if !matches!(self.state, RuntimeState::Active | RuntimeState::Quiescing) {
            return Err(ProtocolError::new(
                RPC_INVALID_STATE,
                "PLUGIN_NOT_ACTIVE",
                "This method requires an active plugin generation.",
            ));
        }
        self.require_current_generation(generation)
    }

    fn require_current_generation(&self, generation: u64) -> Result<(), ProtocolError> {
        if self.generation < 1 || generation != self.generation {
            return Err(
                generation_error("Request does not belong to the active generation.")
                    .with_data("activeGeneration", json!(self.generation)),
            );
        }
        Ok(())
    }

    fn require_control_generation(
        &self,
        generation: u64,
        allow_zero: bool,
    ) -> Result<(), ProtocolError> {
        if matches!(self.state, RuntimeState::Active | RuntimeState::Quiescing) {
            self.require_current_generation(generation)
        } else if allow_zero && generation == 0 {
            Ok(())
        } else {
            Err(generation_error(
                "Inactive control requests must use generation 0.",
            ))
        }
    }
}

#[derive(Clone, Debug)]
struct ProtocolError {
    rpc_code: i64,
    code: &'static str,
    message: String,
    data: Map<String, Value>,
}

impl ProtocolError {
    fn new(rpc_code: i64, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            rpc_code,
            code,
            message: message.into(),
            data: Map::new(),
        }
    }

    fn invalid_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(RPC_INVALID_REQUEST, code, message)
    }

    fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(RPC_INVALID_PARAMS, "INVALID_CONTRACT", message)
    }

    fn with_data(mut self, name: &str, value: Value) -> Self {
        self.data.insert(name.to_string(), value);
        self
    }

    fn from_adapter(error: AdapterError) -> Self {
        let rpc_code = match error.kind() {
            ErrorKind::InvalidContract => RPC_INVALID_PARAMS,
            ErrorKind::InvalidState => RPC_INVALID_STATE,
            ErrorKind::NotFound | ErrorKind::Core => RPC_CONTRACT_VIOLATION,
            ErrorKind::Internal => RPC_INTERNAL_ERROR,
        };
        let mut result = Self::new(rpc_code, error.code(), error.message());
        if let Some(path) = error.path() {
            result
                .data
                .insert("path".to_string(), Value::String(path.to_string()));
        }
        result
    }
}

fn generation_error(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(RPC_GENERATION_MISMATCH, "GENERATION_MISMATCH", message)
}

fn success(id: Value, generation: u64, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
        "generation": generation,
    })
}

fn failure(id: Value, generation: u64, error: ProtocolError) -> Value {
    let mut data = error.data;
    data.insert("code".to_string(), Value::String(error.code.to_string()));
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": error.rpc_code,
            "message": error.message,
            "data": data,
        },
        "generation": generation,
    })
}

fn best_effort_identity(value: &Value) -> (Value, u64) {
    let Some(object) = value.as_object() else {
        return (Value::Null, 0);
    };
    let id = object
        .get("id")
        .filter(|value| validate_request_id(value).is_ok())
        .cloned()
        .unwrap_or(Value::Null);
    let generation = object
        .get("generation")
        .and_then(Value::as_u64)
        .filter(|value| *value <= MAX_SAFE_INTEGER)
        .unwrap_or_default();
    (id, generation)
}

fn validate_request_id(value: &Value) -> Result<(), ProtocolError> {
    match value {
        Value::String(value) if !value.is_empty() && value.len() <= 256 => Ok(()),
        Value::Number(value)
            if value
                .as_u64()
                .is_some_and(|value| value <= MAX_SAFE_INTEGER) =>
        {
            Ok(())
        }
        _ => Err(ProtocolError::invalid_request(
            "INVALID_CONTRACT",
            "id must be a non-empty string or non-negative safe integer",
        )),
    }
}

fn declared_contributions() -> BTreeSet<&'static str> {
    [
        SYMBOLS_CONTRIBUTION_ID,
        MARKET_DATA_CONTRIBUTION_ID,
        MARKETFORGE_CONTROL_CONTRIBUTION_ID,
    ]
    .into_iter()
    .collect()
}

fn exact_keys(
    object: &Map<String, Value>,
    path: &str,
    required: &[&str],
    optional: &[&str],
) -> Result<(), ProtocolError> {
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
        return Err(ProtocolError::invalid_params(format!(
            "{path} has an invalid shape; missing={missing:?}, unknown={unknown:?}"
        )));
    }
    Ok(())
}

fn bounded_string<'a>(
    object: &'a Map<String, Value>,
    name: &str,
    path: &str,
    maximum: usize,
) -> Result<&'a str, ProtocolError> {
    let value = object
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| ProtocolError::invalid_params(format!("{path} must be a string")))?;
    if value.is_empty() || value != value.trim() || value.len() > maximum {
        return Err(ProtocolError::invalid_params(format!(
            "{path} must be a bounded canonical string"
        )));
    }
    Ok(value)
}

fn integer(
    object: &Map<String, Value>,
    name: &str,
    path: &str,
    minimum: u64,
    maximum: u64,
) -> Result<u64, ProtocolError> {
    let value = object.get(name).and_then(Value::as_u64).ok_or_else(|| {
        ProtocolError::invalid_params(format!("{path} must be a non-negative integer"))
    })?;
    if !(minimum..=maximum).contains(&value) {
        return Err(ProtocolError::invalid_params(format!(
            "{path} must be from {minimum} to {maximum}"
        )));
    }
    Ok(value)
}

fn string_array(
    object: &Map<String, Value>,
    name: &str,
    path: &str,
    allow_empty: bool,
) -> Result<Vec<String>, ProtocolError> {
    let values = object
        .get(name)
        .and_then(Value::as_array)
        .ok_or_else(|| ProtocolError::invalid_params(format!("{path} must be an array")))?;
    if !allow_empty && values.is_empty() {
        return Err(ProtocolError::invalid_params(format!(
            "{path} must not be empty"
        )));
    }
    let mut unique = BTreeSet::new();
    let mut result = Vec::new();
    for value in values {
        let value = value
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ProtocolError::invalid_params(format!("{path} entries must be strings"))
            })?;
        if !unique.insert(value) {
            return Err(ProtocolError::invalid_params(format!(
                "{path} entries must be unique"
            )));
        }
        result.push(value.to_string());
    }
    Ok(result)
}

fn validate_trace_id(value: &str) -> Result<(), ProtocolError> {
    if value.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | ':' | '-')
    }) {
        Ok(())
    } else {
        Err(ProtocolError::invalid_params(
            "requestContext.traceId contains unsupported characters",
        ))
    }
}

#[derive(Debug)]
pub struct JsonLineServer {
    runtime: PlatformRuntime,
}

impl Default for JsonLineServer {
    fn default() -> Self {
        Self::new()
    }
}

impl JsonLineServer {
    pub fn new() -> Self {
        Self {
            runtime: PlatformRuntime::new(),
        }
    }

    pub fn with_runtime(runtime: PlatformRuntime) -> Self {
        Self { runtime }
    }

    pub fn runtime(&self) -> &PlatformRuntime {
        &self.runtime
    }

    pub fn runtime_mut(&mut self) -> &mut PlatformRuntime {
        &mut self.runtime
    }

    pub fn handle_line(&mut self, line: &str) -> Vec<Value> {
        if line.len() > MAX_MESSAGE_BYTES {
            return vec![failure(
                Value::Null,
                0,
                ProtocolError::invalid_request(
                    "MESSAGE_TOO_LARGE",
                    format!("control line exceeds {MAX_MESSAGE_BYTES} bytes"),
                )
                .with_data("maxMessageBytes", json!(MAX_MESSAGE_BYTES)),
            )];
        }
        match parse_strict_json(line) {
            Ok(value) => self.runtime.handle_value(value),
            Err(message) => vec![failure(
                Value::Null,
                0,
                ProtocolError::new(
                    RPC_PARSE_ERROR,
                    "PARSE_ERROR",
                    "Control line is not valid bounded JSON.",
                )
                .with_data("detail", Value::String(message)),
            )],
        }
    }

    pub fn serve<R: BufRead, W: Write>(&mut self, mut input: R, output: &mut W) -> io::Result<()> {
        let mut line = String::new();
        loop {
            line.clear();
            let bytes = input.read_line(&mut line)?;
            if bytes == 0 {
                break;
            }
            while line.ends_with(['\n', '\r']) {
                line.pop();
            }
            for response in self.handle_line(&line) {
                serde_json::to_writer(&mut *output, &response)?;
                output.write_all(b"\n")?;
                output.flush()?;
            }
            if self.runtime.shutdown_requested() {
                break;
            }
        }
        Ok(())
    }
}

fn parse_strict_json(source: &str) -> Result<Value, String> {
    let mut deserializer = serde_json::Deserializer::from_str(source);
    let StrictValue(value) =
        StrictValue::deserialize(&mut deserializer).map_err(|error| error.to_string())?;
    deserializer.end().map_err(|error| error.to_string())?;
    validate_json_limits(&value, 0)?;
    Ok(value)
}

fn validate_json_limits(value: &Value, depth: usize) -> Result<(), String> {
    if depth > MAX_JSON_DEPTH {
        return Err(format!("JSON depth exceeds {MAX_JSON_DEPTH}"));
    }
    match value {
        Value::String(value) if value.len() > MAX_STRING_BYTES => {
            Err(format!("JSON string exceeds {MAX_STRING_BYTES} bytes"))
        }
        Value::Array(values) => {
            if values.len() > MAX_CONTAINER_ITEMS {
                return Err(format!("JSON array exceeds {MAX_CONTAINER_ITEMS} entries"));
            }
            for value in values {
                validate_json_limits(value, depth + 1)?;
            }
            Ok(())
        }
        Value::Object(values) => {
            if values.len() > MAX_CONTAINER_ITEMS {
                return Err(format!("JSON object exceeds {MAX_CONTAINER_ITEMS} entries"));
            }
            for (key, value) in values {
                if key.len() > MAX_STRING_BYTES {
                    return Err(format!("JSON key exceeds {MAX_STRING_BYTES} bytes"));
                }
                validate_json_limits(value, depth + 1)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictValueVisitor)
    }
}

struct StrictValueVisitor;

impl<'de> Visitor<'de> for StrictValueVisitor {
    type Value = StrictValue;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(value))))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(value))))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .map(StrictValue)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value.to_string())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        StrictValue::deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(StrictValue(value)) = sequence.next_element()? {
            values.push(value);
        }
        Ok(StrictValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut entries: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        while let Some(key) = entries.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom(format!(
                    "duplicate JSON object key {key:?}"
                )));
            }
            let StrictValue(value) = entries.next_value()?;
            values.insert(key, value);
        }
        Ok(StrictValue(Value::Object(values)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_parser_rejects_duplicate_keys() {
        let error = parse_strict_json(r#"{"id":1,"id":2}"#).unwrap_err();
        assert!(error.contains("duplicate JSON object key"));
    }

    #[test]
    fn parse_error_is_a_json_rpc_failure() {
        let response = JsonLineServer::new().handle_line("{");
        assert_eq!(response[0]["error"]["code"], RPC_PARSE_ERROR);
        assert_eq!(response[0]["error"]["data"]["code"], "PARSE_ERROR");
    }
}
