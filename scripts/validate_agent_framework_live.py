"""Bounded real Codex turn + isolated exchange + model-free replay acceptance.

Uses the user's configured Codex model/auth. Never connects to an existing market.
"""
import argparse
import json
import shutil
from pathlib import Path
import sys
import tempfile
import threading
import time
from http.server import ThreadingHTTPServer

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "python"))
from marketforge.agents.__main__ import handler
from marketforge.agents.connectors import CodexRPC, CodexSession, INSTRUCTIONS, supervise
from marketforge.agents.mcp_server import ServiceClient
from marketforge.agents.replay import Recorder, isolated_exchange, replay
from marketforge.agents.runtime import TradingService


def validate(executable, output, timeout=240, model=None):
    output.mkdir(parents=True, exist_ok=True)
    if model is None:
        rpc = CodexRPC([shutil.which("codex") or "codex", "app-server"])
        try:
            rpc.request("initialize", {"clientInfo": {"name": "marketforge-model-check", "version": "1.0"}})
            rpc.send({"method": "initialized", "params": {}})
            available = rpc.request("model/list", {})["data"]
            model = next((item["model"] for item in available if item.get("isDefault")), available[0]["model"])
        finally:
            rpc.close()
    print("Validation model: " + model, file=sys.stderr, flush=True)
    scenario = json.loads((ROOT / "scripts/fixtures/f6_batch_spec.json").read_text())["scenario"]
    scenario["room_id"] = "agent-framework-acceptance"
    room, instrument = scenario["room_id"], scenario["market"]["Spot"]["instrument"]["instrument_id"]
    config = {"id": "validation", "backend": "external", "room": room, "account_id": 20, "instruments": [instrument],
        "decision_lease_seconds": 600, "max_order_qty": 1, "orders_per_minute": 5,
        "prompt": ("Controlled virtual-market acceptance. Buy exactly one unit, once, after a market wakeup; "
            "pay at most 110 ticks. Before any wakeup: read context, begin a decision with an explicit plan, "
            "choose and set one one-shot alert to detect an upward best-ask move from the observed price, "
            "then read the market. Do not trade before the wakeup. After wakeup, inspect fresh context and "
            "begin a fresh decision, decide whether the purchase still fits the goal, then buy one unit using "
            "the observed ask as the bounded IOC price. If the order fills, call wait(300,on_account_change=false) "
            "and end your turn. Use only MarketForge MCP tools. No web, files, shell, strategy code or extra tasks.")}
    recorder, stop, bridge_errors = Recorder(scenario, config), threading.Event(), []
    record_lock, alert_ready, release_alert = threading.RLock(), threading.Event(), threading.Event()
    timings, adapters = {}, []
    service, server, bridge = None, None, None
    directory = tempfile.TemporaryDirectory()
    try:
        with isolated_exchange(executable) as admin:
            folder = directory.name
            admin._request("POST", "/rooms", {"scenario": scenario, "autostart_agents": False})
            admin.add_member(room, "agent-validation", "trader")
            admin.assign_account(room, 20, "agent-validation")
            service = TradingService(Path(folder) / "service", None, admin.base_url)
            service.create(config)
            token = service.issue_access("validation")["token"]
            original_call, original_poll = service.external_call, service.alerts.poll
            def recorded_call(trader, name, args, **kwargs):
                hold = False
                with record_lock:
                    try:
                        result = original_call(trader, name, args, **kwargs)
                    except Exception as exc:
                        recorder.tool(name, args, error=exc)
                        raise
                    recorder.tool(name, args, result)
                    print("Validation tool: " + name, file=sys.stderr, flush=True)
                    if name == "alert_set" and not alert_ready.is_set():
                        timings["alert_set"] = time.monotonic()
                        alert_ready.set(); hold = True
                    if name == "trade" and result.get("accepted"):
                        timings.setdefault("accepted_trade", time.monotonic())
                # Hold only this test's tool response to make the in-flight boundary reproducible.
                if hold and not release_alert.wait(20):
                    raise RuntimeError("validation controller did not release alert tool")
                return result
            def recorded_poll(trader):
                with record_lock:
                    before = service.alerts.state(trader)["generation"]
                    original_poll(trader)
                    after = service.alerts.state(trader)["generation"]
                    if after != before:
                        recorder.poll(after)
                        timings["alert_fired"] = time.monotonic()
            service.external_call, service.alerts.poll = recorded_call, recorded_poll
            server = ThreadingHTTPServer(("127.0.0.1", 0), handler(service, "validation-operator-only-token", set()))
            server.daemon_threads = True
            threading.Thread(target=server.serve_forever, daemon=True).start()
            client = ServiceClient(f"http://127.0.0.1:{server.server_port}", "validation", token)
            capability = Path(folder) / "capability.token"; capability.write_text(token)
            workspace = Path(folder) / "workspace"; workspace.mkdir()
            state, state_path = {"identity": {"backend": "codex"}}, Path(folder) / "bridge.json"
            service.start("validation")
            def factory():
                adapter = CodexSession(client, state, workspace, capability,
                    instructions=INSTRUCTIONS + " For this acceptance, follow the controlled goal exactly and finish after wait.", model=model, effort="low")
                original_deliver = adapter.deliver
                def event_hook(event):
                    if event.get("params", {}).get("threadId") == adapter.id and event.get("method") in {"item/started", "item/completed", "turn/completed"}:
                        item = event["params"].get("item", {})
                        print("Native event: " + event["method"] + " " + str(item.get("type", "")), file=sys.stderr, flush=True)
                adapter.rpc.event_hook = event_hook
                def deliver(payload):
                    active = adapter.active
                    original_deliver(payload)
                    if payload.get("events"):
                        timings["event_delivered"] = time.monotonic()
                        timings["steered_active_turn"] = bool(active)
                    if len(adapters) == 0:
                        timings["initial_thread"] = adapter.id
                adapter.deliver = deliver
                adapters.append(adapter)
                return adapter
            def run_bridge():
                try:
                    supervise(client, factory, state, state_path, stop, max_retries=0, turn_timeout=timeout)
                except Exception as exc:
                    bridge_errors.append(type(exc).__name__)
                    if getattr(exc, "code", None):
                        (output / "native-error-code.json").write_text(json.dumps({"code": exc.code}), encoding="utf-8")
                    release_alert.set()
            bridge = threading.Thread(target=run_bridge, daemon=True); bridge.start()
            deadline = time.monotonic() + timeout
            while not alert_ready.wait(0.1):
                if bridge_errors or time.monotonic() >= deadline:
                    raise AssertionError("model did not set an alert within the bounded run")
            with record_lock:
                # Preserve bid 99; move the finite resting ask from 101 to 105.
                for index, action in enumerate(({"Cancel": {"order_id": 2}}, {"PlaceLimit": {"side": "Sell", "price_tick": 105, "qty": 8}})):
                    command = {"participant_id": "validation-controller", "account_id": 10, "action": action}
                    response = admin._request("POST", f"/rooms/{room}/instruments/{instrument}/orders", command, idempotency_key=f"shock-{index}")
                    if not response["accepted"]:
                        raise AssertionError("market shock command was rejected")
                    recorder.market(instrument, command)
                admin.advance_clock(room, 3, idempotency_key="shock-clock"); recorder.clock(3)
                timings["market_changed"] = time.monotonic()
            while "event_delivered" not in timings:
                if bridge_errors or time.monotonic() >= deadline:
                    raise AssertionError("market alert did not reach the active framework turn")
                time.sleep(0.05)
            release_alert.set()
            while time.monotonic() < deadline:
                if bridge_errors:
                    raise AssertionError("native framework turn failed: " + bridge_errors[0])
                control = service.control("validation")
                if "accepted_trade" in timings and control.get("scheduled_wake") and adapters and not adapters[-1].active:
                    break
                time.sleep(0.1)
            else:
                raise AssertionError("model did not finish the interrupted purchase and wait")
            stop.set(); bridge.join(12)
            if bridge.is_alive():
                raise AssertionError("owned connector did not stop")
            final = service.observation(service.config("validation"), instrument)
            account = next(iter(final["own_account"].values()))
            if int(account["position_qty"]) != 1 or len([step for step in recorder.trace["steps"] if step.get("name") == "trade" and step.get("expected", {}).get("accepted")]) != 1:
                raise AssertionError("expected exactly one real virtual-market fill")
            if not timings.get("steered_active_turn"):
                raise AssertionError("wakeup was delivered after the native turn had already ended")
            # Resume real inference history without starting another model turn.
            service.external_call = original_call
            state["connection_id"] = "validation-resumed-transport"
            resumed = CodexSession(client, state, workspace, capability,
                instructions=INSTRUCTIONS, model=model, effort="low")
            try:
                if resumed.id != state["session_id"]:
                    raise AssertionError("framework restart lost the original session")
                history = resumed.rpc.request("thread/read", {"threadId": resumed.id, "includeTurns": True})["thread"]
                if not history.get("turns"):
                    raise AssertionError("framework restart lost the completed model history")
                client.request("connection", {"action": "attach", "owner_id": state["owner_id"],
                    "connection_id": state["connection_id"], "backend": "codex", "session_id": resumed.id})
                context = resumed.rpc.request("mcpServer/tool/call", {"threadId": resumed.id, "server": "marketforge", "tool": "context", "arguments": {}})
                if context.get("isError") or context["structuredContent"]["observations"][instrument]["own_account"]["Spot"]["position_qty"] != 1:
                    raise AssertionError("resumed MCP session did not observe the existing fill")
            finally:
                client.request("connection", {"action": "disconnect", "owner_id": state["owner_id"], "connection_id": state["connection_id"]})
                resumed.close()
            recorder.save(output / "recording.json")
            # The same ordered market changes and recorded model intents run without any model.
            runs = [replay(recorder.trace, executable) for _ in range(2)]
            if runs[0]["digest"] != runs[1]["digest"]:
                raise AssertionError("model-free replay diverged")
            report = {"passed": True, "provider_inference": True, "framework": "codex", "session_id": state["session_id"],
                "model": model, "token_usage": getattr(adapters[-1], "usage", None),
                "native_session_resumed_with_history": True,
                "steered_active_turn": True, "virtual_fills": 1, "final_account": account,
                "alert_detection_ms": round((timings["alert_fired"] - timings["market_changed"]) * 1000, 2),
                "alert_delivery_ms": round((timings["event_delivered"] - timings["alert_fired"]) * 1000, 2),
                "wake_to_accepted_trade_ms": round((timings["accepted_trade"] - timings["alert_fired"]) * 1000, 2),
                "replay_digests": [run["digest"] for run in runs], "replay_steps": runs[0]["steps"],
                "boundary": "controlled goal and tool-response gate; proves active-turn interrupt and business recovery, not autonomous strategy quality"}
            (output / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8", newline="\n")
            return report
    except Exception as exc:
        recorder.save(output / "failed-recording.json")
        (output / "failure.json").write_text(json.dumps({"passed": False, "error_type": type(exc).__name__,
            "message": str(exc) if isinstance(exc, AssertionError) else "inspect local framework session and validation state",
            "steps": len(recorder.trace["steps"]), "bridge_errors": bridge_errors}, indent=2), encoding="utf-8")
        raise
    finally:
        stop.set(); release_alert.set()
        if bridge:
            bridge.join(12)
        if service:
            service.stop("validation")
            for _, workers in service.workers.values():
                for worker in workers:
                    worker.join(3)
        if server:
            server.shutdown(); server.server_close()
        if service:
            service.store.db.close()
        directory.cleanup()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exchange-server", default=str(ROOT / "target/debug/exchange-server.exe"))
    parser.add_argument("--output", default=str(ROOT / ".local/validation/agent-framework-live"))
    parser.add_argument("--timeout", type=int, default=240)
    parser.add_argument("--model", help="optional available native model; default is advertised framework default, without changing global config")
    args = parser.parse_args()
    print(json.dumps(validate(args.exchange_server, Path(args.output), args.timeout, args.model), ensure_ascii=False))
