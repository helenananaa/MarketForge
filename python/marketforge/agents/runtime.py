"""Independent agent.v1 sessions. All money and matching remain in the exchange."""
import hashlib
import http.client
import importlib.util
import json
import os
import re
import threading
import time
import urllib.error
import urllib.parse
import uuid
from pathlib import Path

from marketforge import Client, MarketForgeError
from .storage import Store, encode
from .sandbox import DockerSandbox
from .projects import normalize_project, project_version
from .research import Research
from .alerts import Alerts, CONDITION_SCHEMA
from .external import ExternalTools
from .orders import OrderTools
from .policies import Policies
from .market_data import MarketDataTools, indicators, render_chart
from .workspace import Workspaces
from .advanced_tools import AdvancedTools


class UncertainOutcome(RuntimeError):
    """A submitted request may have committed; retain its durable retry identity."""


class DecisionInterrupted(Exception):
    """This decision no longer has permission to execute its remaining plan."""


def integer(value, low, high):
    if type(value) is not int or not low <= value <= high:
        raise ValueError(f"expected integer in [{low}, {high}]")
    return value


def identifier(value):
    if not isinstance(value, str) or not re.fullmatch(r"[a-zA-Z0-9_-]{1,64}", value):
        raise ValueError("identifier must contain 1-64 letters, digits, underscores or hyphens")
    return value


def text_value(value, limit):
    if not isinstance(value, str) or len(value) > limit:
        raise ValueError(f"expected text up to {limit} characters")
    return value


def schema(name, description, properties, required=()):
    return {"type": "function", "function": {"name": name, "description": description,
            "parameters": {"type": "object", "properties": properties, "required": list(required), "additionalProperties": False}}}


S = {"type": "string"}
I = {"type": "integer"}
TOOLS = [
    schema("alert_set", "Set/replace a named alert over your allowed markets/account. priority=urgent (default) immediately fences the old decision; priority=normal queues notifications for the next decision without interrupting work and cannot pause strategies. Checked every 250ms plus read latency; short crossings between samples may be missed. sustain_seconds requires continuously matching samples; missing data breaks the proof. hysteresis is a threshold rearm margin for gt/gte/lt/lte only. interrupt_min_interval_seconds spaces urgent generations across this trader; a still-matching condition is checked later. Default one-shot; repeat also requires a false condition and cooldown. Urgent alerts hold unsent actions, retain the plan and wake with fresh data: continue, revise or abandon. Strategies remain deployed by default; pause_strategies=true suspends them. Existing orders/fills remain. Prices are ticks, balances native money units; market_time_ms is simulation time. sustain/cooldown/spacing use wall-clock seconds.",
           {"name": S, "conditions": {"type": "array", "minItems": 1, "maxItems": 8, "items": CONDITION_SCHEMA},
            "match": {"type": "string", "enum": ["all", "any"]}, "repeat": {"type": "boolean"},
            "cooldown_seconds": I, "pause_strategies": {"type": "boolean"}, "reason": S,
            "priority": {"type": "string", "enum": ["normal", "urgent"]}, "sustain_seconds": I,
            "hysteresis": I, "interrupt_min_interval_seconds": I}, ["name", "conditions"]),
    schema("alerts", "List your persistent interrupt conditions and armed/triggered/cancelled state.", {}),
    schema("alert_cancel", "Cancel future checks for a named alert. Already triggered interrupts still require replanning.", {"name": S}, ["name"]),
    schema("market_read", "Read public market data or your own account/orders. Each result has its own time; cross-market reads are not atomic.",
           {"instrument": S, "kind": {"type": "string", "enum": ["observe", "ticker", "candles"]}, "interval_ms": I}, ["instrument", "kind"]),
    schema("trade", "Place/cancel/amend an order against your own account. amend uses order_id and price_tick and/or qty (new remaining quantity). The exchange permits quantity reduction and less aggressive repricing only; increasing size or aggression needs explicit cancel plus a new intent. Inspect OrderAmended/AmendRejected events: accepted=true means command admission, not a successful amendment or fill. execution_mode defaults to bounded: price_tick is maximum buy/minimum sell price; market/reduce_only use bounded IOC. Explicit execution_mode=unbounded is allowed only for market/reduce_only and must omit price_tick; it sweeps available liquidity without a price bound. Cash, margin, ownership, quantity/rate limits and enabled optional policies still apply. Optional valid_until_market_time_ms is an absolute decision deadline from observed simulation time, checked by the exchange in either mode. expires_at_market_time_ms expires only limit/post_only resting orders. Never change an old intent's mode or deadline on retry.",
           {"instrument": S, "action": {"type": "string", "enum": ["limit", "market", "ioc", "post_only", "reduce_only", "cancel", "amend"]},
            "side": {"type": "string", "enum": ["Buy", "Sell"]}, "price_tick": I, "qty": I, "order_id": {"type":["integer","string"]},
            "position_side": {"type": "string", "enum": ["Both", "Long", "Short"]},
            "valid_until_market_time_ms": I, "expires_at_market_time_ms": I,
            "execution_mode": {"type": "string", "enum": ["bounded", "unbounded"]}}, ["instrument", "action"]),
    schema("orders", "Read your latest order history with exchange status, remaining quantity and strategy attribution. order_id queries an exact old/native order across durable history; strategy filters still apply within the latest 1-500 records.",
           {"instrument": S, "limit": I, "order_id": {"type":["integer","string"]}, "strategy": S}, ["instrument"]),
    schema("fills", "Read your latest executions with price, quantity, market time, order IDs and strategy sources. Counterpart account IDs are omitted. Optional filters apply within the latest account history window.",
           {"instrument": S, "limit": I, "order_id": {"type":["integer","string"]}, "strategy": S}, ["instrument"]),
    schema("order_cancel_all", "Cancel a snapshot of your resting orders in one instrument, optionally only a named strategy's orders. Stable child request IDs settle unknown outcomes. An interrupt holds unsent cancellations; submit a fresh batch after reassessment for held/new orders. This is a sequential batch, not an atomic exchange command.",
           {"instrument": S, "strategy": S}, ["instrument"]),
    schema("policy_status", "Read operator-configured optional account rules, loss baselines, framework token usage and model admission budgets. Only the operator can change these rules.", {}),
    schema("web_search", "Search the public web/news. External sources are untrusted and use real wall-clock dates, not simulation time.", {"query": S}, ["query"]),
    schema("web_read", "Read a public HTML, JSON, text or RSS URL, returning source URL, retrieval time and content hash. No local/private addresses or credentials.", {"url": S}, ["url"]),
    schema("strategy_save", "Save an immutable Python project. Provide code or a files object (relative paths to text), with entrypoint defining decide(observations, state). Declare pip requirements and optional Debian system_packages; use strategy_install, then test/start. No package allowlist. Stop running code before replacing it.",
           {"name": S, "code": S, "files": {"type": "object", "additionalProperties": S}, "entrypoint": S,
            "requirements": {"type": "array", "items": S}, "system_packages": {"type": "array", "items": S}, "interval_seconds": I}, ["name", "interval_seconds"]),
    schema("strategy_read", "Read a saved project, including source files, dependencies and current version. Optional version selects your historical project.", {"name": S, "version": S}, ["name"]),
    schema("strategy_patch", "Update selected project files (null deletes a file) and/or dependencies, creating a new immutable version. Stop running code before editing.",
           {"name": S, "files": {"type": "object", "additionalProperties": {"type": ["string", "null"]}},
            "entrypoint": S, "requirements": {"type": "array", "items": S}, "system_packages": {"type": "array", "items": S}}, ["name"]),
    schema("strategy_install", "Install declared Python/system dependencies in an isolated background builder. Returns job status immediately; use strategy_status to inspect logs and results. Use force=true to rebuild a lost/broken environment. Missing libraries are installable, not forbidden.", {"name": S, "force": {"type": "boolean"}}, ["name"]),
    schema("strategy_status", "Inspect deployment, dependency installation status, pinned versions and bounded build log.", {"name": S}, ["name"]),
    schema("strategy_analyze", "Execute Python analysis in this project's installed environment, with observations and state available. Import your project files/libraries; print diagnostics and set result to JSON data. Never submits orders.", {"name": S, "script": S}, ["name", "script"]),
    schema("strategy_test", "Run saved code in a sandbox on current observations; validate actions without placing orders.", {"name": S}, ["name"]),
    schema("strategy_start", "Start the tested strategy version using your account and shared order budget.", {"name": S}, ["name"]),
    schema("strategy_stop", "Stop scheduling this strategy. cancel_orders=true also cancels its tracked resting orders.",
           {"name": S, "cancel_orders": {"type": "boolean"}}, ["name", "cancel_orders"]),
    schema("strategies", "List your code versions, state and deployment status.", {}),
    schema("note", "Replace your durable private notebook (up to 12000 characters).", {"text": S}, ["text"]),
    schema("announce", "Publish a short trading statement for viewers. Do not disclose hidden reasoning.", {"text": S}, ["text"]),
    schema("wait", "Finish this decision round and wake after 2-300 seconds, or earlier on your own account/order change when on_account_change=true. Deployed strategies keep running.",
           {"seconds": I, "on_account_change": {"type": "boolean"}}, ["seconds"]),
]
DATA_FIELDS = {"instrument": S, "interval_ms": I, "limit": I, "before_open_time_ms": I, "after_open_time_ms": I}
TOOLS += [schema("market_history", "Read up to 2000 traded bars with explicit simulation timestamps and exclusive history cursors. Empty periods are omitted.", DATA_FIELDS, ["instrument"]),
    schema("market_indicators", "Compute window-local SMA, EMA, Wilder RSI/ATR and VWAP from the same source bars. Null means insufficient warmup; includes bars and provenance.", {**DATA_FIELDS, "period": I}, ["instrument"]),
    schema("chart_export", "Return a PNG candlestick/volume chart with SMA/EMA from public market bars. Headless render, not a screenshot of the human workspace. MCP returns native image content.", {**DATA_FIELDS, "period": I, "width": I, "height": I}, ["instrument"]),
    schema("risk_events", "Read only your durable margin, liquidation and funding events. Use next_after_command_seq even for empty pages; has_more requests the next page. Last page by default; from_start=true starts history.", {"instrument":S,"after_command_seq":I,"from_start":{"type":"boolean"},"limit":I}, ["instrument"])]
trade_spec = next(t["function"]["parameters"] for t in TOOLS if t["function"]["name"] == "trade")
trade_spec["properties"]["action"]["enum"] += ["bracket", "protection"]
trade_spec["properties"].update(take_profit_tick=I, stop_loss_tick=I, trigger={"type":"string","enum":["Mark","Last"]})
next(t["function"] for t in TOOLS if t["function"]["name"] == "trade")["description"] += " action=bracket atomically opens a perpetual position with TP/SL: price_tick submits a resting limit entry; omit price only with execution_mode=unbounded for a market entry. action=protection sets/replaces the selected position leg's full TP/SL spec; omit both TP/SL to remove. trigger=Mark (default) or Last. Triggered exits are exchange-owned unbounded reduce-only IOC and retry remainder on market mutations; execution price is not guaranteed."
next(t["function"]["parameters"]["properties"] for t in TOOLS if t["function"]["name"] == "strategy_save")["market_data"] = {"type":"object","additionalProperties":False,"properties":{"interval_ms":I,"limit":I},"required":["interval_ms","limit"]}
TOOLS += [
    schema("workspace_start", "Start/resume your persistent Docker workbench. Full shell, Python, internet and background jobs inside your container; named-volume files survive stops. No host filesystem/socket or operator credentials. Programs use marketforge_program.Client for scoped trading and resumable events.", {}),
    schema("workspace_exec", "Run arbitrary /bin/sh command in your Docker workspace. Returns durable job_id, bounded merged output and exit status; long jobs continue in background. Install Python packages with python -m pip install --user. Retry the same request_id to avoid duplicate launches.", {"command":S,"cwd":S,"wait_seconds":I},["command"]),
    schema("workspace_process", "Read a workspace job's output by byte offset; stop=true terminates its process group. Poll fresh request IDs, preserving next_offset.", {"job_id":S,"offset":I,"stop":{"type":"boolean"}},["job_id"]),
    schema("workspace_write", "Write a UTF-8 file in your persistent /work directory, including arbitrary Python programs.", {"path":S,"text":S},["path","text"]),
    schema("workspace_read", "Read a UTF-8 file (up to 256 KiB) or list a directory within /work.", {"path":S},["path"]),
    schema("workspace_stop", "Stop your workspace and all its processes; retain its files. Does not cancel exchange orders.", {}),
    schema("account_history", "Page your complete durable execution/activity history with own order lifecycle, fills, exact fees, realized PnL, funding and liquidation. Cursor advances across empty filtered pages. include_market also includes public trades and price updates; never other accounts. order_id filters only matching activity within each scanned page.", {"instrument":S,"after_command_seq":I,"from_start":{"type":"boolean"},"limit":I,"order_id":S,"include_market":{"type":"boolean"}},["instrument"]),
    schema("market_rules", "Read instrument units, tick/lot sizes, fees, margin configuration, supported order actions and venue rules.", {"instrument":S},["instrument"]),
    schema("portfolio", "Read only your account's portfolio and venue balances.", {}),
    schema("ledger", "Page your complete settlement activity; same durable account history cursor as account_history.", {"instrument":S,"after_command_seq":I,"from_start":{"type":"boolean"},"limit":I},["instrument"]),
    schema("indicator_catalog", "Read the same CandleScope builtin indicator and script-runtime catalog used by the human workbench.", {}),
    schema("indicator_compute", "Run a CandleScope builtin or Pine/Pyne script on authoritative traded bars, returning structured lines/drawings and provenance. Requires the configured CandleScope analysis service. securityMode is safe.", {**DATA_FIELDS,"name":S,"language":{"type":"string","enum":["pine","pyne"]},"script":S,"params":{"type":"object","additionalProperties":True}},["instrument"]),
]
trade_spec["properties"]["action"]["enum"] += ["fok","reduce_only_fok","reduce_only_limit","reduce_only_post_only"]
PROTECTION_EXTRA = {"trailing_distance_tick":I,"exit_price_tick":I,"exit_qty":I,
    "take_profit_steps":{"type":"array","maxItems":16,"items":{"type":"object","additionalProperties":False,"properties":{"price_tick":I,"qty":I},"required":["price_tick","qty"]}}}
trade_spec["properties"].update(PROTECTION_EXTRA)
trade_spec["properties"].update(conditional_key=S,conditional_spec={"type":["object","null"],"additionalProperties":False,
    "properties":{"side":{"type":"string","enum":["Buy","Sell"]},"position_side":{"type":"string","enum":["Both","Long","Short"]},
        "qty":I,"trigger_price_tick":I,"above":{"type":"boolean"},"trigger":{"type":"string","enum":["Mark","Last"]},"limit_price_tick":I,
        "protection":{"type":"object","additionalProperties":False,"properties":{"take_profit_tick":I,"stop_loss_tick":I,"trigger":{"type":"string","enum":["Mark","Last"]},**PROTECTION_EXTRA}}},
    "required":["side","qty","trigger_price_tick","above"]})
trade_spec["properties"]["action"]["enum"].append('conditional')
TOOLS.append(schema('conditional_orders','Read your exchange-owned conditional entries and their submitted/rejected state.',{'instrument':S},['instrument']))
next(t['function']['parameters']['properties'] for t in TOOLS if t['function']['name']=='chart_export')['indicator']={"type":"object","additionalProperties":False,"properties":{"name":S,"script":S,"language":{"type":"string","enum":["pine","pyne"]},"params":{"type":"object","additionalProperties":True}}}
TOOL_FIELDS = {t["function"]["name"]: t["function"]["parameters"] for t in TOOLS}


def load_plugins(root):
    plugins = {}
    for manifest in sorted(Path(root).glob("*/agent.json")):
        data = json.loads(manifest.read_text(encoding="utf-8"))
        if data.get("protocol_version") != "agent.v1" or data["id"] in plugins:
            raise ValueError("unsupported or duplicate agent plugin")
        entry = (manifest.parent / data["entrypoint"]).resolve()
        if not entry.is_relative_to(manifest.parent.resolve()):
            raise ValueError("plugin entrypoint escapes its installed directory")
        spec = importlib.util.spec_from_file_location("mf_agent_" + uuid.uuid4().hex, entry)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        if not callable(getattr(module, "complete", None)):
            raise ValueError("agent plugin must export complete(connection, messages, tools)")
        plugins[data["id"]] = (data, module)
    return plugins


class TradingService(Workspaces, AdvancedTools, MarketDataTools, OrderTools, Policies, ExternalTools):
    def __init__(self, data_dir, plugin_dir, exchange_url, sandbox=None, client_factory=None):
        self.store = Store(Path(data_dir) / "agents.sqlite3")
        self.plugins = load_plugins(plugin_dir) if plugin_dir else {}
        self.exchange_url = exchange_url
        self.sandbox = sandbox or DockerSandbox()
        self.client_factory = client_factory
        self.connections = {}  # API keys never enter the database, logs or strategies.
        self.locks = {}
        self.workers = {}
        self.installers = {}
        self.wake_events = {}
        self.research = Research()
        self.guard = threading.RLock()
        self.alerts = Alerts(self)
        self.requests = {}
        self.workspace_instances = {}
        self.workspace_relays = {}
        for trader in self.store.all("trader"):
            self.wake_events[trader["id"]] = threading.Event()
            trader.update(status="paused", error="Service restarted; explicitly resume the trader." if trader.get("backend") == "external" else "Service restarted; reconnect model and explicitly resume.")
            self.store.put("trader", trader["id"], trader)
            self.invalidate_decision(trader["id"])
            alert_state = self.alerts.state(trader["id"])
            for alert in alert_state["alerts"]:
                alert["candidate_since"] = None
            self.store.put("alerts_state", trader["id"], alert_state)
            connection = self.store.get("framework_connection", trader["id"])
            if connection:
                connection.update(state="offline", expires_at=0, error_code="service_restarted", active_turn_id=None)
                self.store.put("framework_connection", trader["id"], connection)
        for job in self.store.all("install_job"):
            if job["status"] in ("queued", "running"):
                job.update(status="interrupted", error="Service restarted; call strategy_install to retry.")
                self.store.put("install_job", job["id"], job)

    def lock(self, trader):
        existing = self.locks.get(trader)
        if existing is not None:
            return existing
        with self.guard:
            return self.locks.setdefault(trader, threading.RLock())

    def config(self, trader):
        value = self.store.get("trader", trader)
        if value is None:
            raise ValueError("unknown trader")
        return value

    def connect(self, args):
        key = identifier(args["id"])
        plugin = args.get("plugin_id", "marketforge.llm-trader")
        if plugin not in self.plugins:
            raise ValueError("unknown installed agent plugin")
        connection = {"base_url": text_value(args["base_url"], 1000), "model": text_value(args["model"], 200),
                      "api_key": text_value(args.get("api_key", ""), 4096), "plugin_id": plugin}
        # Use an actual completion to check auth/model access, not just /models.
        message, _ = self.plugins[plugin][1].complete(connection, [{"role": "user", "content": "Reply OK."}], [])
        with self.guard:
            if any(t["connection"] == key and t["status"] == "running" for t in self.store.all("trader")):
                raise ValueError("pause traders before replacing their model connection")
            self.connections[key] = connection
        return {"id": key, "model": connection["model"], "connected": True}

    def create(self, args):
        trader = identifier(args["id"])
        with self.guard, self.lock(trader):
            if self.store.get("trader", trader):
                raise ValueError("trader already exists; existing identity cannot be overwritten")
            instruments = args["instruments"]
            if not isinstance(instruments, list) or not 1 <= len(instruments) <= 8 or len(set(instruments)) != len(instruments):
                raise ValueError("choose 1-8 distinct instruments")
            for instrument in instruments:
                text_value(instrument, 128)
                if not instrument:
                    raise ValueError("empty instrument")
            backend = args.get("backend", "legacy" if self.plugins else "external")
            if backend not in ("external", "legacy"):
                raise ValueError("backend must be external or legacy")
            connection = identifier(args["connection"]) if backend == "legacy" else None
            if backend == "legacy" and connection not in self.connections:
                raise ValueError("test and connect the model first")
            token_env = args.get("exchange_token_env", "")
            if token_env and not re.fullmatch(r"MARKETFORGE_TRADER_TOKEN_[A-Z0-9_]+", token_env):
                raise ValueError("exchange token must reference MARKETFORGE_TRADER_TOKEN_* environment")
            config = {"id": trader, "room": text_value(args["room"], 128), "account_id": integer(args["account_id"], 1, 2**53-1),
                      "instruments": instruments, "backend": backend, "connection": connection,
                      "plugin_id": self.connections[connection]["plugin_id"] if backend == "legacy" else None,
                      "prompt": text_value(args.get("prompt", "Trade freely and manage your risk."), 8000),
                      "interval_seconds": integer(args.get("interval_seconds", 15), 2, 300),
                      "max_model_calls": integer(args.get("max_model_calls", 100), 1, 10000),
                      "max_order_qty": integer(args.get("max_order_qty", 100), 1, 10**9),
                      "orders_per_minute": integer(args.get("orders_per_minute", 30), 1, 300),
                      "decision_lease_seconds": integer(args.get("decision_lease_seconds", 120), 10, 600),
                      "exchange_token_env": token_env, "status": "paused", "error": None, "model_calls": 0}
            if any(t["room"] == config["room"] and t["account_id"] == config["account_id"] for t in self.store.all("trader")):
                raise ValueError("account already belongs to another AI trader")
            self.store.put("trader", trader, config)
            self.wake_events[trader] = threading.Event()
            self.store.event(trader, "created", config)
            return config

    def client(self, config):
        if self.client_factory:
            return self.client_factory(config)
        token = os.environ.get(config["exchange_token_env"]) if config["exchange_token_env"] else None
        if config["exchange_token_env"] and not token:
            raise ValueError("configured exchange bearer environment variable is missing")
        return Client(self.exchange_url, bearer=token, user_id="agent-" + config["id"], timeout=10)

    def observation(self, config, instrument):
        self.check_instrument(config, instrument)
        data = self.client(config).observe(urllib.parse.quote(config["room"], safe=""), config["account_id"], instrument)
        return data.get("observation", data)

    def observations(self, config):
        return {instrument: self.observation(config, instrument) for instrument in config["instruments"]}

    @staticmethod
    def check_instrument(config, instrument):
        if instrument not in config["instruments"]:
            raise ValueError("instrument is outside this trader's scope")

    def validate_trade(self, config, args, source):
        if not isinstance(args, dict):
            raise ValueError("strategy action must be an object")
        self.check_instrument(config, args.get("instrument"))
        action = args.get("action")
        if action=='conditional':
            if set(args)-{'instrument','action','conditional_key','conditional_spec'}: raise ValueError('unexpected conditional fields')
            key=identifier(args['conditional_key']); spec=args.get('conditional_spec')
            if spec is not None:
                if not isinstance(spec,dict) or set(spec)-{'side','position_side','qty','trigger_price_tick','above','trigger','limit_price_tick','protection'}: raise ValueError('invalid conditional specification')
                if spec.get('side') not in ('Buy','Sell') or type(spec.get('above')) is not bool: raise ValueError('invalid conditional side or direction')
                spec=dict(spec);spec['qty']=integer(spec.get('qty'),1,config['max_order_qty']);spec['trigger_price_tick']=integer(spec.get('trigger_price_tick'),1,2**53-1)
                if self.policy(config['id'])['account']: raise ValueError('native conditional entries require account policies to be disabled; use a workspace program for policy-checked entries')
            return {'SetConditional':{'key':key,'spec':spec}}
        allowed = {"instrument", "action", "order_id"} if action == "cancel" else {"instrument", "action", "position_side", "take_profit_tick", "stop_loss_tick", "trigger"} if action == "protection" else {"instrument", "action", "order_id", "price_tick", "qty"} if action == "amend" else {
            "instrument", "action", "qty", "side", "position_side", "price_tick", "valid_until_market_time_ms", "expires_at_market_time_ms", "execution_mode", "take_profit_tick", "stop_loss_tick", "trigger"}
        if action in ('protection','bracket'): allowed |= set(PROTECTION_EXTRA)
        if set(args) - allowed:
            raise ValueError("unexpected trade fields")
        if action == "cancel":
            order = order_identifier(args.get("order_id"))
            if source not in ("direct", "workspace") and self.store.get("order_owner", f"{config['id']}:{args['instrument']}:{order}") != source:
                raise ValueError("strategy can only cancel its own orders")
            return {"Cancel": {"order_id": order}}
        if action == "amend":
            order = order_identifier(args.get("order_id"))
            if "price_tick" not in args and "qty" not in args:
                raise ValueError("amend requires price_tick and/or new remaining qty")
            if source not in ("direct", "workspace") and self.store.get("order_owner", f"{config['id']}:{args['instrument']}:{order}") != source:
                raise ValueError("strategy can only amend its own orders")
            return {"Amend": {"order_id": order,
                "price_tick": integer(args["price_tick"], 1, 2**53-1) if "price_tick" in args else None,
                "qty": integer(args["qty"], 1, config["max_order_qty"]) if "qty" in args else None}}
        if action in ("bracket", "protection"):
            leg = args.get("position_side", "Both")
            if leg not in ("Both", "Long", "Short"): raise ValueError("invalid position_side")
            trigger = args.get("trigger", "Mark")
            if trigger not in ("Mark", "Last"): raise ValueError("trigger must be Mark or Last")
            spec = {field: integer(args[field],1,2**53-1) if field in args else None for field in ("take_profit_tick","stop_loss_tick")}
            spec["trigger"] = trigger
            for field in ('trailing_distance_tick','exit_price_tick','exit_qty'):
                if field in args: spec[field]=integer(args[field],1,config['max_order_qty'] if field=='exit_qty' else 2**53-1)
            if 'take_profit_steps' in args:
                steps=args['take_profit_steps']
                if not isinstance(steps,list) or not 1<=len(steps)<=16: raise ValueError('take_profit_steps requires 1-16 levels')
                spec['take_profit_steps']=[{'price_tick':integer(s['price_tick'],1,2**53-1),'qty':integer(s['qty'],1,config['max_order_qty'])} for s in steps]
            active = spec["take_profit_tick"] is not None or spec["stop_loss_tick"] is not None or spec.get('trailing_distance_tick') is not None or bool(spec.get('take_profit_steps'))
            if action == "protection": return {"SetPositionProtection": {"position_side":leg,"protection":spec if active else None}}
            if not active: raise ValueError("bracket requires TP and/or SL")
            if args.get("side") not in ("Buy","Sell"): raise ValueError("invalid bracket side")
            if "valid_until_market_time_ms" in args or "expires_at_market_time_ms" in args: raise ValueError("bracket entry deadlines are not supported; use ordinary protected orders")
            price = integer(args["price_tick"],1,2**53-1) if "price_tick" in args else None
            if price is None and args.get("execution_mode") != "unbounded": raise ValueError("market bracket requires explicit execution_mode=unbounded")
            if price is not None and args.get("execution_mode", "bounded") != "bounded": raise ValueError("limit bracket requires bounded mode")
            return {"PlaceBracket":{"side":args["side"],"position_side":leg,"qty":integer(args.get("qty"),1,config["max_order_qty"]),"price_tick":price,"protection":spec}}
        if any(field in args for field in ("take_profit_tick","stop_loss_tick","trigger",*PROTECTION_EXTRA)): raise ValueError("TP/SL fields require bracket or protection action")
        variants = {"fok":"PlaceFillOrKill", "reduce_only_fok":"PlaceReduceOnlyFillOrKill", "reduce_only_limit":"PlaceLimit", "reduce_only_post_only":"PlacePostOnly", "limit": "PlaceLimit", "market": "PlaceImmediateOrCancel", "ioc": "PlaceImmediateOrCancel",
                    "post_only": "PlacePostOnly", "reduce_only": "PlaceReduceOnlyImmediateOrCancel"}
        if action not in variants or args.get("side") not in ("Buy", "Sell"):
            raise ValueError("invalid order action or side")
        value = {"side": args["side"], "qty": integer(args.get("qty"), 1, config["max_order_qty"])}
        if "position_side" in args:
            if args["position_side"] not in ("Both", "Long", "Short"):
                raise ValueError("position_side must be Both, Long or Short")
            value["position_side"] = args["position_side"]
        mode = args.get("execution_mode", "bounded")
        if mode not in ("bounded", "unbounded"):
            raise ValueError("execution_mode must be bounded or unbounded")
        deadlines = {field: integer(args[field], 1, 2**53-1)
                     for field in ("valid_until_market_time_ms", "expires_at_market_time_ms") if field in args}
        if "expires_at_market_time_ms" in deadlines and action not in ("limit", "post_only", "reduce_only_limit", "reduce_only_post_only"):
            raise ValueError("only limit/post_only resting orders accept expires_at_market_time_ms")
        if mode == "unbounded":
            if action not in ("market", "reduce_only"):
                raise ValueError("unbounded execution is only supported for market/reduce_only")
            if "price_tick" in args:
                raise ValueError("unbounded execution must omit price_tick; use bounded mode to set a price limit")
            if deadlines or "position_side" in args:
                return {"PlaceUnboundedMarket": {**value, **deadlines, "reduce_only": action == "reduce_only"}}
            return {("PlaceReduceOnlyMarket" if action == "reduce_only" else "PlaceMarket"): value}
        if "price_tick" not in args:
            raise ValueError("price_tick is required in bounded mode: set a maximum buy/minimum sell price, or explicitly choose execution_mode=unbounded for market/reduce_only")
        value["price_tick"] = integer(args["price_tick"], 1, 2**53-1)
        if deadlines or "position_side" in args or action in ("reduce_only_limit","reduce_only_post_only"):
            return {"PlaceProtected": {**value, **deadlines, "reduce_only": action.startswith("reduce_only"),
                "order_type": {"limit": "Limit", "post_only": "PostOnly", "reduce_only_limit":"Limit", "reduce_only_post_only":"PostOnly", "fok":"FillOrKill", "reduce_only_fok":"FillOrKill"}.get(action, "ImmediateOrCancel")}}
        return {variants[action]: value}

    def call(self, trader, key, name, args, source="direct", stop=None):
        with self.lock(trader):
            if stop is not None and stop.is_set():
                return {"error": "session paused; action not executed"}
            config = self.config(trader)
            if name not in TOOL_FIELDS or not isinstance(args, dict):
                raise ValueError("unknown tool or invalid arguments")
            spec = TOOL_FIELDS[name]
            if set(args) - set(spec["properties"]) or set(spec["required"]) - set(args):
                raise ValueError("unexpected or missing tool arguments")
            receipt = self.store.reserve(trader, key, name, {"input": args, "source": source})
            if receipt and receipt["status"] == "done":
                return json.loads(receipt["result"])
            if (receipt and name == "trade" and args.get("action") in ("market", "reduce_only")
                    and "price_tick" not in args and args.get("execution_mode") != "unbounded"):
                raise UncertainOutcome("legacy unbounded order outcome is unresolved; inspect exchange receipts before resuming; do not replace or reprice the pending request")
            self.store.event(trader, "tool_request", {"call_id": key, "name": name, "args": args, "source": source})
            if name.startswith('workspace_') or name in ("indicator_compute", "indicator_catalog", "account_history", "ledger", "web_read", "web_search", "strategy_test", "strategy_analyze", "chart_export", "market_history", "market_indicators", "order_cancel_all"):
                slow = True
            else:
                return self.finish_call(config, key, name, args, source)
        # Research and code testing never hold the trading lane while waiting.
        if slow:
            return self.finish_call(config, key, name, args, source, stop=stop)

    def finish_call(self, config, key, name, args, source, stop=None):
        trader = config["id"]
        try:
            result = self.execute(config, key, name, args, source, stop=stop)
        except MarketForgeError as exc:
            if exc.status >= 500:
                raise  # Unknown order outcome stays pending with the same idempotency key.
            result = {"error": str(exc)}
        except (ValueError, KeyError, TypeError) as exc:
            result = {"error": str(exc)[:2000]}
        except (OSError, urllib.error.URLError, http.client.HTTPException) as exc:
            if name not in ("web_read", "web_search", "strategy_test", "strategy_analyze"):
                raise
            result = {"error": str(exc)[:2000]}
        self.store.finish(trader, key, result)
        self.store.event(trader, "tool_result", {"call_id": key, "name": name, "source": source, "result": result})
        with self.lock(trader):
            decision = self.store.get("decision", trader)
            if decision and any(a["call_id"] == key for a in decision["remaining_actions"]):
                decision["remaining_actions"] = [a for a in decision["remaining_actions"] if a["call_id"] != key]
                self.store.put("decision", trader, decision)
        return result

    def execute(self, config, key, name, args, source, stop=None):
        if name.startswith('workspace_'):
            return self.workspace_execute(config,key,name,args,stop)
        if name in ('conditional_orders','account_history','ledger','portfolio','market_rules','indicator_catalog','indicator_compute'):
            return self.advanced_execute(config,name,args)
        trader = config["id"]
        client = self.client(config)
        room = urllib.parse.quote(config["room"], safe="")
        if name in ("market_history", "market_indicators", "chart_export"):
            data = self.candle_data(config, args)
            if name == "market_history": return data
            study = indicators(data["candles"], integer(args.get("period",14),2,200))
            if name == "market_indicators": return {**data,"indicators":study}
            overlay=self.compute_indicator(data,{**args['indicator'],'instrument':args['instrument']})['indicator'] if 'indicator' in args else None
            return render_chart(data,study,args.get("width",1000),args.get("height",600),overlay=overlay)
        if name == "risk_events": return self.read_risk_events(config,args)
        if name == "market_read":
            instrument = args["instrument"]
            self.check_instrument(config, instrument)
            kind = args["kind"]
            if kind == "observe":
                return self.observation(config, instrument)
            if kind not in ("ticker", "candles"):
                raise ValueError("unsupported market query")
            query = {"interval_ms": integer(args.get("interval_ms", 1000), 1, 86400000)} if kind == "candles" else None
            data = client._request("GET", f"/rooms/{room}/instruments/{urllib.parse.quote(instrument, safe='')}/{kind}", query=query)
            if kind == "candles":
                data["candles"] = data["candles"][-200:]
            return data
        if name == "web_search":
            return self.research.search(text_value(args["query"], 500))
        if name == "web_read":
            return self.research.read(text_value(args["url"], 4096))
        if name == "trade":
            action = self.validate_trade(config, args, source)
            intent_key = f"{trader}:{key}"
            if self.store.get("exchange_intent", intent_key) is None:
                self.check_account_policy(config, args)
            budget = self.store.get("order_budget", trader, {"at": 0, "keys": []})
            if time.time() - budget["at"] >= 60:
                budget = {"at": time.time(), "keys": []}
            if key not in budget["keys"]:
                if len(budget["keys"]) >= config["orders_per_minute"]:
                    raise ValueError("shared trader order budget exhausted; wait before new orders")
                budget["keys"].append(key)
                self.store.put("order_budget", trader, budget)
            self.store.put("exchange_intent", intent_key, {"action": action})
            try:
                result = client._request("POST", f"/rooms/{room}/instruments/{urllib.parse.quote(args['instrument'], safe='')}/orders",
                                         {"participant_id": trader, "account_id": config["account_id"], "action": action},
                                         idempotency_key="agent-" + hashlib.sha256(f"{trader}:{key}".encode()).hexdigest())
            except ValueError as exc:
                raise UncertainOutcome("unreadable exchange response; retry the same request") from exc
            if not isinstance(result, dict) or type(result.get("accepted")) is not bool or type(result.get("command_seq")) is not int:
                raise UncertainOutcome("incomplete exchange receipt; retry the same request")
            if args["action"] not in ("cancel", "amend"):
                for event in result.get("events", []):
                    if event.get("type") == "OrderAccepted" and event.get("order_id"):
                        self.store.put("order_owner", f"{trader}:{args['instrument']}:{event['order_id']}", source)
            return result
        if name in ("orders", "fills"):
            return self.order_history(config, name, args)
        if name == "order_cancel_all":
            return self.cancel_batch(config, key, args, source, stop)
        if name == "policy_status":
            return self.policy_status(trader)
        if name == "strategies":
            return self.strategies(trader)
        if name == "alert_set":
            return self.alerts.set(config, args)
        if name == "alerts":
            return self.alerts.list(trader)
        if name == "alert_cancel":
            return self.alerts.cancel(trader, args["name"])
        if name.startswith("strategy_"):
            strategy_name = identifier(args["name"])
            if strategy_name == "direct":
                raise ValueError("strategy name 'direct' is reserved")
            skey = f"{trader}:{strategy_name}"
            current = self.store.get("strategy", skey)
            if name == "strategy_patch":
                if current is None:
                    raise ValueError("unknown strategy")
                project = self.store.get("project", f"{skey}:{current['version']}")
                if project is None:
                    project = normalize_project({"code": self.store.get("code", f"{skey}:{current['version']}")})
                changes = args.get("files", {})
                if not isinstance(changes, dict):
                    raise ValueError("files must be an object")
                for path, content in changes.items():
                    if content is None:
                        project["files"].pop(path, None)
                    else:
                        project["files"][path] = content
                for field in ("entrypoint", "requirements", "system_packages"):
                    if field in args:
                        project[field] = args[field]
                return self.execute(config, key, "strategy_save", {"name": strategy_name,
                    "interval_seconds": current["interval_seconds"], **({"market_data": current["market_data"]} if "market_data" in current else {}), **project}, source)
            if name == "strategy_save":
                project = normalize_project(args)
                if current and current["running"]:
                    raise ValueError("stop strategy before saving a new version")
                if current and current.get("pending"):
                    raise ValueError("resume and settle the pending tick before replacing code")
                if current and self.install_status(current).get("status") in ("queued", "running"):
                    raise ValueError("dependency installation is still running; pause the trader to cancel it before replacing the project")
                if not current and len(self.strategies(trader)) >= 4:
                    raise ValueError("at most 4 strategies per trader")
                version = project_version(project)
                self.store.put("project", f"{skey}:{version}", project)
                record = {"trader": trader, "name": strategy_name, "version": version, "running": False, "tested": False,
                          "interval_seconds": integer(args["interval_seconds"], 2, 300), "state": {}, "tick": uuid.uuid4().hex,
                          "next_at": 0, "pending": None, "error": None, "project_format": 1,
                          "requirements": project["requirements"], "system_packages": project["system_packages"],
                          "files": list(project["files"]), "install_job": None, "environment": None}
                if (current and current.get("environment") and current.get("requirements") == project["requirements"]
                        and current.get("system_packages") == project["system_packages"]):
                    environment = self.store.get("environment", f"{skey}:{current['version']}")
                    if environment:
                        self.store.put("environment", f"{skey}:{version}", environment)
                        record.update(environment=current["environment"], install_job=current.get("install_job"))
                if "market_data" in args:
                    settings=args["market_data"]
                    if not isinstance(settings,dict) or set(settings)!={"interval_ms","limit"}: raise ValueError("market_data needs interval_ms and limit")
                    record["market_data"]={"interval_ms":integer(settings["interval_ms"],1,2678400000),"limit":integer(settings["limit"],1,2000)}
                self.store.put("strategy", skey, record)
                return record
            if current is None:
                raise ValueError("unknown strategy")
            if name == "strategy_read":
                version = args.get("version", current["version"])
                project = self.store.get("project", f"{skey}:{version}")
                if project is None:
                    raise ValueError("unknown project version")
                return {"version": version, **project}
            if name == "strategy_status":
                return {**current, "installation": self.install_status(current)}
            if name == "strategy_install":
                if current["running"]:
                    raise ValueError("stop strategy before installing dependencies")
                return self.install(config, current, force=args.get("force", False))
            if name == "strategy_analyze":
                return self.run_strategy(config, current, self.strategy_observations(config,current), analysis=text_value(args["script"], 32768))
            if name == "strategy_test":
                observed = self.strategy_observations(config,current)
                result = self.run_strategy(config, current, observed)
                for action in result.get("actions", []):
                    self.validate_trade(config, action, strategy_name)
                with self.lock(trader):
                    latest = self.store.get("strategy", skey)
                    if (stop is not None and stop.is_set()) or latest["version"] != current["version"] or latest.get("environment") != current.get("environment"):
                        raise ValueError("test result discarded because the session/project changed")
                    latest["tested"] = True
                    self.store.put("strategy", skey, latest)
                return {"version": current["version"], "observations": observed, "result": result, "orders_submitted": False}
            if name == "strategy_start":
                if not current["tested"]:
                    raise ValueError("test this code version before starting")
                if not self.sandbox.check()["available"]:
                    raise ValueError("strategy sandbox unavailable")
                current.update(running=True, error=None)
            if name == "strategy_stop":
                if type(args["cancel_orders"]) is not bool:
                    raise ValueError("cancel_orders must be boolean")
                current["running"] = False
                if current.get("pending"):
                    self.store.event(trader, "strategy_tick_aborted", {"name": strategy_name, "tick": current["tick"],
                        "detail": "Unsent actions discarded; committed orders remain. Inspect receipts before restarting."})
                    current.update(state=current["pending"]["result"].get("state", {}), pending=None, tick=uuid.uuid4().hex)
                self.store.put("strategy", skey, current)
                if args["cancel_orders"]:
                    for instrument, observation in self.observations(config).items():
                        for order in observation["own_orders"]:
                            oid = order["order_id"]
                            if self.store.get("order_owner", f"{trader}:{instrument}:{oid}") == strategy_name:
                                result = self.call(trader, f"{key}:cancel:{instrument}:{oid}", "trade",
                                                   {"instrument": instrument, "action": "cancel", "order_id": oid}, strategy_name)
                                if "error" in result or not result.get("accepted"):
                                    raise ValueError("strategy stopped but cancellation failed: " + str(result.get("error") or result.get("reject_reason")))
            self.store.put("strategy", skey, current)
            return current
        if name == "note":
            self.store.put("note", trader, text_value(args["text"], 12000))
            return {"saved": True}
        if name == "announce":
            return {"statement": text_value(args["text"], 1000)}
        if name == "wait":
            if type(args.get("on_account_change", False)) is not bool:
                raise ValueError("on_account_change must be boolean")
            if args.get("on_account_change"):
                self.store.put("wake", trader, self.account_fingerprint(config))
            else:
                self.store.put("wake", trader, None)
            return {"seconds": integer(args["seconds"], 2, 300)}
        raise ValueError("unsupported tool")

    def install_status(self, strategy):
        job = self.store.get("install_job", strategy.get("install_job"), {})
        if not job:
            return {"status": "not_requested"}
        return job

    def install(self, config, strategy, force=False):
        trader, name = config["id"], strategy["name"]
        if type(force) is not bool:
            raise ValueError("force must be boolean")
        existing = self.install_status(strategy)
        if existing.get("status") in ("queued", "running") or (existing.get("status") == "ready" and not force):
            return existing
        if any(j["trader"] == trader and j["status"] in ("queued", "running") for j in self.store.all("install_job")):
            raise ValueError("one dependency installation per trader at a time")
        job_id = uuid.uuid4().hex
        job = {"id": job_id, "trader": trader, "name": name, "version": strategy["version"], "status": "queued", "log": "", "error": None}
        self.store.put("install_job", job_id, job)
        strategy["install_job"] = job_id
        strategy["tested"] = False
        strategy["environment"] = None
        self.store.put("strategy", f"{trader}:{name}", strategy)
        cancel = threading.Event()
        worker = threading.Thread(target=self.install_worker, args=(job_id, cancel), daemon=True)
        self.installers[job_id] = (cancel, worker)
        worker.start()
        return job

    def install_worker(self, job_id, cancel):
        job = self.store.get("install_job", job_id)
        trader, name, version = job["trader"], job["name"], job["version"]
        skey = f"{trader}:{name}"
        try:
            job["status"] = "running"
            self.store.put("install_job", job_id, job)
            project = self.store.get("project", f"{skey}:{version}")
            def progress(chunk):
                job["log"] = (job["log"] + chunk)[-12000:]
                self.store.put("install_job", job_id, job)
            environment = self.sandbox.prepare(project, cancel=cancel, progress=progress)
            with self.lock(trader):
                current = self.store.get("strategy", skey)
                if cancel.is_set() or current["version"] != version or current.get("install_job") != job_id:
                    job.update(status="cancelled", error="Installation result not activated.")
                else:
                    self.store.put("environment", f"{skey}:{version}", environment)
                    current["environment"] = environment["image"]
                    self.store.put("strategy", skey, current)
                    job.update(status="ready", image=environment["image"], packages=environment["packages"], log=environment["log"])
                self.store.put("install_job", job_id, job)
                self.store.event(trader, "dependencies_installed", {**job, "environment": environment})
        except Exception as exc:
            job.update(status="cancelled" if cancel.is_set() else "failed", error=str(exc)[-4000:])
            self.store.put("install_job", job_id, job)
            self.store.event(trader, "dependency_error", job)
        finally:
            self.wake_events[trader].set()

    def run_strategy(self, config, strategy, observed, analysis=None):
        skey = f"{config['id']}:{strategy['name']}:{strategy['version']}"
        if not strategy.get("project_format"):
            return self.sandbox.run(self.store.get("code", skey), observed, strategy["state"])
        project = self.store.get("project", skey)
        environment = self.store.get("environment", skey)
        if environment and environment["image"] != strategy.get("environment"):
            environment = None
        return self.sandbox.run_project(project, environment, observed, strategy["state"], analysis=analysis)

    def strategies(self, trader):
        return [s | {"dependency_status": self.install_status(s).get("status"),
            "held_actions": [a | {"receipt": self.store.receipt(trader, a["call_id"])} for a in s.get("held_actions", [])]}
            for s in self.store.all("strategy") if s["trader"] == trader]

    def start(self, trader):
        with self.guard, self.lock(trader):
            config = self.config(trader)
            old = self.workers.get(trader)
            if old and any(t.is_alive() for t in old[1]):
                raise ValueError("previous session is still running or stopping; wait for it to finish")
            external = config.get("backend") == "external"
            if not external:
                connection = self.connections.get(config["connection"])
                if not connection or connection["plugin_id"] != config["plugin_id"] or not hasattr(self, "model_loop"):
                    raise ValueError("legacy model loop is disabled; use external backend or explicitly enable legacy compatibility")
            observations = self.observations(config)
            if any(o.get("own_account") is None or o.get("status") != "Running" for o in observations.values()):
                raise ValueError("account must exist and all selected markets must be running")
            # Reconcile unknown outcomes before allowing any new model/strategy decisions.
            for call in self.store.pending(trader):
                if external and call["name"] not in {"trade", "order_cancel_all"}:
                    continue  # A new external decision must explicitly reassess old non-trade actions.
                data = json.loads(call["args"])
                if call["name"] == "trade" and self.store.get("exchange_intent", f"{trader}:{call['id']}") is None:
                    if not self.store.get("exchange_reservation", f"{trader}:{call['id']}"):
                        raise UncertainOutcome("pending order predates submission tracking; reconcile its exchange receipt before resuming")
                    self.store.finish(trader, call["id"], {"error": "old request was not submitted; reassess and use a new request_id"})
                    continue
                if call["name"] == "order_cancel_all":
                    from .external import Fence
                    self.finish_call(config, call["id"], call["name"], data["input"], data["source"], stop=Fence(self, trader, "revoked", -1))
                    continue
                self.call(trader, call["id"], call["name"], data["input"], data["source"])
            config.update(status="running", error=None)
            self.store.put("trader", trader, config)
            self.invalidate_decision(trader)
            stop = threading.Event()
            threads = [threading.Thread(target=self.external_wake_loop if external else self.model_loop, args=(trader, stop), daemon=True),
                       threading.Thread(target=self.strategy_loop, args=(trader, stop), daemon=True),
                       threading.Thread(target=self.alerts.loop, args=(trader, stop), daemon=True)]
            self.workers[trader] = (stop, threads)
            for thread in threads:
                thread.start()
            self.store.event(trader, "started", {})
            return config

    def stop(self, trader):
        if trader in self.workers:
            self.workers[trader][0].set()
        for job_id, (cancel, _) in list(self.installers.items()):
            if self.store.get("install_job", job_id)["trader"] == trader:
                cancel.set()
        with self.lock(trader):
            config = self.config(trader)
            self.invalidate_decision(trader)
            config["status"] = "paused"
            self.store.put("trader", trader, config)
            self.store.event(trader, "paused", {"resting_orders": "unchanged", "strategies": "suspended"})
            return config

    def account_fingerprint(self, config):
        return hashlib.sha256(encode({k: {"account": v["own_account"], "orders": v["own_orders"]}
            for k, v in self.observations(config).items()}).encode()).hexdigest()

    def fail(self, trader, stop, exc):
        with self.lock(trader):
            if stop.is_set():
                return
            stop.set()
            config = self.config(trader)
            # No exception body from a provider: it can echo credentials/request content.
            detail = str(exc) if str(exc) in ("model call budget exhausted", "decision context budget exceeded; use shorter queries/notebook") else "session stopped; inspect receipts and reconnect/resume."
            config.update(status="error", error=f"{type(exc).__name__}: {detail}")
            self.store.put("trader", trader, config)
            self.store.event(trader, "session_error", {"type": type(exc).__name__})

    def strategy_loop(self, trader, stop):
        try:
            while not stop.wait(0.25):
                with self.lock(trader):
                    if stop.is_set():
                        return
                    config = self.config(trader)
                    strategies = self.strategies(trader)
                for strategy in strategies:
                    if strategy["running"] and time.time() >= strategy["next_at"]:
                        self.tick(config, strategy, stop)
        except Exception as exc:
            self.fail(trader, stop, exc)

    def tick(self, config, strategy, stop=None):
        trader, name = config["id"], strategy["name"]
        token = self.alerts.token(trader, stop or threading.Event())
        skey = f"{trader}:{name}"
        if self.alerts.state(trader)["pending"]:
            return
        if not strategy.get("pending"):
            observed = self.strategy_observations(config,strategy)
            if any(o["status"] != "Running" for o in observed.values()):
                return
            try:
                result = self.run_strategy(config, strategy, observed)
                for action in result.get("actions", []):
                    self.validate_trade(config, action, name)
            except (ValueError, OSError) as exc:
                with self.lock(trader):
                    current = self.store.get("strategy", skey)
                    if not token.interrupt.is_set() and current["tick"] == strategy["tick"]:
                        strategy.update(running=False, error=str(exc)[:2000])
                        self.store.put("strategy", skey, strategy)
                        self.store.event(trader, "strategy_error", {"name": name, "error": strategy["error"]})
                return
            with self.lock(trader):
                current = self.store.get("strategy", skey)
                if (token.interrupt.is_set() or current["tick"] != strategy["tick"]
                        or current["version"] != strategy["version"] or not current["running"]):
                    self.store.event(trader, "strategy_tick_aborted", {"name": name, "tick": strategy["tick"], "reason": "plan changed during computation"})
                    return
                strategy["pending"] = {"result": result, "observations": observed}
                self.store.put("strategy", skey, strategy)
        return self.submit_tick(config, strategy, token)

    def submit_tick(self, config, strategy, token):
        trader, name = config["id"], strategy["name"]
        skey = f"{trader}:{name}"
        pending = strategy["pending"]
        for index, action in enumerate(pending["result"].get("actions", [])):
            with self.lock(trader):
                current = self.store.get("strategy", skey)
                if token.is_set() or current["tick"] != strategy["tick"] or not current["running"]:
                    return
                self.call(trader, f"strategy:{name}:{strategy['tick']}:{index}", "trade", action, name, stop=token)
        with self.lock(trader):
            if token.is_set() or self.store.get("strategy", skey)["tick"] != strategy["tick"]:
                return
            self.store.event(trader, "strategy_tick", {"name": name, "version": strategy["version"], "tick": strategy["tick"], **pending})
            strategy.update(state=pending["result"].get("state", {}), pending=None,
                            next_at=time.time() + strategy["interval_seconds"], tick=uuid.uuid4().hex)
            self.store.put("strategy", skey, strategy)


# Old embedding/API users retain the opt-in compatibility harness.
from .legacy import LegacyHarness


class Runtime(LegacyHarness, TradingService):
    pass


def order_identifier(value):
    """Native orders use the full u64 range; accept decimal strings losslessly."""
    if isinstance(value,str):
        if not value.isascii() or not value.isdecimal(): raise ValueError("order_id must be a decimal integer")
        value=int(value)
    return integer(value,1,2**64-1)
