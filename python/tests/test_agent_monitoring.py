import json
from pathlib import Path
import tempfile
import threading
import time
import unittest

from marketforge.agents.connectors import Heartbeat, RpcError, TurnFailed, supervise
from python.tests import test_agent_external as fixture


class MonitoringTests(fixture.ExternalTests):
    # Reuse setup helpers, not the external suite itself.
    def attach(self, owner="bridge", epoch="epoch", **updates):
        result = self.client.request("connection", {"action": "attach", "owner_id": owner, "connection_id": epoch,
            "backend": "codex", "session_id": "same-thread", "state": "thinking", **updates})
        self.client.connection_id = epoch
        return result

    def test_expired_lease_rejects_new_orders_but_returns_old_receipts(self):
        decision = self.begin()
        args = self.action(decision)
        result = self.client.call("trade", args)
        control = self.service.control("alice")
        control["expires_at"] = time.time() - 1
        self.service.store.put("external_control", "alice", control)
        with self.assertRaisesRegex(ValueError, "stale"):
            self.client.call("trade", self.action(decision, "expired"))
        self.assertEqual(self.client.call("trade", args), result)
        self.service.check_liveness("alice")
        self.assertTrue(any(event["kind"] == "decision_expired" for event in self.service.store.events("alice")))
        fresh = self.begin()
        self.assertGreater(fresh["decision_expires_at"], time.time())
        self.assertTrue(self.client.call("trade", self.action(fresh, "fresh"))["accepted"])

    def test_crashed_connector_is_fenced_before_the_watchdog_runs(self):
        self.attach()
        decision = self.begin()
        connection = self.service.store.get("framework_connection", "alice")
        connection["expires_at"] = time.time() - 1
        self.service.store.put("framework_connection", "alice", connection)
        with self.assertRaisesRegex(ValueError, "stale"):
            self.client.call("trade", self.action(decision))
        with self.assertRaisesRegex(ValueError, "expired"):
            self.client.request("connection", {"action": "heartbeat", "owner_id": "bridge", "connection_id": "epoch"})
        self.service.check_liveness("alice")
        status = self.client.request("runtime")
        self.assertEqual(status["phase"], "offline")
        self.assertFalse(status["framework"]["online"])
        self.assertEqual(status["decision"]["plan"]["statement"], "Original plan")
        self.attach(epoch="replacement")
        self.client.connection_id = "epoch"
        with self.assertRaisesRegex(ValueError, "superseded"):
            self.begin()
        self.client.connection_id = "replacement"
        fresh = self.begin()
        with self.assertRaisesRegex(ValueError, "stale"):
            self.client.call("trade", self.action(decision, "old-after-reconnect"))
        self.assertTrue(self.client.call("trade", self.action(fresh, "new"))["accepted"])

    def test_connector_ownership_and_old_heartbeats_cannot_take_over(self):
        self.attach()
        with self.assertRaisesRegex(ValueError, "another live"):
            self.attach(owner="other")
        self.attach(epoch="replacement")
        with self.assertRaisesRegex(ValueError, "superseded"):
            self.client.request("connection", {"action": "heartbeat", "owner_id": "bridge", "connection_id": "epoch"})
        with self.assertRaisesRegex(ValueError, "superseded"):
            self.client.request("connection", {"action": "disconnect", "owner_id": "bridge", "connection_id": "epoch"})
        self.assertTrue(self.client.request("runtime")["framework"]["online"])

    def test_runtime_exposes_unknown_orders_and_safe_tool_status(self):
        self.attach()
        decision = self.begin()
        self.exchange.lose_response = True
        with self.assertRaises(ValueError):
            self.client.call("trade", self.action(decision, "unknown"))
        status = self.client.request("runtime")
        self.assertEqual(status["pending_orders"][0]["request_id"], "external:unknown")
        self.assertEqual(status["last_tool"]["status"], "error")
        self.assertEqual(status["phase"], "thinking")
        self.assertNotIn(self.token, json.dumps(status))
        self.client.request("connection", {"action": "heartbeat", "owner_id": "bridge", "connection_id": "epoch",
            "state": "idle", "delivery_seq": 5})
        self.assertEqual(self.client.request("runtime")["framework"]["last_delivery_seq"], 5)

    def test_last_alert_remains_visible_after_event_pagination(self):
        self.service.store.event("alice", "alert_triggered", {"name": "older-alert"})
        for _ in range(205):
            self.service.store.event("alice", "tool_result", {})
        self.assertEqual(self.client.request("runtime")["last_alert"]["data"]["name"], "older-alert")

    def test_graceful_disconnect_retains_unknown_order_reconciliation(self):
        self.attach()
        decision = self.begin(); args = self.action(decision)
        self.exchange.lose_response = True
        with self.assertRaises(ValueError):
            self.client.call("trade", args)
        self.client.request("connection", {"action": "disconnect", "owner_id": "bridge", "connection_id": "epoch",
            "state": "reconnecting", "error_code": "RpcError", "retry_count": 1})
        self.assertEqual(self.client.request("runtime")["phase"], "reconnecting")
        self.assertTrue(self.client.call("trade", args)["accepted"])
        self.assertEqual(len(self.exchange.orders), 1)
        with self.assertRaisesRegex(ValueError, "offline"):
            self.begin()


# unittest also finds inherited test methods: keep this module focused on new cases.
for _name in vars(fixture.ExternalTests):
    if _name.startswith("test_"):
        setattr(MonitoringTests, _name, None)


class SupervisorTests(unittest.TestCase):
    def test_new_delivery_during_heartbeat_is_not_lost(self):
        class Adapter:
            def health(self):
                return {"state": "idle"}
        class Client:
            def request(self, endpoint, payload):
                self.assertion = payload["delivery_seq"]
                heartbeat.delivered(2)
                heartbeat.stop.set()
        client = Client()
        heartbeat = Heartbeat(client, Adapter(), {"owner_id": "owner"}, "epoch")
        heartbeat.delivered(1)
        heartbeat.run()
        self.assertEqual(client.assertion, 1)
        self.assertEqual(heartbeat.delivery_seq, 2)

    def test_transport_restart_resumes_same_session_with_fresh_connection_epoch(self):
        stop = threading.Event()
        state = {"identity": {"backend": "codex"}, "session_id": "retained"}
        calls, adapters = [], []
        class Client:
            def request(self, endpoint, body=None):
                if endpoint == "model":
                    return {"allowed": True}
                if endpoint == "connection":
                    calls.append(dict(body)); return {}
                if "tail" in endpoint:
                    return [{"seq": 2}]
                if len(adapters) == 1:
                    raise OSError("broken transport")
                stop.set(); return []
            def call(self, name, args):
                return {"status": "running", "generation": 1}
        class Adapter:
            id, active = "retained", None
            def deliver(self, payload):
                pass
            def pump(self):
                pass
            def health(self):
                return {"state": "idle", "active_turn_id": None}
            def pause(self):
                pass
            def close(self):
                self.closed = True
        def factory():
            self.assertEqual(state["session_id"], "retained")
            adapter = Adapter(); adapters.append(adapter); return adapter
        with tempfile.TemporaryDirectory() as folder:
            supervise(Client(), factory, state, Path(folder) / "state.json", stop, retry_delay=0)
        attached = [call for call in calls if call["action"] == "attach"]
        self.assertEqual(len(attached), 2)
        self.assertNotEqual(attached[0]["connection_id"], attached[1]["connection_id"])
        self.assertEqual({call["session_id"] for call in attached}, {"retained"})
        self.assertTrue(all(adapter.closed for adapter in adapters))
        self.assertEqual(state["retry_count"], 1)

    def test_turn_timeout_interrupts_recovery_without_replaying_orders(self):
        class Adapter:
            active = "turn"
        heartbeat = Heartbeat(None, Adapter(), {}, "epoch", turn_timeout=10)
        heartbeat.active_since = time.monotonic() - 11
        with self.assertRaises(TurnFailed):
            heartbeat.check()
        heartbeat.failed.set()
        with self.assertRaises(RpcError):
            heartbeat.check()

    def test_failed_transport_retries_are_bounded_and_state_has_no_error_text(self):
        state = {"identity": {"backend": "codex"}}
        calls = []
        def factory():
            calls.append(1)
            raise RpcError("secret-provider-value")
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "state.json"
            with self.assertRaises(RpcError):
                supervise(None, factory, state, path, threading.Event(), max_retries=2, retry_delay=0)
            self.assertNotIn("secret-provider-value", path.read_text())
        self.assertEqual(len(calls), 3)
        self.assertEqual(state["connection_status"], "error")


if __name__ == "__main__":
    unittest.main()
