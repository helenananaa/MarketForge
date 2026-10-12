//! Schema-2 broker translation for the hash-qualified entry/close/cancel sources.
use super::{error, io};
use exchange_core::BotError;
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{ToPrimitive, Zero};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, str::FromStr};

pub const MAX_EXACT: i64 = (1i64 << 53) - 1;
type Result<T> = std::result::Result<T, BotError>;
pub fn text(v: &Value) -> Result<&str> {
    v.as_str().ok_or_else(|| error("expected string"))
}
pub fn array(v: &Value) -> Result<&Vec<Value>> {
    v.as_array().ok_or_else(|| error("expected array"))
}
pub fn number(v: &Value, minimum: i64) -> Result<i64> {
    let n = if let Some(s) = v.as_str() {
        s.parse::<i64>().map_err(io)?
    } else if let Some(n) = v.as_i64() {
        n
    } else if let Some(n) = v
        .as_f64()
        .filter(|n| n.is_finite() && n.fract() == 0.0 && n.abs() <= MAX_EXACT as f64)
    {
        n as i64
    } else {
        return Err(error("outside exact integer range"));
    };
    if n < minimum || n > MAX_EXACT {
        return Err(error("outside exact integer range"));
    }
    Ok(n)
}
pub fn decimal(v: &Value) -> Result<String> {
    if let Some(s) = v.as_str() {
        Ok(s.into())
    } else {
        Ok(number(v, -MAX_EXACT)?.to_string())
    }
}
fn default_int(v: &Value, fallback: i64) -> Result<i64> {
    if v.is_null() {
        Ok(fallback)
    } else {
        number(v, -MAX_EXACT)
    }
}
fn rational(v: &Value) -> Result<BigRational> {
    let value = text(v)?;
    if let Some((n, d)) = value.split_once('/') {
        Ok(BigRational::new(
            BigInt::from_str(n).map_err(io)?,
            BigInt::from_str(d).map_err(io)?,
        ))
    } else {
        Ok(BigRational::from_integer(
            BigInt::from_str(value).map_err(io)?,
        ))
    }
}
fn rat(n: i64) -> BigRational {
    BigRational::from_integer(n.into())
}
fn rat_text(v: &BigRational) -> String {
    if v.is_integer() {
        v.numer().to_string()
    } else {
        format!("{}/{}", v.numer(), v.denom())
    }
}
fn rat_json(v: &BigRational) -> Result<Value> {
    if v.is_integer() {
        serde_json::to_value(
            v.to_integer()
                .to_i64()
                .ok_or_else(|| error("Pine integer overflow"))?,
        )
        .map_err(io)
    } else {
        serde_json::to_value(v.to_f64().ok_or_else(|| error("Pine number overflow"))?).map_err(io)
    }
}

pub fn account<'a>(view: &'a Value, config: &Value) -> Result<(&'a str, &'a Value, i64, i64)> {
    let raw = view["own_account"]
        .as_object()
        .ok_or_else(|| error("funded account required"))?;
    if raw.len() != 1 {
        return Err(error("funded account required"));
    }
    let (kind, a) = raw.iter().next().unwrap();
    if !matches!(kind.as_str(), "Spot" | "Perp") {
        return Err(error("funded account required"));
    }
    let mut position = number(&a["position_qty"], -MAX_EXACT)?;
    let mut avg = default_int(&a["avg_entry_price_tick"], 0)?;
    let leg = text(&config["position_side"])?;
    if kind == "Perp" && a.get("hedge_positions").is_some_and(|p| !p.is_null()) {
        if !matches!(leg, "Long" | "Short") {
            return Err(error("hedge-mode account requires explicit position_side"));
        }
        let selected = &a["hedge_positions"][leg.to_lowercase()];
        position = number(&selected["qty"], 0)? * if leg == "Long" { 1 } else { -1 };
        avg = number(&selected["avg_entry_price_tick"], 0)?;
    } else if leg != "Both" {
        return Err(error(
            "one-way and spot accounts require Both position_side",
        ));
    }
    if avg < 0 {
        return Err(error("negative average price"));
    }
    Ok((kind, a, position, avg))
}
pub fn bars(data: &Value, now: i64) -> Result<Vec<Value>> {
    if data["truncated"] != false {
        return Err(error("Pine history was truncated; reconstruction refused"));
    }
    let mut bars = Vec::new();
    let mut last = None;
    for c in array(&data["candles"])? {
        if c["is_final"] != true || number(&c["close_time_ms"], 0)? > now {
            return Err(error("forming or future candle is not decision-visible"));
        }
        let time = number(&c["open_time_ms"], 0)?;
        if last.is_some_and(|n| time <= n) {
            return Err(error("candle history is not strictly ordered"));
        }
        last = Some(time);
        bars.push(json!({"time":time,"open":number(&c["open_tick"],1)?,"high":number(&c["high_tick"],1)?,
            "low":number(&c["low_tick"],1)?,"close":number(&c["close_tick"],1)?,"volume":number(&c["volume"],0)?}));
    }
    Ok(bars)
}

pub fn feedback(state: &mut Value, views: &[&Value], config: &Value) -> Result<()> {
    let mut orders = state.get("orders").cloned().unwrap_or(json!({}));
    let receipts = state
        .as_object_mut()
        .ok_or_else(|| error("invalid Pine state"))?
        .remove("_host_receipts")
        .unwrap_or(json!([]));
    for receipt in array(&receipts)? {
        let key = text(&receipt["key"])?;
        let Some(order) = orders.get_mut(key) else {
            continue;
        };
        let operation = receipt["action"]
            .as_object()
            .and_then(|o| o.keys().next())
            .ok_or_else(|| error("invalid execution receipt"))?;
        order["pending"] = json!(false);
        order["last_receipt"] = receipt.clone();
        if receipt["accepted"] == true {
            order["rejected"] = json!(false);
            if matches!(
                operation.as_str(),
                "PlaceProtected" | "PlaceImmediateOrCancel" | "PlaceReduceOnlyImmediateOrCancel"
            ) {
                order["order_id"] = receipt["order_id"].clone();
            } else if operation == "Cancel" {
                order["canceled"] = json!(true);
            }
        } else {
            order["rejected"] = json!(true);
            state["last_rejection"] = receipt.clone();
        }
    }
    let mut entries = state.get("entries").cloned().unwrap_or(json!({}));
    let mut ledgers = state.get("ledgers").cloned().unwrap_or(json!({}));
    for view in views {
        let instrument = text(&view["instrument_id"])?;
        let (kind, a, position, _) = account(view, config)?;
        let data = &view["bot_market_data"];
        if data.is_null() || data["truncated"] != false {
            return Err(error("complete own-fill history is required"));
        }
        let fills = array(&data["own_fills"])?;
        let details = array(&data["fill_details"])?;
        if fills.len() != details.len() {
            return Err(error("authoritative fill accounting receipts are required"));
        }
        let mut normalized = Vec::new();
        for (fill, detail) in fills.iter().zip(details) {
            if fill["trade_id"] != detail["trade_id"]
                || number(&detail["market_time_ms"], 0)? > number(&view["market_time_ms"], 0)?
            {
                return Err(error("invalid fill ID or future receipt"));
            }
            let maker = fill["maker_account_id"] == a["account_id"];
            let taker = fill["taker_account_id"] == a["account_id"];
            if maker == taker {
                return Err(error("invalid own fill or self trade"));
            }
            if a.get("hedge_positions").is_some_and(|v| !v.is_null())
                && fill
                    .get(if taker {
                        "taker_position_side"
                    } else {
                        "maker_position_side"
                    })
                    .unwrap_or(&json!("Both"))
                    != &config["position_side"]
            {
                continue;
            }
            let buy = fill["taker_side"] == if taker { "Buy" } else { "Sell" };
            normalized.push(json!({"id":number(&fill["trade_id"],0)?,"time":detail["market_time_ms"],
                "order_id":fill[if taker {"taker_order_id"} else {"maker_order_id"}],"qty":number(&fill["qty"],1)?,
                "price":number(&fill["price_tick"],1)?,"fee":number(&detail["fee_paid"],0)?,"buy":buy}));
        }
        if normalized
            .windows(2)
            .any(|w| w[1]["id"].as_i64() <= w[0]["id"].as_i64())
        {
            return Err(error("own-fill history is not strictly ordered"));
        }
        if ledgers.get(instrument).is_none() {
            let delta: i128 = normalized
                .iter()
                .map(|r| {
                    i128::from(r["qty"].as_i64().unwrap()) * if r["buy"] == true { 1 } else { -1 }
                })
                .sum();
            let opening = i64::try_from(i128::from(position) - delta)
                .map_err(|_| error("opening position overflow"))?;
            let initial = number(&config["initial_avg_price_tick"], 0)?;
            if kind == "Spot" && opening != 0 && initial == 0 {
                return Err(error(
                    "pre-existing spot inventory requires initial_avg_price_tick",
                ));
            }
            let cash = BigInt::from(number(&a["cash_balance"], -MAX_EXACT)?)
                + normalized
                    .iter()
                    .map(|r| {
                        BigInt::from(r["qty"].as_i64().unwrap())
                            * r["price"].as_i64().unwrap()
                            * if r["buy"] == true { 1 } else { -1 }
                            + r["fee"].as_i64().unwrap()
                    })
                    .sum::<BigInt>();
            ledgers[instrument] = json!({"fills":[],"position":opening,"cost":(BigInt::from(opening)*initial).to_string(),
                "opening_position":opening,"opening_cost":(BigInt::from(opening)*initial).to_string(),
                "opening_cash":cash.to_i64().ok_or_else(||error("opening cash overflow"))?});
        }
        let ledger = &mut ledgers[instrument];
        let previous = array(&ledger["fills"])?;
        if normalized.len() < previous.len() || normalized[..previous.len()] != *previous {
            return Err(error("committed own-fill history changed or disappeared"));
        }
        let previous_len = previous.len();
        for condition in data
            .get("conditional_orders")
            .map(array)
            .transpose()?
            .unwrap_or(&Vec::new())
        {
            for order in orders.as_object_mut().unwrap().values_mut() {
                if order["instrument"] == instrument && order["conditional_key"] == condition["key"]
                {
                    order["condition_status"] = condition["status"].clone();
                    if !condition["submitted_order_id"].is_null() {
                        order["order_id"] = condition["submitted_order_id"].clone();
                    }
                }
            }
        }
        let by_id: BTreeMap<String, String> = orders
            .as_object()
            .unwrap()
            .iter()
            .filter(|(_, o)| o["instrument"] == instrument && !o["order_id"].is_null())
            .map(|(k, o)| (o["order_id"].to_string(), k.clone()))
            .collect();
        for r in &normalized[previous_len..] {
            let qty = number(&r["qty"], 1)? * if r["buy"] == true { 1 } else { -1 };
            let old_position = number(&ledger["position"], -MAX_EXACT)?;
            let mut cost = rational(&ledger["cost"])?;
            if kind == "Spot" {
                if qty > 0 {
                    cost += rat(qty) * rat(number(&r["price"], 1)?);
                } else if old_position > 0 {
                    cost -= cost.clone() * rat((-qty).min(old_position)) / rat(old_position);
                } else {
                    cost = BigRational::zero();
                }
            }
            ledger["position"] = json!(old_position + qty);
            ledger["cost"] = json!(rat_text(&cost));
            let order_key = by_id.get(&r["order_id"].to_string());
            let order = order_key.and_then(|key| orders.get(key));
            let mut remaining = qty;
            if old_position.signum() * qty.signum() < 0 {
                let mut reduction = old_position.abs().min(qty.abs());
                let mut candidates: Vec<String> = entries
                    .as_object()
                    .unwrap()
                    .iter()
                    .filter(|(k, e)| {
                        k.starts_with(&format!("{instrument}::"))
                            && e["qty"]
                                .as_i64()
                                .is_some_and(|q| q.signum() * old_position.signum() > 0)
                    })
                    .map(|(k, _)| k.clone())
                    .collect();
                if let Some(entry) = order.and_then(|o| o["entry"].as_str()) {
                    candidates.sort_by_key(|k| k != &format!("{instrument}::{entry}"));
                }
                for key in candidates {
                    let entry = &mut entries[&key];
                    let current = number(&entry["qty"], -MAX_EXACT)?;
                    let reduced = current.abs().min(reduction);
                    entry["qty"] = json!(current - reduced * old_position.signum());
                    reduction -= reduced;
                }
                remaining = qty + old_position.abs().min(qty.abs()) * old_position.signum();
            }
            if let Some(key) = order_key {
                let order = &mut orders[key];
                order["filled_qty"] = json!(default_int(&order["filled_qty"], 0)? + qty.abs());
                if let Some(entry) = order["entry"].as_str() {
                    let key = format!("{instrument}::{entry}");
                    if entries.get(&key).is_none() {
                        entries[&key] = json!({"qty":0});
                    }
                    entries[&key]["qty"] =
                        json!(number(&entries[&key]["qty"], -MAX_EXACT)? + remaining);
                }
            }
        }
        ledger["fills"] = json!(normalized);
        ledger["last_account"] = view["own_account"].clone();
        ledger["last_account_time"] = view["market_time_ms"].clone();
        if kind == "Spot" && number(&ledger["position"], -MAX_EXACT)? != position {
            return Err(error(
                "spot inventory changed without complete fill history",
            ));
        }
    }
    state["orders"] = orders;
    state["entries"] = entries;
    state["ledgers"] = ledgers;
    Ok(())
}

pub fn frame(
    view: &Value,
    config: &Value,
    state: &Value,
    bar: &Value,
    cutoff: Option<i64>,
) -> Result<Value> {
    let mut raw = &view["own_account"];
    if let Some(cutoff) = cutoff {
        raw = state.get("last_account").unwrap_or(raw);
        if let Some(events) = view["bot_market_data"].get("account_events") {
            for event in array(events)? {
                if number(&event["market_time_ms"], 0)? < cutoff {
                    raw = &event["account"];
                }
            }
        }
    }
    let historical = json!({"own_account":raw});
    let (kind, a, mut position, avg) = account(&historical, config)?;
    let mut cash = rat(number(&a["cash_balance"], -MAX_EXACT)?);
    let mark = rat(number(&bar["close"], 1)?);
    let (average, equity, openprofit, netprofit) = if kind == "Spot" {
        let ledger = &state["ledgers"][text(&view["instrument_id"])?];
        let mut cost = rational(&ledger["cost"])?;
        if let Some(cutoff) = cutoff {
            position = number(&ledger["opening_position"], -MAX_EXACT)?;
            cost = rational(&ledger["opening_cost"])?;
            cash = rat(number(&ledger["opening_cash"], -MAX_EXACT)?);
            for r in array(&ledger["fills"])? {
                if number(&r["time"], 0)? >= cutoff {
                    break;
                }
                let qty = number(&r["qty"], 1)?;
                let price = number(&r["price"], 1)?;
                let buy = r["buy"] == true;
                if buy {
                    cost += rat(qty) * rat(price);
                    position += qty;
                } else if position > 0 {
                    cost -= cost.clone() * rat(qty.min(position)) / rat(position);
                    position -= qty;
                }
                cash += rat(qty) * rat(price) * rat(if buy { -1 } else { 1 })
                    - rat(number(&r["fee"], 0)?);
            }
        }
        let average = if position != 0 {
            json!(
                (cost.clone() / rat(position))
                    .to_f64()
                    .ok_or_else(|| error("average price overflow"))?
            )
        } else {
            Value::Null
        };
        let equity = cash + rat(position) * mark;
        let openprofit = rat(position) * rat(number(&bar["close"], 1)?) - cost;
        let netprofit = equity.clone()
            - rat(number(&state["initial_capital"], -MAX_EXACT)?)
            - openprofit.clone();
        (
            average,
            rat_json(&equity)?,
            openprofit.to_f64().ok_or_else(|| error("PnL overflow"))?,
            netprofit.to_f64().ok_or_else(|| error("PnL overflow"))?,
        )
    } else {
        (
            if position != 0 {
                json!(avg)
            } else {
                Value::Null
            },
            json!(number(&a["equity"], -MAX_EXACT)?),
            number(&a["unrealized_pnl"], -MAX_EXACT)? as f64,
            (number(&a["realized_pnl"], -MAX_EXACT)? + default_int(&a["funding_pnl"], 0)?
                - number(&a["fees_paid"], 0)?) as f64,
        )
    };
    number(&equity, -MAX_EXACT)?;
    number(&json!(position), -MAX_EXACT)?;
    Ok(
        json!({"time":bar["time"],"position_size":position,"position_avg_price":average,"initial_capital":state["initial_capital"],
        "equity":equity,"netprofit":netprofit,"openprofit":openprofit}),
    )
}

fn key(state: &Value, instrument: &str, name: &str) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(format!("{}::{instrument}::{name}", text(&state["identity"])?).as_bytes())
    )[..48]
        .into())
}
fn tracked(instrument: &str, key: &str, action: Value) -> Value {
    json!({"BotOrder":{"instrument_id":instrument,"key":key,"action":action}})
}
fn owned<'a>(
    state: &'a Value,
    view: &'a Value,
    name: Option<&str>,
) -> Result<Vec<(String, Value, Option<Value>)>> {
    let mut result = Vec::new();
    for (key, order) in state["orders"]
        .as_object()
        .ok_or_else(|| error("invalid order state"))?
    {
        if order["instrument"] == view["instrument_id"] && name.is_none_or(|n| order["name"] == n) {
            let live = array(&view["own_orders"])?
                .iter()
                .find(|o| !order["order_id"].is_null() && o["order_id"] == order["order_id"])
                .cloned();
            result.push((key.clone(), order.clone(), live));
        }
    }
    Ok(result)
}
fn cancel(state: &mut Value, view: &Value, name: Option<&str>) -> Result<Vec<Value>> {
    let mut actions = Vec::new();
    for (key, order, live) in owned(state, view, name)? {
        state["orders"][&key]
            .as_object_mut()
            .unwrap()
            .remove("replacement");
        if let Some(live) = live {
            actions.push(tracked(
                text(&view["instrument_id"])?,
                &key,
                json!({"Cancel":{"order_id":live["order_id"]}}),
            ));
        }
        if order["condition_status"] == "armed" {
            actions.push(tracked(
                text(&view["instrument_id"])?,
                &key,
                json!({"SetConditional":{"key":order["conditional_key"],"spec":null}}),
            ));
        }
    }
    Ok(actions)
}
#[derive(Default)]
struct Budget {
    reserved: BTreeMap<(String, String), i64>,
    cash: BTreeMap<String, i128>,
}

#[allow(clippy::too_many_arguments)]
fn submit(
    state: &mut Value,
    view: &Value,
    config: &Value,
    budget: &mut Budget,
    name: &str,
    side: &str,
    mut qty: i64,
    limit: Option<i64>,
    reduce: bool,
    entry: Option<&str>,
    mut replacing: bool,
) -> Result<Vec<Value>> {
    let instrument = text(&view["instrument_id"])?;
    let key = key(state, instrument, name)?;
    let (kind, a, position, _) = account(view, config)?;
    let tick = default_int(&view["instrument"]["tick_size"], 1)?;
    let lot = default_int(&view["instrument"]["lot_size"], 1)?;
    if tick < 1 || lot < 1 || qty % lot != 0 {
        return Err(error("order must align with the exchange grid"));
    }
    let requested = qty;
    let old = state["orders"].get(&key).cloned();
    let live = array(&view["own_orders"])?.iter().find(|o| {
        old.as_ref()
            .is_some_and(|old| !old["order_id"].is_null() && o["order_id"] == old["order_id"])
    });
    if old.as_ref().is_some_and(|o| o["pending"] == true) {
        return Err(error("host execution receipt missing for pending order"));
    }
    replacing |= old
        .as_ref()
        .is_some_and(|o| !o["replacement"].is_null() && live.is_none());
    if (live.is_some() || replacing) && old.as_ref().is_some_and(|o| o["side"] == side) {
        qty = (qty - default_int(&old.as_ref().unwrap()["filled_qty"], 0)?).max(0);
    }
    qty = qty.min(number(&config["max_qty"], 1)?);
    let pending = *budget
        .reserved
        .get(&(instrument.into(), side.into()))
        .unwrap_or(&0);
    let resting = array(&view["own_orders"])?
        .iter()
        .filter(|o| o["side"] == side && live.is_none_or(|live| o["order_id"] != live["order_id"]))
        .map(|o| number(&o["remaining_qty"], 0))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .try_fold(0i64, |total, qty| total.checked_add(qty))
        .ok_or_else(|| error("resting quantity overflow"))?;
    if kind == "Spot" {
        if side == "Sell" {
            qty = qty.min(
                (number(&a["available_position"], 0)?
                    + live
                        .filter(|o| o["side"] == "Sell")
                        .map(|o| number(&o["remaining_qty"], 0))
                        .transpose()?
                        .unwrap_or(0)
                    - pending)
                    .max(0),
            );
        }
        if side == "Buy" {
            qty = qty
                .min((number(&config["inventory_cap"], 1)? - position - resting - pending).max(0));
        }
    } else if !reduce {
        qty = qty.min(
            (number(&config["inventory_cap"], 1)?
                - if side == "Buy" { position } else { -position }
                - resting
                - pending)
                .max(0),
        );
    }
    if reduce {
        qty = qty.min((position.abs() - pending).max(0));
    }
    qty = qty / lot * lot;
    if qty <= 0 {
        return Ok(live
            .map(|o| {
                vec![tracked(
                    instrument,
                    &key,
                    json!({"Cancel":{"order_id":o["order_id"]}}),
                )]
            })
            .unwrap_or_default());
    }
    if limit.is_some_and(|n| n < 1 || n % tick != 0) {
        return Err(error("order price must align with exchange tick size"));
    }
    let (price, order_type) = if let Some(limit) = limit {
        (limit, "Limit")
    } else {
        let levels = array(&view["book"][if side == "Buy" { "asks" } else { "bids" }])?;
        let Some(top) = levels.first() else {
            return Ok(Vec::new());
        };
        let top = number(&top["price_tick"], 1)?;
        let slippage = number(&config["slippage_ticks"], 0)? * tick;
        (
            if side == "Buy" {
                top + slippage
            } else {
                tick.max(top - slippage)
            },
            "ImmediateOrCancel",
        )
    };
    let mut unit = 0i128;
    if kind == "Spot" && side == "Buy" {
        unit = price as i128
            + (price as i128 * number(&config["fee_buffer_ppm"], 0)? as i128 + 999999) / 1000000;
        let mut available = (number(&a["available_cash"], 0)? as i128
            - budget.cash.get(instrument).copied().unwrap_or(0))
        .max(0);
        if let Some(live) = live.filter(|o| o["side"] == "Buy") {
            available += number(&live["price_tick"], 1)? as i128
                * number(&live["remaining_qty"], 0)? as i128;
        }
        qty = (qty as i128).min(available / unit) as i64;
        qty = qty / lot * lot;
    }
    if qty <= 0 {
        return Ok(Vec::new());
    }
    let action = if let Some(live) = live {
        let oldprice = number(&live["price_tick"], 1)?;
        let oldqty = number(&live["remaining_qty"], 0)?;
        if live["side"] != side
            || qty > oldqty
            || (side == "Buy" && price > oldprice)
            || (side == "Sell" && price < oldprice)
        {
            state["orders"][&key]["replacement"] = json!({"name":name,"side":side,"qty":requested,"limit":limit,"stop":null,"reduce":reduce,"entry":entry,"post_only":false});
            state["orders"][&key]["pending"] = json!(true);
            return Ok(vec![tracked(
                instrument,
                &key,
                json!({"Cancel":{"order_id":live["order_id"]}}),
            )]);
        }
        if oldprice == price && oldqty == qty {
            return Ok(Vec::new());
        }
        json!({"Amend":{"order_id":live["order_id"],"price_tick":price,"qty":qty}})
    } else {
        budget
            .reserved
            .insert((instrument.into(), side.into()), pending + qty);
        if kind == "Spot" && side == "Buy" {
            *budget.cash.entry(instrument.into()).or_default() += unit * qty as i128;
        }
        json!({"PlaceProtected":{"side":side,"position_side":config["position_side"],"qty":qty,"order_type":order_type,"price_tick":price,"reduce_only":reduce}})
    };
    if state["orders"].get(&key).is_none() {
        state["orders"][&key] = json!({"instrument":instrument,"name":name,"filled_qty":0});
    }
    let order = &mut state["orders"][&key];
    order.as_object_mut().unwrap().remove("replacement");
    if live.is_none() && (!replacing || order["side"] != side) {
        order["filled_qty"] = json!(0);
    }
    order["pending"] = json!(true);
    order["entry"] = json!(entry);
    order["side"] = json!(side);
    order["requested_qty"] = json!(requested);
    Ok(vec![tracked(instrument, &key, action)])
}

pub fn translate(
    intents: &[Value],
    pyramiding: usize,
    views: &[&Value],
    config: &Value,
    state: &mut Value,
) -> Result<Vec<Value>> {
    let primary = text(&views[0]["instrument_id"])?;
    let mut actions = Vec::new();
    let mut budget = Budget::default();
    for intent in intents {
        let action = text(&intent["action"])?;
        let name = text(&intent["id"])?;
        let (instrument, name) = name.split_once("::").unwrap_or((primary, name));
        let view = views
            .iter()
            .find(|v| v["instrument_id"] == instrument)
            .ok_or_else(|| error("undeclared instrument"))?;
        if name.is_empty() || name.chars().count() > 120 {
            return Err(error("invalid Pine order ID"));
        }
        let (kind, _, position, _) = account(view, config)?;
        match action {
            "cancel" => actions.extend(cancel(state, view, Some(name))?),
            "entry" => {
                if owned(state, view, Some(name))?
                    .iter()
                    .any(|(_, o, _)| o["pending"] == true)
                {
                    continue;
                }
                let side = if intent["direction"] == "long" {
                    "Buy"
                } else {
                    "Sell"
                };
                let sign = if side == "Buy" { 1 } else { -1 };
                if kind == "Spot" && side == "Sell" {
                    return Err(error("spot account cannot open a short position"));
                }
                let mut qty = number(&intent["qty"], 1)?;
                if position.signum() * sign > 0 {
                    let active = state["entries"]
                        .as_object()
                        .unwrap()
                        .iter()
                        .filter(|(key, e)| {
                            key.starts_with(&format!("{instrument}::"))
                                && e["qty"].as_i64().is_some_and(|q| q.signum() * sign > 0)
                        })
                        .count();
                    let live = owned(state, view, Some(name))?
                        .iter()
                        .any(|(_, _, live)| live.is_some());
                    if active >= pyramiding.max(1) && !live {
                        continue;
                    }
                }
                if position.signum() * sign < 0 {
                    qty += position.abs();
                }
                if !intent["stop"].is_null() {
                    return Err(error("unqualified conditional intent"));
                }
                let limit = if intent["limit"].is_null() {
                    None
                } else {
                    Some(number(&intent["limit"], 1)?)
                };
                actions.extend(submit(
                    state,
                    view,
                    config,
                    &mut budget,
                    name,
                    side,
                    qty,
                    limit,
                    false,
                    Some(name),
                    false,
                )?);
            }
            "close" => {
                actions.extend(cancel(state, view, Some(name))?);
                let owned =
                    default_int(&state["entries"][format!("{instrument}::{name}")]["qty"], 0)?;
                let close = if position.signum() * owned.signum() > 0 {
                    position.abs().min(owned.abs()) * position.signum()
                } else {
                    0
                };
                if close == 0 {
                    continue;
                }
                let qty = if intent["qty"].is_null() {
                    close.abs()
                } else {
                    close.abs().min(number(&intent["qty"], 1)?)
                };
                actions.extend(submit(
                    state,
                    view,
                    config,
                    &mut budget,
                    &format!("{name}_close"),
                    if close > 0 { "Sell" } else { "Buy" },
                    qty,
                    None,
                    kind == "Perp",
                    Some(name),
                    false,
                )?);
            }
            _ => {
                return Err(error(
                    "intent is outside the qualified in-process Pine profile",
                ));
            }
        }
    }
    // Reservations belong to the complete decision, including replacements.
    store_budget(state, &budget);
    Ok(actions)
}
fn store_budget(state: &mut Value, budget: &Budget) {
    // Temporary keys are removed before the durable response is produced.
    state["_inprocess_budget"] = json!({"reserved":budget.reserved.iter().map(|((i,s),q)|json!([i,s,q])).collect::<Vec<_>>(),
        "cash":budget.cash.iter().map(|(i,q)|json!([i,q.to_string()])).collect::<Vec<_>>()});
}
pub fn replacements(state: &mut Value, views: &[&Value], config: &Value) -> Result<Vec<Value>> {
    let temporary = state.as_object_mut().unwrap().remove("_inprocess_budget");
    let mut budget = Budget::default();
    if let Some(temp) = temporary {
        for r in array(&temp["reserved"])? {
            budget.reserved.insert(
                (text(&r[0])?.into(), text(&r[1])?.into()),
                number(&r[2], 0)?,
            );
        }
        for r in array(&temp["cash"])? {
            budget
                .cash
                .insert(text(&r[0])?.into(), text(&r[1])?.parse().map_err(io)?);
        }
    }
    let orders = state["orders"].clone();
    let mut actions = Vec::new();
    for (key, order) in orders.as_object().unwrap() {
        let replacement = &order["replacement"];
        if replacement.is_null() || order["pending"] == true {
            continue;
        }
        let receipt = &order["last_receipt"];
        if receipt["accepted"] != true || receipt["action"].get("Cancel").is_none() {
            continue;
        }
        let view = views
            .iter()
            .find(|v| v["instrument_id"] == order["instrument"])
            .ok_or_else(|| error("missing replacement market"))?;
        if array(&view["own_orders"])?
            .iter()
            .any(|o| o["order_id"] == order["order_id"])
        {
            continue;
        }
        state["orders"][key]
            .as_object_mut()
            .unwrap()
            .remove("replacement");
        if !replacement["stop"].is_null() || replacement["post_only"] == true {
            return Err(error("unqualified replacement"));
        }
        actions.extend(submit(
            state,
            view,
            config,
            &mut budget,
            text(&replacement["name"])?,
            text(&replacement["side"])?,
            number(&replacement["qty"], 0)?,
            if replacement["limit"].is_null() {
                None
            } else {
                Some(number(&replacement["limit"], 1)?)
            },
            replacement["reduce"] == true,
            replacement["entry"].as_str(),
            true,
        )?);
    }
    Ok(actions)
}
