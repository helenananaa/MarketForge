"""Durable, declarative interrupts over participant observations."""
import operator
import http.client
import threading
import time
import uuid


METRICS = ("best_bid", "best_ask", "spread", "last_price", "bid_qty", "ask_qty",
           "cash_balance", "available_cash", "position_qty", "equity", "own_order_count", "market_time_ms", "mark_price", "maintenance_margin", "margin_buffer", "margin_ratio_ppm", "liquidatable")
OPS = {"gt": operator.gt, "gte": operator.ge, "lt": operator.lt,
       "lte": operator.le, "eq": operator.eq, "ne": operator.ne}
CONDITION_SCHEMA = {"type": "object", "additionalProperties": False,
    "properties": {"instrument": {"type": "string"}, "metric": {"type": "string", "enum": list(METRICS)},
                   "op": {"type": "string", "enum": list(OPS)}, "value": {"type": "integer"}},
    "required": ["instrument", "metric", "op", "value"]}


class DecisionToken:
    def __init__(self, stop, interrupt):
        self.stop, self.interrupt = stop, interrupt

    def is_set(self):
        return self.stop.is_set() or self.interrupt.is_set()


def metric_value(observation, metric):
    book = observation.get("book", {})
    bids, asks = book.get("bids", []), book.get("asks", [])
    if metric in ("best_bid", "bid_qty"):
        return bids[0].get("price_tick" if metric == "best_bid" else "qty") if bids else None
    if metric in ("best_ask", "ask_qty"):
        return asks[0].get("price_tick" if metric == "best_ask" else "qty") if asks else None
    if metric == "spread":
        return asks[0]["price_tick"] - bids[0]["price_tick"] if asks and bids else None
    if metric == "last_price":
        trades = observation.get("public_trades", [])
        return trades[-1]["price_tick"] if trades else None
    if metric == "own_order_count":
        return len(observation.get("own_orders", []))
    if metric == "market_time_ms":
        return observation.get(metric)
    if metric == "mark_price": return (observation.get("risk") or {}).get("mark_price_tick") or (observation.get("perp_price") or {}).get("mark_price_tick")
    if metric in ("margin_buffer", "margin_ratio_ppm"): return (observation.get("risk") or {}).get(metric)
    account = observation.get("own_account") or {}
    account = account.get("Spot", account.get("Perp", {}))
    if metric == "liquidatable": return int(account["margin_status"] == "liquidatable") if "margin_status" in account else None
    if metric == "equity" and "equity" not in account:
        # Spot equity needs a real observed mark; never invent a zero price.
        price = metric_value(observation, "last_price")
        if price is None:
            price = metric_value(observation, "best_bid")
        if price is None or "cash_balance" not in account or "position_qty" not in account:
            return None
        return account["cash_balance"] + account["position_qty"] * price
    return account.get(metric)


class Alerts:
    def __init__(self, runtime):
        self.runtime = runtime
        self.signals = {}

    def state(self, trader):
        return self.runtime.store.get("alerts_state", trader, {"generation": 0, "alerts": [], "pending": []})

    def token(self, trader, stop):
        with self.runtime.lock(trader):
            return DecisionToken(stop, self.signals.setdefault(trader, threading.Event()))

    def list(self, trader):
        return self.state(trader)["alerts"]

    def set(self, config, args):
        from .runtime import identifier, integer, text_value
        name = identifier(args["name"])
        conditions = args["conditions"]
        if not isinstance(conditions, list) or not 1 <= len(conditions) <= 8:
            raise ValueError("choose 1-8 alert conditions")
        for condition in conditions:
            if not isinstance(condition, dict) or set(condition) != {"instrument", "metric", "op", "value"}:
                raise ValueError("invalid alert condition fields")
            self.runtime.check_instrument(config, condition["instrument"])
            if condition["metric"] not in METRICS or condition["op"] not in OPS:
                raise ValueError("invalid alert metric or comparison")
            integer(condition["value"], -(2**127), 2**127 - 1)
        mode, repeat = args.get("match", "all"), args.get("repeat", False)
        pause = args.get("pause_strategies", False)
        priority = args.get("priority", "urgent")
        if priority not in {"normal", "urgent"} or (priority == "normal" and pause):
            raise ValueError("normal alerts cannot pause strategies; use urgent priority")
        hysteresis = integer(args.get("hysteresis", 0), 0, 2**127-1)
        if hysteresis and any(c["op"] in {"eq", "ne"} for c in conditions):
            raise ValueError("hysteresis requires gt/gte/lt/lte conditions")
        if mode not in ("all", "any") or type(repeat) is not bool or type(pause) is not bool:
            raise ValueError("invalid alert match/repeat/pause_strategies")
        trader = config["id"]
        state = self.state(trader)
        old = next((a for a in state["alerts"] if a["name"] == name), None)
        if old is None and len(state["alerts"]) >= 32:
            raise ValueError("at most 32 named alerts per trader; replace or reuse a name")
        alert = {"name": name, "version": uuid.uuid4().hex, "conditions": conditions, "match": mode,
                 "repeat": repeat, "pause_strategies": pause,
                 "cooldown_seconds": integer(args.get("cooldown_seconds", 5), 2, 3600),
                 "reason": text_value(args.get("reason", ""), 1000), "status": "armed",
                 "latched": False, "last_trigger_at": None, "trigger_count": 0}
        alert.update(priority=priority, sustain_seconds=integer(args.get("sustain_seconds", 0), 0, 3600),
            hysteresis=hysteresis, candidate_since=None,
            interrupt_min_interval_seconds=integer(args.get("interrupt_min_interval_seconds", 0), 0, 3600))
        state["alerts"] = [a for a in state["alerts"] if a["name"] != name] + [alert]
        self.runtime.store.put("alerts_state", trader, state)
        self.runtime.store.event(trader, "alert_set", alert)
        return alert

    def cancel(self, trader, name):
        from .runtime import identifier
        identifier(name)
        state = self.state(trader)
        alert = next((a for a in state["alerts"] if a["name"] == name), None)
        if alert is None:
            raise ValueError("unknown alert")
        alert["status"] = "cancelled"
        self.runtime.store.put("alerts_state", trader, state)
        self.runtime.store.event(trader, "alert_cancelled", {"name": name})
        return alert

    def suspend(self, trader, triggers):
        runtime = self.runtime
        for strategy in runtime.strategies(trader):
            pending = strategy.get("pending")
            if pending:
                strategy["held_actions"] = [{"call_id": f"strategy:{strategy['name']}:{strategy['tick']}:{index}", "action": action}
                    for index, action in enumerate(pending["result"].get("actions", []))
                    if runtime.store.receipt(trader, f"strategy:{strategy['name']}:{strategy['tick']}:{index}")["status"] != "done"]
                runtime.store.event(trader, "strategy_tick_aborted", {"name": strategy["name"], "tick": strategy["tick"], "reason": "alert interrupt"})
            strategy.update(pending=None, tick=uuid.uuid4().hex)
            if any(t["pause_strategies"] for t in triggers):
                strategy["running"] = False
            runtime.store.put("strategy", f"{trader}:{strategy['name']}", strategy)

    def poll(self, trader):
        runtime = self.runtime
        # Slow network reads must leave trading/model lanes available.
        state = self.state(trader)
        instruments = {c["instrument"] for a in state["alerts"] if a["status"] == "armed" for c in a["conditions"]}
        if not instruments:
            self.flush_normal(trader)
            return
        observed = {i: runtime.observation(runtime.config(trader), i) for i in sorted(instruments)}
        versions = {a["version"] for a in state["alerts"]}
        with runtime.lock(trader):
            if runtime.config(trader)["status"] != "running":
                return
            state = self.state(trader)
            fired, changed = [], False
            now = time.time()
            for alert in state["alerts"]:
                if alert["status"] != "armed" or alert["version"] not in versions:
                    continue
                values, matches, resets = [], [], []
                for condition in alert["conditions"]:
                    observation = observed[condition["instrument"]]
                    value = metric_value(observation, condition["metric"])
                    valid = observation.get("status") == "Running" and type(value) is int
                    values.append({**condition, "observed": value, "market_time_ms": observation["market_time_ms"], "step": observation["step"]})
                    matches.append(OPS[condition["op"]](value, condition["value"]) if valid else None)
                    margin = alert.get("hysteresis", 0)
                    threshold = condition["value"] - margin if condition["op"] in {"gt", "gte"} else condition["value"] + margin
                    resets.append(not OPS[condition["op"]](value, threshold) if valid else None)
                # Unknown data cannot trigger or rearm an alert.
                matched = (all(m is True for m in matches) if alert["match"] == "all" else any(m is True for m in matches))
                definitely_false = (any(m is True for m in resets) if alert["match"] == "all" else all(m is True for m in resets))
                if definitely_false and alert["latched"]:
                    alert["latched"] = False
                    changed = True
                if matched:
                    if alert.get("candidate_since") is None:
                        alert["candidate_since"] = now
                        changed = True
                elif alert.get("candidate_since") is not None:
                    # Missing data breaks a sustained-condition proof too.
                    alert["candidate_since"] = None
                    changed = True
                ready = alert["last_trigger_at"] is None or now - alert["last_trigger_at"] >= alert["cooldown_seconds"]
                sustained = matched and now - alert["candidate_since"] >= alert.get("sustain_seconds", 0)
                interrupt_ready = (alert.get("priority", "urgent") == "normal" or
                    now - state.get("last_interrupt_at", -1e20) >= alert.get("interrupt_min_interval_seconds", 0))
                if sustained and not alert["latched"] and ready and interrupt_ready:
                    alert.update(latched=True, last_trigger_at=now, trigger_count=alert["trigger_count"] + 1)
                    if not alert["repeat"]:
                        alert["status"] = "triggered"
                    fired.append({"id": uuid.uuid4().hex, "name": alert["name"], "reason": alert["reason"],
                                  "conditions": values, "match": alert["match"], "wall_time": now,
                                  "pause_strategies": alert["pause_strategies"], "priority": alert.get("priority", "urgent")})
                    changed = True
            if fired:
                urgent = [trigger for trigger in fired if trigger["priority"] == "urgent"]
                # At most one unacknowledged trigger per named alert. Keep latest evidence/count.
                for trigger in fired:
                    lane = "pending" if trigger["priority"] == "urgent" else "queued"
                    state[lane] = [t for t in state.get(lane, []) if t["name"] != trigger["name"]] + [trigger]
                if urgent:
                    state["generation"] += 1
                    state["last_interrupt_at"] = now
                    state["interrupted_plan"] = runtime.store.get("decision", trader)
                runtime.store.put("alerts_state", trader, state)
                if urgent:
                    runtime.invalidate_decision(trader)
                    self.signals.setdefault(trader, threading.Event()).set()
                    self.signals[trader] = threading.Event()
                    self.suspend(trader, urgent)
                for trigger in fired:
                    runtime.store.event(trader, "alert_triggered" if trigger["priority"] == "urgent" else "alert_queued", {**trigger, "generation": state["generation"]})
                if urgent:
                    runtime.wake_events[trader].set()
            elif changed:
                runtime.store.put("alerts_state", trader, state)
        self.flush_normal(trader)

    def flush_normal(self, trader):
        """Wake only between decisions; a normal notification never fences work."""
        runtime = self.runtime
        with runtime.lock(trader):
            state = self.state(trader)
            queued = state.get("queued", [])
            connection = runtime.store.get("framework_connection", trader) or {}
            if not queued or runtime.config(trader)["status"] != "running" or runtime.control(trader)["decision_id"] or connection.get("active_turn_id"):
                return
            ids = [trigger["id"] for trigger in queued]
            if ids != state.get("notification_ids"):
                state["notification_ids"] = ids
                runtime.store.put("alerts_state", trader, state)
                runtime.store.event(trader, "alert_notification", {"alerts": queued, "priority": "normal"})
                runtime.wake_events[trader].set()

    def acknowledge(self, trader, generation):
        state = self.state(trader)
        if state["generation"] == generation and (state["pending"] or state.get("queued")):
            state["pending"] = []
            state["queued"] = []
            state["notification_ids"] = []
            self.runtime.store.put("alerts_state", trader, state)

    def loop(self, trader, stop):
        from marketforge import MarketForgeError
        failed = False
        while not stop.wait(0.25):
            try:
                self.poll(trader)
                self.runtime.poll_risk_events(trader)
                if failed:
                    self.runtime.store.event(trader, "alert_monitor_recovered", {})
                failed = False
            except (OSError, ValueError, KeyError, TypeError, MarketForgeError, http.client.HTTPException) as exc:
                if not failed:
                    self.runtime.store.event(trader, "alert_monitor_error", {"type": type(exc).__name__})
                failed = True
