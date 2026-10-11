"""Sampled compound conditions over exact window aggregates and scoped state."""
import math
import time
import uuid
from fractions import Fraction

from .alerts import METRICS, OPS, metric_value
from .window_data import WINDOW_METRICS, query_args

SOURCE_FIELDS = {"source": {"type": "string", "enum": ["window", "state", "indicator"]},
    "instrument": {"type": "string"}, "metric": {"type": "string", "enum": sorted(set(METRICS+WINDOW_METRICS+("sma", "ema", "rsi", "atr", "vwap")))},
    **{k: {"type": "integer"} for k in ("window_ms", "scan_limit", "book_levels", "period", "interval_ms", "limit")},
    "large_trade_min_notional": {"type": ["integer", "string"]}}
SOURCE_SCHEMA = {"type": "object", "additionalProperties": False, "properties": SOURCE_FIELDS,
    "required": ["source", "instrument", "metric"]}
RULE_SCHEMA = {"type": "object", "additionalProperties": False,
    "properties": {"left": SOURCE_SCHEMA, "op": {"type": "string", "enum": list(OPS)},
        "right": {"anyOf": [{"type": "number"}, {"type": "string"}, SOURCE_SCHEMA]}}, "required": ["left", "op", "right"]}


def numeric(value):
    if type(value) is int: return Fraction(value)
    if isinstance(value, dict) and set(value) == {"numerator", "denominator"}:
        return numeric(value["numerator"]) / numeric(value["denominator"])
    if type(value) is float and math.isfinite(value): return Fraction(str(value))
    if isinstance(value, str) and len(value) <= 128:
        # Decimal literals only; no expressions, ratios, infinity or NaN.
        import re
        if re.fullmatch(r"-?[0-9]+(?:\.[0-9]+)?", value, flags=re.ASCII): return Fraction(value)
    raise ValueError("comparison needs a finite number or exact decimal string")


def validate_rules(runtime, config, rules):
    from .runtime import integer
    if not isinstance(rules, list) or not 1 <= len(rules) <= 8: raise ValueError("choose 1-8 combined rules")
    for rule in rules:
        if not isinstance(rule, dict) or set(rule) != {"left", "op", "right"} or rule["op"] not in OPS:
            raise ValueError("invalid combined rule")
        for index, ref in enumerate((rule["left"], rule["right"])):
            if not isinstance(ref, dict):
                if index == 0: raise ValueError("left operand must be a source")
                numeric(ref); continue
            runtime.check_instrument(config, ref.get("instrument"))
            common = {"source", "instrument", "metric"}
            source = ref.get("source")
            if source == "state":
                allowed, metrics = common, METRICS
            elif source == "window":
                allowed = common | {"window_ms", "scan_limit", "book_levels", "large_trade_min_notional"}
                metrics = WINDOW_METRICS
                query_args(ref)
            elif source == "indicator":
                allowed, metrics = common | {"period", "interval_ms", "limit"}, ("sma", "ema", "rsi", "atr", "vwap")
                integer(ref.get("period", 14), 2, 200)
                integer(ref.get("interval_ms", 1000), 1, 2678400000)
                integer(ref.get("limit", 500), 1, 2000)
            else: raise ValueError("invalid combined source")
            if not common <= set(ref) or not set(ref) <= allowed or ref["metric"] not in metrics:
                raise ValueError("invalid combined source fields or metric")


def evaluate(runtime, config, alert, cache):
    from .market_data import indicators
    def resolve(ref):
        if not isinstance(ref, dict): return numeric(ref), None
        source, instrument = ref["source"], ref["instrument"]
        params = {k: v for k, v in ref.items() if k not in ("source", "metric")}
        key = (source, tuple(sorted(params.items())))
        if key not in cache:
            if source == "window": cache[key] = runtime.read_window_stats(config, params)
            elif source == "state": cache[key] = runtime.observation(config, instrument)
            else:
                bars = runtime.candle_data(config, params)
                cache[key] = {**bars, "latest": indicators(bars["candles"], ref.get("period", 14))["latest"]}
        data = cache[key]
        clock = data.get("market_time_ms")
        valid = type(clock) is int and data.get("status", "Running") == "Running"
        if source == "window":
            valid = valid and data.get("complete") is True and data.get("end_market_time_ms") == clock
            value = data["metrics"].get(ref["metric"])
        elif source == "state": value = metric_value(data, ref["metric"])
        else: value = (data.get("latest") or {}).get(ref["metric"])
        evidence = {**ref, "value": value, "market_time_ms": clock}
        if "through_command_seq" in data: evidence["through_command_seq"] = data["through_command_seq"]
        if "complete" in data: evidence["complete"] = data["complete"]
        if not valid or value is None: return None, evidence
        try: return numeric(value), evidence
        except (ValueError, ZeroDivisionError): return None, evidence
    matches, resets, evidence, clocks = [], [], [], []
    for rule in alert["rules"]:
        left, l = resolve(rule["left"]); right, r = resolve(rule["right"])
        evidence.append({"left": l, "op": rule["op"], "right": r if r else rule["right"]})
        clocks.extend(e["market_time_ms"] for e in (l, r) if e and type(e.get("market_time_ms")) is int)
        valid = left is not None and right is not None
        matches.append(OPS[rule["op"]](left, right) if valid else None)
        margin = alert.get("hysteresis", 0)
        threshold = right - margin if valid and rule["op"] in ("gt", "gte") else right + margin if valid else None
        resets.append(not OPS[rule["op"]](left, threshold) if valid else None)
    # Never mix unrelated market times or use partial/unknown inputs to rearm.
    if not clocks or max(clocks)-min(clocks) > alert["max_skew_market_time_ms"] or any(m is None for m in matches):
        return False, False, evidence
    matched = all(matches) if alert["match"] == "all" else any(matches)
    reset = any(resets) if alert["match"] == "all" else all(resets)
    return matched, reset, evidence


def poll(alerts, trader):
    from marketforge import MarketForgeError
    import http.client
    runtime, cache = alerts.runtime, {}
    snapshot = alerts.state(trader)
    now_mono = time.monotonic()
    active = {a["version"] for a in snapshot["alerts"] if a.get("kind") == "combined" and a["status"] == "armed"}
    checks = alerts.combined_checks.setdefault(trader, {})
    for version in list(checks):
        if version not in active: del checks[version]
    results = []
    for saved in snapshot["alerts"]:
        if saved["version"] not in active or now_mono-checks.get(saved["version"], -1e20) < saved["evaluation_interval_seconds"]: continue
        checks[saved["version"]] = now_mono
        try:
            matched, reset, evidence = evaluate(runtime, runtime.config(trader), saved, cache)
            error = None
        except (OSError, ValueError, KeyError, TypeError, MarketForgeError, http.client.HTTPException) as exc:
            matched, reset, evidence, error = False, False, [], type(exc).__name__
        results.append((saved["version"], matched, reset, evidence, error))
    with runtime.lock(trader):
        if runtime.config(trader)["status"] != "running": return
        state, fired, changed, now = alerts.state(trader), [], False, time.time()
        for version, matched, reset, evidence, error in results:
            alert = next((a for a in state["alerts"] if a["version"] == version and a["status"] == "armed"), None)
            if alert is None: continue
            before = dict(alert)
            if error != alert.get("evaluation_error"):
                alert["evaluation_error"] = error
                runtime.store.event(trader, "combined_alert_monitor_error" if error else "combined_alert_monitor_recovered", {"name":alert["name"], "error":error})
            if reset: alert["latched"] = False
            if not matched: alert["candidate_since"] = None
            elif alert["candidate_since"] is None: alert["candidate_since"] = now
            ready = alert["last_trigger_at"] is None or now-alert["last_trigger_at"] >= alert["cooldown_seconds"]
            spaced = alert["priority"] == "normal" or now-state.get("last_interrupt_at", -1e20) >= alert["interrupt_min_interval_seconds"]
            if matched and now-alert["candidate_since"] >= alert["sustain_seconds"] and not alert["latched"] and ready and spaced:
                alert.update(latched=True, last_trigger_at=now, trigger_count=alert["trigger_count"]+1)
                if not alert["repeat"]: alert["status"] = "triggered"
                fired.append({"id":uuid.uuid4().hex, "name":alert["name"], "reason":alert["reason"], "kind":"combined",
                    "conditions":evidence, "match":alert["match"], "wall_time":now,
                    "pause_strategies":alert["pause_strategies"], "priority":alert["priority"]})
            changed = changed or alert != before
        if fired: alerts.deliver(trader, state, fired, now)
        elif changed: runtime.store.put("alerts_state", trader, state)
    alerts.flush_normal(trader)
