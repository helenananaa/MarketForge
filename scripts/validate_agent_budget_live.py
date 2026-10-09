"""One bounded native Codex inference to validate framework usage accounting."""
import argparse
import json
from pathlib import Path
import tempfile
import threading
import time
import uuid
from http.server import ThreadingHTTPServer

from marketforge.agents.__main__ import handler
from marketforge.agents.connectors import CodexSession, Heartbeat, deliver_model, pump_model
from marketforge.agents.mcp_server import ServiceClient
from marketforge.agents.replay import isolated_exchange
from marketforge.agents.runtime import TradingService


def validate(output, model):
    root = Path(__file__).resolve().parents[1]
    scenario = json.loads((root / "scripts/fixtures/f6_batch_spec.json").read_text())["scenario"]
    instrument = scenario["market"]["Spot"]["instrument"]["instrument_id"]
    output.mkdir(parents=True, exist_ok=True)
    with isolated_exchange(root / "target/debug/exchange-server.exe") as admin, tempfile.TemporaryDirectory() as folder:
        directory = Path(folder)
        admin._request("POST", "/rooms", {"scenario": scenario, "autostart_agents": False})
        admin.add_member(scenario["room_id"], "agent-budget", "trader")
        admin.assign_account(scenario["room_id"], 20, "agent-budget")
        service = TradingService(directory, None, admin.base_url)
        config = service.create({"id": "budget", "room": scenario["room_id"], "account_id": 20, "instruments": [instrument],
            "prompt": "仅验证框架 Token 用量上报：回复 budget check，不调用任何工具，不交易。"})
        config["status"] = "running"; service.store.put("trader", "budget", config)
        service.policy_update("budget", {"account": {}, "model": {"max_admissions": 1, "max_tokens": 1}})
        token = service.issue_access("budget")["token"]
        capability = directory / "tools.token"; capability.write_text(token, encoding="utf-8"); capability.chmod(0o600)
        server = ThreadingHTTPServer(("127.0.0.1", 0), handler(service, uuid.uuid4().hex, set()))
        server.daemon_threads = True
        thread = threading.Thread(target=server.serve_forever); thread.start()
        state = {"owner_id": uuid.uuid4().hex, "connection_id": uuid.uuid4().hex, "identity": {"backend": "codex"}}
        client = ServiceClient(f"http://127.0.0.1:{server.server_port}", "budget", token)
        adapter, heartbeat = None, None
        try:
            adapter = CodexSession(client, state, directory, capability, model=model, effort="low")
            state["session_id"] = adapter.id
            client.request("connection", {"action": "attach", "owner_id": state["owner_id"], "connection_id": state["connection_id"],
                "backend": "codex", "session_id": adapter.id, "state": "idle"})
            heartbeat = Heartbeat(client, adapter, state, state["connection_id"]); heartbeat.thread.start()
            delivered = deliver_model(client, adapter, state, {"context": client.call("context", {})})
            if not delivered:
                raise AssertionError("first permitted message was blocked")
            deadline = time.monotonic() + 90
            while time.monotonic() < deadline:
                pump_model(client, adapter, state)
                heartbeat.check()
                if service.policy_status("budget")["model_usage"]["tokens"]:
                    break
                time.sleep(0.1)
            before = service.policy_status("budget")
            if before["model_usage"]["tokens"] < 1:
                raise AssertionError("native token usage was not reported")
            duplicate = client.request("model", {"action": "usage", "connection_id": state["connection_id"], "session_id": adapter.id,
                "meter_id": "thread_total", "tokens": adapter.usage["totalTokens"]})
            if duplicate["status"]["model_usage"]["tokens"] != before["model_usage"]["tokens"]:
                raise AssertionError("duplicate native usage was counted twice")
            second = deliver_model(client, adapter, state, {"context": client.call("context", {})})
            if second:
                raise AssertionError("budget allowed another model message")
            observation = service.observation(config, instrument)
            if observation["own_account"]["Spot"]["position_qty"] != 0 or observation["own_orders"]:
                raise AssertionError("budget validation unexpectedly traded")
            report = {"passed": True, "model": model, "session_id": adapter.id, "native_usage": adapter.usage,
                "policy": service.policy_status("budget"), "first_delivery": delivered, "second_delivery": second,
                "usage_duplicate_deduplicated": True, "trades": 0, "provider_invoice_cost": None}
            (output / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
            return report
        finally:
            if heartbeat: heartbeat.close()
            if adapter:
                try: adapter.pause()
                finally: adapter.close()
            server.shutdown(); server.server_close(); thread.join(2)
            service.store.db.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=Path(".local/validation/agent-budget-live"))
    parser.add_argument("--model", default="gpt-5.6-sol", help="choose a model actually available in the installed Codex CLI")
    args = parser.parse_args()
    report = validate(args.output, args.model)
    print(json.dumps({"passed": report["passed"], "tokens": report["native_usage"]["totalTokens"], "second_delivery": report["second_delivery"]}))


if __name__ == "__main__":
    main()
