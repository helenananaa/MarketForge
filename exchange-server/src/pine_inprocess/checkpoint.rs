//! Compact durable execution for the existing hash-qualified Pine programs.
//! VM checkpoints plus at most 31 frozen passes replace full transcripts.
//! Candle windows are authorized by the host's complete-prefix hash chain.
use super::*;
use exchange_core::{
    Candle, CandleHistoryProof, candle_history_append, candle_history_hex, candle_history_seed,
};
use serde::Deserialize;

const CHECKPOINT_EVERY: usize = 32;

#[cfg(test)]
fn stamp(state: &Value) -> Value {
    json!({"checkpoint": state["vm_checkpoint"], "tail": state["vm_tail"], "intents": state["vm_tail_intents"]})
}

#[cfg(test)]
fn stamp_matches(saved: &Value, state: &Value) -> bool {
    saved.as_object().is_some_and(|object| {
        object.len() == 3
            && object.get("checkpoint") == Some(&state["vm_checkpoint"])
            && object.get("tail") == Some(&state["vm_tail"])
            && object.get("intents") == Some(&state["vm_tail_intents"])
    })
}

fn execution_seal_json(state: &Value) -> Result<String, BotError> {
    use super::json_compat::{PythonJsonRef as Ref, python_json_ref};
    let vm = [
        ("checkpoint", Ref::Value(&state["vm_checkpoint"])),
        ("tail", Ref::Value(&state["vm_tail"])),
        ("intents", Ref::Value(&state["vm_tail_intents"])),
    ];
    let fields = [
        ("protocol", Ref::String("marketforge.pine-execution/1")),
        ("vm", Ref::Object(&vm)),
        ("evaluated", Ref::Value(&state["evaluated_bars"])),
        ("last_time", Ref::Value(&state["last_bar_time"])),
        ("audits", Ref::Value(&state["candle_audits"])),
    ];
    python_json_ref(Ref::Object(&fields), false)
}

fn execution_seal(state: &Value) -> Result<String, BotError> {
    #[cfg(feature = "bench-execution-timing")]
    let _phase_timer =
        crate::bench_execution_timing::Timer::start(&crate::bench_execution_timing::BOT_PINE_SEAL);
    Ok(format!(
        "{:x}",
        Sha256::digest(execution_seal_json(state)?.as_bytes())
    ))
}

/// Check the complete prefix even when only a suffix is supplied. A proof is
/// issued by the authoritative host; the bot never manufactures a prefix.
fn audit_stream(data: &Value, previous: Option<&Value>) -> Result<(usize, Value), BotError> {
    #[cfg(feature = "bench-execution-timing")]
    let _phase_timer =
        crate::bench_execution_timing::Timer::start(&crate::bench_execution_timing::BOT_PINE_AUDIT);
    let interval = number(&data["interval_ms"], 1)? as u64;
    let proof = data
        .get("history_proof")
        .filter(|p| !p.is_null())
        .map(|p| serde_json::from_value::<CandleHistoryProof>(p.clone()).map_err(io))
        .transpose()?;
    let (prefix, mut hash) = if let Some(proof) = proof {
        (
            proof.prefix_bars,
            proof
                .prefix_digest()
                .ok_or_else(|| error("invalid authoritative candle proof"))?,
        )
    } else {
        if data["truncated"] != false {
            return Err(error(
                "truncated candle history requires authoritative prefix proof",
            ));
        }
        (0, candle_history_seed(interval))
    };
    if prefix == 0 && hash != candle_history_seed(interval) {
        return Err(error("candle proof interval/seed mismatch"));
    }
    let candles = array(&data["candles"])?;
    let total = prefix
        .checked_add(candles.len() as u64)
        .ok_or_else(|| error("candle cursor overflow"))?;
    let saved = previous
        .map(|v| {
            v["count"]
                .as_u64()
                .ok_or_else(|| error("invalid saved candle count"))
        })
        .transpose()?
        .unwrap_or(0);
    if saved < prefix || saved > total {
        return Err(error(
            "saved candle cursor is outside the authoritative window",
        ));
    }
    let saved_hash = previous.map(|v| text(&v["sha256"])).transpose()?;
    let mut verified = previous.is_none();
    if saved == prefix {
        verified = saved_hash.is_none_or(|old| old == candle_history_hex(&hash));
        if !verified {
            return Err(error("committed candle prefix changed"));
        }
    }
    for (index, value) in candles.iter().enumerate() {
        let candle = Candle::deserialize(value).map_err(io)?;
        hash = candle_history_append(&hash, &candle);
        if prefix + index as u64 + 1 == saved {
            if saved_hash.is_some_and(|old| old != candle_history_hex(&hash)) {
                return Err(error("committed candle prefix changed"));
            }
            verified = true;
        }
    }
    if !verified {
        return Err(error("committed candle prefix was not verified"));
    }
    let last = candles
        .last()
        .cloned()
        .or_else(|| previous.and_then(|v| v.get("last")).cloned());
    Ok((
        (saved - prefix) as usize,
        json!({"count":total,"sha256":candle_history_hex(&hash),"last":last}),
    ))
}

fn fresh(
    program: &Program,
    config: &Value,
    observation: &Value,
) -> Result<HistoricalRuntime<'static>, BotError> {
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
    let values = calls
        .iter()
        .map(|call| {
            let value = call
                .title
                .as_ref()
                .and_then(|title| config["inputs"].get(title));
            let value = if let Some(value) = value {
                number(value, -broker::MAX_EXACT)?
            } else if let Some(PineValue::Int(value)) = call.default_value {
                value
            } else {
                return Err(error("compact history requires fixed integer inputs"));
            };
            Ok((call.call_site_id, value))
        })
        .collect::<Result<Vec<_>, BotError>>()?;
    let specialized = PreparedProgram::new(
        pine_sema::specialize_integer_input_history(&program.program, &values)
            .ok_or_else(|| error("Pine input history cannot be statically specialized"))?,
    );
    if specialized
        .series_history
        .iter()
        .any(|requirement| requirement.has_dynamic_offsets)
    {
        return Err(error(
            "Pine configured history has no static retention bound",
        ));
    }
    let tick = observation["instrument"]
        .get("tick_size")
        .map(|v| number(v, 1))
        .transpose()?
        .unwrap_or(1);
    let chart = ChartContext::new(
        text(&observation["instrument_id"])?,
        RequestTimeframe::parse(&timeframe(number(&config["bar_interval_ms"], 1)?)?).map_err(io)?,
    )
    .with_price_grid(u32::try_from(tick).map_err(io)?, 1)
    .map_err(io)?
    .with_quantity_precision(0)
    .map_err(io)?;
    Ok(
        HistoricalRuntime::from_prepared_with_request_environment_and_input_overrides(
            &specialized,
            RequestEnvironment::new(chart, Arc::new(NoRequestDataProvider)),
            overrides,
        ),
    )
}

// Preserve floating-point input bits across the real durable JSON parser as
// well as VM snapshots, without changing the global host parser's features.
fn freeze(value: Value) -> Value {
    match value {
        Value::Number(ref n) if n.is_f64() => {
            json!({"$f64":format!("{:016x}", n.as_f64().unwrap().to_bits())})
        }
        Value::Array(values) => Value::Array(values.into_iter().map(freeze).collect()),
        Value::Object(values) => {
            Value::Object(values.into_iter().map(|(k, v)| (k, freeze(v))).collect())
        }
        value => value,
    }
}
fn thaw(value: &Value) -> Result<Value, BotError> {
    if let Some(object) = value.as_object() {
        if let Some(bits) = object.get("$f64") {
            if object.len() != 1 {
                return Err(error("invalid frozen float object"));
            }
            let bits = text(bits)?;
            if bits.len() != 16 {
                return Err(error("invalid frozen float bits"));
            }
            let value = f64::from_bits(u64::from_str_radix(bits, 16).map_err(io)?);
            return serde_json::Number::from_f64(value)
                .map(Value::Number)
                .ok_or_else(|| error("nonfinite frozen input"));
        }
        return object
            .iter()
            .map(|(k, v)| Ok((k.clone(), thaw(v)?)))
            .collect::<Result<serde_json::Map<_, _>, BotError>>()
            .map(Value::Object);
    }
    if let Some(values) = value.as_array() {
        return values
            .iter()
            .map(thaw)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array);
    }
    Ok(value.clone())
}
fn append(
    runtime: &mut HistoricalRuntime<'static>,
    frozen: &[Value],
    deadline: Instant,
) -> Result<Vec<Value>, BotError> {
    if frozen.is_empty() {
        return Ok(vec![]);
    }
    let passes = frozen
        .iter()
        .map(|v| serde_json::from_value::<ExternalPass>(thaw(v)?).map_err(io))
        .collect::<Result<Vec<_>, _>>()?;
    let bars = passes.iter().map(|p| p.bar).collect::<Vec<_>>();
    let frames = passes.iter().map(|p| p.account.clone()).collect::<Vec<_>>();
    let groups = Some(passes.into_iter().map(|p| vec![p]).collect());
    if runtime.external_intents().is_ok() {
        runtime
            .extend_external_accounts_and_passes(frames, groups)
            .map_err(|e| error(format!("{e:?}")))?;
    } else {
        *runtime = runtime
            .clone()
            .with_external_accounts_and_passes(frames, groups)
            .map_err(|e| error(format!("{e:?}")))?;
    }
    runtime.set_host_deadline(Some(deadline));
    {
        #[cfg(feature = "bench-execution-timing")]
        let _phase_timer = crate::bench_execution_timing::Timer::start(
            &crate::bench_execution_timing::BOT_PINE_VM_EXECUTE,
        );
        runtime
            .append_bars(&bars)
            .map_err(|e| error(format!("{e:?}")))?;
    }
    let intents = serde_json::to_value(
        runtime
            .take_external_intents()
            .map_err(|e| error(format!("{e:?}")))?,
    )
    .map_err(io)?;
    runtime
        .compact_external_feedback()
        .map_err(|e| error(format!("{e:?}")))?;
    runtime.compact_external_outputs();
    Ok(array(&intents)?.clone())
}

pub(super) fn decide(
    host: &Host,
    input: &Value,
    committed: &Value,
    deadline: Instant,
) -> Result<(Vec<Value>, Value), BotError> {
    let config = &input["config"];
    let observation = &input["observation"];
    let program = &host.programs[text(&config["script"])?];
    let identity = digest(
        &json!({"source":program.source,"config":config,"participant":input["participant"]}),
    )?;
    let mut state = {
        #[cfg(feature = "bench-execution-timing")]
        let _phase_timer = crate::bench_execution_timing::Timer::start(
            &crate::bench_execution_timing::BOT_PINE_WORKING_CLONE,
        );
        if committed.is_null() {
            json!({})
        } else {
            committed.clone()
        }
    };
    let cold = state.as_object().is_some_and(|v| v.is_empty());
    if !cold && (state["broker_schema"] != 3 || state["identity"] != identity) {
        return Err(error("Pine checkpoint source/config/account changed"));
    }
    if !cold && state["execution_seal"].as_str() != Some(execution_seal(&state)?.as_str()) {
        return Err(error("Pine checkpoint execution seal mismatch"));
    }
    let mut views = vec![observation];
    if let Some(related) = observation.get("related_markets") {
        views.extend(array(related)?);
    }
    let mut seen = std::collections::BTreeSet::new();
    for view in &views {
        if !seen.insert(text(&view["instrument_id"])?) {
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
        if view["bot_market_data"]["private_history_complete"] != true {
            return Err(error("complete authoritative private history required"));
        }
    }
    let data = &observation["bot_market_data"];
    if data["interval_ms"] != config["bar_interval_ms"] {
        return Err(error("host must supply configured history"));
    }
    let now = number(&observation["market_time_ms"], 0)?;
    let bars = broker::checkpoint_bars(data, now)?;
    if cold && bars.len() > number(&config["history_limit"], 0)? as usize {
        return Err(error("cold initialization history_limit exceeded"));
    }
    let primary_key = format!(
        "{}:{}",
        text(&observation["instrument_id"])?,
        timeframe(number(&data["interval_ms"], 1)?)?
    );
    let mut streams = BTreeMap::new();
    for view in &views {
        let stream = &view["bot_market_data"];
        let key = format!(
            "{}:{}",
            text(&view["instrument_id"])?,
            timeframe(number(&stream["interval_ms"], 1)?)?
        );
        streams.insert(key, stream);
    }
    if let Some(additional) = data.get("streams") {
        for stream in array(additional)? {
            let key = format!(
                "{}:{}",
                text(&stream["instrument_id"])?,
                timeframe(number(&stream["interval_ms"], 1)?)?
            );
            if streams.insert(key, stream).is_some() {
                return Err(error("duplicate requested stream"));
            }
        }
    }
    if let Some(old) = state.get("candle_audits").and_then(Value::as_object)
        && old.keys().any(|key| !streams.contains_key(key))
    {
        return Err(error("committed requested stream disappeared"));
    }
    let mut audits = serde_json::Map::new();
    let mut requested = BTreeMap::new();
    let mut start = 0;
    for (key, stream) in streams {
        let previous = state.get("candle_audits").and_then(|v| v.get(&key));
        let (offset, audit) = audit_stream(stream, previous)?;
        if key == primary_key {
            start = offset;
        }
        let mut values = broker::checkpoint_bars(stream, now)?;
        // A proof can start exactly at the saved cursor. Retain one prior
        // authoritative provider sample for gaps_off's carry-forward value.
        if let Some(last) = previous
            .and_then(|v| v.get("last"))
            .filter(|v| !v.is_null())
            && values
                .first()
                .is_none_or(|v| last["open_time_ms"].as_u64() < v["time"].as_u64())
        {
            let prior = broker::checkpoint_bars(&json!({"candles":[last]}), now)?;
            values.splice(0..0, prior);
        }
        requested.insert(key.clone(), values);
        audits.insert(key, audit);
    }
    let count = audits[&primary_key]["count"].as_u64().unwrap();
    if !cold {
        let evaluated = state["evaluated_bars"]
            .as_u64()
            .ok_or_else(|| error("invalid checkpoint evaluation cursor"))?;
        let checkpoint_bars = if state["vm_checkpoint"].is_null() {
            0
        } else {
            state["vm_checkpoint"]["bars"]
                .as_u64()
                .ok_or_else(|| error("invalid VM checkpoint cursor"))?
        };
        let tail = array(&state["vm_tail"])?;
        let audited = state["candle_audits"][&primary_key]["count"].as_u64();
        if tail.len() >= CHECKPOINT_EVERY
            || audited != Some(evaluated)
            || checkpoint_bars.checked_add(tail.len() as u64) != Some(evaluated)
            || evaluated.checked_add((bars.len() - start) as u64) != Some(count)
        {
            return Err(error("Pine checkpoint/tail/history cursor mismatch"));
        }
        let last_time = tail
            .last()
            .map(|v| &v["bar"]["time"])
            .unwrap_or(&state["vm_checkpoint"]["previous_time"]);
        if evaluated != 0 && *last_time != state["last_bar_time"] {
            return Err(error("Pine checkpoint tail time mismatch"));
        }
        array(&state["vm_tail_intents"])?;
    }
    if cold {
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
        state = json!({"identity":identity,"broker_schema":3,"initial_capital":equity,
            "vm_checkpoint":null,"vm_tail":[],"vm_tail_intents":[],"evaluated_bars":0});
    }
    {
        #[cfg(feature = "bench-execution-timing")]
        let _phase_timer = crate::bench_execution_timing::Timer::start(
            &crate::bench_execution_timing::BOT_PINE_BROKER_FEEDBACK,
        );
        broker::feedback(&mut state, &views, config)?;
    }
    let mut actions = Vec::new();
    if start < bars.len() {
        let settings = json!({"config":config,"seed":input["seed"],"tick":observation["instrument"]["tick_size"],"primary":observation["instrument_id"]});
        let slot = host.session(&format!("checkpoint:{identity}:{}", input["seed"]))?;
        let mut slot = slot.lock().map_err(io)?;
        let cached = slot.take();
        let reusable = cached.as_ref().is_some_and(|s| {
            s.settings == settings
                && s.checkpoint_stamp
                    .as_ref()
                    .is_some_and(|stamp| stamp.matches(committed))
        });
        crate::bot_runtime_diagnostics::record_evaluation(
            "direct",
            if reusable {
                "append"
            } else {
                "checkpoint_restore"
            },
            if reusable {
                None
            } else {
                Some(if cached.is_some() {
                    "state_mismatch"
                } else {
                    "session_missing"
                })
            },
        );
        #[cfg(feature = "bench-execution-timing")]
        let _session_timer = crate::bench_execution_timing::Timer::start(if reusable {
            &crate::bench_execution_timing::BOT_INPROCESS_APPEND
        } else {
            &crate::bench_execution_timing::BOT_INPROCESS_REBUILD
        });
        let mut cached = if reusable {
            cached.unwrap()
        } else {
            let mut runtime = fresh(program, config, observation)?;
            if !state["vm_checkpoint"].is_null() {
                runtime
                    .restore_external_checkpoint(&state["vm_checkpoint"], &identity)
                    .map_err(|e| error(format!("{e:?}")))?;
            }
            let replayed = append(&mut runtime, array(&state["vm_tail"])?, deadline)?;
            if freeze(Value::Array(replayed)) != state["vm_tail_intents"] {
                return Err(error("checkpoint tail intents differ on recovery"));
            }
            Session {
                runtime,
                settings,
                transcript: Value::Null,
                checkpoint_stamp: None,
            }
        };
        let mut frozen = Vec::new();
        for (index, value) in bars.iter().enumerate().skip(start) {
            let cutoff = number(&data["candles"][index]["close_time_ms"], 0)?;
            let frame = {
                #[cfg(feature = "bench-execution-timing")]
                let _phase_timer = crate::bench_execution_timing::Timer::start(
                    &crate::bench_execution_timing::BOT_PINE_BROKER_FRAME,
                );
                broker::frame(
                    observation,
                    config,
                    &state,
                    value,
                    (index + 1 != bars.len()).then_some(cutoff),
                )?
            };
            let mut provider_streams = Vec::new();
            // Among the six exact sources only basis requests data, and its
            // expression is scalar close at the identical chart timeframe.
            // Two visible samples preserve the current/prior merge boundary.
            if config["script"] == "basis.pine" {
                for (key, values) in &requested {
                    let (symbol, tf) = key.rsplit_once(':').unwrap();
                    let step = interval(tf)?;
                    let visible = values
                        .iter()
                        .filter(|b| {
                            number(&b["time"], 0)
                                .is_ok_and(|t| t as i128 + step as i128 <= cutoff as i128)
                        })
                        .collect::<Vec<_>>();
                    provider_streams.push(ExternalRequestStream {
                        symbol: symbol.into(),
                        timeframe: tf.into(),
                        bars: visible
                            .into_iter()
                            .rev()
                            .take(2)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .map(bar)
                            .collect::<Result<_, _>>()?,
                    });
                }
            }
            let pass = ExternalPass {
                account: account_frame(&frame)?,
                bar: bar(value)?,
                event_time_ms: cutoff,
                confirmed: true,
                request_data: Some(provider_streams),
            };
            frozen.push(freeze(serde_json::to_value(pass).map_err(io)?));
        }
        let intents = append(&mut cached.runtime, &frozen, deadline)?;
        let latest = intents
            .iter()
            .filter(|v| v["bar_index"].as_u64() == Some(count - 1))
            .cloned()
            .collect::<Vec<_>>();
        {
            #[cfg(feature = "bench-execution-timing")]
            let _phase_timer = crate::bench_execution_timing::Timer::start(
                &crate::bench_execution_timing::BOT_PINE_BROKER_TRANSLATE,
            );
            actions.extend(broker::translate(
                &latest,
                program.program.strategy_settings.pyramiding_limit,
                &views,
                config,
                &mut state,
            )?);
        }
        let mut tail = take_array(&mut state, "vm_tail")?;
        tail.extend(frozen);
        let mut tail_intents = take_array(&mut state, "vm_tail_intents")?;
        tail_intents.extend(intents.into_iter().map(freeze));
        if tail.len() >= CHECKPOINT_EVERY {
            state["vm_checkpoint"] = {
                #[cfg(feature = "bench-execution-timing")]
                let _phase_timer = crate::bench_execution_timing::Timer::start(
                    &crate::bench_execution_timing::BOT_PINE_VM_CHECKPOINT,
                );
                cached
                    .runtime
                    .export_external_checkpoint(&identity)
                    .map_err(|e| error(format!("{e:?}")))?
            };
            tail.clear();
            tail_intents.clear();
        }
        state["vm_tail"] = Value::Array(tail);
        state["vm_tail_intents"] = Value::Array(tail_intents);
        state["evaluated_bars"] = json!(count);
        state["last_bar_time"] = bars.last().unwrap()["time"].clone();
        state["last_intents"] = Value::Array(latest);
        state["last_actions"] = json!(actions);
        state["skipped_bar_decisions"] = json!(
            state["skipped_bar_decisions"].as_u64().unwrap_or(0)
                + if cold {
                    0
                } else {
                    (bars.len() - start - 1) as u64
                }
        );
        {
            #[cfg(feature = "bench-execution-timing")]
            let _phase_timer = crate::bench_execution_timing::Timer::start(
                &crate::bench_execution_timing::BOT_PINE_STAMP_CLONE,
            );
            cached
                .checkpoint_stamp
                .get_or_insert_with(stamp_cache::Stamp::default)
                .update(&state)
                .map_err(io)?;
        }
        *slot = Some(cached);
    } else {
        crate::bot_runtime_diagnostics::record_evaluation("direct", "no_evaluation", None);
    }
    state["candle_audits"] = Value::Object(audits);
    state["execution_seal"] = json!(execution_seal(&state)?);
    {
        #[cfg(feature = "bench-execution-timing")]
        let _phase_timer = crate::bench_execution_timing::Timer::start(
            &crate::bench_execution_timing::BOT_PINE_BROKER_REPLACEMENTS,
        );
        actions.extend(broker::replacements(&mut state, &views, config)?);
    }
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
    let size = {
        #[cfg(feature = "bench-execution-timing")]
        let _phase_timer = crate::bench_execution_timing::Timer::start(
            &crate::bench_execution_timing::BOT_PINE_RESPONSE_LENGTH,
        );
        python_json_len(&metadata, true)?
            + 2
            + 11
            + python_json_len(&Value::Array(actions.clone()), true)?
            + 2
            + 9
            + python_json_len(&state, true)?
    };
    if actions.len() > 64 || size > 1_048_576 {
        return Err(error("Pine decision exceeds bot action/state limits"));
    }
    if Instant::now() >= deadline {
        return Err(error("Pine decision timed out"));
    }
    Ok((actions, state))
}

#[cfg(test)]
mod borrowed_seal_tests {
    use super::super::json_compat::python_json;
    use super::*;

    fn previous_payload(state: &Value) -> Value {
        json!({"protocol":"marketforge.pine-execution/1","vm":stamp(state),
            "evaluated":state["evaluated_bars"],"last_time":state["last_bar_time"],
            "audits":state["candle_audits"]})
    }

    #[test]
    fn borrowed_seal_is_byte_identical_for_missing_null_nested_and_numeric_fields() {
        let mut cases = vec![
            Value::Null,
            json!({}),
            json!({
                "vm_checkpoint":null,"vm_tail":null,"vm_tail_intents":null,
                "evaluated_bars":null,"last_bar_time":null,"candle_audits":null
            }),
            json!({
                "vm_checkpoint":{"z":"策略🐱", "a":[true,false,null]},
                "vm_tail":[{"bar":{"time":0,"close":{"$f64":"8000000000000000"}}}],
                "vm_tail_intents":[],"evaluated_bars":33,"last_bar_time":1000,
                "candle_audits":{"Z:1S":{"sha256":"abcd","last":{"close_tick":17},"count":33}}
            }),
        ];
        for number in [
            json!(i64::MIN),
            json!(u64::MAX),
            json!(0),
            json!(1.0),
            json!(-0.0),
            json!(1e-7),
            json!(1e20),
            json!(f64::from_bits(1)),
        ] {
            cases.push(json!({"vm_checkpoint":{"z":number,"a":number},
                "vm_tail":[number],"vm_tail_intents":[{"x":number}],
                "evaluated_bars":number,"last_bar_time":number,
                "candle_audits":{"z":number,"a":{"last":number}}}));
        }
        for state in &cases {
            let expected = previous_payload(state);
            assert_eq!(
                execution_seal_json(state).unwrap(),
                python_json(&expected, false).unwrap()
            );
            assert_eq!(execution_seal(state).unwrap(), digest(&expected).unwrap());
            // Every missing field retains Value indexing's existing null rule.
            if let Some(object) = state.as_object() {
                for key in object.keys() {
                    let mut missing = state.clone();
                    missing.as_object_mut().unwrap().remove(key);
                    assert_eq!(
                        execution_seal_json(&missing).unwrap(),
                        python_json(&previous_payload(&missing), false).unwrap()
                    );
                }
            }
        }
        // Field insertion order, including nested objects, cannot affect bytes.
        let first: Value = serde_json::from_str(
            r#"{"vm_checkpoint":{"z":-0.0,"a":1e20},"vm_tail":[],"candle_audits":{"b":2,"a":1}}"#,
        )
        .unwrap();
        let second: Value = serde_json::from_str(
            r#"{"candle_audits":{"a":1,"b":2},"vm_tail":[],"vm_checkpoint":{"a":1e20,"z":-0.0}}"#,
        )
        .unwrap();
        assert_eq!(
            execution_seal_json(&first).unwrap(),
            execution_seal_json(&second).unwrap()
        );
    }

    #[test]
    fn borrowed_stamp_matches_old_equality_and_owned_stamp_survives_cow() {
        let mut committed = Arc::new(json!({"vm_checkpoint":{"bars":32},
            "vm_tail":[{"bar":{"time":33}}],"vm_tail_intents":[]}));
        let original = committed.clone();
        let saved = stamp(&committed);
        assert!(stamp_matches(&saved, &committed));
        for key in ["vm_checkpoint", "vm_tail", "vm_tail_intents"] {
            let mut changed = original.clone();
            Arc::make_mut(&mut changed)[key] = json!({"changed":true});
            assert!(!stamp_matches(&saved, &changed));
            assert_eq!(stamp_matches(&saved, &changed), saved == stamp(&changed));
        }
        Arc::make_mut(&mut committed)["vm_tail"] = json!([]);
        assert!(stamp_matches(&saved, &original));
        assert!(!stamp_matches(&saved, &committed));
        for saved in [
            Value::Null,
            json!({}),
            json!({"checkpoint":null,"tail":null}),
            stamp(&json!({})),
            json!({"checkpoint":null,"tail":null,"intents":null,"extra":0}),
        ] {
            for state in [Value::Null, json!({}), committed.as_ref().clone()] {
                assert_eq!(stamp_matches(&saved, &state), saved == stamp(&state));
            }
        }
    }
}

#[cfg(test)]
mod borrowed_candle_tests {
    use super::*;

    fn candle_value() -> Value {
        json!({"schema_version":1,"open_time_ms":0,"close_time_ms":1000,
            "open_tick":1,"high_tick":2,"low_tick":1,"close_tick":2,
            "volume":1,"quote_volume":2,"trades":1,"is_final":true})
    }

    fn equivalent(value: &Value) -> Result<Candle, String> {
        let owned = serde_json::from_value::<Candle>(value.clone());
        let borrowed = Candle::deserialize(value);
        match (owned, borrowed) {
            (Ok(owned), Ok(borrowed)) => {
                assert_eq!(owned, borrowed);
                assert_eq!(
                    candle_history_append(&candle_history_seed(1000), &owned),
                    candle_history_append(&candle_history_seed(1000), &borrowed),
                );
                Ok(borrowed)
            }
            (Err(owned), Err(borrowed)) => {
                assert_eq!(owned.classify(), borrowed.classify());
                assert_eq!(owned.to_string(), borrowed.to_string());
                Err(borrowed.to_string())
            }
            (owned, borrowed) => panic!("Candle decode diverged: {owned:?} vs {borrowed:?}"),
        }
    }

    #[test]
    fn borrowed_candle_matches_owned_at_integer_and_optional_boundaries() {
        let omitted = equivalent(&candle_value()).unwrap();
        assert_eq!(omitted.taker_buy_base, None);
        assert_eq!(omitted.taker_buy_quote, None);
        let mut explicit_null = candle_value();
        explicit_null["taker_buy_base"] = Value::Null;
        explicit_null["taker_buy_quote"] = Value::Null;
        assert_eq!(equivalent(&explicit_null).unwrap(), omitted);

        let mut limits = candle_value();
        limits["schema_version"] = json!(u16::MAX);
        for key in [
            "open_time_ms",
            "close_time_ms",
            "volume",
            "trades",
            "taker_buy_base",
        ] {
            limits[key] = json!(u64::MAX);
        }
        for key in [
            "open_tick",
            "high_tick",
            "low_tick",
            "close_tick",
            "quote_volume",
            "taker_buy_quote",
        ] {
            limits[key] = json!(i64::MIN);
        }
        let lower = equivalent(&limits).unwrap();
        assert_eq!(lower.quote_volume, i64::MIN as i128);
        assert_eq!(lower.taker_buy_quote, Some(i64::MIN as i128));
        assert_eq!(lower.volume, u64::MAX);
        assert_eq!(lower.taker_buy_base, Some(u64::MAX));

        // Value stores signed/unsigned 64-bit integers, both of which must widen
        // exactly into Candle's i128 fields; larger JSON numbers become floats.
        for key in ["quote_volume", "taker_buy_quote"] {
            limits[key] = json!(u64::MAX);
        }
        for key in ["open_tick", "high_tick", "low_tick", "close_tick"] {
            limits[key] = json!(i64::MAX);
        }
        let upper = equivalent(&limits).unwrap();
        assert_eq!(upper.quote_volume, u64::MAX as i128);
        assert_eq!(upper.taker_buy_quote, Some(u64::MAX as i128));
        limits["unknown_future_field"] = json!({"nested":[null,"ignored"]});
        assert_eq!(equivalent(&limits).unwrap(), upper);
    }

    #[test]
    fn borrowed_candle_preserves_owned_rejection_and_errors() {
        for (key, invalid) in [
            ("schema_version", json!(u16::MAX as u64 + 1)),
            ("open_time_ms", json!(-1)),
            ("volume", json!(-1)),
            ("volume", json!(1.5)),
            (
                "volume",
                serde_json::from_str("18446744073709551616").unwrap(),
            ),
            ("open_tick", json!(u64::MAX)),
            ("quote_volume", json!(1.5)),
            (
                "quote_volume",
                serde_json::from_str("170141183460469231731687303715884105727").unwrap(),
            ),
            (
                "quote_volume",
                json!("170141183460469231731687303715884105727"),
            ),
            ("taker_buy_base", json!(-1)),
            ("taker_buy_quote", json!(1.5)),
            ("is_final", json!(1)),
        ] {
            let mut value = candle_value();
            value[key] = invalid;
            assert!(equivalent(&value).is_err(), "{key}: {value}");
        }
        for key in ["schema_version", "quote_volume", "is_final"] {
            let mut value = candle_value();
            value.as_object_mut().unwrap().remove(key);
            assert!(equivalent(&value).is_err(), "missing {key}");
        }
        for value in [Value::Null, json!([]), json!("candle")] {
            assert!(equivalent(&value).is_err());
        }
    }
}
