//! Position protection follows the Python adapter, including decision-local
//! insertion order, trigger latching and between-bar price wakes.
use super::*;

fn order(state: &mut Value) -> Result<&mut Vec<Value>> {
    if state.get("_inprocess_exit_order").is_none() {
        let keys: Vec<_> = state
            .get("exit_specs")
            .map(|v| {
                v.as_object()
                    .ok_or_else(|| error("invalid exit state"))
                    .map(|m| m.keys().cloned().map(Value::String).collect())
            })
            .transpose()?
            .unwrap_or_default();
        state["_inprocess_exit_order"] = json!(keys);
    }
    state["_inprocess_exit_order"]
        .as_array_mut()
        .ok_or_else(|| error("invalid exit order"))
}

pub(super) fn register(
    state: &mut Value,
    views: &[&Value],
    config: &Value,
    instrument: &str,
    name: &str,
    intent: &Value,
) -> Result<()> {
    let entry = match intent["from_entry"].as_str().filter(|s| !s.is_empty()) {
        None if intent["from_entry"].is_null() || intent["from_entry"] == "" => None,
        None => return Err(error("invalid exit entry ID")),
        Some(raw) => {
            let (market, id) = raw.split_once("::").unwrap_or((instrument, raw));
            if market != instrument
                || id.is_empty()
                || id.chars().count() > 120
                || !views.iter().any(|v| v["instrument_id"] == market)
            {
                return Err(error("exit and entry must use the same declared market"));
            }
            Some(id)
        }
    };
    let key = format!("exit_{}", key(state, instrument, name)?);
    order(state)?;
    if state.get("exit_specs").is_none() {
        state["exit_specs"] = json!({});
    }
    if state["exit_specs"].get(&key).is_none() {
        order(state)?.push(json!(key));
    }
    let old = &state["exit_specs"][&key];
    let spec = json!({"instrument":instrument,"name":name,"entry_id":entry,
        "position_side":config["position_side"],"intent":intent,"installed":old["installed"],
        "trail_active":old.get("trail_active").cloned().unwrap_or(json!(false))});
    state["exit_specs"][&key] = spec;
    Ok(())
}

pub(super) fn cancel(state: &mut Value, view: &Value, name: Option<&str>) -> Result<Vec<Value>> {
    if state.get("exit_specs").is_none() {
        return Ok(Vec::new());
    }
    let keys = order(state)?.clone();
    let mut actions = Vec::new();
    for key in keys {
        let key = text(&key)?;
        let spec = &state["exit_specs"][key];
        if spec["instrument"] != view["instrument_id"] || name.is_some_and(|n| spec["name"] != n) {
            continue;
        }
        if let Some(installed) = spec["installed"].as_object().filter(|m| !m.is_empty()) {
            if view["position_protections"].as_array().is_some_and(|ps| {
                ps.iter().any(|p| {
                    p["position_side"] == spec["position_side"]
                        && installed.iter().all(|(k, v)| p["spec"][k] == *v)
                })
            }) {
                actions.push(tracked(text(&view["instrument_id"])?, key,
                    json!({"SetPositionProtection":{"position_side":spec["position_side"],"protection":null}})));
            }
        }
        state["exit_specs"]
            .as_object_mut()
            .ok_or_else(|| error("invalid exit state"))?
            .remove(key);
        order(state)?.retain(|v| v != key);
    }
    Ok(actions)
}

// Python performs integer arithmetic until a float participates. Preserve that
// distinction before its decimal-string grid normalization (do not use FMA).
fn relative(value: &Value, tick: i64, sign: i64, avg: i64) -> Result<Value> {
    if let Some(n) = value.as_i64() {
        let n = i128::from(avg) + i128::from(sign) * i128::from(n) * i128::from(tick);
        return Ok(json!(i64::try_from(n).map_err(io)?));
    }
    let n = value
        .as_f64()
        .filter(|n| n.is_finite())
        .ok_or_else(|| error("invalid exit distance"))?;
    let delta = (sign as f64 * n) * tick as f64;
    serde_json::to_value(avg as f64 + delta).map_err(io)
}

pub(super) fn evaluate(
    state: &mut Value,
    views: &[&Value],
    config: &Value,
    budget: &mut Budget,
) -> Result<Vec<Value>> {
    if state
        .get("exit_specs")
        .is_none_or(|v| v.as_object().is_some_and(|m| m.is_empty()))
    {
        state
            .as_object_mut()
            .unwrap()
            .remove("_inprocess_exit_order");
        state.as_object_mut().unwrap().remove("_wake_prices");
        return Ok(Vec::new());
    }
    let keys = order(state)?.clone();
    state
        .as_object_mut()
        .unwrap()
        .remove("_inprocess_exit_order");
    let mut actions = Vec::new();
    let mut wakes: BTreeMap<String, Value> = BTreeMap::new();
    let mut wake = |instrument: &str, direction: i64, price: i64| {
        let level = wakes
            .entry(instrument.into())
            .or_insert_with(|| json!({"instrument_id":instrument}));
        let field = if direction > 0 {
            "at_or_above"
        } else {
            "at_or_below"
        };
        let previous = level[field].as_i64().unwrap_or(price);
        level[field] = json!(if direction > 0 {
            previous.min(price)
        } else {
            previous.max(price)
        });
    };
    for key in keys {
        let key = text(&key)?;
        let mut spec = state["exit_specs"][key].clone();
        let instrument = text(&spec["instrument"])?.to_owned();
        let view = views
            .iter()
            .find(|v| v["instrument_id"] == instrument)
            .ok_or_else(|| error("missing exit market"))?;
        let (_, _, position, avg) = account(view, config)?;
        if position == 0 {
            state["exit_specs"][key]["installed"] = Value::Null;
            state["exit_specs"][key]["trail_active"] = json!(false);
            continue;
        }
        if view["position_protections"].as_array().is_some_and(|ps| {
            ps.iter()
                .any(|p| p["position_side"] == spec["position_side"] && p["status"] == "triggered")
        }) {
            continue;
        }
        let intent = spec["intent"].clone();
        let sign = position.signum();
        let tick = number(&json!(default_int(&view["instrument"]["tick_size"], 1)?), 1)?;
        let owned = if let Some(entry) = spec["entry_id"].as_str() {
            default_int(
                &state["entries"][format!("{instrument}::{entry}")]["qty"],
                0,
            )?
        } else {
            position
        };
        if owned.signum() * sign <= 0 {
            continue;
        }
        let qty = close_quantity(&intent, position.abs().min(owned.abs()), config)?;
        if qty == 0 {
            continue;
        }
        let mut take = intent["limit"].clone();
        let mut stop = intent["stop"].clone();
        if take.is_null() && !intent["profit"].is_null() {
            take = relative(&intent["profit"], tick, sign, avg)?;
        }
        if stop.is_null() && !intent["loss"].is_null() {
            stop = relative(&intent["loss"], tick, -sign, avg)?;
        }
        let last = view["public_trades"]
            .as_array()
            .and_then(|ts| ts.last())
            .map(|t| &t["price_tick"]);
        let Some(last) = last.filter(|p| !p.is_null()) else {
            wake(&instrument, 1, 1);
            continue;
        };
        let last = number(last, 1)?;
        let close_side = if sign > 0 { "Sell" } else { "Buy" };
        let trigger_side = if sign > 0 { "Buy" } else { "Sell" };
        let take = strategy_price(&take, tick, close_side, config)?;
        // Trigger rounding is the reverse of limit rounding.
        let stop = strategy_price(&stop, tick, trigger_side, config)?;
        if [take, stop].into_iter().flatten().any(|p| p % tick != 0) {
            return Err(error("exit price must align with the exchange tick size"));
        }
        if let Some(p) = take {
            wake(&instrument, sign, p);
        }
        if let Some(p) = stop {
            wake(&instrument, -sign, p);
        }
        if take.is_some_and(|p| sign * (last - p) >= 0)
            || stop.is_some_and(|p| sign * (last - p) <= 0)
        {
            actions.extend(submit(
                state,
                view,
                config,
                budget,
                &format!("{}_triggered", text(&spec["name"])?),
                close_side,
                qty,
                &Value::Null,
                true,
                spec["entry_id"].as_str(),
                false,
            )?);
            continue;
        }
        let mut activation = intent["trail_price"].clone();
        if activation.is_null() && !intent["trail_points"].is_null() {
            activation = relative(&intent["trail_points"], tick, sign, avg)?;
        }
        let activation = strategy_price(&activation, tick, close_side, config)?;
        if activation.is_some_and(|p| sign * (last - p) >= 0) {
            spec["trail_active"] = json!(true);
        }
        if let Some(p) = activation.filter(|_| spec["trail_active"] != true) {
            wake(&instrument, sign, p);
        }
        let trail = if spec["trail_active"] == true {
            let distance = relative(&intent["trail_offset"], tick, 1, 0)?;
            Some(if config["quantize_strategy_orders"] == true {
                quantized(&distance, tick, true)?
            } else {
                number(&distance, 1)?
            })
        } else {
            None
        };
        // Activation is durable even when no protection is installed yet.
        state["exit_specs"][key] = spec.clone();
        if take.is_none() && stop.is_none() && trail.is_none() {
            continue;
        }
        let protection = json!({"take_profit_tick":take,"stop_loss_tick":stop,"trigger":"Last","exit_qty":qty,"trailing_distance_tick":trail});
        if state["orders"].get(key).is_none() {
            state["orders"][key] =
                json!({"instrument":instrument,"name":spec["name"],"entry":null});
        }
        let order = &mut state["orders"][key];
        if order["pending"] != true
            && (spec["installed"] != protection || order["rejected"] == true)
        {
            order["pending"] = json!(true);
            state["exit_specs"][key]["installed"] = protection.clone();
            actions.push(tracked(&instrument, key, json!({"SetPositionProtection":{"position_side":spec["position_side"],"protection":protection}})));
        }
    }
    state.as_object_mut().unwrap().remove("_wake_prices");
    if !wakes.is_empty() {
        state["_wake_prices"] = json!(wakes.into_values().collect::<Vec<_>>());
    }
    Ok(actions)
}
