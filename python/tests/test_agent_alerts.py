import json
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from unittest.mock import patch

from marketforge.agents.runtime import DecisionInterrupted, Runtime
from python.tests import test_agent_runtime as fixture


class AlertTests(unittest.TestCase):
    setUp = fixture.RuntimeTests.setUp
    call = fixture.RuntimeTests.call
    buy = fixture.RuntimeTests.buy
    save = fixture.RuntimeTests.save

    def tearDown(self):
        self.runtime.stop("alice")
        for _, threads in self.runtime.workers.values():
            for thread in threads:
                thread.join(3)
        self.runtime.store.db.close()
        self.directory.cleanup()

    def running(self):
        config = self.runtime.config("alice")
        config["status"] = "running"
        self.runtime.store.put("trader", "alice", config)

    def alert(self, **updates):
        return {"name": "watch", "conditions": [{"instrument": "SPOT", "metric": "market_time_ms", "op": "gte", "value": 2000}], **updates}

    def poll(self):
        self.runtime.alerts.poll("alice")

    def test_validation_scope_and_tool_receipt_replay(self):
        cases = [self.alert(conditions=[]), self.alert(conditions=[{"instrument": "OTHER", "metric": "best_bid", "op": "gt", "value": 1}]),
                 self.alert(repeat="yes"), self.alert(cooldown_seconds=1), self.alert(match="execute"),
                 self.alert(conditions=[{"instrument": "SPOT", "metric": "__import__", "op": "gt", "value": 1}]),
                 self.alert(conditions=[{"instrument": "SPOT", "metric": "cash_balance", "op": "gt", "value": True}])]
        for i, args in enumerate(cases):
            self.assertIn("error", self.call(str(i), "alert_set", args))
        args = self.alert()
        result = self.call("set", "alert_set", args)
        self.assertFalse(result["pause_strategies"])
        self.assertEqual(result, self.call("set", "alert_set", args))
        self.assertEqual(len(self.call("list", "alerts", {})), 1)

    def test_one_shot_persistent_context_and_cancellation(self):
        self.running()
        self.call("set", "alert_set", self.alert(reason="Reassess stale momentum"))
        self.poll(); self.poll()
        state = self.runtime.alerts.state("alice")
        self.assertEqual(state["generation"], 1)
        self.assertEqual(state["alerts"][0]["status"], "triggered")
        self.assertEqual(state["pending"][0]["conditions"][0]["observed"], 2000)
        self.call("cancel", "alert_cancel", {"name": "watch"})
        self.assertTrue(self.runtime.alerts.state("alice")["pending"])
        self.runtime.store.db.close()
        self.runtime = Runtime(self.directory.name, fixture.PLUGIN, "http://unused", self.sandbox, lambda _: self.exchange)
        self.assertEqual(self.runtime.config("alice")["status"], "paused")
        self.assertEqual(self.runtime.alerts.state("alice")["pending"], state["pending"])
        self.assertEqual(self.runtime.alerts.list("alice")[0]["status"], "cancelled")

    def test_repeat_requires_false_then_cooldown_and_missing_data_does_not_rearm(self):
        self.running()
        price = [101]
        observe = self.exchange.observe
        def snapshot(*args):
            data = observe(*args)
            data["observation"]["book"]["bids"] = [] if price[0] is None else [{"price_tick": price[0], "qty": 10}]
            return data
        self.exchange.observe = snapshot
        self.call("set", "alert_set", self.alert(repeat=True, cooldown_seconds=5,
            conditions=[{"instrument": "SPOT", "metric": "best_bid", "op": "gte", "value": 100}]))
        with patch("marketforge.agents.alerts.time.time", return_value=100):
            self.poll()
        with patch("marketforge.agents.alerts.time.time", return_value=110):
            self.poll()
            price[0] = None; self.poll()
            price[0] = 101; self.poll()
        self.assertEqual(self.runtime.alerts.state("alice")["generation"], 1)
        with patch("marketforge.agents.alerts.time.time", return_value=102):
            price[0] = 99; self.poll()
            price[0] = 101; self.poll()
        self.assertEqual(self.runtime.alerts.state("alice")["generation"], 1)
        with patch("marketforge.agents.alerts.time.time", return_value=106):
            self.poll()
        state = self.runtime.alerts.state("alice")
        self.assertEqual(state["generation"], 2)
        self.assertEqual(state["alerts"][0]["trigger_count"], 2)
        self.assertEqual(len(state["pending"]), 1)

    def test_price_account_all_any_and_empty_book(self):
        self.running()
        observe = self.exchange.observe
        def snapshot(*args):
            data = observe(*args)
            data["observation"].update(book={"bids": [{"price_tick": 100, "qty": 8}], "asks": [{"price_tick": 103, "qty": 2}]},
                own_account={"Spot": {"cash_balance": 1000, "available_cash": 900, "position_qty": 2}})
            return data
        self.exchange.observe = snapshot
        self.call("set", "alert_set", self.alert(conditions=[
            {"instrument": "SPOT", "metric": "spread", "op": "gte", "value": 3},
            {"instrument": "SPOT", "metric": "equity", "op": "eq", "value": 1200}]))
        self.call("any", "alert_set", self.alert(name="any", match="any", conditions=[
            {"instrument": "SPOT", "metric": "last_price", "op": "ne", "value": 1},
            {"instrument": "SPOT", "metric": "available_cash", "op": "lte", "value": 900}]))
        self.call("unknown", "alert_set", self.alert(name="missing", conditions=[
            {"instrument": "SPOT", "metric": "last_price", "op": "ne", "value": 1}]))
        self.poll()
        self.assertEqual({t["name"] for t in self.runtime.alerts.state("alice")["pending"]}, {"watch", "any"})

    def test_cancelled_alert_and_paused_trader_do_not_trigger(self):
        self.call("set", "alert_set", self.alert())
        self.poll()
        self.assertFalse(self.runtime.alerts.state("alice")["pending"])
        self.running()
        self.call("cancel", "alert_cancel", {"name": "watch"})
        self.poll()
        self.assertFalse(self.runtime.alerts.state("alice")["pending"])

    def test_monitor_failure_receipt_and_recovery(self):
        self.running()
        self.call("set", "alert_set", self.alert())
        stop = threading.Event()
        original = self.exchange.observe
        def failing(*args):
            raise TimeoutError("unavailable")
        self.exchange.observe = failing
        worker = threading.Thread(target=self.runtime.alerts.loop, args=("alice", stop))
        worker.start()
        try:
            self.until(lambda: any(e["kind"] == "alert_monitor_error" for e in self.runtime.store.events("alice")))
            self.exchange.observe = original
            self.until(lambda: bool(self.runtime.alerts.state("alice")["pending"]))
        finally:
            stop.set(); worker.join(2)
        self.assertIn("alert_monitor_recovered", [e["kind"] for e in self.runtime.store.events("alice")])

    def until(self, predicate, seconds=3):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(0.01)
        self.fail("expected event did not arrive")

    def test_slow_legacy_model_is_interrupted_and_reentry_has_trigger_context(self):
        entered, release, resumed = threading.Event(), threading.Event(), threading.Event()
        contexts = []
        def complete(_, messages, tools):
            context = json.loads(messages[1]["content"])
            contexts.append(context)
            if len(contexts) == 1:
                entered.set(); release.wait(5)
                return {"role": "assistant", "tool_calls": [{"id": "old", "function": {"name": "trade", "arguments": json.dumps(self.buy())}}]}, {}
            resumed.set()
            return {"role": "assistant", "tool_calls": [{"id": "new", "function": {"name": "wait", "arguments": '{"seconds":300}'}}]}, {}
        self.runtime.plugins["marketforge.llm-trader"][1].complete = complete
        self.call("set", "alert_set", self.alert())
        try:
            self.runtime.start("alice")
            self.assertTrue(entered.wait(2))
            self.assertTrue(resumed.wait(2), "must redecide without waiting for the old transport")
            self.assertTrue(contexts[1]["interrupts"])
            self.assertEqual(contexts[1]["interrupts"][0]["name"], "watch")
            self.assertEqual(contexts[1]["observations"]["SPOT"]["market_time_ms"], 2000)
        finally:
            release.set()
        self.until(lambda: all(e.is_set() for e in self.runtime.requests["alice"]))
        self.assertEqual(self.exchange.orders, [])
        self.assertEqual(self.runtime.config("alice")["model_calls"], 2)

    def test_wait_is_woken_and_old_batched_actions_are_abandoned(self):
        self.running()
        self.call("set", "alert_set", self.alert())
        def complete(*_):
            return {"role": "assistant", "tool_calls": [
                {"id": "n", "function": {"name": "note", "arguments": '{"text":"old plan"}'}},
                {"id": "t", "function": {"name": "trade", "arguments": json.dumps(self.buy())}}]}, {}
        self.runtime.plugins["marketforge.llm-trader"][1].complete = complete
        original = self.runtime.call
        def call(*args, **kwargs):
            result = original(*args, **kwargs)
            if args[2] == "note":
                self.poll()
            return result
        self.runtime.call = call
        self.runtime.round("alice", threading.Event())
        self.assertEqual(self.exchange.orders, [])
        self.assertTrue(self.runtime.wake_events["alice"].is_set())
        held = self.runtime.alerts.state("alice")["interrupted_plan"]
        self.assertEqual(held["remaining_actions"][0]["function"]["name"], "trade")
        self.assertEqual(json.loads(held["remaining_actions"][0]["function"]["arguments"]), self.buy())
        # New information need not change the goal/plan. A fresh decision may
        # explicitly continue with precisely the same trade proposal.
        contexts = []
        def continue_plan(_, messages, tools):
            contexts.append(json.loads(messages[1]["content"]))
            return {"role": "assistant", "content": "Continue the same plan", "tool_calls": [
                {"id": "t", "function": {"name": "trade", "arguments": json.dumps(self.buy())}},
                {"id": "w", "function": {"name": "wait", "arguments": '{"seconds":300}'}}]}, {}
        self.runtime.plugins["marketforge.llm-trader"][1].complete = continue_plan
        self.runtime.round("alice", threading.Event())
        self.assertEqual(len(self.exchange.orders), 1)
        self.assertEqual(contexts[0]["interrupted_plan"]["round"], held["round"])

    def test_alert_wakes_300_second_wait_without_changing_the_old_plan(self):
        now, contexts = [2000], []
        observe = self.exchange.observe
        def snapshot(*args):
            data = observe(*args)
            data["observation"]["market_time_ms"] = now[0]
            return data
        self.exchange.observe = snapshot
        def complete(_, messages, tools):
            contexts.append(json.loads(messages[1]["content"]))
            return {"role": "assistant", "content": "Keep accumulating patiently", "tool_calls": [
                {"id": "w", "function": {"name": "wait", "arguments": '{"seconds":300}'}}]}, {}
        self.runtime.plugins["marketforge.llm-trader"][1].complete = complete
        args = self.alert()
        args["conditions"][0]["value"] = 3000
        self.call("set", "alert_set", args)
        self.runtime.start("alice")
        self.until(lambda: any(e["kind"] == "tool_result" and e["data"]["name"] == "wait" for e in self.runtime.store.events("alice")))
        now[0] = 3000
        self.until(lambda: len(contexts) >= 2)
        self.assertEqual(contexts[1]["interrupted_plan"]["statement"], "Keep accumulating patiently")
        self.assertEqual(len(contexts), 2)
        self.assertEqual(self.runtime.config("alice")["status"], "running")

    def test_slow_research_can_be_abandoned_without_waiting_or_submitting_old_trade(self):
        self.running()
        self.call("set", "alert_set", self.alert())
        entered, release = threading.Event(), threading.Event()
        def search(_):
            entered.set(); release.wait(3)
            return {"data": "old research"}
        self.runtime.research.search = search
        self.runtime.plugins["marketforge.llm-trader"][1].complete = lambda *_: ({"role": "assistant", "tool_calls": [
            {"id": "s", "function": {"name": "web_search", "arguments": '{"query":"market"}'}},
            {"id": "t", "function": {"name": "trade", "arguments": json.dumps(self.buy())}}]}, {})
        errors = []
        def run():
            try:
                self.runtime.round("alice", threading.Event())
            except DecisionInterrupted:
                errors.append("interrupted")
        worker = threading.Thread(target=run); worker.start()
        try:
            self.assertTrue(entered.wait(2)); self.poll(); worker.join(1)
            self.assertFalse(worker.is_alive())
            self.assertEqual(errors, ["interrupted"])
            self.assertEqual(self.exchange.orders, [])
        finally:
            release.set(); worker.join(3)
            self.until(lambda: all(e.is_set() for e in self.runtime.requests["alice"]))

    def test_strategy_computation_does_not_block_monitor_and_late_actions_are_discarded(self):
        self.save(); self.running()
        self.call("set", "alert_set", self.alert(pause_strategies=True))
        entered, release = threading.Event(), threading.Event()
        def run(*_):
            entered.set(); release.wait(3)
            return {"actions": [self.buy()], "state": {"stale": True}}
        self.sandbox.run = run
        worker = threading.Thread(target=self.runtime.tick, args=(self.runtime.config("alice"), self.runtime.strategies("alice")[0]))
        worker.start()
        try:
            self.assertTrue(entered.wait(2)); self.poll()
            self.assertFalse(self.runtime.strategies("alice")[0]["running"])
        finally:
            release.set(); worker.join(3)
        self.assertEqual(self.exchange.orders, [])
        self.assertNotIn("stale", self.runtime.strategies("alice")[0]["state"])

    def test_interrupt_retains_unknown_trade_identity_and_drops_remaining_strategy_actions(self):
        self.sandbox.result["actions"] = [self.buy(), self.buy(instrument="PERP")]
        self.save(); self.running()
        self.exchange.lose_response = True
        with self.assertRaises(TimeoutError):
            self.runtime.tick(self.runtime.config("alice"), self.runtime.strategies("alice")[0])
        old_key = self.runtime.store.pending("alice")[0]["id"]
        self.call("set", "alert_set", self.alert(pause_strategies=False))
        self.poll()
        self.assertTrue(self.runtime.strategies("alice")[0]["running"])
        self.assertIsNone(self.runtime.strategies("alice")[0]["pending"])
        self.runtime.plugins["marketforge.llm-trader"][1].complete = lambda *_: ({"role": "assistant", "content": "replanned"}, {})
        self.runtime.round("alice", threading.Event())
        self.assertFalse(self.runtime.store.pending("alice"))
        self.assertEqual(len(self.exchange.orders), 1)
        self.assertEqual(self.runtime.store.reserve("alice", old_key, "trade", {"input": self.buy(), "source": "slice"})["status"], "done")

    def test_alert_between_strategy_orders_holds_only_the_unsent_action(self):
        self.sandbox.result["actions"] = [self.buy(), self.buy(instrument="PERP")]
        self.save(); self.running()
        self.call("set", "alert_set", self.alert())
        original = self.runtime.call
        def call(*args, **kwargs):
            result = original(*args, **kwargs)
            if args[2] == "trade" and args[3]["instrument"] == "SPOT":
                self.poll()
            return result
        self.runtime.call = call
        self.runtime.tick(self.runtime.config("alice"), self.runtime.strategies("alice")[0])
        self.assertEqual(len(self.exchange.orders), 1)
        strategy = self.runtime.strategies("alice")[0]
        self.assertTrue(strategy["running"])
        self.assertEqual(len(strategy["held_actions"]), 1)
        self.assertEqual(strategy["held_actions"][0]["action"]["instrument"], "PERP")
        self.assertEqual(strategy["held_actions"][0]["receipt"]["status"], "not_submitted")

    def test_stopped_strategy_cannot_submit_a_stale_pending_snapshot(self):
        self.sandbox.result["actions"] = [self.buy()]
        self.save()
        paused = threading.Event(); paused.set()
        self.runtime.tick(self.runtime.config("alice"), self.runtime.strategies("alice")[0], paused)
        old = self.runtime.strategies("alice")[0]
        self.call("stop", "strategy_stop", {"name": "slice", "cancel_orders": False})
        self.runtime.tick(self.runtime.config("alice"), old)
        self.assertEqual(self.exchange.orders, [])
        self.assertFalse(self.runtime.strategies("alice")[0]["running"])

    def test_repeated_interrupts_bound_old_requests_without_spending_queued_budget(self):
        releases = [threading.Event(), threading.Event()]
        contexts = []
        def complete(_, messages, tools):
            index = len(contexts)
            contexts.append(json.loads(messages[1]["content"]))
            if index < 2:
                releases[index].wait(5)
            return {"role": "assistant", "tool_calls": [{"id": "w", "function": {"name": "wait", "arguments": '{"seconds":300}'}}]}, {}
        self.runtime.plugins["marketforge.llm-trader"][1].complete = complete
        self.runtime.start("alice")
        try:
            self.until(lambda: len(contexts) == 1)
            self.call("a", "alert_set", self.alert(name="a")); self.poll()
            self.until(lambda: len(contexts) == 2)
            self.call("b", "alert_set", self.alert(name="b")); self.poll()
            self.until(lambda: len([e for e in self.runtime.store.events("alice") if e["kind"] == "model_response" and e["data"].get("discarded")]) == 2)
            self.assertEqual(self.runtime.config("alice")["model_calls"], 2)
            self.assertEqual(len([e for e in self.runtime.requests["alice"] if not e.is_set()]), 2)
            releases[0].set()
            self.until(lambda: len(contexts) == 3)
            self.assertEqual({t["name"] for t in contexts[2]["interrupts"]}, {"a", "b"})
            self.assertEqual(self.runtime.config("alice")["model_calls"], 3)
        finally:
            for release in releases:
                release.set()
            self.until(lambda: all(e.is_set() for e in self.runtime.requests["alice"]))

    def test_shipped_http_adapter_redecides_before_slow_reply(self):
        entered, resumed, release = threading.Event(), threading.Event(), threading.Event()
        requests = []
        class Provider(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass
            def do_POST(server):
                body = json.loads(server.rfile.read(int(server.headers["Content-Length"])))
                requests.append(body)
                if len(requests) == 1:
                    entered.set(); release.wait(4)
                    message = {"role": "assistant", "tool_calls": [{"id": "stale", "function": {"name": "trade", "arguments": json.dumps(self.buy())}}]}
                else:
                    resumed.set()
                    message = {"role": "assistant", "tool_calls": [{"id": "wait", "function": {"name": "wait", "arguments": '{"seconds":300}'}}]}
                raw = json.dumps({"choices": [{"message": message}]}).encode()
                try:
                    server.send_response(200); server.send_header("Content-Length", str(len(raw))); server.end_headers(); server.wfile.write(raw)
                except OSError:
                    pass
        server = ThreadingHTTPServer(("127.0.0.1", 0), Provider); server.daemon_threads = True
        serving = threading.Thread(target=server.serve_forever); serving.start()
        self.runtime.connections["model"]["base_url"] = f"http://127.0.0.1:{server.server_port}/v1"
        self.call("set", "alert_set", self.alert())
        try:
            self.runtime.start("alice")
            self.assertTrue(entered.wait(2)); self.assertTrue(resumed.wait(2))
            self.assertTrue(json.loads(requests[1]["messages"][1]["content"])["interrupts"])
            self.assertEqual(self.exchange.orders, [])
            release.set()
            self.until(lambda: all(e.is_set() for e in self.runtime.requests["alice"]))
            self.assertEqual(self.exchange.orders, [])
            self.assertEqual(len(requests), 2)
        finally:
            release.set(); self.runtime.stop("alice"); server.shutdown(); server.server_close(); serving.join(2)


if __name__ == "__main__":
    unittest.main()
