#!/usr/bin/env python3
"""Closed-bar Pine decisions with authoritative spot fills and frozen replay.

The host supplies history and submits orders; this process does neither IO to
the exchange nor native-broker execution. Unsupported execution fails closed.
"""
from __future__ import annotations

import copy
from fractions import Fraction
import hashlib
import json
from functools import lru_cache
import math
import os
from pathlib import Path
import sys
import time

PACKAGE = Path(__file__).resolve().parent
MAX_EXACT = 2**53 - 1


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":"),
                                     allow_nan=False).encode()).hexdigest()


def integer(value, name, minimum=0, maximum=MAX_EXACT):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError(f"{name}: expected an integer")
    if not math.isfinite(value) or int(value) != value or not minimum <= value <= maximum:
        raise ValueError(f"{name}: outside exact integer range")
    return int(value)


@lru_cache(maxsize=1024)
def script_path(name):
    directory = (PACKAGE / "scripts").resolve()
    path = (directory / name).resolve()
    if not path.is_relative_to(directory) or path.suffix != ".pine":
        raise ValueError("script must be an installed .pine file inside scripts/")
    return path


SOURCES = {}  # path -> (mtime_ns, size, text); a changed file is read again.


def load_source(name):
    path = script_path(name)
    stat = path.stat()
    if stat.st_size > 65536:
        raise ValueError("Pine source exceeds 64 KiB")
    cached = SOURCES.get(path)
    if cached is None or cached[:2] != (stat.st_mtime_ns, stat.st_size):
        cached = SOURCES[path] = (stat.st_mtime_ns, stat.st_size, path.read_text(encoding="utf-8-sig"))
    return cached[2]


def account_feedback(state, observation, data):
    account = observation.get("own_account") or {}
    if set(account) != {"Spot"}:
        raise ValueError("Pine v1 requires an exclusive spot account")
    account = account["Spot"]
    position = integer(account["position_qty"], "position")
    cash = integer(account["cash_balance"], "cash")
    fees = integer(account["fees_paid"], "fees")
    if observation["own_orders"]:
        raise ValueError("Pine v1 requires no resting or externally submitted orders")
    if not state:
        if position or data["own_fills"] or fees:
            raise ValueError("start Pine v1 with a fresh, flat account")
        state.update(initial_capital=cash, filled_position=0, cost="0", last_fill_id=-1,
                     cash=cash, fees=fees, fill_receipts=[])
    cost = Fraction(state["cost"])
    expected = state["filled_position"]
    expected_cash = state["cash"] - (fees - state["fees"])
    if fees < state["fees"]:
        raise ValueError("account fee history moved backwards")
    previous_id = -1
    seen_cursor = state["last_fill_id"] == -1
    if len(data["own_fills"]) != len(data["fill_details"]):
        raise ValueError("authoritative timed fee receipts are required")
    stored = {receipt["id"]: receipt for receipt in state["fill_receipts"]}
    new_fees = 0
    for fill, detail in zip(data["own_fills"], data["fill_details"]):
        trade_id = integer(fill["trade_id"], "trade id")
        if detail["trade_id"] != trade_id:
            raise ValueError("fill accounting receipt ID mismatch")
        if trade_id <= previous_id:
            raise ValueError("fill history is not strictly ordered")
        previous_id = trade_id
        if trade_id == state["last_fill_id"]:
            seen_cursor = True
        maker = fill["maker_account_id"] == account["account_id"]
        taker = fill["taker_account_id"] == account["account_id"]
        if maker == taker:
            raise ValueError("invalid own fill or self trade")
        side = fill["taker_side"]
        if side not in ("Buy", "Sell"):
            raise ValueError("invalid fill side")
        buy = (side == "Buy") if taker else (side == "Sell")
        qty = integer(fill["qty"], "fill quantity", 1)
        price = integer(fill["price_tick"], "fill price", 1)
        time = integer(detail["market_time_ms"], "fill time")
        if time > observation["market_time_ms"]:
            raise ValueError("future fill receipt is not decision-visible")
        fee = integer(detail["fee_paid"], "fill fee")
        receipt = {"id": trade_id, "time": time, "buy": buy, "qty": qty, "price": price, "fee": fee}
        if trade_id <= state["last_fill_id"]:
            if stored.get(trade_id) != receipt:
                raise ValueError("committed fill receipt changed or disappeared")
            continue
        if state["fill_receipts"] and time < state["fill_receipts"][-1]["time"]:
            raise ValueError("fill receipt time moved backwards")
        state["fill_receipts"].append(receipt)
        new_fees += fee
        if buy:
            cost += price * qty
            expected += qty
            expected_cash -= price * qty
        else:
            if qty > expected:
                raise ValueError("fill history would create a short position")
            cost -= cost * Fraction(qty, expected)
            expected -= qty
            expected_cash += price * qty
        state["last_fill_id"] = trade_id
    if not seen_cursor or expected != position or expected_cash != cash or new_fees != fees - state["fees"]:
        raise ValueError("account and complete fill history disagree; exclusive account required")
    state.update(cost=str(cost), filled_position=expected, cash=cash, fees=fees)
    return account, cost


def ledger_before(state, time):
    """Authoritative account at a missed bar close; never a simulated fill."""
    cash, position, cost = state["initial_capital"], 0, Fraction(0)
    for receipt in state["fill_receipts"]:
        if receipt["time"] >= time:
            break
        qty, price = receipt["qty"], receipt["price"]
        if receipt["buy"]:
            cost += qty * price
            position += qty
            cash -= qty * price
        else:
            cost -= cost * Fraction(qty, position)
            position -= qty
            cash += qty * price
        cash -= receipt["fee"]
    return cash, position, cost


@lru_cache(maxsize=1024)
def prepared_source(source):
    import pine_compat
    if pine_compat.__version__ not in ("0.3.1", "0.3.1+mf1"):
        raise ValueError("Pine bot requires qualified pine-compat-runtime==0.3.1")
    report = pine_compat.analyze_script(source)
    # Titles are the public configuration keys; stable callSiteIds are runtime keys.
    available = {}
    ambiguous = set()
    for item in report["inputs"]:
        title = item.get("title")
        if not isinstance(title, str) or not title:
            continue
        if title in available:
            ambiguous.add(title)
        available[title] = item["callSiteId"]
    return pine_compat.compile_script(source), available, ambiguous


def compile_program(source, inputs):
    program, available, ambiguous = prepared_source(source)
    if set(inputs) & ambiguous:
        raise ValueError("overridden input titles must be unique")
    unknown = set(inputs) - set(available)
    if unknown:
        raise ValueError(f"unknown Pine inputs: {sorted(unknown)}")
    return program, {available[key]: value for key, value in inputs.items()}


def validate_intents(intents, pyramiding, state):
    if pyramiding not in (0, 1):
        raise ValueError("Pine v1 does not support pyramiding")
    counts = {}
    for intent in intents:
        bar = intent["bar_index"]
        counts[bar] = counts.get(bar, 0) + 1
        if counts[bar] > 1:
            raise ValueError("Pine v1 supports at most one order intent per closed bar")
        if intent["action"] not in ("entry", "close", "close_all"):
            raise ValueError("Pine v1 supports market long entry, close and close_all only")
        if any(intent.get(key) is not None for key in
               ("limit", "stop", "profit", "loss", "trail_price", "trail_points", "trail_offset", "from_entry")):
            raise ValueError("Pine v1 does not support resting, stop, bracket or trailing orders")
        if intent["action"] == "entry":
            if intent["direction"] != "long":
                raise ValueError("Pine v1 cannot short spot inventory")
            integer(intent["qty"], "entry quantity", 1)
            if state.get("entry_id") not in (None, intent["id"]):
                raise ValueError("Pine v1 supports one entry ID per account")
            state["entry_id"] = intent["id"]
        elif intent["action"] == "close" and state.get("entry_id") not in (None, intent["id"]):
            raise ValueError("close refers to an unknown entry ID")


def translate(intents, pyramiding, account, observation, config, state):
    validate_intents(intents, pyramiding, state)
    if not intents:
        return []
    intent = intents[0]
    position = int(account["position_qty"])
    if intent["action"] == "entry":
        qty = integer(intent["qty"], "entry quantity", 1)
        if position:
            return []
        side, levels = "Buy", observation["book"]["asks"]
        qty = min(qty, config["max_qty"], config["inventory_cap"] - position)
    else:
        if intent["action"] == "close" and intent["id"] != state.get("entry_id"):
            if not position:
                return []
            raise ValueError("close refers to an unknown entry ID")
        side, levels = "Sell", observation["book"]["bids"]
        qty = position
        if intent.get("qty") is not None:
            qty = min(qty, integer(intent["qty"], "close quantity", 1))
        elif intent.get("qty_percent") is not None:
            percent = intent["qty_percent"]
            if isinstance(percent, bool) or not isinstance(percent, (int, float)) or not 0 < percent <= 100:
                raise ValueError("invalid close percentage")
            qty = min(qty, int(Fraction(str(percent)) * position / 100))
        qty = min(qty, config["max_qty"], int(account["available_position"]))
    if not levels or qty <= 0:
        return []
    top = integer(levels[0]["price_tick"], "book price", 1)
    price = integer(top + config["slippage_ticks"] if side == "Buy" else max(1, top - config["slippage_ticks"]),
                    "protected order price", 1)
    if side == "Buy":
        # Round the configured fee buffer up per unit to reserve conservatively
        # even when the IOC crosses many one-unit orders.
        unit_cost = price + (price * config["fee_buffer_ppm"] + 999999) // 1000000
        qty = min(qty, int(account["available_cash"]) // unit_cost)
    if qty <= 0:
        return []
    return [{"PlaceImmediateOrCancel": {"side": side, "price_tick": price, "qty": qty}}]


def legacy_decide(request):
    if (request["protocol_version"], request["plugin_id"], request["plugin_version"], request["state_version"]) != (
            "bot.v1", "pine.strategy", "1.0.0", 1):
        if (request["protocol_version"], request["plugin_id"], request["plugin_version"], request["state_version"]) != ("bot.v1", "pine.strategy", "2.0.0", 1):
            raise ValueError("unsupported Pine bot identity")
    config = dict(request["config"])
    if not config.get("quantize_strategy_orders", False):config.pop("quantize_strategy_orders", None)
    observation = request["observation"]
    data = observation.get("bot_market_data")
    if data is None or data["interval_ms"] != config["bar_interval_ms"]:
        raise ValueError("host must supply the configured closed-bar stream")
    if data["truncated"]:
        raise ValueError("Pine history was truncated; reconstruction refused")
    source = load_source(config["script"])
    identity = digest({"source": source, "config": config, "participant": request["participant"]})
    state = copy.deepcopy(request["state"] or {})
    if state and state.get("identity") != identity:
        raise ValueError("Pine source/config/account changed; create a fresh instance and account")
    account, cost = account_feedback(state, observation, data)
    state["identity"] = identity
    bars = []
    for candle in data["candles"]:
        if not candle["is_final"] or candle["close_time_ms"] > observation["market_time_ms"]:
            raise ValueError("forming or future candle is not decision-visible")
        if bars and candle["open_time_ms"] <= bars[-1]["time"]:
            raise ValueError("candle history is not strictly ordered")
        integer(candle["open_time_ms"], "bar time")
        integer(candle["volume"], "bar volume")
        for key in ("open_tick", "high_tick", "low_tick", "close_tick"):
            integer(candle[key], key, 1)
        bars.append({"time": candle["open_time_ms"], "open": candle["open_tick"],
                     "high": candle["high_tick"], "low": candle["low_tick"],
                     "close": candle["close_tick"], "volume": candle["volume"]})
    if len(bars) > config["history_limit"]:
        raise ValueError("Pine history_limit reached; no rolling reset of strategy state")
    previous = state.get("bars", [])
    if len(bars) < len(previous) or bars[:len(previous)] != previous:
        raise ValueError("committed Pine candle history changed or disappeared")
    actions = []
    if len(bars) > len(previous):
        frames = state.get("accounts", [])
        # First observation warms up history with a genuinely flat, new account;
        # historical intents are never sent to the matching engine.
        for index in range(len(previous), len(bars)):
            bar = bars[index]
            mark = integer(bar["close"], "close price", 1)
            if index == len(bars) - 1:
                cash, position, bar_cost = int(account["cash_balance"]), int(account["position_qty"]), cost
            else:
                cash, position, bar_cost = ledger_before(state, data["candles"][index]["close_time_ms"])
            equity = cash + position * mark
            integer(equity, "equity")
            frames.append({"time": bar["time"], "position_size": position,
                           "position_avg_price": float(bar_cost / position) if position else None,
                           "initial_capital": state["initial_capital"], "equity": equity,
                           "netprofit": float(cash - state["initial_capital"] + bar_cost),
                           "openprofit": float(position * mark - bar_cost)})
        program, overrides = compile_program(source, config["inputs"])
        interval = config["bar_interval_ms"]
        timeframe = "1D" if interval == 86400000 else (
            str(interval // 60000) if interval % 60000 == 0 else f"{interval // 1000}S")
        result = program.run_external(bars, frames, input_overrides=overrides,
                                      chart_symbol=observation["instrument_id"], chart_timeframe=timeframe)
        if result["protocol"] != "external-broker/1" or result["output"].get("strategy"):
            raise ValueError("external Pine execution unexpectedly produced a native broker ledger")
        intents = result["intents"]
        validate_intents(intents, result["pyramiding"], state)
        prefix = [item for item in intents if item["bar_index"] < len(previous)]
        if previous and digest(prefix) != state["intents_digest"]:
            raise ValueError("previous Pine intents changed during reconstruction")
        latest = [item for item in intents if item["bar_index"] == len(bars) - 1]
        actions = translate(latest, result["pyramiding"], account, observation, config, state)
        state.update(bars=bars, accounts=frames, intents_digest=digest(intents),
                     last_bar_time=bars[-1]["time"], last_intents=latest,
                     last_actions=actions, evaluated_bars=len(bars))
        skipped = max(0, len(bars) - len(previous) - 1) if previous else 0
        state["skipped_bar_decisions"] = state.get("skipped_bar_decisions", 0) + skipped
    state["observed_position"] = int(account["position_qty"])
    state["observed_cash"] = str(account["cash_balance"])
    response = {key: request[key] for key in ("protocol_version", "plugin_id", "plugin_version", "state_version")}
    response.update(actions=actions, state=state)
    if len(json.dumps(response, allow_nan=False).encode()) > 1_048_576:
        raise ValueError("Pine state exceeds bot response limit")
    return response


def decide(request, profile=None, *, borrow_history=False, encoded_response=None, defer_state=False,
           immutable_input=False):
    """With `defer_state`, a broker decision leaves the response encoding and
    its size check to the caller, and fills no `encoded_response`. See the
    broker for `immutable_input`."""
    # Keep previously journaled v1 instances readable, including their original
    # source/config digest. New instances use the full broker adapter by default.
    state = request.get("state") or {}
    if request["config"].get("execution_mode") == "legacy" or (state.get("identity") and not state.get("broker_schema")):
        request = copy.deepcopy(request)
        request["config"] = {key: value for key, value in request["config"].items() if key in
            {"script", "inputs", "bar_interval_ms", "history_limit", "max_qty", "inventory_cap", "slippage_ticks", "fee_buffer_ppm"}}
        return legacy_decide(request)
    broker = broker_module()
    return broker.decide(request, load_source, compile_program, digest, incremental_module().evaluate,
                         profile, borrow_history, encoded_response, defer_state, immutable_input)


@lru_cache(maxsize=1)
def incremental_module():
    import importlib.util
    spec = importlib.util.spec_from_file_location("marketforge_pine_incremental", PACKAGE / "incremental.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@lru_cache(maxsize=1)
def broker_module():
    import importlib.util
    spec = importlib.util.spec_from_file_location("marketforge_pine_broker", PACKAGE / "broker.py")
    broker = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(broker)
    return broker


def serve_worker():
    """Durable state travels with each call; speculative caches require exact committed prefixes.

    A host that asks for `reply_state_delta` gets the new state as patches from
    the request's state. Both sides then fold those patches into their cached
    request, so the state itself crosses the pipe only when a delta cannot
    describe it exactly."""
    import importlib.util
    spec = importlib.util.spec_from_file_location("marketforge_pine_transport", PACKAGE / "transport.py")
    transport = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(transport)
    decoder = transport.RequestDecoder()
    lengths = transport.StateLengths()
    trace_dir = os.environ.get("PINE_BENCH_PY_TRACE_DIR")
    trace = (Path(trace_dir) / f"worker-{os.getpid()}.jsonl").open("w", encoding="utf-8") if trace_dir else None
    diagnostics = os.environ.get("MARKETFORGE_BOT_WORKER_DIAGNOSTICS") == "1"
    for line in sys.stdin:
        begun = time.perf_counter()
        envelope = json.loads(line)
        if envelope.get("protocol") != "bot.worker.v1" or not isinstance(envelope.get("id"), int):
            raise ValueError("unsupported worker envelope")
        response = {"protocol": "bot.worker.v1", "id": envelope["id"]}
        profile = {} if trace else None
        encoded_response = []
        key = envelope.get("cache_key")
        fold = None
        try:
            request = decoder.decode_readonly(envelope)
            decoded = time.perf_counter()
            if diagnostics:
                incremental_module().LAST_EVALUATION = None
            delta_reply = envelope.get("reply_state_delta") is True and key is not None
            # Decoded requests are copy-on-write: no object changes in place.
            response["response"] = decide(request, profile, borrow_history=True, encoded_response=encoded_response,
                                           defer_state=delta_reply, immutable_input=True)
            if diagnostics:
                response["runtime_profile"] = incremental_module().LAST_EVALUATION or {
                    "mode": "replay" if request["config"].get("execution_mode") == "legacy" else "no_evaluation",
                    "reason": "legacy" if request["config"].get("execution_mode") == "legacy" else None,
                }
            decided = time.perf_counter()
            if delta_reply and not encoded_response:
                fold = state_reply(transport, lengths, key, request["state"], response, encoded_response)
        except Exception as exc:
            response.pop("response", None)
            response.pop("runtime_profile", None)
            response["error"] = str(exc)
        if fold is not None:
            encoded = fold[0]
        else:
            encoded = encode_worker_response(response, encoded_response)
        finished = time.perf_counter()
        print(encoded, flush=True)
        if fold is not None:
            # The host folds the same patches once it accepts this reply.
            decoder.fold_state(key, fold[1])
            lengths.remember(key, decoder.requests[key][1]["state"], fold[2])
        if trace and "response" in response:
            data = request["observation"].get("bot_market_data") or {}
            trace.write(json.dumps({"id":request["participant"]["participant_id"], "sequence":envelope["id"],
                "market_time_ms":request["observation"]["market_time_ms"],
                "decode_ms":(decoded-begun)*1000, "decide_ms":(decided-decoded)*1000,
                "encode_ms":(finished-decided)*1000, "input_bytes":len(line), "reply_bytes":len(encoded),
                "history":{name: len(data.get(name) or ()) for name in ("candles", "own_fills", "account_events")},
                **profile}) + "\n")
            trace.flush()


def state_reply(transport, lengths, key, old, response, encoded_response):
    """The worker reply carrying the new state as patches from `old`, with those
    patches and the state's json.dumps length. Without an exact delta, returns
    None and leaves the full encoding in `encoded_response`, as the broker
    does. Either way it raises as the broker does for an oversized response."""
    decision = response["response"]
    try:
        if type(old) is not dict or type(decision["state"]) is not dict:
            raise transport.NotDelta()
        patches, change = transport.state_delta(old, decision["state"])
    except transport.NotDelta:
        patches = None
    head = dict(decision, state=None)
    if patches is None:
        encoded = json.dumps(decision, allow_nan=False)
        size = len(encoded)
    else:
        length = lengths.length(key, old) + change
        size = len(json.dumps(head, allow_nan=False)) - len("null") + length
    # The broker's check of the full response: json.dumps defaults, ASCII.
    if size > 1048576:
        raise ValueError("Pine decision exceeds bot action/state limits")
    if patches is None:
        encoded_response.append(encoded)
        return None
    reply = {key: response[key] for key in ("protocol", "id", "runtime_profile") if key in response}
    reply.update(response=head, state_delta=patches)
    encoded = json.dumps(reply, separators=(",", ":"), allow_nan=False)
    if len(encoded) + 1 > 1_048_576:
        # The full reply fits; the state passed the size check above.
        encoded_response.append(json.dumps(decision, allow_nan=False))
        return None
    return encoded, patches, length


def encode_worker_response(response, encoded_response):
    if encoded_response:
        # Reuse the exact default-spacing JSON that passed the original Pine
        # size/finite-number check. JSON whitespace has no protocol meaning.
        prefix = json.dumps({key: response[key] for key in ("protocol", "id", "runtime_profile") if key in response},
                            separators=(",", ":"), allow_nan=False)
        encoded = prefix[:-1] + ',"response":' + encoded_response[0] + '}'
        # Near the worker framing bound, retain the original compact encoding
        # so a formerly valid response cannot be rejected by extra whitespace.
        if len(encoded) + 1 <= 1_048_576:
            return encoded
    encoded = json.dumps(response, separators=(",", ":"), allow_nan=False)
    if len(encoded.encode("utf-8")) + 1 > 1_048_576 and "runtime_profile" in response:
        # Telemetry cannot make a previously valid strategy response too large.
        # The host explicitly counts the omitted profile as unreported.
        encoded = json.dumps({key: value for key, value in response.items() if key != "runtime_profile"},
                             separators=(",", ":"), allow_nan=False)
    return encoded


if __name__ == "__main__":
    extension = PACKAGE / ".runtime" / "strategy-ecosystem"
    has_extension = (extension / "pine_compat.pyd").is_file() or (extension / "pine_compat.so").is_file()
    if has_extension:
        sys.path.insert(0, str(extension))
    if sys.argv[1:] == ["--worker"]:
        runtime = PACKAGE / ".runtime"
        if not has_extension and runtime.is_dir():
            sys.path.insert(0, str(runtime))
        serve_worker()
    else:
        print(json.dumps(decide(json.loads(sys.stdin.readline())), separators=(",", ":"), allow_nan=False), flush=True)
