import copy
import json
import os
from pathlib import Path
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from http.server import ThreadingHTTPServer
from unittest.mock import patch

from marketforge.agents.runtime import TradingService
from marketforge.agents.connectors import deliver_model, sync_model_usage, CodexSession, OpenCodeSession
from marketforge.agents.__main__ import handler
from marketforge.agents.mcp_server import ServiceClient
from marketforge.agents.replay import isolated_exchange
from python.tests import test_agent_runtime as fixture
from python.tests import test_agent_connectors as connector_fixture


class BusinessTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.exchange = fixture.FakeExchange()
        self.service = TradingService(self.directory.name, None, "http://unused", fixture.FakeSandbox(), lambda _: self.exchange)
        config = self.service.create({"id": "alice", "room": "room", "account_id": 20, "instruments": ["SPOT"]})
        config["status"] = "running"
        self.service.store.put("trader", "alice", config)
        self.observed = {"status": "Running", "market_time_ms": 2000, "step": 2,
            "book": {"bids": [{"price_tick": 100, "qty": 10}], "asks": [{"price_tick": 101, "qty": 10}]},
            "own_account": {"Spot": {"account_id": 20, "cash_balance": 1000, "position_qty": 0}}, "own_orders": []}
        self.exchange.observe = lambda *args: {"observation": copy.deepcopy(self.observed)}

    def tearDown(self):
        self.service.store.db.close()
        self.directory.cleanup()

    def begin(self):
        return self.service.external_call("alice", "decision_begin", {"generation": self.service.alerts.state("alice")["generation"]})

    def call(self, key, name, args):
        return self.service.call("alice", key, name, args)

    def buy(self, **args):
        return {"instrument": "SPOT", "action": "limit", "side": "Buy", "price_tick": 100, "qty": 2, **args}

    def policy(self, account=None, model=None):
        return self.service.policy_update("alice", {"account": {"SPOT": account} if account else {}, "model": model or {}})

    def alert(self, **args):
        return {"name": "watch", "conditions": [{"instrument": "SPOT", "metric": "best_bid", "op": "gte", "value": 100}], **args}

    def test_amend_validation_and_strategy_ownership(self):
        self.service.store.put("order_owner", "alice:SPOT:1", "slice")
        args = {"instrument": "SPOT", "action": "amend", "order_id": 1, "qty": 1}
        result = self.service.call("alice", "amend", "trade", args, "slice")
        self.assertTrue(result["accepted"])
        self.assertEqual(self.exchange.orders[-1]["action"], {"Amend": {"order_id": 1, "price_tick": None, "qty": 1}})
        self.assertEqual(result, self.service.call("alice", "amend", "trade", args, "slice"))
        self.assertIn("error", self.service.call("alice", "foreign", "trade", args, "other"))
        self.assertIn("error", self.call("empty", "trade", {"instrument": "SPOT", "action": "amend", "order_id": 1}))
        self.assertIn("error", self.call("zero", "trade", {**args, "qty": 0}))
        self.assertEqual(self.service.store.get("order_owner", "alice:SPOT:1"), "slice")

    def test_hedge_policy_counts_gross_legs_and_allows_only_selected_leg_reduction(self):
        self.observed["own_account"] = {"Perp": {"account_id": 20, "position_qty": 0, "equity": 1000,
            "hedge_positions": {"long": {"qty": 5}, "short": {"qty": 5}}}}
        self.policy(account={"max_abs_position": 10})
        self.assertIn("position limit", self.call("hedge-open", "trade", self.buy(position_side="Short", side="Sell"))["error"])
        result = self.call("hedge-close", "trade", self.buy(position_side="Long", side="Sell", action="ioc"))
        self.assertTrue(result["accepted"])
        self.observed["own_orders"] = [{"order_id": 1, "side": "Buy", "position_side": "Long", "remaining_qty": 2, "price_tick": 100}]
        self.assertIn("position limit", self.call("hedge-extra", "trade", self.buy(position_side="Long"))["error"])

    def test_cancel_batch_settles_lost_child_but_holds_unsent_after_interrupt(self):
        self.observed["own_orders"] = [{"order_id": 1}, {"order_id": 2}]
        decision = self.begin()
        args = {"request_id": "cancel-many", "decision_id": decision["decision_id"], "generation": 0, "instrument": "SPOT"}
        self.exchange.lose_response = True
        with self.assertRaises(TimeoutError):
            self.service.external_call("alice", "order_cancel_all", args)
        self.service.invalidate_decision("alice")
        # A new order after the snapshot must never be included on retry.
        self.observed["own_orders"].append({"order_id": 3})
        result = self.service.external_call("alice", "order_cancel_all", args)
        self.assertEqual(result["order_ids"], [1, 2])
        self.assertEqual(result["held_order_ids"], [2])
        self.assertEqual(len(self.exchange.orders), 1)
        self.assertFalse(result["complete"])
        self.assertEqual(result, self.service.external_call("alice", "order_cancel_all", args))

    def test_batch_strategy_snapshot_excludes_untracked_orders(self):
        self.observed["own_orders"] = [{"order_id": 1}, {"order_id": 2}]
        self.service.store.put("order_owner", "alice:SPOT:2", "slice")
        result = self.call("batch", "order_cancel_all", {"instrument": "SPOT", "strategy": "slice"})
        self.assertEqual(result["order_ids"], [2])

    def test_history_filters_own_account_and_redacts_counterpart(self):
        self.service.store.put("order_owner", "alice:SPOT:2", "slice")
        self.exchange._request = lambda *a, **k: {"trades": [
            {"instrument_id": "SPOT", "maker_account_id": 10, "taker_account_id": 20, "maker_order_id": 1, "taker_order_id": 2, "qty": 1},
            {"instrument_id": "SPOT", "maker_account_id": 10, "taker_account_id": 11, "maker_order_id": 3, "taker_order_id": 4}]}
        result = self.call("fills", "fills", {"instrument": "SPOT", "strategy": "slice"})
        self.assertEqual(len(result["trades"]), 1)
        self.assertNotIn("maker_account_id", result["trades"][0])
        self.assertEqual(result["trades"][0]["sources"], {"2": "slice"})

    def test_optional_disabled_allows_unbounded_and_quantity_policy_counts_pending(self):
        self.assertIsNone(self.service.policy_status("alice")["model_usage"]["estimated_cost_microusd"])
        self.assertTrue(self.call("sweep", "trade", {"instrument": "SPOT", "action": "market", "side": "Buy", "qty": 20, "execution_mode": "unbounded"})["accepted"])
        self.policy({"max_abs_position": 5})
        self.observed["own_orders"] = [{"order_id": 1, "side": "Buy", "price_tick": 100, "remaining_qty": 4}]
        self.assertIn("position limit", self.call("more", "trade", self.buy())["error"])
        self.assertTrue(self.call("smaller", "trade", {"instrument": "SPOT", "action": "amend", "order_id": 1, "qty": 3})["accepted"])
        self.assertIn("position limit", self.call("increase", "trade", {"instrument": "SPOT", "action": "amend", "order_id": 1, "qty": 6})["error"])

    def test_loss_reference_survives_changes_and_reduction_allowed(self):
        self.policy({"max_loss": 100})
        reference = self.service.policy_status("alice")["loss_reference"]
        self.observed["own_account"]["Spot"].update(cash_balance=700, position_qty=1)
        self.assertIn("loss limit", self.call("loss", "trade", self.buy())["error"])
        self.assertTrue(self.call("exit", "trade", self.buy(side="Sell", qty=1))["accepted"])
        self.policy({"max_loss": 200})
        self.assertEqual(reference, self.service.policy_status("alice")["loss_reference"])
        self.policy()
        self.assertFalse(self.service.policy_status("alice")["loss_reference"])

    def test_leverage_limit_uses_worst_resting_price_and_unbounded_opt_out(self):
        self.policy({"max_leverage_bps": 1000})
        self.assertIn("leverage limit", self.call("lever", "trade", self.buy())["error"])
        self.assertIn("bounded execution", self.call("unbounded", "trade", {"instrument": "SPOT", "action": "market", "side": "Buy", "qty": 1, "execution_mode": "unbounded"})["error"])
        self.policy()
        self.assertTrue(self.call("enabled", "trade", self.buy())["accepted"])

    def test_unknown_trade_reconciliation_does_not_reprice_under_new_policy(self):
        self.exchange.lose_response = True
        with self.assertRaises(TimeoutError):
            self.call("original", "trade", self.buy())
        self.policy({"max_abs_position": 1})
        self.assertTrue(self.call("original", "trade", self.buy())["accepted"])
        self.assertEqual(len(self.exchange.orders), 1)

    def test_reserved_but_never_submitted_order_cannot_escape_stale_fence(self):
        decision = self.begin()
        args = self.buy()
        self.service.store.reserve("alice", "external:never-sent", "trade", {"input": args, "source": "direct"})
        self.service.invalidate_decision("alice")
        result = self.service.external_call("alice", "trade", {**args, "request_id": "never-sent", "decision_id": decision["decision_id"], "generation": 0})
        self.assertIn("not submitted", result["error"])
        self.assertFalse(self.exchange.orders)

    def test_resume_holds_reserved_unsent_order(self):
        self.service.store.reserve("alice", "external:never-sent", "trade", {"input": self.buy(), "source": "direct"})
        self.service.start("alice")
        try:
            self.assertFalse(self.exchange.orders)
            self.assertIn("not submitted", self.service.store.receipt("alice", "external:never-sent")["result"]["error"])
        finally:
            self.service.stop("alice")
            for thread in self.service.workers["alice"][1]: thread.join(2)

    def test_pre_tracking_unknown_receipt_remains_uncertain(self):
        from marketforge.agents.runtime import UncertainOutcome
        decision = self.begin()
        args = self.buy()
        self.service.store.reserve("alice", "external:legacy-unknown", "trade", {"input": args, "source": "direct"})
        with self.service.store.lock, self.service.store.db:
            self.service.store.db.execute("DELETE FROM objects WHERE kind='exchange_reservation' AND id='alice:external:legacy-unknown'")
        self.service.invalidate_decision("alice")
        with self.assertRaises(UncertainOutcome):
            self.service.external_call("alice", "trade", {**args, "request_id": "legacy-unknown", "decision_id": decision["decision_id"], "generation": 0})
        self.assertEqual(self.service.store.receipt("alice", "external:legacy-unknown")["status"], "pending")
        self.assertFalse(self.exchange.orders)

    def test_policy_meter_and_loss_reference_survive_service_restart(self):
        self.policy({"max_loss": 100}, {"max_tokens": 50})
        self.service.connection_update("alice", {"action": "attach", "owner_id": "owner", "connection_id": "epoch", "backend": "codex", "session_id": "session"})
        usage = {"action": "usage", "connection_id": "epoch", "session_id": "session", "meter_id": "thread_total", "tokens": 30}
        self.service.model_control("alice", usage)
        before = self.service.policy_status("alice")
        self.service.store.db.close()
        self.service = TradingService(self.directory.name, None, "http://unused", fixture.FakeSandbox(), lambda _: self.exchange)
        self.assertEqual(before, self.service.policy_status("alice"))
        self.service.connection_update("alice", {"action": "attach", "owner_id": "owner", "connection_id": "new_epoch", "backend": "codex", "session_id": "session"})
        self.service.model_control("alice", {**usage, "connection_id": "new_epoch"})
        self.assertEqual(before, self.service.policy_status("alice"))

    def test_policy_http_operator_only_and_scoped_model_meter(self):
        operator = "operator-fixture-token-very-long"
        token = self.service.issue_access("alice")["token"]
        server = ThreadingHTTPServer(("127.0.0.1", 0), handler(self.service, operator, set()))
        thread = threading.Thread(target=server.serve_forever); thread.start()
        url = f"http://127.0.0.1:{server.server_port}"
        try:
            client = ServiceClient(url, "alice", token)
            request = urllib.request.Request(url + "/traders/alice/policy", data=b'{"account":{},"model":{"max_admissions":1}}',
                headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"})
            with self.assertRaises(urllib.error.HTTPError) as denied:
                urllib.request.urlopen(request)
            self.assertEqual(denied.exception.code, 401)
            request.add_header("Authorization", "Bearer " + operator)
            with urllib.request.urlopen(request) as response:
                result = json.load(response)
            self.assertEqual(result["rules"]["model"], {"max_admissions": 1})
            self.assertEqual(client.call("policy_status", {})["rules"], result["rules"])
            client.request("connection", {"action": "attach", "owner_id": "owner", "connection_id": "epoch", "backend": "codex", "session_id": "session"})
            body = {"action": "admit", "connection_id": "epoch", "session_id": "session", "request_id": "wake"}
            self.assertTrue(client.request("model", body)["allowed"])
            self.assertFalse(client.request("model", {**body, "request_id": "next"})["allowed"])
        finally:
            server.shutdown(); server.server_close(); thread.join(2)

    def test_normal_alert_queues_without_revoking_and_batches_after_wait(self):
        decision = self.begin()
        self.call("alert", "alert_set", self.alert(priority="normal"))
        self.service.alerts.poll("alice")
        self.assertTrue(self.service.lease_live("alice", decision["decision_id"], 0))
        self.assertEqual(self.service.alerts.state("alice")["generation"], 0)
        self.assertEqual(len(self.service.context("alice")["notifications"]), 1)
        self.assertIsNone(self.service.store.last_event("alice", "alert_notification"))
        self.service.invalidate_decision("alice")
        self.service.alerts.poll("alice"); self.service.alerts.poll("alice")
        events = [e for e in self.service.store.events("alice") if e["kind"] == "alert_notification"]
        self.assertEqual(len(events), 1)
        next_round = self.begin()
        self.assertEqual(len(next_round["notifications"]), 1)
        self.assertFalse(self.service.context("alice")["notifications"])

    def test_sustained_condition_missing_data_breaks_proof(self):
        self.call("alert", "alert_set", self.alert(sustain_seconds=3))
        with patch("marketforge.agents.alerts.time.time", return_value=100):
            self.service.alerts.poll("alice")
        self.observed["book"]["bids"] = []
        with patch("marketforge.agents.alerts.time.time", return_value=102):
            self.service.alerts.poll("alice")
        self.observed["book"]["bids"] = [{"price_tick": 100, "qty": 1}]
        with patch("marketforge.agents.alerts.time.time", return_value=104):
            self.service.alerts.poll("alice")
        self.assertEqual(self.service.alerts.state("alice")["generation"], 0)
        with patch("marketforge.agents.alerts.time.time", return_value=107):
            self.service.alerts.poll("alice")
        self.assertEqual(self.service.alerts.state("alice")["generation"], 1)

    def test_hysteresis_rearm_and_global_interrupt_spacing(self):
        self.call("alert", "alert_set", self.alert(repeat=True, cooldown_seconds=2, hysteresis=3, interrupt_min_interval_seconds=10))
        for now, price in ((100, 100), (103, 99), (104, 100), (105, 96), (106, 100)):
            self.observed["book"]["bids"][0]["price_tick"] = price
            with patch("marketforge.agents.alerts.time.time", return_value=now):
                self.service.alerts.poll("alice")
        self.assertEqual(self.service.alerts.state("alice")["generation"], 1)
        with patch("marketforge.agents.alerts.time.time", return_value=111):
            self.service.alerts.poll("alice")
        self.assertEqual(self.service.alerts.state("alice")["generation"], 2)

    def test_model_admissions_and_usage_dedup_survive_reconnect(self):
        self.policy(model={"max_admissions": 1, "max_tokens": 10, "max_estimated_cost_microusd": 100, "microusd_per_million_tokens": 1000000})
        self.service.connection_update("alice", {"action": "attach", "owner_id": "owner", "connection_id": "epoch", "backend": "codex", "session_id": "session"})
        identity = {"connection_id": "epoch", "session_id": "session"}
        first = self.service.model_control("alice", {**identity, "action": "admit", "request_id": "wake"})
        self.assertTrue(first["allowed"])
        self.assertEqual(first, self.service.model_control("alice", {**identity, "action": "admit", "request_id": "wake"}))
        usage = {**identity, "action": "usage", "meter_id": "thread_total", "tokens": 4}
        self.assertTrue(self.service.model_control("alice", usage)["allowed"])
        self.service.model_control("alice", usage)
        self.assertFalse(self.service.model_control("alice", {**identity, "action": "admit", "request_id": "next"})["allowed"])
        self.service.connection_update("alice", {"action": "attach", "owner_id": "owner", "connection_id": "new_epoch", "backend": "codex", "session_id": "session"})
        identity["connection_id"] = "new_epoch"
        result = self.service.model_control("alice", {**usage, **identity, "tokens": 12})
        self.assertFalse(result["allowed"])
        self.assertEqual(result["status"]["model_usage"]["tokens"], 12)
        self.assertEqual(result["status"]["model_usage"]["estimated_cost_microusd"], 12)
        with self.assertRaisesRegex(ValueError, "regressed"):
            self.service.model_control("alice", {**usage, **identity, "tokens": 8})

    def test_connector_denies_delivery_and_stops_turn_after_reported_overshoot(self):
        class Client:
            def request(self, endpoint, body):
                return {"allowed": False}
        class Adapter:
            id = "session"
            usage_reports = {"thread_total": 50}
            pauses = 0
            delivered = False
            def pump(self): pass
            def pause(self): self.pauses += 1
            def deliver(self, payload): self.delivered = True
        adapter = Adapter()
        state = {"connection_id": "epoch"}
        self.assertFalse(deliver_model(Client(), adapter, state, {}))
        self.assertFalse(adapter.delivered)
        self.assertEqual(adapter.pauses, 2)
        self.assertFalse(adapter.usage_reports)

    def test_codex_cumulative_and_opencode_per_message_meter_shapes(self):
        adapter = connector_fixture.ConnectorTests().codex()
        adapter.rpc.events.put({"method": "thread/tokenUsage/updated", "params": {"threadId": adapter.id, "tokenUsage": {"total": {"totalTokens": 20}}}})
        adapter.pump()
        self.assertEqual(adapter.usage_reports, {"thread_total": 20})
        opencode = OpenCodeSession.__new__(OpenCodeSession)
        opencode.id, opencode.delivered_at = "session", 0
        opencode.request = lambda path: {"session": {"type": "busy"}} if path == "/session/status" else [
            {"info": {"id": "msg1", "role": "assistant", "tokens": {"input": 10, "output": 3, "reasoning": 2, "cache": {"read": 4, "write": 1}}}},
            {"info": {"id": "msg2", "role": "assistant", "tokens": {"input": 5, "output": 1}}}]
        opencode.pump()
        self.assertEqual(opencode.usage_reports, {"msg1": 20, "msg2": 6})
        class Client:
            def request(self, endpoint, body): return {"allowed": True}
        sync_model_usage(Client(), opencode, {"connection_id": "epoch"})
        opencode.pump()
        self.assertFalse(opencode.usage_reports)

    @unittest.skipUnless(os.environ.get("MARKETFORGE_REPLAY_EXCHANGE_TEST") == "1", "set MARKETFORGE_REPLAY_EXCHANGE_TEST=1 for isolated real exchange")
    def test_real_exchange_amend_fills_cancel_batch_and_optional_limits(self):
        root = Path(__file__).resolve().parents[2]
        scenario = json.loads((root / "scripts/fixtures/f6_batch_spec.json").read_text())["scenario"]
        instrument, room = scenario["market"]["Spot"]["instrument"]["instrument_id"], scenario["room_id"]
        with isolated_exchange(root / "target/debug/exchange-server.exe") as admin, tempfile.TemporaryDirectory() as directory:
            admin._request("POST", "/rooms", {"scenario": scenario, "autostart_agents": False})
            admin.add_member(room, "agent-real", "trader"); admin.assign_account(room, 20, "agent-real")
            service = TradingService(directory, None, admin.base_url)
            config = service.create({"id": "real", "room": room, "account_id": 20, "instruments": [instrument]})
            config["status"] = "running"; service.store.put("trader", "real", config)
            def call(key, name, args): return service.call("real", key, name, args)
            try:
                receipt = call("place", "trade", {"instrument": instrument, "action": "limit", "side": "Buy", "price_tick": 97, "qty": 3})
                oid = next(event["order_id"] for event in receipt["events"] if event["type"] == "OrderAccepted")
                amended = call("amend", "trade", {"instrument": instrument, "action": "amend", "order_id": oid, "price_tick": 96, "qty": 2})
                self.assertTrue(any(event["type"] == "OrderAmended" for event in amended["events"]))
                history = call("history", "orders", {"instrument": instrument, "order_id": oid})["orders"]
                self.assertEqual(history[0]["remaining_qty"], 2)
                self.assertEqual(history[0]["limit_price_tick"], 96)
                rejected = call("aggressive-amend", "trade", {"instrument": instrument, "action": "amend", "order_id": oid, "price_tick": 98})
                self.assertTrue(any(event["type"] == "AmendRejected" for event in rejected["events"]))
                self.assertEqual(history[0]["sources"], {str(oid): "direct"})
                fill = call("buy", "trade", {"instrument": instrument, "action": "ioc", "side": "Buy", "price_tick": 101, "qty": 1})
                self.assertTrue(fill["accepted"])
                executions = call("fills", "fills", {"instrument": instrument})["trades"]
                self.assertEqual(len(executions), 1)
                self.assertEqual(executions[0]["qty"], 1)
                service.policy_update("real", {"account": {instrument: {"max_abs_position": 3}}, "model": {}})
                blocked = call("risk", "trade", {"instrument": instrument, "action": "limit", "side": "Buy", "price_tick": 98, "qty": 1})
                self.assertIn("position limit", blocked["error"])
                cancelled = call("cancel", "order_cancel_all", {"instrument": instrument})
                self.assertTrue(cancelled["complete"])
                self.assertEqual(cancelled["order_ids"], [oid])
                self.assertFalse(service.observation(config, instrument)["own_orders"])
                self.assertEqual(cancelled, call("cancel", "order_cancel_all", {"instrument": instrument}))
                service.policy_update("real", {"account": {}, "model": {}})
                self.assertTrue(call("sweep", "trade", {"instrument": instrument, "action": "market", "side": "Buy", "execution_mode": "unbounded", "qty": 1})["accepted"])
                self.assertEqual(service.observation(config, instrument)["own_account"]["Spot"]["position_qty"], 2)
            finally:
                service.store.db.close()
