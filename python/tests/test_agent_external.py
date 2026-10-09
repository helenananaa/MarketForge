import asyncio
import json
import os
import socket
import subprocess
from pathlib import Path
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.request
from http.server import ThreadingHTTPServer

from marketforge.agents.runtime import TradingService
from marketforge.agents.__main__ import handler
from marketforge.agents.mcp_server import ServiceClient
from python.tests import test_agent_runtime as fixture


class ExternalTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.exchange, self.sandbox = fixture.FakeExchange(), fixture.FakeSandbox()
        self.service = TradingService(self.directory.name, None, "http://unused", self.sandbox, lambda _: self.exchange)
        self.service.create({"id": "alice", "room": "room", "account_id": 20, "instruments": ["SPOT", "PERP"], "prompt": "Keep the same goal"})
        self.operator = "operator-fixture-token-very-long"
        self.token = self.service.issue_access("alice")["token"]
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), handler(self.service, self.operator, set()))
        self.server.daemon_threads = True
        self.serving = threading.Thread(target=self.server.serve_forever); self.serving.start()
        self.client = ServiceClient(f"http://127.0.0.1:{self.server.server_port}", "alice", self.token)
        self.service.start("alice")

    def tearDown(self):
        for trader in self.service.store.all("trader"):
            self.service.stop(trader["id"])
        for _, threads in self.service.workers.values():
            for thread in threads:
                thread.join(3)
        self.server.shutdown(); self.server.server_close(); self.serving.join(2)
        self.service.store.db.close(); self.directory.cleanup()

    def begin(self):
        return self.client.call("decision_begin", {"generation": self.client.call("context", {})["generation"], "plan": "Original plan"})

    def action(self, decision, request="order", **kwargs):
        return {"decision_id": decision["decision_id"], "generation": decision["generation"], "request_id": request,
            "instrument": "SPOT", "action": "limit", "side": "Buy", "price_tick": 100, "qty": 2, **kwargs}

    def trigger(self, decision):
        self.client.call("alert_set", {"decision_id": decision["decision_id"], "generation": decision["generation"], "request_id": "alert",
            "name": "watch", "conditions": [{"instrument": "SPOT", "metric": "market_time_ms", "op": "gte", "value": 2000}]})
        self.service.alerts.poll("alice")

    def test_model_free_start_scope_and_superseding_decisions(self):
        self.assertFalse(hasattr(self.service, "model_loop"))
        self.assertFalse(self.service.plugins)
        self.assertEqual(self.service.config("alice")["model_calls"], 0)
        a = self.begin(); b = self.begin()
        with self.assertRaisesRegex(ValueError, "lease is stale"):
            self.client.call("trade", self.action(a))
        self.assertTrue(self.client.call("trade", self.action(b))["accepted"])
        self.assertIn("error", self.client.call("trade", self.action(b, "foreign", instrument="OTHER")))
        self.assertEqual({o["account_id"] for o in self.exchange.orders}, {20})

    def test_alert_fences_new_actions_but_keeps_completed_receipts_and_plan(self):
        old = self.begin()
        args = self.action(old)
        receipt = self.client.call("trade", args)
        self.trigger(old)
        with self.assertRaisesRegex(ValueError, "lease is stale"):
            self.client.call("trade", self.action(old, "stale"))
        self.assertEqual(self.client.call("trade", args), receipt)
        self.assertEqual(len(self.exchange.orders), 1)
        context = self.client.call("context", {})
        self.assertEqual(context["generation"], 1)
        self.assertEqual(context["previous_plan"]["statement"], "Original plan")
        self.assertEqual(context["goal"], "Keep the same goal")
        self.assertEqual(context["interrupts"][0]["name"], "watch")
        fresh = self.begin()
        self.assertEqual(fresh["generation"], 1)
        self.assertTrue(self.client.call("trade", self.action(fresh, "continue"))["accepted"])
        self.assertFalse(self.client.call("context", {})["interrupts"])

    def test_unknown_order_reconciliation_keeps_original_identity_after_interrupt(self):
        decision = self.begin()
        args = self.action(decision)
        self.exchange.lose_response = True
        with self.assertRaisesRegex(ValueError, "TimeoutError"):
            self.client.call("trade", args)
        self.assertEqual(self.client.call("receipt", {"request_id": "order"})["status"], "pending")
        self.trigger(decision)
        self.begin()
        self.assertEqual(len(self.exchange.orders), 1)
        self.assertEqual(self.client.call("receipt", {"request_id": "order"})["status"], "done")
        self.assertTrue(self.client.call("trade", args)["accepted"])
        with self.assertRaisesRegex(ValueError, "reused"):
            self.client.call("trade", {**args, "qty": 3})

    def test_unknown_order_can_be_settled_while_paused_but_new_orders_cannot(self):
        decision = self.begin(); args = self.action(decision)
        self.exchange.lose_response = True
        with self.assertRaises(ValueError):
            self.client.call("trade", args)
        self.service.stop("alice")
        self.assertTrue(self.client.call("trade", args)["accepted"])
        with self.assertRaises(ValueError):
            self.client.call("trade", self.action(decision, "new"))
        self.assertEqual(len(self.exchange.orders), 1)

    def test_scoped_auth_rotation_and_operator_separation(self):
        self.service.create({"id": "bob", "room": "room", "account_id": 30, "instruments": ["SPOT"]})
        for path in ("/traders", "/traders/alice/start", "/tools/bob/schema"):
            req = urllib.request.Request(self.client.url + path, headers={"Authorization": "Bearer " + self.token})
            with self.assertRaises(urllib.error.HTTPError) as error:
                urllib.request.urlopen(req)
            self.assertEqual(error.exception.code, 401)
        self.service.issue_access("alice")
        with self.assertRaises(ValueError):
            self.client.call("context", {})
        stored = self.service.store.get("tool_access", "alice")
        self.assertNotEqual(stored, self.token)
        self.assertNotIn(self.token, json.dumps(self.service.store.events("alice")))

    def test_wait_schedules_account_wakeup_and_closes_lease(self):
        decision = self.begin()
        result = self.client.call("wait", {"decision_id": decision["decision_id"], "generation": 0,
            "request_id": "sleep", "seconds": 300, "on_account_change": True})
        self.assertEqual(result["seconds"], 300)
        with self.assertRaises(ValueError):
            self.client.call("trade", self.action(decision))
        self.exchange.orders.append({"instrument": "SPOT"})
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            if any(e["kind"] == "account_wakeup" for e in self.client.request("events")):
                break
            time.sleep(0.05)
        else:
            self.fail("account change failed to wake the external trader")
        self.assertEqual(self.service.config("alice")["model_calls"], 0)

    def test_explicit_migration_preserves_account_notes_and_unknown_order_identity(self):
        with self.assertRaisesRegex(ValueError, "pause"):
            self.service.migrate_external("alice")
        decision = self.begin()
        self.exchange.lose_response = True
        with self.assertRaises(ValueError):
            self.client.call("trade", self.action(decision))
        self.service.stop("alice")
        for thread in self.service.workers["alice"][1]:
            thread.join(2)
        config = self.service.config("alice")
        config.update(backend="legacy", connection="old-model", plugin_id="old-plugin")
        self.service.store.put("trader", "alice", config)
        self.service.store.put("note", "alice", "preserve this goal")
        migrated = self.service.migrate_external("alice")
        self.assertEqual(migrated["account_id"], 20)
        self.assertEqual(migrated["backend"], "external")
        self.assertEqual(self.service.store.get("note", "alice"), "preserve this goal")
        self.service.start("alice")
        self.assertEqual(len(self.exchange.orders), 1)
        self.assertEqual(self.client.call("receipt", {"request_id": "order"})["status"], "done")

    def test_context_retries_instead_of_labeling_pre_interrupt_data_as_current(self):
        original = self.exchange.observe
        calls = []
        def observe(*args):
            calls.append(args)
            result = original(*args)
            if len(calls) == 1:
                state = self.service.alerts.state("alice")
                state["generation"] += 1
                self.service.store.put("alerts_state", "alice", state)
            return result
        self.exchange.observe = observe
        context = self.client.call("context", {})
        self.assertEqual(context["generation"], 1)
        self.assertEqual(len(calls), 4)

    def test_official_mcp_sdk_stdio_discovery_calls_and_stale_action(self):
        from mcp import ClientSession, StdioServerParameters
        from mcp.client.stdio import stdio_client
        async def scenario():
            params = StdioServerParameters(command=os.sys.executable, args=["-m", "marketforge.agents.mcp_server", "--service-url", self.client.url, "--trader", "alice"],
                env={"PYTHONPATH": str(fixture.ROOT / "python"), "MARKETFORGE_TOOL_TOKEN": self.token})
            async with stdio_client(params) as (read, write):
                async with ClientSession(read, write) as session:
                    await session.initialize()
                    tools = await session.list_tools()
                    self.assertIn("decision_begin", {t.name for t in tools.tools})
                    self.assertIn("strategy_install", {t.name for t in tools.tools})
                    decision = (await session.call_tool("decision_begin", {"generation": 0})).structuredContent
                    result = await session.call_tool("trade", self.action(decision))
                    self.assertFalse(result.isError)
                    self.trigger(decision)
                    stale = await session.call_tool("trade", self.action(decision, "stale"))
                    self.assertTrue(stale.isError)
                    self.assertIn("stale", stale.structuredContent["error"])
            self.assertEqual(len(self.exchange.orders), 1)
        asyncio.run(scenario())

    @unittest.skipUnless(os.environ.get("MARKETFORGE_NATIVE_CODEX_TEST") == "1", "set MARKETFORGE_NATIVE_CODEX_TEST=1 for installed Codex app-server acceptance")
    def test_native_codex_discovers_and_calls_the_same_mcp_tools_without_inference(self):
        from marketforge.agents.connectors import CodexSession
        folder = Path(self.directory.name)
        capability = folder / "tool.token"; capability.write_text(self.token)
        workspace = folder / "workspace"; workspace.mkdir()
        adapter = CodexSession(self.client, {}, workspace, capability, ephemeral=True)
        try:
            status = adapter.rpc.request("mcpServerStatus/list", {"threadId": adapter.id, "detail": "toolsAndAuthOnly"})
            names = {tool["name"] for item in status["data"] if item["name"] == "marketforge" for tool in item["tools"].values()}
            self.assertIn("decision_begin", names)
            response = adapter.rpc.request("mcpServer/tool/call", {"threadId": adapter.id, "server": "marketforge", "tool": "context", "arguments": {}})
            self.assertFalse(response.get("isError"))
            self.assertEqual(response["structuredContent"]["account_id"], 20)
            def tool(name, args):
                return adapter.rpc.request("mcpServer/tool/call", {"threadId": adapter.id, "server": "marketforge", "tool": name, "arguments": args})
            decision = tool("decision_begin", {"generation": 0})["structuredContent"]
            self.assertFalse(tool("trade", self.action(decision)).get("isError"))
            self.trigger(decision)
            self.assertTrue(tool("trade", self.action(decision, "stale-native")).get("isError"))
            self.assertEqual(len(self.exchange.orders), 1)
            Path(".local/validation/native-codex-mcp.json").write_text(json.dumps({"tools": sorted(names), "account_id": 20,
                "provider_inference": False, "virtual_orders": 1, "stale_order_blocked": True,
                "protocol": "Codex app-server + official MCP SDK"}, indent=2), encoding="utf-8")
        finally:
            adapter.close()

    @unittest.skipUnless(os.environ.get("MARKETFORGE_NATIVE_OPENCODE_TEST") == "1", "set MARKETFORGE_NATIVE_OPENCODE_TEST=1 for OpenCode serve acceptance")
    def test_native_opencode_connects_mcp_and_resumes_its_session_without_inference(self):
        from marketforge.agents.connectors import OpenCodeSession
        executable = os.environ["MARKETFORGE_TEST_OPENCODE"]
        folder = Path(self.directory.name)
        workspace = folder / "opencode-workspace"; workspace.mkdir()
        capability = folder / "tool.token"; capability.write_text(self.token)
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0)); port = sock.getsockname()[1]
        env = {k: v for k, v in os.environ.items() if not k.startswith("MARKETFORGE_")}
        for variable, name in (("XDG_DATA_HOME", "data"), ("XDG_CONFIG_HOME", "config"), ("XDG_CACHE_HOME", "cache")):
            env[variable] = str(folder / name)
        process = subprocess.Popen([executable, "serve", "--pure", "--hostname", "127.0.0.1", "--port", str(port)],
            cwd=workspace, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
        try:
            deadline = time.monotonic() + 20
            url = f"http://127.0.0.1:{port}"
            while time.monotonic() < deadline:
                try:
                    urllib.request.urlopen(url + "/global/health", timeout=1).close(); break
                except OSError:
                    time.sleep(0.1)
            else:
                self.fail("OpenCode server did not become healthy")
            adapter = OpenCodeSession(self.client, {}, workspace, capability, url)
            status = adapter.request("/mcp")
            self.assertEqual(status["marketforge"]["status"], "connected")
            resumed = OpenCodeSession(self.client, {"session_id": adapter.id}, workspace, capability, url)
            self.assertEqual(resumed.id, adapter.id)
            Path(".local/validation/native-opencode-mcp.json").write_text(json.dumps({"mcp_status": status["marketforge"]["status"],
                "session_resume": True, "provider_inference": False}), encoding="utf-8")
        finally:
            process.terminate(); process.wait(5)


if __name__ == "__main__":
    unittest.main()
