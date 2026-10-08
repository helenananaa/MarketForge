import copy
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.request
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler

from marketforge import Client, MarketForgeError
from marketforge.agents.runtime import Runtime, UncertainOutcome
from marketforge.agents.__main__ import handler
from marketforge.agents.sandbox import DockerSandbox

ROOT = Path(__file__).resolve().parents[2]
PLUGIN = ROOT / "agent-plugins"


class FakeExchange:
    def __init__(self):
        self.orders = []
        self.cache = {}
        self.lose_response = False

    def observe(self, room, account, instrument):
        return {"observation": {"room_id": room, "instrument_id": instrument, "step": 2, "market_time_ms": 2000,
                "status": "Running", "book": {"bids": [], "asks": []}, "own_account": {"Spot": {"account_id": account}},
                "own_orders": [{"order_id": index + 1} for index, order in enumerate(self.orders) if order["instrument"] == instrument]}}

    def _request(self, method, path, body=None, idempotency_key=None, query=None):
        if method == "POST":
            if idempotency_key in self.cache:
                return self.cache[idempotency_key]
            self.orders.append({**body, "instrument": path.split("/")[-2]})
            result = {"accepted": True, "command_seq": len(self.orders), "events": [{"type": "OrderAccepted", "order_id": len(self.orders)}]}
            self.cache[idempotency_key] = result
            if self.lose_response:
                self.lose_response = False
                raise TimeoutError("response lost after commit")
            return result
        return {"market_time_ms": 2000, "ticker": {"last_price_tick": 100}}


class FakeSandbox:
    def __init__(self):
        self.result = {"actions": [], "state": {"n": 1}}
        self.runs = 0
    def check(self):
        return {"available": True}
    def run(self, code, observations, state):
        self.runs += 1
        return copy.deepcopy(self.result)
    def run_project(self, project, environment, observations, state, analysis=None):
        return self.run(project["files"][project["entrypoint"]], observations, state)


class RuntimeTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.exchange = FakeExchange()
        self.sandbox = FakeSandbox()
        self.runtime = Runtime(self.directory.name, PLUGIN, "http://unused", self.sandbox, lambda _: self.exchange)
        self.runtime.connections["model"] = {"plugin_id": "marketforge.llm-trader", "model": "fixture"}
        self.runtime.create({"id": "alice", "room": "room", "account_id": 20, "instruments": ["SPOT", "PERP"], "connection": "model"})

    def tearDown(self):
        self.runtime.stop("alice")
        self.runtime.store.db.close()
        self.directory.cleanup()

    def call(self, key, tool, args, source="direct"):
        return self.runtime.call("alice", key, tool, args, source)

    def buy(self, **updates):
        return {"instrument": "SPOT", "action": "limit", "side": "Buy", "price_tick": 100, "qty": 2, **updates}

    def save(self):
        self.call("save", "strategy_save", {"name": "slice", "code": "def decide(observations, state): return {}", "interval_seconds": 2})
        self.call("test", "strategy_test", {"name": "slice"})
        self.call("start", "strategy_start", {"name": "slice"})

    def test_account_bound_cross_instrument_orders_and_replay(self):
        first = self.call("a", "trade", self.buy())
        self.assertEqual(first, self.call("a", "trade", self.buy()))
        self.call("b", "trade", self.buy(instrument="PERP"))
        self.assertEqual(len(self.exchange.orders), 2)
        self.assertEqual({o["account_id"] for o in self.exchange.orders}, {20})
        self.assertIn("error", self.call("c", "trade", self.buy(instrument="OTHER")))
        with self.assertRaises(ValueError):
            self.call("spoof", "trade", {**self.buy(), "account_id": 30})
        with self.assertRaises(ValueError):
            self.call("a", "trade", self.buy(qty=5))

    def test_unknown_outcome_retries_same_order_after_restart(self):
        self.exchange.lose_response = True
        with self.assertRaises(TimeoutError):
            self.call("lost", "trade", self.buy())
        self.assertEqual(len(self.runtime.store.pending("alice")), 1)
        self.runtime.store.db.close()
        self.runtime = Runtime(self.directory.name, PLUGIN, "http://unused", self.sandbox, lambda _: self.exchange)
        self.assertEqual(self.runtime.config("alice")["status"], "paused")
        self.assertTrue(self.call("lost", "trade", self.buy())["accepted"])
        self.assertEqual(len(self.exchange.orders), 1)
        self.assertFalse(self.runtime.store.pending("alice"))

    def test_parent_and_children_share_order_budget(self):
        config = self.runtime.config("alice")
        config["orders_per_minute"] = 1
        self.runtime.store.put("trader", "alice", config)
        self.call("direct", "trade", self.buy())
        self.assertIn("error", self.call("child", "trade", self.buy(), "slice"))
        self.assertEqual(len(self.exchange.orders), 1)

    def test_malformed_exchange_response_keeps_request_pending(self):
        original = self.exchange._request
        def damaged(*args, **kwargs):
            original(*args, **kwargs)
            raise ValueError("invalid JSON after an actual commit")
        self.exchange._request = damaged
        with self.assertRaises(UncertainOutcome):
            self.call("damaged", "trade", self.buy())
        self.exchange._request = original
        self.call("damaged", "trade", self.buy())
        self.assertEqual(len(self.exchange.orders), 1)

    def test_strategy_pause_preserves_unsubmitted_tick(self):
        self.sandbox.result["actions"] = [self.buy()]
        self.save()
        stop = threading.Event(); stop.set()
        self.runtime.tick(self.runtime.config("alice"), self.runtime.strategies("alice")[0], stop)
        self.assertEqual(len(self.exchange.orders), 0)
        self.assertIsNotNone(self.runtime.strategies("alice")[0]["pending"])
        stop.clear()
        self.runtime.tick(self.runtime.config("alice"), self.runtime.strategies("alice")[0], stop)
        self.assertEqual(len(self.exchange.orders), 1)

    def test_generated_code_never_executes_during_save_and_requires_test(self):
        self.call("s", "strategy_save", {"name": "slice", "code": "raise Exception('not host code')", "interval_seconds": 2})
        self.assertEqual(self.sandbox.runs, 0)
        self.assertIn("error", self.call("start", "strategy_start", {"name": "slice"}))

    def test_dry_run_no_orders_and_version_update_requires_stop(self):
        self.sandbox.result["actions"] = [self.buy()]
        self.save()
        self.assertEqual(self.exchange.orders, [])
        self.assertIn("error", self.call("update", "strategy_save", {"name": "slice", "code": "x", "interval_seconds": 2}))
        self.runtime.tick(self.runtime.config("alice"), self.runtime.strategies("alice")[0])
        self.assertEqual(len(self.exchange.orders), 1)
        self.assertEqual(self.runtime.store.get("order_owner", "alice:SPOT:1"), "slice")
        self.assertIn("error", self.call("steal", "trade", {"instrument": "SPOT", "action": "cancel", "order_id": 1}, "other"))
        self.call("stop", "strategy_stop", {"name": "slice", "cancel_orders": True})
        self.assertEqual(self.exchange.orders[-1]["action"], {"Cancel": {"order_id": 1}})

    def test_strategy_pending_tick_resumes_without_reexecuting_code(self):
        self.sandbox.result["actions"] = [self.buy(), self.buy(instrument="PERP")]
        self.save()
        self.exchange.lose_response = True
        with self.assertRaises(TimeoutError):
            self.runtime.tick(self.runtime.config("alice"), self.runtime.strategies("alice")[0])
        runs = self.sandbox.runs
        self.runtime.tick(self.runtime.config("alice"), self.runtime.strategies("alice")[0])
        self.assertEqual(self.sandbox.runs, runs)
        self.assertEqual(len(self.exchange.orders), 2)
        self.assertIsNone(self.runtime.strategies("alice")[0]["pending"])

    def test_bad_strategy_stops_itself_only(self):
        self.save()
        self.sandbox.result["actions"] = [self.buy(instrument="FORBIDDEN")]
        self.runtime.tick(self.runtime.config("alice"), self.runtime.strategies("alice")[0])
        strategy = self.runtime.strategies("alice")[0]
        self.assertFalse(strategy["running"])
        self.assertIn("scope", strategy["error"])
        self.assertEqual(self.exchange.orders, [])

    def test_model_response_arriving_after_stop_is_discarded(self):
        entered, release, stop = threading.Event(), threading.Event(), threading.Event()
        def complete(*_):
            entered.set()
            release.wait(3)
            return {"role": "assistant", "tool_calls": [{"id": "x", "function": {"name": "trade", "arguments": json.dumps(self.buy())}}]}, {}
        self.runtime.plugins["marketforge.llm-trader"][1].complete = complete
        worker = threading.Thread(target=self.runtime.round, args=("alice", stop))
        self.runtime.workers["alice"] = (stop, [worker])
        worker.start()
        self.assertTrue(entered.wait(2))
        self.runtime.stop("alice")
        release.set(); worker.join(3)
        self.assertFalse(worker.is_alive())
        self.assertEqual(self.exchange.orders, [])
        self.assertTrue(self.runtime.store.events("alice")[-1]["data"]["discarded"])

    def test_agent_can_query_write_test_deploy_and_trade_in_one_round(self):
        calls = [[("market_read", {"instrument": "PERP", "kind": "observe"})],
                 [("strategy_save", {"name": "slice", "code": "def decide(observations, state): return {}", "interval_seconds": 2})],
                 [("strategy_test", {"name": "slice"})], [("strategy_start", {"name": "slice"})],
                 [("trade", self.buy())], [("announce", {"text": "Starting a spot position."})], [("wait", {"seconds": 20})]]
        def complete(*_):
            batch = calls.pop(0)
            return {"role": "assistant", "content": None, "tool_calls": [{"id": str(i), "type": "function", "function": {"name": n, "arguments": json.dumps(a)}} for i, (n, a) in enumerate(batch)]}, {"total_tokens": 10}
        self.runtime.plugins["marketforge.llm-trader"][1].complete = complete
        self.assertEqual(self.runtime.round("alice", threading.Event()), 20)
        self.assertEqual(len(self.exchange.orders), 1)
        self.assertTrue(self.runtime.strategies("alice")[0]["running"])
        self.assertEqual(self.runtime.config("alice")["model_calls"], 7)

    def test_duplicate_account_and_unknown_plugin_rejected(self):
        with self.assertRaises(ValueError):
            self.runtime.create({"id": "bob", "room": "room", "account_id": 20, "instruments": ["SPOT"], "connection": "model"})
        with self.assertRaises(ValueError):
            self.runtime.connect({"id": "x", "plugin_id": "unknown", "base_url": "x", "model": "x"})


    def test_operator_auth_origin_and_secrets(self):
        server = ThreadingHTTPServer(("127.0.0.1", 0), handler(self.runtime, "operator-secret", {"http://127.0.0.1:57304"}))
        thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
        base = f"http://127.0.0.1:{server.server_port}"
        try:
            for headers, status in [({}, 401), ({"Authorization": "Bearer operator-secret", "Origin": "https://evil.example"}, 403)]:
                with self.assertRaises(urllib.error.HTTPError) as exc:
                    urllib.request.urlopen(urllib.request.Request(base + "/status", headers=headers))
                self.assertEqual(exc.exception.code, status)
            self.runtime.connections["model"]["api_key"] = "NEVER-RETURN-ME"
            request = urllib.request.Request(base + "/status", headers={"Authorization": "Bearer operator-secret"})
            with urllib.request.urlopen(request) as response:
                self.assertNotIn("NEVER-RETURN-ME", response.read().decode())
        finally:
            server.shutdown(); server.server_close(); thread.join()


class SandboxLiveTests(unittest.TestCase):
    @unittest.skipUnless(os.environ.get("MARKETFORGE_AGENT_SANDBOX_TEST") == "1", "set MARKETFORGE_AGENT_SANDBOX_TEST=1 for Docker acceptance")
    def test_isolation_limits_and_generated_python(self):
        sandbox = DockerSandbox()
        self.assertTrue(sandbox.check()["available"], "build scripts/agent-sandbox first")
        code = "def decide(observations, state):\n return {'actions': [], 'state': {'sum': sum([1, 2, 3])}}"
        self.assertEqual(sandbox.run(code, {}, {})["state"]["sum"], 6)
        for code in ["while True: pass", "open('/tmp/write-test', 'w').write('x')", "import socket; socket.create_connection(('1.1.1.1', 80), 1)"]:
            with self.assertRaises(ValueError):
                sandbox.run(code, {}, {})


if __name__ == "__main__":
    unittest.main()
