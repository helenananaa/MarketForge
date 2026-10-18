"""Pine external broker: actual accounts, keyed orders and routed execution.

All prices and quantities are exchange ticks/lots. Only the host executes orders.
Frozen bar/account/provider frames ensure that recovery never resends old intents.
"""
from __future__ import annotations

import copy
from fractions import Fraction
import hashlib
import json
import math
import time
import re

MAX_EXACT = 2**53 - 1
ORDER_PREFIX = "__marketforge_order__"


def number(value, name, minimum=-MAX_EXACT):
    if isinstance(value, bool):
        raise ValueError(f"{name}: boolean is not an integer")
    # Rust serializes large account integers as decimal strings.
    if isinstance(value, str):
        if not value.lstrip("-").isdigit():
            raise ValueError(f"{name}: invalid integer")
        value = int(value)
    if not isinstance(value, (int, float)) or not math.isfinite(value) or int(value) != value or not minimum <= value <= MAX_EXACT:
        raise ValueError(f"{name}: outside exact integer range")
    return int(value)


def quantized(value, name, quantum, up=False):
    """Explicit strategy-intent normalization; authoritative receipts stay exact."""
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or not 0 < value <= MAX_EXACT:
        raise ValueError(f"{name}: expected a positive finite strategy value")
    scaled = Fraction(str(value)) / quantum
    units = -(-scaled.numerator // scaled.denominator) if up else scaled.numerator // scaled.denominator
    return units * quantum


def strategy_price(value, name, tick, side, stop, config):
    if value is None: return None
    if not config.get("quantize_strategy_orders", False): return number(value, name, 1)
    # Limit buys floor / sells ceil; trigger buys ceil / sells floor. This
    # preserves comparisons against prices on the exchange's discrete grid.
    result = quantized(value, name, tick, up=(side == "Buy") if stop else (side == "Sell"))
    if result <= 0: raise ValueError(f"{name}: below the exchange price grid")
    return result


def timeframe(interval):
    if interval == 86400000:
        return "1D"
    if interval % 60000 == 0:
        return str(interval // 60000)
    if interval % 1000 == 0:
        return f"{interval // 1000}S"
    raise ValueError("Pine timeframes require integral seconds")


def bars_of(data, now):
    if data["truncated"]:
        raise ValueError("Pine history was truncated; reconstruction refused")
    bars = []
    for candle in data["candles"]:
        if not candle["is_final"] or candle["close_time_ms"] > now:
            raise ValueError("forming or future candle is not decision-visible")
        if bars and candle["open_time_ms"] <= bars[-1]["time"]:
            raise ValueError("candle history is not strictly ordered")
        bar = {"time": number(candle["open_time_ms"], "bar time", 0),
               "volume": number(candle["volume"], "volume", 0)}
        for target, origin in (("open", "open_tick"), ("high", "high_tick"), ("low", "low_tick"), ("close", "close_tick")):
            bar[target] = number(candle[origin], origin, 1)
        bars.append(bar)
    return bars


def lower_order_calls(source):
    """Lower strategy.order into entry intents with an explicit host discriminator.

    A lexical walk preserves strings/comments and nested/dynamic ID expressions.
    The broker retains order semantics (no entry reversal or pyramiding filter).
    """
    result, i = [], 0
    while i < len(source):
        if source.startswith("//", i):
            end = source.find("\n", i)
            end = len(source) if end < 0 else end
            result.append(source[i:end]); i = end
        elif source[i] in "\"'":
            quote, start = source[i], i
            i += 1
            while i < len(source):
                if source[i] == "\\": i += 2
                elif source[i] == quote: i += 1; break
                else: i += 1
            result.append(source[start:i])
        elif source.startswith("strategy.order", i) and (i == 0 or not (source[i-1].isalnum() or source[i-1] == "_")):
            cursor = i + len("strategy.order")
            while cursor < len(source) and source[cursor].isspace(): cursor += 1
            if cursor >= len(source) or source[cursor] != "(":
                result.append(source[i]); i += 1; continue
            start, depth, quote = cursor + 1, 0, None
            cursor = start
            while cursor < len(source):
                char = source[cursor]
                if quote:
                    if char == "\\": cursor += 2; continue
                    if char == quote: quote = None
                elif char in "\"'": quote = char
                elif char in "([": depth += 1
                elif char in ")]":
                    if depth == 0: break
                    depth -= 1
                elif char == "," and depth == 0: break
                cursor += 1
            expr = source[start:cursor].strip()
            expr = re.sub(r"^id\s*=\s*", "", expr)
            if not expr or cursor >= len(source) or source[cursor] != ",":
                raise ValueError("strategy.order requires ID followed by direction")
            result.append(f'strategy.entry("{ORDER_PREFIX}" + ({expr})')
            i = cursor
        else:
            result.append(source[i]); i += 1
    return "".join(result)


def account_of(view, config):
    raw = view.get("own_account") or {}
    if len(raw) != 1 or next(iter(raw)) not in ("Spot", "Perp"):
        raise ValueError("a funded spot or perpetual account is required")
    kind, account = next(iter(raw.items()))
    position = number(account["position_qty"], "position")
    avg = number(account.get("avg_entry_price_tick", 0), "average price", 0)
    leg = config.get("position_side", "Both")
    if kind == "Perp" and account.get("hedge_positions"):
        if leg not in ("Long", "Short"):
            raise ValueError("hedge-mode account requires explicit Long or Short position_side")
        selected = account["hedge_positions"][leg.lower()]
        position = number(selected["qty"], "leg position", 0) * (1 if leg == "Long" else -1)
        avg = number(selected["avg_entry_price_tick"], "leg average price", 0)
    elif leg != "Both":
        raise ValueError("one-way and spot accounts require Both position_side")
    return kind, account, position, avg


def external_pass_source(source):
    # The external runtime gates pass-local providers behind this setting. Each
    # host pass is still a single bar-close evaluation, with no simulated fills.
    match = re.search(r"(?m)^\s*strategy\s*\(", source)
    if not match: raise ValueError("Pine broker requires a strategy declaration")
    start = match.end()
    depth, quote, cursor = 1, None, start
    while cursor < len(source) and depth:
        char = source[cursor]
        if quote:
            if char == "\\": cursor += 2; continue
            if char == quote: quote = None
        elif char in "\"'": quote = char
        elif char == "(": depth += 1
        elif char == ")": depth -= 1
        cursor += 1
    declaration = source[start:cursor-1]
    if "calc_on_order_fills" in declaration:
        if not re.search(r"\bcalc_on_order_fills\s*=\s*false\b", declaration):
            raise ValueError("fill-triggered Pine recalculation is not a bar-close strategy")
        declaration = re.sub(r"\bcalc_on_order_fills\s*=\s*false\b", "calc_on_order_fills=true", declaration)
    else:
        # Keep the positional title first, then append the host setting.
        declaration += ", calc_on_order_fills=true"
    return source[:start] + declaration + source[cursor-1:]


def entry_name(instrument, name):
    return instrument + "::" + name


def order_key(state, instrument, name):
    # Native conditional keys are scoped by account, not process. Two instances
    # sharing an account must still own distinct Pine IDs.
    return hashlib.sha256((state["identity"] + "::" + entry_name(instrument, name)).encode()).hexdigest()[:48]


def split_id(name, primary, views):
    direct_order = name.startswith(ORDER_PREFIX)
    if direct_order: name = name[len(ORDER_PREFIX):]
    if "::" in name:
        instrument, name = name.split("::", 1)
    else:
        instrument = primary
    if instrument not in views or not name or len(name) > 120:
        raise ValueError("Pine order ID targets an undeclared instrument or is invalid")
    return instrument, name, direct_order


def tracked(instrument, key, action):
    return {"BotOrder": {"instrument_id": instrument, "key": key, "action": action}}


def sync_feedback(state, views, config):
    orders = state.setdefault("orders", {})
    for receipt in state.pop("_host_receipts", []):
        order = orders.get(receipt["key"])
        if order is None:
            continue
        operation = next(iter(receipt["action"]))
        order["pending"] = False
        order["last_receipt"] = receipt
        if receipt["accepted"]:
            order["rejected"] = False
            if operation in ("PlaceProtected", "PlaceImmediateOrCancel", "PlaceReduceOnlyImmediateOrCancel"):
                order["order_id"] = receipt["order_id"]
            elif operation == "Cancel": order["canceled"] = True
        else:
            order["rejected"] = True
            state["last_rejection"] = receipt
    entries = state.setdefault("entries", {})
    ledgers = state.setdefault("ledgers", {})
    for instrument, view in views.items():
        kind, account, position, _ = account_of(view, config)
        data = view.get("bot_market_data")
        if data is None or data["truncated"]:
            raise ValueError("complete own-fill history is required for every declared market")
        fills, details = data["own_fills"], data["fill_details"]
        if len(fills) != len(details): raise ValueError("authoritative fill accounting receipts are required")
        account_id = account["account_id"]
        normalized = []
        for fill, detail in zip(fills, details):
            if fill["trade_id"] != detail["trade_id"] or detail["market_time_ms"] > view["market_time_ms"]:
                raise ValueError("invalid fill ID or future fill receipt")
            maker, taker = fill["maker_account_id"] == account_id, fill["taker_account_id"] == account_id
            if maker == taker: raise ValueError("invalid own fill or self trade")
            if account.get("hedge_positions") and fill.get("taker_position_side" if taker else "maker_position_side", "Both") != config["position_side"]:
                continue
            buy = (fill["taker_side"] == "Buy") if taker else (fill["taker_side"] == "Sell")
            normalized.append({"id": number(fill["trade_id"], "trade ID", 0), "time": detail["market_time_ms"],
                "order_id": fill["taker_order_id"] if taker else fill["maker_order_id"],
                "qty": number(fill["qty"], "fill quantity", 1), "price": number(fill["price_tick"], "fill price", 1),
                "fee": number(detail["fee_paid"], "fill fee", 0), "buy": buy})
        if any(b["id"] <= a["id"] for a, b in zip(normalized, normalized[1:])):
            raise ValueError("own-fill history is not strictly ordered")
        ledger = ledgers.get(instrument)
        if ledger is None:
            opening = position - sum(r["qty"] * (1 if r["buy"] else -1) for r in normalized)
            initial_avg = config.get("initial_avg_price_tick", 0)
            if kind == "Spot" and opening and not initial_avg:
                raise ValueError("pre-existing spot inventory requires initial_avg_price_tick")
            ledger = ledgers[instrument] = {"fills": [], "position": opening, "cost": str(opening * initial_avg),
                "opening_position": opening, "opening_cost": str(opening * initial_avg),
                "opening_cash": number(account["cash_balance"], "cash") + sum((r["qty"] * r["price"] * (1 if r["buy"] else -1) + r["fee"]) for r in normalized)}
        previous = ledger["fills"]
        if normalized[:len(previous)] != previous:
            raise ValueError("committed own-fill history changed or disappeared")
        # Conditional entries get their real submitted ID from the host's state.
        for condition in data.get("conditional_orders", []):
            for order in orders.values():
                if order["instrument"] == instrument and order.get("conditional_key") == condition["key"]:
                    order["condition_status"] = condition["status"]
                    if condition.get("submitted_order_id") is not None: order["order_id"] = condition["submitted_order_id"]
        by_id = {o["order_id"]: o for o in orders.values() if o["instrument"] == instrument and o.get("order_id") is not None}
        for receipt in normalized[len(previous):]:
            qty = receipt["qty"] * (1 if receipt["buy"] else -1)
            old_position, cost = ledger["position"], Fraction(ledger["cost"])
            if kind == "Spot":
                if qty > 0: cost += qty * receipt["price"]
                elif old_position > 0: cost -= cost * min(-qty, old_position) / old_position
                else: cost = Fraction(0)
            ledger.update(position=old_position + qty, cost=str(cost))
            order = by_id.get(receipt["order_id"])
            remaining = qty
            if old_position * qty < 0:
                reduction = min(abs(old_position), abs(qty))
                candidates = [(key, e) for key, e in entries.items() if key.startswith(instrument + "::") and e["qty"] * old_position > 0]
                if order and order.get("entry"):
                    candidates.sort(key=lambda item: item[0] != entry_name(instrument, order["entry"]))
                for _, entry in candidates:
                    reduced = min(abs(entry["qty"]), reduction)
                    entry["qty"] -= reduced * (1 if old_position > 0 else -1)
                    reduction -= reduced
                remaining = qty + min(abs(old_position), abs(qty)) * (1 if old_position > 0 else -1)
            if order:
                order["filled_qty"] = order.get("filled_qty", 0) + abs(qty)
                if order.get("entry"):
                    entry = entries.setdefault(entry_name(instrument, order["entry"]), {"qty": 0})
                    entry["qty"] += remaining
        ledger["fills"] = normalized
        # Deposits/transfers/manual activity are allowed. Cash and perpetual
        # averages/PnL always come from the current authoritative snapshot.
        ledger["last_account"] = copy.deepcopy(view["own_account"])
        ledger["last_account_time"] = view["market_time_ms"]
        if kind == "Spot" and ledger["position"] != position:
            raise ValueError("spot inventory changed without complete fill history")


def frame(view, config, state, bar, cutoff=None):
    raw = view["own_account"]
    if cutoff is not None:
        raw = state.get("last_account", raw)
        for event in view["bot_market_data"].get("account_events", []):
            if event["market_time_ms"] < cutoff: raw = event["account"]
    historical = dict(view, own_account=raw)
    kind, account, position, avg = account_of(historical, config)
    cash = number(account["cash_balance"], "cash")
    mark = bar["close"]
    if kind == "Spot":
        ledger = state["ledgers"][view["instrument_id"]]
        if cutoff is None:
            cost = Fraction(ledger["cost"])
        else:
            position, cost = ledger["opening_position"], Fraction(ledger["opening_cost"])
            cash = ledger["opening_cash"]
            for r in ledger["fills"]:
                if r["time"] >= cutoff: break
                if r["buy"]: cost += r["qty"] * r["price"]; position += r["qty"]
                elif position > 0: cost -= cost * min(r["qty"], position) / position; position -= r["qty"]
                cash += r["qty"] * r["price"] * (-1 if r["buy"] else 1) - r["fee"]
        avg = float(cost / position) if position else None
        equity, openprofit = cash + position * mark, position * mark - cost
        netprofit = equity - state["initial_capital"] - openprofit
    else:
        equity = number(account["equity"], "equity")
        openprofit = number(account["unrealized_pnl"], "unrealized PnL")
        netprofit = number(account["realized_pnl"], "realized PnL") + number(account.get("funding_pnl", 0), "funding") - number(account["fees_paid"], "fees", 0)
    for value in (position, equity): number(value, "Pine feedback")
    return {"time": bar["time"], "position_size": position, "position_avg_price": avg if position else None,
        "initial_capital": state["initial_capital"], "equity": equity, "netprofit": float(netprofit), "openprofit": float(openprofit)}


def owned_orders(state, view, names=None):
    live = {order["order_id"]: order for order in view["own_orders"]}
    return [(key, order, live.get(order.get("order_id"))) for key, order in state["orders"].items()
            if order["instrument"] == view["instrument_id"] and (names is None or order["name"] in names)]


def cancel_orders(state, view, names=None):
    actions = []
    for key, order, live in owned_orders(state, view, names):
        order.pop("replacement", None)
        if live:
            actions.append(tracked(view["instrument_id"], key, {"Cancel": {"order_id": live["order_id"]}}))
        if order.get("condition_status") == "armed":
            actions.append(tracked(view["instrument_id"], key, {"SetConditional": {"key": order["conditional_key"], "spec": None}}))
    for key, spec in list(state.get("exit_specs", {}).items()):
        if spec["instrument"] == view["instrument_id"] and (names is None or spec["name"] in names):
            installed = spec.get("installed")
            if installed and any(p["position_side"] == spec["position_side"] and all(p["spec"].get(k) == v for k, v in installed.items()) for p in view.get("position_protections", [])):
                actions.append(tracked(view["instrument_id"], key, {"SetPositionProtection": {"position_side": spec["position_side"], "protection": None}}))
            del state["exit_specs"][key]
    return actions


def qty_for_close(intent, position, quantize=False):
    qty = abs(position)
    if intent.get("qty") is not None: qty = min(qty, quantized(intent["qty"], "close quantity", 1) if quantize else number(intent["qty"], "close quantity", 1))
    elif intent.get("qty_percent") is not None:
        percent = intent["qty_percent"]
        if isinstance(percent, bool) or not isinstance(percent, (int, float)) or not 0 < percent <= 100: raise ValueError("invalid close percentage")
        qty = int(Fraction(str(percent)) * qty / 100)
    return qty


def submit_order(state, view, config, name, side, qty, limit=None, stop=None, reduce=False, entry=None, post_only=False, replacing=False):
    instrument = view["instrument_id"]
    key = order_key(state, instrument, name)
    kind, account, position, _ = account_of(view, config)
    metadata = view.get("instrument") or {}
    tick = number(metadata.get("tick_size", 1), "tick size", 1)
    lot = number(metadata.get("lot_size", 1), "lot size", 1)
    if config.get("quantize_strategy_orders", False):
        qty = qty // lot * lot
        if qty <= 0: return []
    if qty % lot: raise ValueError("order quantity must align with the exchange lot size")
    requested_qty = qty
    old = state["orders"].get(key)
    live = next((o for o in view["own_orders"] if old and o["order_id"] == old.get("order_id")), None)
    if old and old.get("pending"): raise ValueError("host execution receipt missing for pending order")
    replacing = replacing or bool(old and old.get("replacement") and not live)
    if (live or replacing) and old.get("side") == side:
        qty = max(0, qty - old.get("filled_qty", 0))
    qty = min(qty, config["max_qty"])
    reserved = config.setdefault("_reserved", {})
    cash_reserved = config.setdefault("_cash_reserved", {})
    resting_qty = sum(o["remaining_qty"] for o in view["own_orders"] if o["side"] == side and (not live or o["order_id"] != live["order_id"]))
    pending_qty = reserved.get((instrument, side), 0)
    if kind == "Spot":
        if side == "Sell": qty = min(qty, max(0, number(account["available_position"], "available inventory", 0) + (live["remaining_qty"] if live and live["side"] == "Sell" else 0) - pending_qty))
        if side == "Buy": qty = min(qty, max(0, config["inventory_cap"] - position - resting_qty - pending_qty))
    elif not reduce:
        signed = 1 if side == "Buy" else -1
        qty = min(qty, max(0, config["inventory_cap"] - signed * position - resting_qty - pending_qty))
    if reduce: qty = min(qty, max(0, abs(position) - pending_qty))
    qty = qty // lot * lot
    if qty <= 0:
        return [tracked(instrument, key, {"Cancel": {"order_id": live["order_id"]}})] if live else []
    limit = strategy_price(limit, "limit price", tick, side, False, config)
    stop = strategy_price(stop, "stop price", tick, side, True, config)
    if any(price is not None and price % tick for price in (limit, stop)):
        raise ValueError("order price must align with the exchange tick size")
    if stop is not None:
        if kind != "Perp": raise ValueError("exchange conditional entries require a perpetual market")
        if live:
            old["replacement"] = {"name": name, "side": side, "qty": requested_qty, "limit": limit, "stop": stop, "reduce": reduce, "entry": entry, "post_only": post_only}
            old["pending"] = True
            return [tracked(instrument, key, {"Cancel": {"order_id": live["order_id"]}})]
        spec = {"side": side, "position_side": config["position_side"], "qty": qty,
                "trigger_price_tick": stop, "above": side == "Buy", "trigger": "Last", "limit_price_tick": limit}
        if old and old.get("condition_status") == "armed" and old.get("conditional_spec") == spec: return []
        action = {"SetConditional": {"key": key, "spec": spec}}
    else:
        if old and old.get("condition_status") == "armed":
            old["replacement"] = {"name": name, "side": side, "qty": requested_qty, "limit": limit, "stop": stop, "reduce": reduce, "entry": entry, "post_only": post_only}
            old["pending"] = True
            return [tracked(instrument, key, {"SetConditional": {"key": old["conditional_key"], "spec": None}})]
        levels = view["book"]["asks" if side == "Buy" else "bids"]
        if limit is None:
            if not levels: return []
            top = number(levels[0]["price_tick"], "book price", 1)
            price = top + config["slippage_ticks"] * tick if side == "Buy" else max(tick, top - config["slippage_ticks"] * tick)
            order_type = "ImmediateOrCancel"
        else:
            price, order_type = limit, "PostOnly" if post_only else "Limit"
        if kind == "Spot" and side == "Buy":
            unit = price + (price * config["fee_buffer_ppm"] + 999999) // 1000000
            available = max(0, number(account["available_cash"], "available cash", 0) - cash_reserved.get(instrument, 0))
            if live and live["side"] == "Buy": available += live["price_tick"] * live["remaining_qty"]
            qty = min(qty, available // unit)
            qty = qty // lot * lot
        if qty <= 0: return []
        if live:
            if live["side"] != side:
                # Cancellation is acknowledged before replacing with the other
                # side; never leave opposing live orders for the same Pine ID.
                old["replacement"] = {"name": name, "side": side, "qty": requested_qty, "limit": limit,
                    "stop": stop, "reduce": reduce, "entry": entry, "post_only": post_only}
                old["pending"] = True
                return [tracked(instrument, key, {"Cancel": {"order_id": live["order_id"]}})]
            remaining = qty
            if not remaining: return [tracked(instrument, key, {"Cancel": {"order_id": live["order_id"]}})]
            if live["price_tick"] == price and live["remaining_qty"] == remaining: return []
            if remaining > live["remaining_qty"] or (side == "Buy" and price > live["price_tick"]) or (side == "Sell" and price < live["price_tick"]):
                # Native amend intentionally forbids increased aggression.
                # Cancel first; replacement waits for the actual receipt.
                old["replacement"] = {"name": name, "side": side, "qty": requested_qty, "limit": limit,
                    "stop": stop, "reduce": reduce, "entry": entry, "post_only": post_only}
                old["pending"] = True
                return [tracked(instrument, key, {"Cancel": {"order_id": live["order_id"]}})]
            action = {"Amend": {"order_id": live["order_id"], "price_tick": price, "qty": remaining}}
        else:
            action = {"PlaceProtected": {"side": side, "position_side": config["position_side"], "qty": qty,
                "order_type": order_type, "price_tick": price, "reduce_only": reduce}}
            reserved[(instrument, side)] = pending_qty + qty
            if kind == "Spot" and side == "Buy": cash_reserved[instrument] = cash_reserved.get(instrument, 0) + unit * qty
    order = state["orders"].setdefault(key, {"instrument": instrument, "name": name, "filled_qty": 0})
    order.pop("replacement", None)
    if not live and stop is None and (not replacing or order.get("side") != side): order["filled_qty"] = 0
    order.update(pending=True, entry=entry, side=side, requested_qty=requested_qty)
    if stop is not None: order.update(conditional_key=key, conditional_spec=spec, condition_status="armed")
    return [tracked(instrument, key, action)]


def replacements(state, views, config):
    actions = []
    for key, order in list(state["orders"].items()):
        replacement = order.get("replacement")
        if not replacement or order.get("pending"): continue
        receipt = order.get("last_receipt") or {}
        acknowledged = "Cancel" in receipt.get("action", {}) or receipt.get("action", {}).get("SetConditional", {}).get("spec", "not-cancel") is None
        if not receipt.get("accepted") or not acknowledged:
            continue
        view = views[order["instrument"]]
        if any(o["order_id"] == order.get("order_id") for o in view["own_orders"]): continue
        order.pop("replacement")
        actions += submit_order(state, view, config, **replacement, replacing=True)
    return actions


def translate(intents, pyramiding, views, primary, config, state):
    actions = []
    for intent in intents:
        action, raw_id = intent["action"], intent["id"]
        instrument, name, direct = split_id(raw_id or "_all", primary, views)
        view = views[instrument]
        kind, account, position, avg = account_of(view, config)
        if action in ("cancel", "cancel_all"):
            actions.extend(cancel_orders(state, view, None if action == "cancel_all" else {name}))
            continue
        if action == "entry":
            if any(order["name"] == name and order.get("pending") for _, order, _ in owned_orders(state, view)):
                continue
            side = "Buy" if intent["direction"] == "long" else "Sell"
            if kind == "Spot" and side == "Sell" and not direct:
                raise ValueError("spot account cannot open a short position")
            qty = quantized(intent["qty"], "entry quantity", (view.get("instrument") or {}).get("lot_size", 1)) if config.get("quantize_strategy_orders", False) else number(intent["qty"], "entry quantity", 1)
            if qty <= 0: continue
            signed = 1 if side == "Buy" else -1
            if not direct and position * signed > 0:
                active = [e for key, e in state["entries"].items() if key.startswith(instrument + "::") and e["qty"] * signed > 0]
                live_entry = any(order["name"] == name and live for _, order, live in owned_orders(state, view))
                if len(active) >= max(1, pyramiding) and not live_entry: continue
            if not direct and position * signed < 0:
                # Pine reversal trades the closing quantity plus the new entry.
                qty += abs(position)
            actions.extend(submit_order(state, view, config, name, side, qty, intent.get("limit"), intent.get("stop"), entry=name))
        elif action in ("close", "close_all"):
            # Pine closes an entry once per bar: a repeated close in the same
            # bar is a no-op, as a repeated pending entry is above.
            if any(order["name"] == name + "_close" and order.get("pending") for _, order, _ in owned_orders(state, view)):
                continue
            actions.extend(cancel_orders(state, view, None if action == "close_all" else {name}))
            owned = state["entries"].get(entry_name(instrument, name), {}).get("qty", 0)
            close_position = position if action == "close_all" else (min(abs(position), abs(owned)) * (1 if position > 0 else -1) if position * owned > 0 else 0)
            if not close_position: continue
            qty = qty_for_close(intent, close_position, config.get("quantize_strategy_orders", False))
            actions.extend(submit_order(state, view, config, name + "_close", "Sell" if close_position > 0 else "Buy", qty, reduce=kind == "Perp", entry=name if action == "close" else None))
        elif action == "exit":
            if kind != "Perp": raise ValueError("exchange position protection requires a perpetual market")
            from_entry = intent.get("from_entry")
            if from_entry:
                entry_instrument, entry_id, _ = split_id(from_entry, instrument, views)
                if entry_instrument != instrument: raise ValueError("exit and entry must use the same market")
            else: entry_id = None
            key = "exit_" + order_key(state, instrument, name)
            exits = state.setdefault("exit_specs", {})
            old = exits.get(key, {})
            exits[key] = {"instrument": instrument, "name": name, "entry_id": entry_id,
                "position_side": config["position_side"], "intent": intent,
                "installed": old.get("installed"), "trail_active": old.get("trail_active", False)}
        else:
            raise ValueError(f"unsupported Pine broker intent: {action}")
    return actions


def evaluate_exits(state, views, config):
    actions = []
    for key, spec in state.get("exit_specs", {}).items():
        view = views[spec["instrument"]]
        _, _, position, avg = account_of(view, config)
        if not position:
            spec.update(installed=None, trail_active=False)
            continue
        if any(p["position_side"] == spec["position_side"] and p["status"] == "triggered" for p in view.get("position_protections", [])):
            continue  # Exchange exit remains latched until its real fills finish.
        intent, signed = spec["intent"], 1 if position > 0 else -1
        tick = number((view.get("instrument") or {}).get("tick_size", 1), "tick size", 1)
        owned = state["entries"].get(entry_name(spec["instrument"], spec["entry_id"]), {}).get("qty", 0) if spec["entry_id"] else position
        if owned * position <= 0: continue
        qty = qty_for_close(intent, min(abs(position), abs(owned)), config.get("quantize_strategy_orders", False))
        if not qty: continue
        take, stop = intent.get("limit"), intent.get("stop")
        if take is None and intent.get("profit") is not None: take = avg + signed * intent["profit"] * tick
        if stop is None and intent.get("loss") is not None: stop = avg - signed * intent["loss"] * tick
        last = view.get("public_trades", [])
        last_price = last[-1]["price_tick"] if last else None
        if last_price is None: continue
        close_side = "Sell" if position > 0 else "Buy"
        take = strategy_price(take, "take profit", tick, close_side, False, config)
        stop = strategy_price(stop, "stop loss", tick, close_side, True, config)
        if any(price is not None and price % tick for price in (take, stop)):
            raise ValueError("exit price must align with the exchange tick size")
        if ((take is not None and signed * (last_price - take) >= 0) or
                (stop is not None and signed * (last_price - stop) <= 0)):
            actions += submit_order(state, view, config, spec["name"] + "_triggered", "Sell" if position > 0 else "Buy", qty, reduce=True, entry=spec["entry_id"])
            continue
        activation = intent.get("trail_price")
        if activation is None and intent.get("trail_points") is not None: activation = avg + signed * intent["trail_points"] * tick
        activation = strategy_price(activation, "trailing activation", tick, "Buy" if signed > 0 else "Sell", True, config)
        if activation is not None and signed * (last_price - activation) >= 0:
            spec["trail_active"] = True
        trail = (quantized(intent["trail_offset"] * tick, "trailing distance", tick, up=True) if config.get("quantize_strategy_orders", False) else number(intent["trail_offset"] * tick, "trailing distance", 1)) if spec["trail_active"] else None
        if take is None and stop is None and trail is None: continue
        protection = {"take_profit_tick": take, "stop_loss_tick": stop, "trigger": "Last", "exit_qty": qty,
            "trailing_distance_tick": trail}
        order = state["orders"].setdefault(key, {"instrument": spec["instrument"], "name": spec["name"], "entry": None})
        if not order.get("pending") and (spec.get("installed") != protection or order.get("rejected")):
            order["pending"] = True
            spec["installed"] = protection
            actions.append(tracked(spec["instrument"], key, {"SetPositionProtection": {"position_side": spec["position_side"], "protection": protection}}))
    return actions


def hedge(state, views, primary, config):
    instrument = config.get("hedge_instrument_id")
    if not instrument: return [], False
    if instrument not in views or instrument == primary:
        raise ValueError("hedge_instrument_id must be a declared peer market")
    primary_view, peer = views[primary], views[instrument]
    kind, primary_account, position, _ = account_of(primary_view, config)
    peer_kind, peer_account, peer_position, _ = account_of(peer, config)
    if {kind, peer_kind} != {"Spot", "Perp"} or config["position_side"] != "Both":
        raise ValueError("automatic hedge requires a spot/perpetual one-way pair")
    primary_meta, peer_meta = primary_view.get("instrument"), peer.get("instrument")
    if primary_meta and peer_meta and (primary_meta["base_asset"] != peer_meta["base_asset"] or primary_meta["quote_asset"] != peer_meta["quote_asset"] or primary_meta["lot_size"] != peer_meta["lot_size"]):
        raise ValueError("automatic hedge requires matching base/quote assets and quantity units")
    residual = position + peer_position
    now = primary_view["market_time_ms"]
    coordinator = state.setdefault("hedge", {})
    coordinator.update(residual_qty=residual, primary_qty=position, peer_qty=peer_position)
    unhealthy = any(k == "Perp" and str(a.get("margin_status", "healthy")).lower().replace("_", "") not in ("healthy", "flat") for k, a in ((kind, primary_account), (peer_kind, peer_account)))
    if unhealthy: coordinator["unwinding"] = True
    if not position and not peer_position:
        coordinator.pop("unhedged_since_ms", None)
        coordinator["unwinding"] = False
        return [], False
    if not residual and not coordinator.get("unwinding"):
        coordinator.pop("unhedged_since_ms", None)
        coordinator["unwinding"] = False
        return [], False
    since = coordinator.setdefault("unhedged_since_ms", now)
    if now - since >= config["hedge_timeout_ms"]:
        coordinator["unwinding"] = True
    # Wait for cancellation acknowledgments before adding a compensating order.
    cancellations = cancel_orders(state, peer)
    if coordinator.get("unwinding"):
        cancellations += cancel_orders(state, primary_view)
        if cancellations: return cancellations, True
        actions = []
        for view in (primary_view, peer):
            market_kind, _, qty, _ = account_of(view, config)
            if qty: actions += submit_order(state, view, config, "_hedge_unwind", "Sell" if qty > 0 else "Buy", abs(qty), reduce=market_kind == "Perp")
        return actions, True
    if cancellations: return cancellations, True
    side = "Sell" if residual > 0 else "Buy"
    reduce = peer_kind == "Perp" and peer_position * (1 if side == "Buy" else -1) < 0
    return submit_order(state, peer, config, "_hedge", side, abs(residual), reduce=reduce), True


def decision_state(value, borrow_history):
    if not borrow_history:
        return copy.deepcopy(value)
    # Only the synchronous worker consumer may borrow old transcript rows until
    # it serializes this response. These fields are read-only or append-only:
    # copy their containers, and keep all mutable broker/feedback state owned.
    histories = {"bars", "accounts", "providers", "evaluations"}
    state = copy.deepcopy({key: item for key, item in value.items() if key not in histories})
    for key in histories & value.keys():
        state[key] = value[key].copy()
    return state


def decide(request, load_source, compile_program, digest, evaluate_external=None, profile=None,
           borrow_history=False, encoded_response=None):
    if request["protocol_version"] != "bot.v1" or request["plugin_id"] != "pine.strategy" or request["plugin_version"] not in ("1.0.0", "2.0.0") or request["state_version"] != 1:
        raise ValueError("unsupported Pine bot identity")
    config, observation = copy.deepcopy(request["config"]), request["observation"]
    if not config.get("quantize_strategy_orders", False):config.pop("quantize_strategy_orders", None)
    primary = observation["instrument_id"]
    source = load_source(config["script"])
    identity = digest({"source": source, "config": config, "participant": request["participant"]})
    copy_started = time.perf_counter() if profile is not None else 0
    state = decision_state(request["state"] or {}, borrow_history)
    if profile is not None: profile["state_copy_ms"] = (time.perf_counter() - copy_started) * 1000
    if state and (state.get("identity") != identity or state.get("broker_schema") != 2):
        raise ValueError("Pine source/config/account changed; create a new instance")
    views = {v["instrument_id"]: v for v in [observation, *observation.get("related_markets", [])]}
    if len(views) != 1 + len(observation.get("related_markets", [])):
        raise ValueError("duplicate related markets")
    for view in views.values():
        _, account, _, _ = account_of(view, config)
        if account["account_id"] != request["participant"]["account_id"] or view["market_time_ms"] != observation["market_time_ms"]:
            raise ValueError("cross-market observations must use the same account and time")
    data = observation.get("bot_market_data")
    if not data or data["interval_ms"] != config["bar_interval_ms"]: raise ValueError("host must supply configured history")
    bars = bars_of(data, observation["market_time_ms"])
    if len(bars) > config["history_limit"]: raise ValueError("Pine history_limit reached; reconstruction refused")
    previous = state.get("bars", [])
    if len(bars) < len(previous) or bars[:len(previous)] != previous: raise ValueError("committed Pine candle history changed or disappeared")
    if not state:
        kind, account, position, _ = account_of(observation, config)
        equity = number(account["equity"], "equity") if kind == "Perp" else number(account["cash_balance"], "cash") + position * (bars[-1]["close"] if bars else config["initial_avg_price_tick"])
        state.update(identity=identity, broker_schema=2, initial_capital=equity)
    sync_feedback(state, views, config)
    actions, block_primary = hedge(state, views, primary, config)
    if len(bars) > len(previous):
        frames = state.setdefault("accounts", [])
        for index in range(len(previous), len(bars)):
            frames.append(frame(observation, config, state, bars[index], None if index == len(bars)-1 else data["candles"][index]["close_time_ms"]))
        requests = {}
        for view in views.values():
            stream = view["bot_market_data"]
            requests[f'{view["instrument_id"]}:{timeframe(stream["interval_ms"])}'] = bars_of(stream, observation["market_time_ms"])
        for stream in data.get("streams", []):
            requests[f'{stream["instrument_id"]}:{timeframe(stream["interval_ms"])}'] = bars_of(stream, observation["market_time_ms"])
        # Persist streams once and record visible prefix lengths per evaluation.
        # A late cross-market bar cannot appear in an already committed pass.
        providers = state.setdefault("providers", {})
        for key, stream in requests.items():
            old = providers.get(key, [])
            if stream[:len(old)] != old: raise ValueError("committed requested-data history changed or disappeared")
            providers[key] = stream
        evaluations = state.setdefault("evaluations", [])
        for index in range(len(previous), len(bars)):
            cutoff = data["candles"][index]["close_time_ms"]
            evaluations.append({"time": cutoff, "providers": {key: sum(bar["time"] + interval_from_timeframe(key.rsplit(":", 1)[1]) <= cutoff for bar in stream) for key, stream in requests.items()}})
        def pass_factory(start):
            passes = []
            for index in range(start, len(evaluations)):
                evaluation = evaluations[index]
                request_data = [{"symbol": key.rsplit(":", 1)[0], "timeframe": key.rsplit(":", 1)[1], "bars": providers[key][:count]} for key, count in evaluation["providers"].items()]
                passes.append([{"account": frames[index], "bar": bars[index], "event_time_ms": evaluation["time"], "confirmed": True, "request_data": request_data}])
            return passes
        lowered = external_pass_source(lower_order_calls(source))
        program, overrides = compile_program(lowered, config["inputs"])
        if evaluate_external is None:
            evaluate = program.run_external
            pass_options = {"execution_passes": pass_factory(0)}
        else:
            evaluate = lambda *args, **kw: evaluate_external(program, source, request, *args, **kw)
            pass_options = {"pass_factory": pass_factory,
                            "execution_transcript": {"providers": providers, "evaluations": evaluations}}
        evaluate_started = time.perf_counter() if profile is not None else 0
        result = evaluate(bars, frames, input_overrides=overrides,
            chart_symbol=primary, chart_timeframe=timeframe(config["bar_interval_ms"]), **pass_options,
            request_bars={"$chart": {"minMove": number((observation.get("instrument") or {}).get("tick_size", 1), "tick size", 1), "priceScale": 1, "quantityPrecision": 0, "pointValue": 1}})
        if profile is not None: profile["evaluate_ms"] = (time.perf_counter() - evaluate_started) * 1000
        if result["protocol"] != "external-broker/1" or result["output"].get("strategy"):
            raise ValueError("external Pine execution produced a native broker ledger")
        prefix = [i for i in result["intents"] if i["bar_index"] < len(previous)]
        if previous and digest(prefix) != state["intents_digest"]: raise ValueError("previous Pine intents changed during reconstruction")
        latest = [i for i in result["intents"] if i["bar_index"] == len(bars)-1]
        if not block_primary:
            if config["hedge_instrument_id"] and any(split_id(i["id"] or "_all", primary, views)[0] != primary for i in latest):
                raise ValueError("automatic hedge owns peer execution; Pine must signal only the primary leg")
            actions += translate(latest, result["pyramiding"], views, primary, config, state)
        state.update(bars=bars, evaluated_bars=len(bars), last_bar_time=bars[-1]["time"], last_intents=latest,
            intents_digest=digest(result["intents"]), last_actions=actions)
        state["skipped_bar_decisions"] = state.get("skipped_bar_decisions", 0) + (max(0, len(bars)-len(previous)-1) if previous else 0)
    # A new bar's cancellation or updated intent takes precedence over a queued
    # replacement; never place an obsolete order before reading strategy.cancel.
    if not block_primary: actions += replacements(state, views, config)
    kind, account, position, _ = account_of(observation, config)
    actions += evaluate_exits(state, views, config)
    state.update(observed_position=position, observed_cash=str(account["cash_balance"]),
        last_fill_id=state["ledgers"][primary]["fills"][-1]["id"] if state["ledgers"][primary]["fills"] else -1,
        last_account=copy.deepcopy(observation["own_account"]))
    response = {key: request[key] for key in ("protocol_version", "plugin_id", "plugin_version", "state_version")}
    response.update(actions=actions, state=state)
    # Timed hedge compensation must wake even in a completely quiet market.
    coordinator = state.get("hedge", {})
    state.pop("_next_wake_ms", None)
    if coordinator.get("residual_qty") or coordinator.get("unwinding"):
        state["_next_wake_ms"] = min(observation["market_time_ms"] + 1000,
            coordinator.get("unhedged_since_ms", observation["market_time_ms"]) + config["hedge_timeout_ms"])
        if state["_next_wake_ms"] <= observation["market_time_ms"]:
            state["_next_wake_ms"] = observation["market_time_ms"] + 1000
    encode_started = time.perf_counter() if profile is not None else 0
    if len(actions) > 64:
        raise ValueError("Pine decision exceeds bot action/state limits")
    encoded = json.dumps(response, allow_nan=False)
    # json.dumps uses ensure_ascii=True, so characters equal UTF-8 bytes.
    if len(encoded) > 1048576:
        raise ValueError("Pine decision exceeds bot action/state limits")
    if encoded_response is not None:
        encoded_response.append(encoded)
    if profile is not None: profile["validate_response_ms"] = (time.perf_counter() - encode_started) * 1000
    return response


def interval_from_timeframe(value):
    if value.endswith("D"): return int(value[:-1]) * 86400000
    if value.endswith("S"): return int(value[:-1]) * 1000
    return int(value) * 60000
