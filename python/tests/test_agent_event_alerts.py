import copy
import threading
import unittest
from http.server import ThreadingHTTPServer
from unittest.mock import patch

from marketforge import Client
from marketforge.agents.alerts import metric_value
from marketforge.agents.__main__ import handler
from marketforge.agents.mcp_server import ServiceClient, build_server
from marketforge.agents.runtime import Runtime
from python.tests import test_agent_runtime as fixture


class EventAlertTests(unittest.TestCase):
    setUp = fixture.RuntimeTests.setUp
    tearDown = fixture.RuntimeTests.tearDown
    call = fixture.RuntimeTests.call

    def install(self):
        self.head, self.pages, self.queries = 7, [], []
        original = self.exchange._request

        def request(method, path, *args, **kwargs):
            if not path.endswith("/market-events"): return original(method, path, *args, **kwargs)
            query = kwargs["query"]
            self.queries.append(copy.deepcopy(query))
            if "after_command_seq" not in query:
                return {"events": [], "latest_command_seq": self.head, "next_after_command_seq": self.head, "has_more": False}
            if self.pages: return self.pages.pop(0)
            return {"events": [], "latest_command_seq": self.head, "next_after_command_seq": query["after_command_seq"], "has_more": False}
        self.exchange._request = request
        config = self.runtime.config("alice")
        config["status"] = "running"
        self.runtime.store.put("trader", "alice", config)

    def alert(self, **updates):
        return {"name": "whale", "instrument": "PERP", "event_type": "liquidation", "min_notional": "10000000000000000000", **updates}

    def page(self, seq, count=1, has_more=False, event_type="LiquidationTrade"):
        self.head = max(self.head, seq)
        return {"events": [{"event_id": f"{seq}:{index}", "command_seq": seq, "market_time_ms": 1000,
            "event": {"type": event_type, "notional": "10000000000000000000"}} for index in range(count)],
            "next_after_command_seq": seq, "latest_command_seq": self.head, "has_more": has_more}

    def test_validation_units_scope_and_idempotent_registration(self):
        self.install()
        for i, args in enumerate([self.alert(instrument="OTHER"), self.alert(min_notional=True), self.alert(min_notional="1.5"),
            self.alert(min_notional=str(2**127)), self.alert(side="Long"), self.alert(event_type="funding"),
            self.alert(repeat=1), self.alert(priority="normal", pause_strategies=True)]):
            self.assertIn("error", self.call(f"bad-{i}", "event_alert_set", args))
        result = self.call("set", "event_alert_set", self.alert())
        self.head = 9
        self.assertEqual(self.call("set", "event_alert_set", self.alert()), result)
        self.assertEqual(result["after_command_seq"], 7)
        self.assertEqual(result["query"]["min_notional"], "10000000000000000000")
        self.assertEqual(len(self.runtime.alerts.list("alice")), 1)
        self.assertNotIn("account_id", self.queries[-1])

    def test_one_shot_skips_history_survives_empty_pages_and_restart(self):
        self.install()
        self.call("set", "event_alert_set", self.alert())
        # State polling must not match the event alert's empty conditions.
        self.runtime.alerts.poll("alice")
        self.assertEqual(self.runtime.alerts.state("alice")["generation"], 0)
        self.pages = [self.page(8, 0, True), self.page(9, 25)]
        self.runtime.alerts.poll_events("alice")
        self.assertEqual(self.runtime.alerts.list("alice")[0]["after_command_seq"], 8)
        self.assertFalse(self.runtime.alerts.state("alice")["pending"])
        token = self.runtime.alerts.token("alice", threading.Event())
        self.runtime.alerts.poll_events("alice")
        state = self.runtime.alerts.state("alice")
        self.assertTrue(token.is_set())
        self.assertTrue(self.runtime.wake_events["alice"].is_set())
        self.assertEqual(state["generation"], 1)
        self.assertEqual(state["pending"][0]["event_count"], 25)
        self.assertEqual(len(state["pending"][0]["evidence"]), 20)
        self.assertTrue(state["pending"][0]["evidence_truncated"])
        self.assertEqual(state["alerts"][0]["status"], "triggered")
        self.runtime.alerts.poll_events("alice")
        self.assertEqual(self.runtime.alerts.state("alice")["generation"], 1)
        self.runtime.store.db.close()
        self.runtime = Runtime(self.directory.name, fixture.PLUGIN, "http://unused", self.sandbox, lambda _: self.exchange)
        self.assertEqual(self.runtime.alerts.state("alice")["pending"], state["pending"])
        self.assertEqual(self.runtime.alerts.list("alice")[0]["after_command_seq"], 9)

    def test_repeat_cooldown_retains_backlog_and_normal_does_not_fence(self):
        self.install()
        self.call("set", "event_alert_set", self.alert(repeat=True, priority="normal", cooldown_seconds=5))
        self.runtime.store.put("external_control", "alice", {"decision_id": "active"})
        self.pages = [self.page(8), self.page(9)]
        token = self.runtime.alerts.token("alice", threading.Event())
        with patch("marketforge.agents.alerts.time.time", return_value=100):
            self.runtime.alerts.poll_events("alice")
        before = len(self.queries)
        with patch("marketforge.agents.alerts.time.time", return_value=102):
            self.runtime.alerts.poll_events("alice")
        self.assertEqual(len(self.queries), before)
        self.assertEqual(len(self.pages), 1)
        self.assertFalse(token.is_set())
        self.assertEqual(self.runtime.control("alice")["decision_id"], "active")
        self.assertFalse(self.runtime.wake_events["alice"].is_set())
        with patch("marketforge.agents.alerts.time.time", return_value=106):
            self.runtime.alerts.poll_events("alice")
        state = self.runtime.alerts.state("alice")
        self.assertEqual(state["generation"], 0)
        self.assertEqual(state["alerts"][0]["trigger_count"], 2)
        self.assertEqual(state["alerts"][0]["after_command_seq"], 9)
        self.runtime.invalidate_decision("alice")
        self.runtime.alerts.flush_normal("alice")
        self.assertTrue(self.runtime.wake_events["alice"].is_set())
        self.assertEqual(self.runtime.store.events("alice")[-1]["kind"], "alert_notification")

    def test_cancel_or_replace_during_read_discards_old_evidence(self):
        self.install()
        self.call("set", "event_alert_set", self.alert())
        original = self.runtime.read_market_events
        def cancelled(config, args):
            data = original(config, args)
            self.runtime.alerts.cancel("alice", "whale")
            return data
        self.runtime.read_market_events = cancelled
        self.pages = [self.page(8)]
        self.runtime.alerts.poll_events("alice")
        self.assertEqual(self.runtime.alerts.state("alice")["generation"], 0)
        self.assertEqual(self.runtime.alerts.list("alice")[0]["status"], "cancelled")
        self.runtime.read_market_events = original
        self.call("replace", "alert_set", {"name": "whale", "conditions": [{"instrument": "SPOT", "metric": "market_time_ms", "op": "gte", "value": 2000}]})
        self.runtime.alerts.poll("alice")
        self.assertEqual(self.runtime.alerts.state("alice")["generation"], 1)

    def test_stale_registration_and_network_failure_leave_cursor_unchanged(self):
        self.install()
        stop = threading.Event(); stop.set()
        with self.assertRaisesRegex(ValueError, "decision changed"):
            self.runtime.alerts.set_event(self.runtime.config("alice"), self.alert(), stop)
        self.assertFalse(self.runtime.alerts.list("alice"))
        self.call("set", "event_alert_set", self.alert())
        def failing(*args): raise TimeoutError("disconnected")
        self.runtime.read_market_events = failing
        with self.assertRaises(TimeoutError): self.runtime.alerts.poll_events("alice")
        self.assertEqual(self.runtime.alerts.list("alice")[0]["after_command_seq"], 7)

    def test_query_filters_and_funding_observation_metrics(self):
        self.install()
        self.call("query", "market_events", {"instrument": "PERP", "event_type": "funding", "max_rate_ppm": -100,
            "from_start": True, "start_market_time_ms": 1000, "end_market_time_ms": 2000})
        self.assertEqual(self.queries[-1]["max_rate_ppm"], -100)
        self.assertTrue(self.queries[-1]["from_start"])
        for i, args in enumerate([{"instrument": "OTHER"}, {"instrument": "PERP", "limit": 501},
            {"instrument": "PERP", "from_start": True, "after_command_seq": 0},
            {"instrument": "PERP", "event_type": "funding", "min_rate_ppm": 1, "max_rate_ppm": -1}]):
            self.assertIn("error", self.call(f"query-bad-{i}", "market_events", args))
        view = {"perp_price": {"index_price_tick": 101, "funding": {"estimated_rate_ppm": -100, "next_funding_time_ms": 3000}},
            "own_account": {"Perp": {"funding_pnl": -200}}}
        for key, value in [("index_price", 101), ("funding_rate_ppm", -100), ("next_funding_time_ms", 3000), ("funding_pnl", -200)]:
            self.assertEqual(metric_value(view, key), value)
            self.assertIsNone(metric_value({}, key))

    def test_http_and_native_mcp_schema_decision_fencing(self):
        import asyncio
        from mcp import types
        self.install()
        config = self.runtime.config("alice"); config["status"] = "paused"
        self.runtime.store.put("trader", "alice", config)
        self.runtime.migrate_external("alice")
        config = self.runtime.config("alice"); config["status"] = "running"
        self.runtime.store.put("trader", "alice", config)
        token = self.runtime.issue_access("alice")["token"]
        server = ThreadingHTTPServer(("127.0.0.1", 0), handler(self.runtime, "operator", set()))
        serving = threading.Thread(target=server.serve_forever); serving.start()
        try:
            client = ServiceClient(f"http://127.0.0.1:{server.server_port}", "alice", token)
            mcp = build_server(client)
            request = types.ListToolsRequest(method="tools/list")
            tools = asyncio.run(mcp.request_handlers[types.ListToolsRequest](request)).root.tools
            schemas = {t.name: t.inputSchema for t in tools}
            self.assertNotIn("decision_id", schemas["market_events"]["required"])
            self.assertIn("decision_id", schemas["event_alert_set"]["required"])
            query = types.CallToolRequest(method="tools/call", params=types.CallToolRequestParams(name="market_events", arguments={"instrument": "PERP"}))
            result = asyncio.run(mcp.request_handlers[types.CallToolRequest](query)).root
            self.assertFalse(result.isError)
            self.assertEqual(result.structuredContent["latest_command_seq"], 7)
            decision = client.call("decision_begin", {"generation": 0})
            fields = {"decision_id": decision["decision_id"], "generation": 0, "request_id": "event-set"}
            client.call("event_alert_set", {**self.alert(), **fields})
            self.pages = [self.page(8)]
            self.runtime.alerts.poll_events("alice")
            with self.assertRaisesRegex(ValueError, "lease is stale"):
                client.call("event_alert_set", {**self.alert(name="stale"), **fields, "request_id": "old"})
            context = client.call("context", {})
            self.assertEqual(context["generation"], 1)
            self.assertEqual(context["interrupts"][0]["event_count"], 1)
        finally:
            server.shutdown(); server.server_close(); serving.join(2)

    def test_sdk_iteration_scans_empty_pages_and_quotes_paths(self):
        client = Client("http://unused")
        calls = []
        pages = [{"events": [], "next_after_command_seq": 5, "has_more": True},
                 {"events": [{"event": {"type": "FundingSettlement"}}], "next_after_command_seq": 8, "has_more": False}]
        def request(method, path, query):
            calls.append((path, copy.deepcopy(query)))
            return pages.pop(0)
        client._request = request
        result = list(client.iter_market_events("room/a", "PERP/a", event_type="funding"))
        self.assertEqual(len(result), 2)
        self.assertIn("room%2Fa", calls[0][0])
        self.assertEqual(calls[1][1]["after_command_seq"], 5)
        self.assertNotIn("from_start", calls[1][1])
