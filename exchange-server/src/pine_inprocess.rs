//! Opt-in in-process adapter for the six installed, hash-qualified bar-close
//! strategies. The process adapter remains authoritative for other sources,
//! legacy mode, automatic hedging and position-protection configurations.
//! Cached sessions are speculative: only the frozen committed transcript can
//! authorize reuse. Orders still go through the existing scheduler/gateway.
use exchange_core::{
    BotConfig, BotDecisionRequest, BotDecisionResponse, BotError, ParticipantObservation,
};
use pine_runtime::{
    Bar, ChartContext, ExternalAccountFrame, ExternalPass, ExternalRequestStream,
    HistoricalRuntime, InputOverrides, NoRequestDataProvider, PineValue, PreparedProgram,
    RequestEnvironment, RequestTimeframe,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

mod broker;
mod json_compat;
use broker::{array, number, text};
use json_compat::{digest, encoded_len, python_json_len};

const QUALIFIED: &[(&str, &str)] = &[
    (
        "ma.pine",
        "5b05dac3d2bd42aff7d3d0d75e6a4d5cd41b64ea6d9e34dfd8d861f3f0b3c165",
    ),
    (
        "rsi.pine",
        "2a8c15eed0be312d956b1e0f4672006e04f77aa4c9798f4abcc8ee890699b3fc",
    ),
    (
        "breakout.pine",
        "0b3ca54621fa9c57a6979a34f2e1c3bb2f9e5ffa03904a740925283cc697f784",
    ),
    (
        "limit.pine",
        "da2d01e9c5789f96b8aab5e724fb45d77c31e9860356fcc9c7a81bcbaa25a301",
    ),
    (
        "perp.pine",
        "ec941ad46aaf184057b26b36bcb0dce249eecf13228a99192f04e2ee743c9b4f",
    ),
    (
        "basis.pine",
        "cce058c8e9ce040d4d771029b61ac7e334abb502b64972b7f4ef070454ae5b90",
    ),
];

fn error(message: impl Into<String>) -> BotError {
    BotError(message.into())
}
fn io(error: impl std::fmt::Display) -> BotError {
    BotError(error.to_string())
}

struct Program {
    source: String,
    program: PreparedProgram,
}
struct Session {
    runtime: HistoricalRuntime<'static>,
    transcript: Value,
    settings: Value,
}
#[derive(Default)]
struct Sessions {
    entries: BTreeMap<String, Arc<Mutex<Option<Session>>>>,
    order: VecDeque<String>,
}
pub struct Host {
    directory: std::path::PathBuf,
    programs: BTreeMap<String, Program>,
    sessions: Mutex<Sessions>,
}

impl Host {
    pub fn load(directory: &Path) -> Result<Self, BotError> {
        let mut programs = BTreeMap::new();
        for &(name, hash) in QUALIFIED {
            let source = match std::fs::read_to_string(directory.join("scripts").join(name)) {
                Ok(source) => source.replace("\r\n", "\n").replace('\r', "\n"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(io(e)),
            };
            if format!("{:x}", Sha256::digest(source.as_bytes())) != hash {
                continue;
            }
            // These exact sources contain one simple strategy declaration and
            // no strategy.order lowering, exit protection or user loops.
            let start = source
                .find("strategy(")
                .ok_or_else(|| error("missing strategy declaration"))?;
            let end = source[start..].find(')').unwrap() + start;
            let lowered = format!(
                "{}, calc_on_order_fills=true{}",
                &source[..end],
                &source[end..]
            );
            let analysis = pine_sema::analyze_source(&pine_syntax::SourceFile::new(name, lowered));
            if analysis
                .diagnostics
                .iter()
                .any(|d| matches!(d.severity, pine_syntax::Severity::Error))
            {
                return Err(error(format!(
                    "qualified Pine source failed analysis: {name}"
                )));
            }
            let program =
                PreparedProgram::new(analysis.hir.ok_or_else(|| error("missing Pine HIR"))?);
            programs.insert(name.into(), Program { source, program });
        }
        Ok(Self {
            directory: directory.to_owned(),
            programs,
            sessions: Mutex::default(),
        })
    }

    pub fn eligible(&self, request: &BotDecisionRequest) -> bool {
        request.plugin_id == "pine.strategy"
            && matches!(request.plugin_version.as_str(), "1.0.0" | "2.0.0")
            && request.state_version == 1
            && request.config["execution_mode"] == "broker"
            && request.config["script"]
                .as_str()
                .is_some_and(|s| self.source_is_current(s))
            && request.config["hedge_instrument_id"].as_str() == Some("")
            && request.config["inputs"]
                .as_object()
                .is_some_and(|o| o.values().all(|v| v.is_i64()))
            && request.state.get("hedge").is_none()
            && request
                .state
                .get("exit_specs")
                .is_none_or(|v| v.as_object().is_some_and(|o| o.is_empty()))
    }

    pub fn eligible_borrowed(&self, config: &BotConfig, state: &Value) -> bool {
        config.plugin_id == "pine.strategy"
            && matches!(config.plugin_version.as_str(), "1.0.0" | "2.0.0")
            && config.state_version == 1
            && config.config["execution_mode"] == "broker"
            && config.config["script"]
                .as_str()
                .is_some_and(|s| self.source_is_current(s))
            && config.config["hedge_instrument_id"].as_str() == Some("")
            && config.config["inputs"]
                .as_object()
                .is_some_and(|o| o.values().all(Value::is_i64))
            && state.get("hedge").is_none()
            && state
                .get("exit_specs")
                .is_none_or(|v| v.as_object().is_some_and(|o| o.is_empty()))
    }

    pub fn decide_borrowed(
        &self,
        config: &BotConfig,
        state: &Value,
        observation: &ParticipantObservation,
        timeout_ms: u64,
    ) -> Result<BotDecisionResponse, BotError> {
        #[derive(serde::Serialize)]
        struct Request<'a> {
            protocol_version: &'a str,
            plugin_id: &'a str,
            plugin_version: &'a str,
            state_version: u16,
            participant: &'a exchange_core::ParticipantConfig,
            seed: u64,
            config: &'a Value,
            state: &'a Value,
            observation: &'a ParticipantObservation,
        }
        let request = Request {
            protocol_version: exchange_core::BOT_PROTOCOL_VERSION,
            plugin_id: &config.plugin_id,
            plugin_version: &config.plugin_version,
            state_version: config.state_version,
            participant: &config.participant,
            seed: config.seed,
            config: &config.config,
            state,
            observation,
        };
        // Committed state remains borrowed until the single mutable working copy.
        let input = json!({"protocol_version":request.protocol_version,"plugin_id":request.plugin_id,"plugin_version":request.plugin_version,"state_version":request.state_version,"participant":request.participant,"seed":request.seed,"config":request.config,"observation":observation});
        let (actions, state) =
            self.decide_parts(&input, state, timeout_ms, || encoded_len(&request))?;
        Ok(BotDecisionResponse {
            protocol_version: request.protocol_version.into(),
            plugin_id: request.plugin_id.into(),
            plugin_version: request.plugin_version.into(),
            state_version: request.state_version,
            actions: serde_json::from_value(Value::Array(actions)).map_err(io)?,
            state,
        })
    }

    fn source_is_current(&self, name: &str) -> bool {
        self.programs.get(name).is_some_and(|program| {
            std::fs::read_to_string(self.directory.join("scripts").join(name)).is_ok_and(|source| {
                source.replace("\r\n", "\n").replace('\r', "\n") == program.source
            })
        })
    }

    fn session(&self, identity: &str) -> Result<Arc<Mutex<Option<Session>>>, BotError> {
        let mut sessions = self.sessions.lock().map_err(io)?;
        if let Some(value) = sessions.entries.get(identity) {
            return Ok(value.clone());
        }
        // Same total bound as four process workers with 512 sessions each.
        while sessions.entries.len() >= 2048 {
            let old = sessions.order.pop_front().unwrap();
            sessions.entries.remove(&old);
        }
        let value = Arc::new(Mutex::new(None));
        sessions.entries.insert(identity.into(), value.clone());
        sessions.order.push_back(identity.into());
        Ok(value)
    }

    /// Diagnostic reset also exercises restart reconstruction. No cached state
    /// is part of the journal or of a successful trading acknowledgement.
    pub fn clear_sessions(&self) {
        *self.sessions.lock().unwrap() = Sessions::default();
    }

    pub fn decide(
        &self,
        request: &BotDecisionRequest,
        timeout_ms: u64,
    ) -> Result<BotDecisionResponse, BotError> {
        if !self.eligible(request) {
            return Err(error(
                "request is outside the qualified in-process Pine profile",
            ));
        }
        serde_json::from_value(
            self.decide_value(&serde_json::to_value(request).map_err(io)?, timeout_ms)?,
        )
        .map_err(io)
    }

    /// Same implementation for frozen differential fixtures, without requiring
    /// fixtures to manufacture unrelated exchange observation fields.
    pub fn decide_value(&self, input: &Value, timeout_ms: u64) -> Result<Value, BotError> {
        let (actions, state) =
            self.decide_parts(input, &input["state"], timeout_ms, || encoded_len(input))?;
        let mut response = serde_json::Map::new();
        for key in [
            "protocol_version",
            "plugin_id",
            "plugin_version",
            "state_version",
        ] {
            response.insert(key.into(), input[key].clone());
        }
        response.insert("actions".into(), Value::Array(actions));
        response.insert("state".into(), state);
        Ok(Value::Object(response))
    }

    fn decide_parts(
        &self,
        input: &Value,
        committed: &Value,
        timeout_ms: u64,
        request_len: impl FnOnce() -> Result<usize, BotError>,
    ) -> Result<(Vec<Value>, Value), BotError> {
        if input["protocol_version"] != "bot.v1"
            || input["plugin_id"] != "pine.strategy"
            || !matches!(input["plugin_version"].as_str(), Some("1.0.0" | "2.0.0"))
            || input["state_version"] != 1
            || input["config"]["execution_mode"] != "broker"
            || input["config"]["hedge_instrument_id"] != ""
            || !input["config"]["inputs"]
                .as_object()
                .is_some_and(|o| o.values().all(Value::is_i64))
            || !input["config"]["script"]
                .as_str()
                .is_some_and(|s| self.source_is_current(s))
            || committed.get("hedge").is_some()
            || committed
                .get("exit_specs")
                .is_some_and(|v| !v.as_object().is_some_and(|o| o.is_empty()))
        {
            return Err(error(
                "request is outside the qualified in-process Pine profile",
            ));
        }
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        #[cfg(feature = "bench-execution-timing")]
        let _call_timer = crate::bench_execution_timing::Timer::start(
            &crate::bench_execution_timing::BOT_INPROCESS_CALL,
        );
        #[cfg(feature = "bench-execution-timing")]
        crate::bench_execution_timing::trace(
            "in_process_start",
            text(&input["participant"]["participant_id"])?,
            input["observation"]["market_time_ms"].as_u64().unwrap_or(0),
            0,
        );
        // Counting remains inside the decision deadline, as on the wire path.
        if request_len()? > crate::bot_plugins::MAX_REQUEST_BYTES {
            return Err(error("bot request exceeds size limit"));
        }
        let config = &input["config"];
        let observation = &input["observation"];
        let program = &self.programs[text(&config["script"])?];
        let identity = digest(
            &json!({"source": program.source, "config":config, "participant":input["participant"]}),
        )?;
        let mut state = if committed.is_null() {
            json!({})
        } else {
            committed.clone()
        };
        if !state.as_object().is_some_and(|o| o.is_empty())
            && (state["identity"] != identity || state["broker_schema"] != 2)
        {
            return Err(error(
                "Pine source/config/account changed; create a new instance",
            ));
        }
        let mut views = vec![observation];
        if let Some(related) = observation.get("related_markets") {
            views.extend(array(related)?.iter());
        }
        let mut instruments = std::collections::BTreeSet::new();
        for view in &views {
            if !instruments.insert(text(&view["instrument_id"])?) {
                return Err(error("duplicate related markets"));
            }
            let (_, account, _, _) = broker::account(view, config)?;
            if account["account_id"] != input["participant"]["account_id"]
                || view["market_time_ms"] != observation["market_time_ms"]
            {
                return Err(error(
                    "cross-market observations must use the same account and time",
                ));
            }
        }
        let data = &observation["bot_market_data"];
        if data.is_null() || data["interval_ms"] != config["bar_interval_ms"] {
            return Err(error("host must supply configured history"));
        }
        let bars = broker::bars(data, number(&observation["market_time_ms"], 0)?)?;
        if bars.len() > number(&config["history_limit"], 0)? as usize {
            return Err(error("Pine history_limit reached; reconstruction refused"));
        }
        let previous = committed
            .get("bars")
            .map(array)
            .transpose()?
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if bars.len() < previous.len() || &bars[..previous.len()] != previous {
            return Err(error(
                "committed Pine candle history changed or disappeared",
            ));
        }
        if state.as_object().unwrap().is_empty() {
            let (kind, account, position, _) = broker::account(observation, config)?;
            let equity = if kind == "Perp" {
                number(&account["equity"], -broker::MAX_EXACT)? as i128
            } else {
                number(&account["cash_balance"], -broker::MAX_EXACT)? as i128
                    + position as i128
                        * bars
                            .last()
                            .map(|b| number(&b["close"], 1))
                            .transpose()?
                            .unwrap_or(number(&config["initial_avg_price_tick"], 0)?)
                            as i128
            };
            state["identity"] = json!(identity);
            state["broker_schema"] = json!(2);
            state["initial_capital"] = serde_json::to_value(equity).map_err(io)?;
        }
        broker::feedback(&mut state, &views, config)?;
        let mut actions = Vec::new();
        if bars.len() > previous.len() {
            let mut frames = take_array(&mut state, "accounts")?;
            for (index, bar) in bars.iter().enumerate().skip(previous.len()) {
                let cutoff = (index != bars.len() - 1)
                    .then(|| number(&data["candles"][index]["close_time_ms"], 0))
                    .transpose()?;
                frames.push(broker::frame(observation, config, &state, bar, cutoff)?);
            }
            let mut requests = BTreeMap::new();
            for view in &views {
                let stream = &view["bot_market_data"];
                requests.insert(
                    format!(
                        "{}:{}",
                        text(&view["instrument_id"])?,
                        timeframe(number(&stream["interval_ms"], 1)?)?
                    ),
                    broker::bars(stream, number(&observation["market_time_ms"], 0)?)?,
                );
            }
            for stream in data
                .get("streams")
                .map(array)
                .transpose()?
                .unwrap_or(&Vec::new())
            {
                requests.insert(
                    format!(
                        "{}:{}",
                        text(&stream["instrument_id"])?,
                        timeframe(number(&stream["interval_ms"], 1)?)?
                    ),
                    broker::bars(stream, number(&observation["market_time_ms"], 0)?)?,
                );
            }
            if state.get("providers").is_none() {
                state["providers"] = json!({});
            }
            for (key, stream) in &requests {
                let old = state["providers"]
                    .get(key)
                    .map(array)
                    .transpose()?
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                if stream.len() < old.len() || &stream[..old.len()] != old {
                    return Err(error(
                        "committed requested-data history changed or disappeared",
                    ));
                }
            }
            let mut evaluations = take_array(&mut state, "evaluations")?;
            for index in previous.len()..bars.len() {
                let cutoff = number(&data["candles"][index]["close_time_ms"], 0)?;
                let mut counts = BTreeMap::new();
                for (key, stream) in &requests {
                    let interval = interval(key.rsplit_once(':').unwrap().1)?;
                    let count = stream
                        .iter()
                        .filter(|bar| {
                            number(&bar["time"], 0)
                                .is_ok_and(|time| time as i128 + interval as i128 <= cutoff as i128)
                        })
                        .count();
                    counts.insert(key.clone(), count);
                }
                evaluations.push(json!({"time":cutoff,"providers":counts}));
            }
            for (key, stream) in requests {
                state["providers"][key] = Value::Array(stream);
            }
            let settings = json!({"config":config, "seed":input["seed"], "tick":observation["instrument"]["tick_size"], "primary":observation["instrument_id"]});
            let slot = self.session(&format!("{identity}:{}", input["seed"]))?;
            let mut slot = slot.lock().map_err(io)?;
            // Taking the session poisons speculative state on every error path.
            let mut cached = slot.take();
            let reusable = cached.as_ref().is_some_and(|s| {
                s.settings == settings
                    && ["bars", "accounts", "providers", "evaluations"]
                        .iter()
                        .all(|k| s.transcript[*k] == committed[*k])
            });
            let offset = if reusable { previous.len() } else { 0 };
            #[cfg(feature = "bench-execution-timing")]
            let _session_timer = crate::bench_execution_timing::Timer::start(if reusable {
                &crate::bench_execution_timing::BOT_INPROCESS_APPEND
            } else {
                &crate::bench_execution_timing::BOT_INPROCESS_REBUILD
            });
            if !reusable {
                let mut overrides = InputOverrides::new();
                let calls = pine_runtime::input_calls(&program.program);
                for (title, value) in config["inputs"].as_object().unwrap() {
                    let matches: Vec<_> = calls
                        .iter()
                        .filter(|c| c.title.as_deref() == Some(title))
                        .collect();
                    if matches.len() != 1 {
                        return Err(error("unknown or ambiguous Pine input title"));
                    }
                    let call = matches[0];
                    let value = number(value, -broker::MAX_EXACT)?;
                    if call
                        .min_value
                        .as_ref()
                        .is_some_and(|v| matches!(v, PineValue::Int(n) if value < *n))
                        || call
                            .max_value
                            .as_ref()
                            .is_some_and(|v| matches!(v, PineValue::Int(n) if value > *n))
                    {
                        return Err(error("Pine input is outside declared bounds"));
                    }
                    overrides.insert(call.call_site_id, PineValue::Int(value));
                }
                let tick = observation["instrument"]
                    .get("tick_size")
                    .map(|v| number(v, 1))
                    .transpose()?
                    .unwrap_or(1);
                let chart = ChartContext::new(
                    text(&observation["instrument_id"])?,
                    RequestTimeframe::parse(&timeframe(number(&config["bar_interval_ms"], 1)?)?)
                        .map_err(io)?,
                )
                .with_price_grid(u32::try_from(tick).map_err(io)?, 1)
                .map_err(io)?
                .with_quantity_precision(0)
                .map_err(io)?;
                let runtime =
                    HistoricalRuntime::from_prepared_with_request_environment_and_input_overrides(
                        &program.program,
                        RequestEnvironment::new(chart, Arc::new(NoRequestDataProvider)),
                        overrides,
                    );
                cached = Some(Session {
                    runtime,
                    transcript: Value::Null,
                    settings,
                });
            }
            let mut cached = cached.unwrap();
            let mut passes = Vec::new();
            for index in offset..bars.len() {
                let evaluation = &evaluations[index];
                let mut streams = Vec::new();
                for (key, count) in evaluation["providers"]
                    .as_object()
                    .ok_or_else(|| error("invalid provider transcript"))?
                {
                    let (symbol, tf) = key
                        .rsplit_once(':')
                        .ok_or_else(|| error("invalid provider key"))?;
                    let count = number(count, 0)? as usize;
                    let values = array(&state["providers"][key])?;
                    if count > values.len() {
                        return Err(error("invalid frozen provider prefix"));
                    }
                    streams.push(ExternalRequestStream {
                        symbol: symbol.into(),
                        timeframe: tf.into(),
                        bars: values[..count].iter().map(bar).collect::<Result<_, _>>()?,
                    });
                }
                passes.push(vec![ExternalPass {
                    account: account_frame(&frames[index])?,
                    bar: bar(&bars[index])?,
                    event_time_ms: number(&evaluation["time"], 0)?,
                    confirmed: true,
                    request_data: Some(streams),
                }]);
            }
            let account_frames = frames[offset..]
                .iter()
                .map(account_frame)
                .collect::<Result<Vec<_>, _>>()?;
            if reusable {
                cached
                    .runtime
                    .extend_external_accounts_and_passes(account_frames, Some(passes))
                    .map_err(|e| error(e.message))?;
            } else {
                cached.runtime = cached
                    .runtime
                    .with_external_accounts_and_passes(account_frames, Some(passes))
                    .map_err(|e| error(e.message))?;
            }
            cached.runtime.set_host_deadline(Some(deadline));
            cached
                .runtime
                .append_bars(
                    &bars[offset..]
                        .iter()
                        .map(bar)
                        .collect::<Result<Vec<_>, _>>()?,
                )
                .map_err(|e| error(e.message))?;
            let intents = serde_json::to_value(
                cached
                    .runtime
                    .external_intents()
                    .map_err(|e| error(e.message))?,
            )
            .map_err(io)?;
            let prefix: Vec<_> = array(&intents)?
                .iter()
                .filter(|v| v["bar_index"].as_u64().unwrap() < previous.len() as u64)
                .cloned()
                .collect();
            if !previous.is_empty()
                && digest(&Value::Array(prefix))? != text(&state["intents_digest"])?
            {
                return Err(error("previous Pine intents changed during reconstruction"));
            }
            let latest: Vec<_> = array(&intents)?
                .iter()
                .filter(|v| v["bar_index"].as_u64() == Some(bars.len() as u64 - 1))
                .cloned()
                .collect();
            // These arrays were taken from the one mutable working state.
            // Return ownership after runtime inputs have been read, avoiding
            // two extra complete-history copies per bar close.
            state["accounts"] = Value::Array(frames);
            state["evaluations"] = Value::Array(evaluations);
            actions.extend(broker::translate(
                &latest,
                program.program.strategy_settings.pyramiding_limit,
                &views,
                config,
                &mut state,
            )?);
            state["evaluated_bars"] = json!(bars.len());
            state["last_bar_time"] = bars.last().unwrap()["time"].clone();
            state["last_intents"] = Value::Array(latest);
            state["intents_digest"] = json!(digest(&intents)?);
            state["last_actions"] = json!(actions);
            state["skipped_bar_decisions"] = json!(
                state
                    .get("skipped_bar_decisions")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    + if previous.is_empty() {
                        0
                    } else {
                        (bars.len() - previous.len() - 1) as u64
                    }
            );
            state["bars"] = Value::Array(bars);
            cached.transcript = json!({"bars":state["bars"],"accounts":state["accounts"],"providers":state["providers"],"evaluations":state["evaluations"]});
            *slot = Some(cached);
        }
        actions.extend(broker::replacements(&mut state, &views, config)?);
        let (_, account, position, _) = broker::account(observation, config)?;
        state["observed_position"] = json!(position);
        state["observed_cash"] = json!(broker::decimal(&account["cash_balance"])?);
        let primary = text(&observation["instrument_id"])?;
        state["last_fill_id"] = state["ledgers"][primary]["fills"]
            .as_array()
            .and_then(|a| a.last())
            .map(|f| f["id"].clone())
            .unwrap_or(json!(-1));
        state["last_account"] = observation["own_account"].clone();
        state.as_object_mut().unwrap().remove("_next_wake_ms");
        let metadata = json!({"protocol_version":input["protocol_version"],"plugin_id":input["plugin_id"],"plugin_version":input["plugin_version"],"state_version":input["state_version"]});
        // Add two object entries without cloning the large state or action array.
        let size = python_json_len(&metadata, true)?
            + 2
            + 11
            + python_json_len(&Value::Array(actions.clone()), true)?
            + 2
            + 9
            + python_json_len(&state, true)?;
        if actions.len() > 64 || size > 1_048_576 {
            return Err(error("Pine decision exceeds bot action/state limits"));
        }
        if Instant::now() >= deadline {
            return Err(error("Pine decision timed out"));
        }
        Ok((actions, state))
    }
}

/// Called only on the owned speculative working copy. Failed decisions leave
/// the committed input untouched, including malformed restored array fields.
fn take_array(state: &mut Value, field: &str) -> Result<Vec<Value>, BotError> {
    match state.as_object_mut().unwrap().remove(field) {
        None => Ok(Vec::new()),
        Some(Value::Array(values)) => Ok(values),
        Some(_) => Err(error("expected array")),
    }
}

fn timeframe(ms: i64) -> Result<String, BotError> {
    if ms == 86_400_000 {
        Ok("1D".into())
    } else if ms % 60_000 == 0 {
        Ok((ms / 60_000).to_string())
    } else if ms % 1000 == 0 {
        Ok(format!("{}S", ms / 1000))
    } else {
        Err(error("Pine timeframes require integral seconds"))
    }
}
fn interval(tf: &str) -> Result<i64, BotError> {
    let (value, scale) = if let Some(n) = tf.strip_suffix('D') {
        (n, 86_400_000)
    } else if let Some(n) = tf.strip_suffix('S') {
        (n, 1000)
    } else {
        (tf, 60_000)
    };
    value
        .parse::<i64>()
        .map_err(io)?
        .checked_mul(scale)
        .ok_or_else(|| error("timeframe overflow"))
}
fn bar(v: &Value) -> Result<Bar, BotError> {
    Ok(Bar {
        time: number(&v["time"], 0)?,
        open: number(&v["open"], 1)? as f64,
        high: number(&v["high"], 1)? as f64,
        low: number(&v["low"], 1)? as f64,
        close: number(&v["close"], 1)? as f64,
        volume: number(&v["volume"], 0)? as f64,
    })
}
fn account_frame(v: &Value) -> Result<ExternalAccountFrame, BotError> {
    serde_json::from_value(v.clone()).map_err(io)
}
