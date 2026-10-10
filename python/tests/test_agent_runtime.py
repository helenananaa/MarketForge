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

    def test_market_and_reduce_only_require_a_price_and_use_bounded_ioc(self):
        for action, variant in (("market", "PlaceImmediateOrCancel"), ("reduce_only", "PlaceReduceOnlyImmediateOrCancel")):
            args = self.buy(action=action)
            args.pop("price_tick")
            self.assertIn("price_tick is required", self.call("unbounded-" + action, "trade", args)["error"])
            self.call("bounded-" + action, "trade", self.buy(action=action, price_tick=101))
            self.assertEqual(self.exchange.orders[-1]["action"], {variant: {"side": "Buy", "qty": 2, "price_tick": 101}})
        self.assertEqual(len(self.exchange.orders), 2)

    def test_hedge_leg_reaches_bounded_and_unbounded_exchange_actions(self):
        self.call("open-long", "trade", self.buy(instrument="PERP", position_side="Long"))
        action = self.exchange.orders[-1]["action"]["PlaceProtected"]
        self.assertEqual((action["position_side"], action["order_type"], action["side"]), ("Long", "Limit", "Buy"))
        self.call("close-short", "trade", self.buy(instrument="PERP", action="reduce_only", position_side="Short"))
        action = self.exchange.orders[-1]["action"]["PlaceProtected"]
        self.assertEqual((action["position_side"], action["order_type"], action["reduce_only"]), ("Short", "ImmediateOrCancel", True))
        args = self.buy(instrument="PERP", action="market", side="Sell", position_side="Short", execution_mode="unbounded")
        args.pop("price_tick")
        self.call("open-short", "trade", args)
        action = self.exchange.orders[-1]["action"]["PlaceUnboundedMarket"]
        self.assertEqual((action["side"], action["position_side"]), ("Sell", "Short"))
        self.assertIn("position_side", self.call("invalid-leg", "trade", self.buy(position_side="invalid"))["error"])

    def test_unbounded_mode_is_explicit_and_maps_to_real_market_orders(self):
        for action, variant in (("market", "PlaceMarket"), ("reduce_only", "PlaceReduceOnlyMarket")):
            args = self.buy(action=action, execution_mode="unbounded")
            args.pop("price_tick")
            self.call("sweep-" + action, "trade", args)
            self.assertEqual(self.exchange.orders[-1]["action"], {variant: {"side": "Buy", "qty": 2}})
        bounded = self.buy(action="market", execution_mode="bounded")
        bounded.pop("price_tick")
        self.assertIn("price_tick is required", self.call("explicit-bounded", "trade", bounded)["error"])
        self.assertEqual(len(self.exchange.orders), 2)

    def test_unbounded_mode_preserves_optional_deadline_and_retry_after_restart(self):
        args = self.buy(action="market", execution_mode="unbounded", valid_until_market_time_ms=2500)
        args.pop("price_tick")
        self.exchange.lose_response = True
        with self.assertRaises(TimeoutError):
            self.call("sweep-lost", "trade", args)
        expected = {"PlaceUnboundedMarket": {"side": "Buy", "qty": 2, "reduce_only": False,
            "valid_until_market_time_ms": 2500}}
        self.assertEqual(self.exchange.orders[0]["action"], expected)
        self.runtime.store.db.close()
        self.runtime = Runtime(self.directory.name, PLUGIN, "http://unused", self.sandbox, lambda _: self.exchange)
        self.assertTrue(self.call("sweep-lost", "trade", args)["accepted"])
        self.assertEqual(len(self.exchange.orders), 1)
        self.assertFalse(self.runtime.store.pending("alice"))
        with self.assertRaisesRegex(ValueError, "different arguments"):
            self.call("sweep-lost", "trade", self.buy(action="market", execution_mode="bounded"))

    def test_unbounded_retry_without_deadline_is_not_mistaken_for_a_legacy_order(self):
        args = self.buy(action="market", execution_mode="unbounded")
        args.pop("price_tick")
        self.exchange.lose_response = True
        with self.assertRaises(TimeoutError):
            self.call("sweep-no-deadline", "trade", args)
        self.call("sweep-no-deadline", "trade", args)
        self.assertEqual(len(self.exchange.orders), 1)
        self.assertEqual(self.exchange.orders[0]["action"], {"PlaceMarket": {"side": "Buy", "qty": 2}})

    def test_invalid_modes_and_conflicting_price_fields_do_not_submit_orders(self):
        for index, mode in enumerate((None, True, "off", {}, [])):
            self.assertIn("execution_mode", self.call("bad-mode-" + str(index), "trade", self.buy(execution_mode=mode))["error"])
        self.assertIn("omit price_tick", self.call("conflicting-price", "trade", self.buy(action="market", execution_mode="unbounded"))["error"])
        for action in ("limit", "post_only", "ioc"):
            args = self.buy(action=action, execution_mode="unbounded")
            args.pop("price_tick")
            self.assertIn("only supported for market/reduce_only", self.call("bad-sweep-" + action, "trade", args)["error"])
        self.assertEqual(self.exchange.orders, [])

    def test_unbounded_child_strategy_still_shares_quantity_and_order_limits(self):
        args = self.buy(action="market", execution_mode="unbounded")
        args.pop("price_tick")
        config = self.runtime.config("alice")
        config["orders_per_minute"] = 1
        self.runtime.store.put("trader", "alice", config)
        self.sandbox.result["actions"] = [args]
        self.save()
        self.runtime.tick(config, self.runtime.strategies("alice")[0])
        self.assertEqual(self.exchange.orders[0]["action"], {"PlaceMarket": {"side": "Buy", "qty": 2}})
        self.assertIn("order budget exhausted", self.call("another-sweep", "trade", args)["error"])
        self.assertIn("error", self.call("oversized-sweep", "trade", {**args, "qty": config["max_order_qty"] + 1}))
        self.assertIn("scope", self.call("other-sweep", "trade", {**args, "instrument": "OTHER"})["error"])
        self.assertEqual(len(self.exchange.orders), 1)

    def test_deadlines_are_absolute_and_unchanged_after_unknown_outcome(self):
        args = self.buy(valid_until_market_time_ms=2500, expires_at_market_time_ms=5000)
        self.exchange.lose_response = True
        with self.assertRaises(TimeoutError):
            self.call("protected-retry", "trade", args)
        expected = {"PlaceProtected": {"side": "Buy", "qty": 2, "price_tick": 100, "order_type": "Limit",
            "reduce_only": False, "valid_until_market_time_ms": 2500, "expires_at_market_time_ms": 5000}}
        self.assertEqual(self.exchange.orders[0]["action"], expected)
        self.call("protected-retry", "trade", args)
        self.assertEqual(len(self.exchange.orders), 1)
        self.assertEqual(self.exchange.orders[0]["action"], expected)

    def test_legacy_unbounded_pending_order_is_not_silently_discarded_on_upgrade(self):
        args = self.buy(action="market")
        args.pop("price_tick")
        self.runtime.store.reserve("alice", "legacy", "trade", {"input": args, "source": "direct"})
        with self.assertRaisesRegex(UncertainOutcome, "legacy unbounded order outcome is unresolved"):
            self.call("legacy", "trade", args)
        self.assertEqual(len(self.runtime.store.pending("alice")), 1)
        self.assertEqual(self.exchange.orders, [])

    def test_bad_deadlines_fail_closed_and_cancellation_needs_no_price(self):
        for value in (True, -1, 0, 1.5, 2**53):
            self.assertIn("error", self.call("bad-" + str(value), "trade", self.buy(valid_until_market_time_ms=value)))
        for action in ("market", "ioc", "reduce_only"):
            self.assertIn("only limit/post_only", self.call("expiry-" + action, "trade",
                self.buy(action=action, expires_at_market_time_ms=5000))["error"])
        self.call("cancel-no-price", "trade", {"instrument": "SPOT", "action": "cancel", "order_id": 1})
        self.assertEqual(self.exchange.orders[-1]["action"], {"Cancel": {"order_id": 1}})

    def test_generated_strategy_uses_the_same_price_and_deadline_protection(self):
        self.sandbox.result["actions"] = [self.buy(action="reduce_only", instrument="PERP",
            side="Sell", price_tick=99, valid_until_market_time_ms=2500)]
        self.save()
        self.runtime.tick(self.runtime.config("alice"), self.runtime.strategies("alice")[0])
        action = self.exchange.orders[0]["action"]["PlaceProtected"]
        self.assertTrue(action["reduce_only"])
        self.assertEqual(action["order_type"], "ImmediateOrCancel")
        self.assertEqual(action["price_tick"], 99)
        self.assertEqual(action["valid_until_market_time_ms"], 2500)

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
