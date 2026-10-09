"""Model-free tools, decision fencing and market wakeups for external harnesses."""
import hashlib
import hmac
import json
import secrets
import time
import uuid

from .storage import encode
from .monitoring import Monitoring

READ_TOOLS = {"market_read", "web_search", "web_read", "strategy_read", "strategy_status", "strategy_analyze", "strategies", "alerts", "orders", "fills", "policy_status"}
WAKE_EVENTS = {"started", "alert_triggered", "alert_notification", "wait_expired", "account_wakeup", "dependencies_installed", "dependency_error", "business_wakeup", "decision_expired"}


class Fence:
    def __init__(self, service, trader, decision, generation):
        self.service, self.trader, self.decision, self.generation = service, trader, decision, generation

    def is_set(self):
        return not self.service.lease_live(self.trader, self.decision, self.generation)


class ExternalTools(Monitoring):
    def control(self, trader):
        return self.store.get("external_control", trader, {"decision_id": None, "scheduled_wake": None})

    def invalidate_decision(self, trader):
        with self.lock(trader):
            self.store.put("external_control", trader, {"decision_id": None, "scheduled_wake": None, "expires_at": None, "connection_id": None})

    def issue_access(self, trader):
        if self.config(trader).get("backend") != "external":
            raise ValueError("tool access requires an external trader")
        token = secrets.token_urlsafe(32)
        with self.lock(trader):
            self.invalidate_decision(trader)
            self.store.put("tool_access", trader, hashlib.sha256(token.encode()).hexdigest())
            self.store.event(trader, "tool_access_rotated", {})
        return {"trader": trader, "token": token}

    def migrate_external(self, trader):
        with self.lock(trader):
            config = self.config(trader)
            workers = self.workers.get(trader)
            if config["status"] == "running" or (workers and any(t.is_alive() for t in workers[1])):
                raise ValueError("pause the trader and wait for its workers to stop before changing backend")
            config.update(backend="external", connection=None, plugin_id=None)
            self.invalidate_decision(trader)
            self.store.put("trader", trader, config)
            self.store.event(trader, "backend_changed", {"backend": "external", "account_id": config["account_id"]})
            return config

    def authorized(self, trader, token):
        digest = self.store.get("tool_access", trader)
        return digest is not None and hmac.compare_digest(digest, hashlib.sha256(token.encode()).hexdigest())

    def tool_schemas(self):
        from .runtime import TOOLS, schema, S, I
        tools = [schema("context", "Read your scope, fresh market/account data, generation and pending interrupts. Never grants execution permission.", {}),
                 schema("decision_begin", "Begin/reassess a decision at the generation returned by context. Returns decision_id; supersedes your previous decision, resolves unknown order receipts, acknowledges delivered alerts. Continue, revise or abandon the previous plan using fresh context.", {"generation": I, "plan": S}, ["generation"]),
                 schema("receipt", "Read the durable result of your own request_id, including pending unknown outcomes. Retry an unknown trade with exactly its original arguments/request_id.", {"request_id": S}, ["request_id"])]
        for tool in TOOLS:
            tool = json.loads(encode(tool))
            function = tool["function"]
            spec = function["parameters"]
            spec["properties"]["request_id"] = S
            if function["name"] not in READ_TOOLS:
                spec["properties"].update(decision_id=S, generation=I)
                spec["required"] += ["request_id", "decision_id", "generation"]
                function["description"] += " Requires current decision_id/generation from decision_begin; use a fresh request_id for a new intent and the original ID for retries."
            if function["name"] == "wait":
                function["description"] = "Schedule a business wakeup in 2-300 wall-clock seconds or earlier on account changes/alerts. Ends this decision lease; finish your framework turn after calling. Background strategies keep running."
            tools.append(tool)
        return tools

    def context(self, trader):
        for _ in range(3):
            config = self.config(trader)
            generation = self.alerts.state(trader)["generation"]
            observed = self.observations(config)
            with self.lock(trader):
                alerts = self.alerts.state(trader)
                if generation != alerts["generation"] or config["status"] != self.config(trader)["status"]:
                    continue
                self.store.put("last_observation", trader, {"at": time.time(), "markets": {
                    instrument: {"market_time_ms": value.get("market_time_ms"), "step": value.get("step")}
                    for instrument, value in observed.items()}})
                return {"trader": trader, "room": config["room"], "account_id": config["account_id"],
                    "instruments": config["instruments"], "status": config["status"], "goal": config["prompt"],
                    "generation": alerts["generation"], "interrupts": alerts["pending"],
                    "notifications": alerts.get("queued", []), "policy": self.policy_status(trader),
                    "previous_plan": self.store.get("decision", trader), "notebook": self.store.get("note", trader, ""),
                    "observations": observed, "alerts": alerts["alerts"], "strategies": self.strategies(trader),
                    "limits": {"max_order_qty": config["max_order_qty"], "orders_per_minute": config["orders_per_minute"]},
                    "runtime": self.runtime_status(trader)}

        raise ValueError("decision context changed during observation; retry context")

    def begin_decision(self, trader, args, connection_id=None):
        from .runtime import integer, text_value
        generation = integer(args["generation"], 0, 2**53-1)
        plan = text_value(args.get("plan", ""), 4000)
        context = self.context(trader)
        with self.lock(trader):
            config = self.config(trader)
            if config.get("backend") != "external" or config["status"] != "running":
                raise ValueError("external trader must be running")
            if not self.connection_live(trader):
                raise ValueError("framework is offline; reconnect before beginning a decision")
            connection = self.store.get("framework_connection", trader)
            if connection and connection["connection_id"] != connection_id:
                raise ValueError("framework transport has been superseded; reconnect before beginning a decision")
            if generation != self.alerts.state(trader)["generation"] or generation != context["generation"]:
                raise ValueError("decision generation is stale; read context and reassess")
            # Keep existing identities even when a previous lease has been revoked.
            pending = [p for p in self.store.pending(trader) if p["name"] in {"trade", "order_cancel_all"}]
            for call in pending:
                data = json.loads(call["args"])
                if call["name"] == "order_cancel_all":
                    self.finish_call(config, call["id"], call["name"], data["input"], data["source"], stop=Fence(self, trader, "revoked", generation))
                else:
                    if self.store.get("exchange_intent", f"{trader}:{call['id']}") is None:
                        if not self.store.get("exchange_reservation", f"{trader}:{call['id']}"):
                            from .runtime import UncertainOutcome
                            raise UncertainOutcome("pending order predates submission tracking; reconcile its exchange receipt before resuming")
                        self.store.finish(trader, call["id"], {"error": "old request was not submitted; reassess and use a new request_id"})
                    else:
                        self.call(trader, call["id"], call["name"], data["input"], data["source"])
            if pending:
                context["observations"] = self.observations(config)
                context["strategies"] = self.strategies(trader)
            decision = uuid.uuid4().hex
            expires = time.time() + config.get("decision_lease_seconds", 120)
            connection = self.store.get("framework_connection", trader)
            self.store.put("external_control", trader, {"decision_id": decision, "scheduled_wake": None,
                "expires_at": expires, "connection_id": connection["connection_id"] if connection else None})
            self.store.put("decision", trader, {"round": decision, "statement": plan, "recent_context": [], "remaining_actions": []})
            self.alerts.acknowledge(trader, generation)
            self.wake_events[trader].clear()
            self.store.event(trader, "decision_started", {"decision_id": decision, "generation": generation, "plan": plan, "expires_at": expires})
            return context | {"decision_id": decision, "decision_expires_at": expires}

    def external_call(self, trader, name, arguments, connection_id=None):
        self.config(trader)
        self.record_tool_activity(trader, name, arguments, "running")
        try:
            result = self._external_call(trader, name, arguments, connection_id)
        except Exception:
            self.record_tool_activity(trader, name, arguments, "error")
            raise
        self.record_tool_activity(trader, name, arguments, "error" if isinstance(result, dict) and "error" in result else "done")
        return result

    def _external_call(self, trader, name, arguments, connection_id=None):
        from .runtime import TOOL_FIELDS, identifier, integer
        if self.config(trader).get("backend") != "external":
            raise ValueError("external tool calls require an external trader")
        if name == "context":
            if arguments:
                raise ValueError("context takes no arguments")
            return self.context(trader)
        if name == "decision_begin":
            if set(arguments) - {"generation", "plan"}:
                raise ValueError("unexpected decision fields")
            return self.begin_decision(trader, arguments, connection_id)
        if name == "receipt":
            if set(arguments) != {"request_id"}:
                raise ValueError("receipt requires only request_id")
            return self.store.receipt(trader, "external:" + identifier(arguments["request_id"]))
        if name not in TOOL_FIELDS:
            raise ValueError("unknown tool")
        args = dict(arguments)
        request_id = identifier(args.pop("request_id", uuid.uuid4().hex))
        key = "external:" + request_id
        decision, generation = args.pop("decision_id", None), args.pop("generation", None)
        if name == "order_cancel_all":
            with self.lock(trader):
                prior = self.store.call_record(trader, key)
                if prior and (prior["name"] != name or prior["args"] != encode({"input": args, "source": "direct"})):
                    raise ValueError("request_id reused with different arguments")
            if prior and prior["status"] == "pending":
                return self.finish_call(self.config(trader), key, name, args, "direct", stop=Fence(self, trader, decision, generation))
        with self.lock(trader):
            prior = self.store.call_record(trader, key)
            if prior:
                if prior["name"] != name or prior["args"] != encode({"input": args, "source": "direct"}):
                    raise ValueError("request_id reused with different arguments")
                if prior["status"] == "done":
                    return json.loads(prior["result"])
                if name == "trade":
                    if self.store.get("exchange_intent", f"{trader}:{key}") is None and Fence(self, trader, decision, generation).is_set():
                        if not self.store.get("exchange_reservation", f"{trader}:{key}"):
                            from .runtime import UncertainOutcome
                            raise UncertainOutcome("pending order predates submission tracking; reconcile its exchange receipt before resuming")
                        result = {"error": "old request was not submitted; reassess and use a new request_id"}
                        self.store.finish(trader, key, result)
                        return result
                    return self.call(trader, key, name, args)  # Settle only already-submitted intents.
            if name not in READ_TOOLS:
                if "request_id" not in arguments:
                    raise ValueError("new action requires a request_id")
                integer(generation, 0, 2**53-1)
                if not isinstance(decision, str) or Fence(self, trader, decision, generation).is_set():
                    raise ValueError("decision lease is stale or paused; read context and begin a new decision")
            fence = Fence(self, trader, decision, generation) if name not in READ_TOOLS else None
            # call() releases its own lane for slow work; leave our outer lane too.
            slow = name in ("web_read", "web_search", "strategy_test", "strategy_analyze", "order_cancel_all")
            if not slow:
                result = self.call(trader, key, name, args, stop=fence)
                if name == "wait" and "seconds" in result:
                    self.store.put("external_control", trader, {"decision_id": None, "scheduled_wake": {
                        "at": time.time() + result["seconds"], "fingerprint": self.store.get("wake", trader)}})
                return result
        return self.call(trader, key, name, args, stop=fence)

    def external_wake_loop(self, trader, stop):
        from marketforge import MarketForgeError
        try:
            while not stop.wait(0.25):
                self.check_liveness(trader)
                wake = self.control(trader)["scheduled_wake"]
                reason = None
                if wake:
                    if time.time() >= wake["at"]:
                        reason = "wait_expired"
                    elif wake["fingerprint"] and self.account_fingerprint(self.config(trader)) != wake["fingerprint"]:
                        reason = "account_wakeup"
                with self.lock(trader):
                    if stop.is_set():
                        return
                    if wake and self.control(trader)["scheduled_wake"] == wake and reason:
                        self.invalidate_decision(trader)
                        self.store.event(trader, reason, {})
                    if self.wake_events[trader].is_set():
                        self.wake_events[trader].clear()
                        # Alerts and installation already have durable wake events.
        except (OSError, ValueError, MarketForgeError) as exc:
            self.fail(trader, stop, exc)
