"""Real exchange HTTP + agent.v1 provider HTTP + optional real Docker.

Provider responses are scripted fixtures, not evidence of autonomous LLM quality.
"""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from marketforge import Client, MarketForgeError
from marketforge.agents.runtime import Runtime
from marketforge.agents.sandbox import DockerSandbox

ROOT = Path(__file__).resolve().parents[2]
SPOT, PERP = "V-BTC-SPOT", "V-BTC-PERP"
CODE = f'''def decide(observations, state):
    if state.get("done"):
        return {{"actions": [], "state": state}}
    return {{"actions": [{{"instrument": "{SPOT}", "action": "ioc", "side": "Buy", "price_tick": 101, "qty": 1}},
                        {{"instrument": "{PERP}", "action": "ioc", "side": "Buy", "price_tick": 101, "qty": 1}}],
             "state": {{"done": True}}, "summary": "One spot and one perpetual execution"}}
'''


def scenario(room):
    value = json.loads((ROOT / "scripts/fixtures/f6_batch_spec.json").read_text())["scenario"]
    value["room_id"] = room
    value["accounts"].append({"Basic": {"account_id": 30, "cash_balance": 100000}})
    instrument = dict(value["market"]["Spot"]["instrument"], instrument_id=PERP, symbol=PERP)
    value["extra_markets"] = [{"Perp": {"instrument": instrument,
        "clearing": {"maker_fee_ppm": 0, "taker_fee_ppm": 0, "leverage": 2},
        "risk": {}, "initial_mark_price_tick": 100}}]
    value["routed_seed_orders"] = [{"instrument_id": PERP, "command": {"NewOrder": {
        "order_id": 100, "account_id": 10, "side": "Sell", "kind": {"Limit": {"price_tick": 101}}, "qty": 8, "reduce_only": False}}}]
    return value


class FixtureProvider(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        message = {"role": "assistant", "content": "OK"}
        if request.get("tools"):
            # A slow network response demonstrates that the market keeps ticking.
            time.sleep(0.3)
            stage = sum(m["role"] == "tool" for m in request["messages"])
            if request["model"] == "fixture-project":
                context = json.loads(request["messages"][1]["content"])
                current = context["strategies"]
                if not current:
                    plan = [("strategy_save", {"name": "executor", "files": {
                        "strategy.py": "from sizing import quantity\n" + CODE.replace('"qty": 1', '"qty": quantity()'),
                        "sizing.py": "import numpy as np\nimport pandas as pd\ndef quantity(): return int(pd.Series(np.array([1,1])).mean())"},
                        "requirements": ["numpy==2.1.3", "pandas==2.2.3"], "interval_seconds": 2}),
                        ("strategy_install", {"name": "executor"})]
                elif current[0].get("dependency_status") != "ready":
                    plan = [("strategy_status", {"name": "executor"})]
                elif not current[0]["running"]:
                    plan = [("strategy_test", {"name": "executor"}), ("strategy_start", {"name": "executor"})]
                else:
                    plan = []
            elif request["model"] == "fixture-direct":
                plan = [("market_read", {"instrument": PERP, "kind": "observe"}),
                        ("trade", {"instrument": SPOT, "action": "ioc", "side": "Buy", "price_tick": 101, "qty": 1}),
                        ("trade", {"instrument": PERP, "action": "ioc", "side": "Buy", "price_tick": 101, "qty": 1}),
                        ("announce", {"text": "Fixture: bought spot and perpetual."})]
            else:
                plan = [("strategy_save", {"name": "executor", "code": CODE, "interval_seconds": 2}),
                        ("strategy_test", {"name": "executor"}), ("strategy_start", {"name": "executor"})]
            tool, args = plan[stage] if stage < len(plan) else ("wait", {"seconds": 300})
            message = {"role": "assistant", "content": None, "tool_calls": [{"id": f"call-{stage}", "type": "function",
                "function": {"name": tool, "arguments": json.dumps(args)}}]}
        data = json.dumps({"choices": [{"message": message}], "usage": {"total_tokens": 1}}).encode()
        self.send_response(200); self.send_header("Content-Type", "application/json"); self.send_header("Content-Length", str(len(data))); self.end_headers(); self.wfile.write(data)


class LiveAgentTests(unittest.TestCase):
    @unittest.skipUnless(os.environ.get("MARKETFORGE_AGENT_LIVE_TEST") == "1", "set MARKETFORGE_AGENT_LIVE_TEST=1 for real exchange/provider transport")
    def test_two_plugin_traders_real_http_spot_perp_and_generated_strategy(self):
        with tempfile.TemporaryDirectory() as folder:
            executable = ROOT / "target/debug" / ("exchange-server.exe" if os.name == "nt" else "exchange-server")
            self.assertTrue(executable.exists(), "cargo build -p exchange-server first")
            with socket.socket() as sock:
                sock.bind(("127.0.0.1", 0)); port = sock.getsockname()[1]
            base = f"http://127.0.0.1:{port}"
            env = {k: v for k, v in os.environ.items() if not k.startswith("MARKETFORGE_")}
            env["MARKETFORGE_BIND_ADDR"] = f"127.0.0.1:{port}"
            env["MARKETFORGE_AUTH_TOKENS_JSON"] = json.dumps({"fixture-admin": "admin", "fixture-direct": "agent-direct", "fixture-coder": "agent-coder"})
            provider = ThreadingHTTPServer(("127.0.0.1", 0), FixtureProvider)
            thread = threading.Thread(target=provider.serve_forever, daemon=True); thread.start()
            runtime = None
            with open(Path(folder) / "exchange.log", "w") as log:
                process = subprocess.Popen([str(executable)], env=env, stdout=log, stderr=subprocess.STDOUT)
                try:
                    admin = Client(base, bearer="fixture-admin")
                    deadline = time.monotonic() + 15
                    while True:
                        try:
                            admin.health_ready(); break
                        except Exception:
                            if process.poll() is not None or time.monotonic() > deadline:
                                self.fail((Path(folder) / "exchange.log").read_text())
                            time.sleep(0.1)
                    room = "agent-live"
                    admin._request("POST", "/rooms", {"scenario": scenario(room), "autostart_agents": False})
                    for name, account in [("direct", 20), ("coder", 30)]:
                        admin.add_member(room, "agent-" + name, "trader")
                        admin.assign_account(room, account, "agent-" + name)
                    admin.start_agents(room, [], interval_ms=100)
                    runtime = Runtime(Path(folder) / "agents", ROOT / "agent-plugins", base)
                    self.assertTrue(runtime.sandbox.check()["available"], "real strategy acceptance requires Docker image")
                    for name, account in [("direct", 20), ("coder", 30)]:
                        model = "fixture-project" if name == "coder" and os.environ.get("MARKETFORGE_AGENT_PROJECT_TEST") == "1" else "fixture-" + name
                        runtime.connect({"id": name, "base_url": f"http://127.0.0.1:{provider.server_port}/v1", "model": model})
                        token_env = "MARKETFORGE_TRADER_TOKEN_TEST_" + name.upper()
                        os.environ[token_env] = "fixture-" + name
                        self.addCleanup(os.environ.pop, token_env, None)
                        runtime.create({"id": name, "room": room, "account_id": account, "instruments": [SPOT, PERP], "connection": name, "exchange_token_env": token_env})
                    with self.assertRaises(MarketForgeError) as denied:
                        runtime.client(runtime.config("direct")).observe(room, 30, SPOT)
                    self.assertEqual(denied.exception.status, 403)
                    before = admin.clock(room)["clock"]["step"]
                    runtime.start("direct"); runtime.start("coder")
                    deadline = time.monotonic() + 180
                    while time.monotonic() < deadline:
                        failures = [t for t in runtime.store.all("trader") if t["status"] == "error"]
                        self.assertFalse(failures, failures)
                        strategies = runtime.strategies("coder")
                        if strategies and strategies[0]["state"].get("done"):
                            break
                        if strategies and runtime.install_status(strategies[0]).get("status") == "failed":
                            self.fail(str(runtime.install_status(strategies[0])))
                        time.sleep(0.2)
                    else:
                        self.fail(json.dumps(runtime.store.events("coder"), ensure_ascii=False))
                    for name in ["direct", "coder"]:
                        for instrument in [SPOT, PERP]:
                            observation = runtime.observation(runtime.config(name), instrument)
                            account = next(iter(observation["own_account"].values()))
                            self.assertEqual(int(account["position_qty"]), 1, (name, instrument, observation))
                    self.assertGreater(admin.clock(room)["clock"]["step"], before + 5)
                    events, after = [], 0
                    while True:
                        page = runtime.store.events("coder", after)
                        events.extend(page)
                        if len(page) < 200:
                            break
                        after = page[-1]["seq"]
                    requests = [e for e in events if e["kind"] == "tool_request"]
                    self.assertTrue(any(e["data"]["source"] == "executor" for e in requests))
                    self.assertFalse(runtime.store.pending("coder"))
                finally:
                    if runtime:
                        for config in runtime.store.all("trader"):
                            runtime.stop(config["id"])
                        for _, threads in runtime.workers.values():
                            for worker in threads:
                                worker.join(12)
                        for _, worker in runtime.installers.values():
                            worker.join(15)
                        runtime.store.db.close()
                    process.terminate(); process.wait(timeout=10)
                    provider.shutdown(); provider.server_close(); thread.join()


if __name__ == "__main__":
    unittest.main()
