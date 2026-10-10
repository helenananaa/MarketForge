"""Real process/HTTP orders, automatic mixed market and PostgreSQL restart.

Set MARKETFORGE_TEST_SERVER to an isolated freshly built binary when the normal
debug executable is in use. PostgreSQL uses only the caller's test database.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "python"))
sys.path.insert(0, str(ROOT / "scripts"))
from background_market import recipe
from marketforge import Client, MarketForgeError


class PineHttpTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            import pine_compat
        except ImportError:
            if os.environ.get("MARKETFORGE_REQUIRE_PINE_TESTS") == "1":
                raise AssertionError("forced Pine acceptance requires the 0.3.1 wheel")
            raise unittest.SkipTest("Pine runtime not installed")
        if pine_compat.__version__ != "0.3.1":
            raise AssertionError("Pine HTTP acceptance requires 0.3.1")
        cls.binary = Path(os.environ.get("MARKETFORGE_TEST_SERVER", str(
            ROOT / "target/debug" / ("exchange-server.exe" if os.name == "nt" else "exchange-server"))))
        if not cls.binary.is_file():
            if os.environ.get("MARKETFORGE_REQUIRE_PINE_TESTS") == "1":
                raise AssertionError("build the server before forced Pine HTTP acceptance")
            raise unittest.SkipTest("build exchange-server before HTTP acceptance")

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="marketforge-pine-")
        self.addCleanup(self.directory.cleanup)
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            self.port = sock.getsockname()[1]
        self.base = f"http://127.0.0.1:{self.port}"
        self.env = {key: value for key, value in os.environ.items() if not key.startswith("MARKETFORGE_")}
        self.env.update(MARKETFORGE_BIND_ADDR=f"127.0.0.1:{self.port}",
                        MARKETFORGE_AUTH_TOKENS_JSON='{"pine-test":"pine-owner"}',
                        MARKETFORGE_BOT_PLUGIN_DIR=str(ROOT / "bot-plugins"),
                        PATH=str(Path(sys.executable).parent) + os.pathsep + self.env["PATH"])
        self.process = None
        self.log = None
        self.receipts = ROOT / "target/pine-http"
        self.receipts.mkdir(parents=True, exist_ok=True)
        self.addCleanup(self.stop)

    def start(self):
        self.log = (self.receipts / f"{self._testMethodName}-{time.time_ns()}.log").open("w", encoding="utf-8")
        self.process = subprocess.Popen([str(self.binary)], cwd=self.directory.name, env=self.env,
                                        stdout=self.log, stderr=subprocess.STDOUT)
        self.client = Client(self.base, bearer="pine-test", trusted_owner_urls=[self.base], timeout=20)
        deadline = time.monotonic() + 25
        while True:
            try:
                self.client.health_ready()
                break
            except (MarketForgeError, OSError):
                if self.process.poll() is not None or time.monotonic() >= deadline:
                    self.fail(f"server readiness failed; see {self.receipts}")
                time.sleep(0.05)

    def stop(self):
        if self.process is not None and self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        if self.log is not None:
            self.log.close()

    def observation(self, room, account):
        result = self.client.observe(room, account)
        return result.get("observation", result)

    def deterministic_room(self, room, script):
        spec = recipe(room, 7, False, pine=True)
        pine = spec["agents"][20]["Plugin"]
        pine["config"].update(script=script, inputs={}, bar_interval_ms=1000)
        spec["agents"] = [{"Plugin": pine}]
        spec["scenario"]["accounts"] = [
            {"Spot": {"account_id": 10, "cash_balance": 1000000, "position_qty": 10000}},
            {"Spot": {"account_id": 20, "cash_balance": 1000000, "position_qty": 10000}},
            {"Spot": {"account_id": 300, "cash_balance": 10000, "position_qty": 0}},
        ]
        spec["scenario"]["seed_orders"] = [
            {"NewOrder": {"order_id": 1, "account_id": 20, "side": "Buy",
                          "kind": {"Limit": {"price_tick": 99}}, "qty": 1, "reduce_only": False}},
            {"NewOrder": {"order_id": 2, "account_id": 10, "side": "Sell",
                          "kind": {"Limit": {"price_tick": 101}}, "qty": 1, "reduce_only": False}},
        ]
        training = {"run_id": room, "scenario": spec["scenario"], "agents": spec["agents"],
                    "manual_agents": True, "trainee_account_id": 20, "target_qty": 10000, "horizon_steps": 1000}
        self.client.start_training(training)
        self.client.pause_room(room)

    def tick(self, room, price, index):
        # Produce a real printed price, then give the Pine buyer only one unit
        # of ask liquidity so its two-unit IOC genuinely partially fills.
        self.client._request("POST", f"/rooms/{room}/resume", {})
        for account in (10, 20):
            for order in self.observation(room, account)["own_orders"]:
                self.client.cancel(room, account, order["order_id"])
        self.client.place(room, 10, "Sell", price, 1)
        self.client.place(room, 20, "Buy", price, 1)
        self.client.place(room, 10, "Sell", price + 1, 1)
        self.client.place(room, 20, "Buy", price - 1, 100)
        self.client.pause_room(room)
        return self.client.step_bots(room, f"{room}-step-{index}")

    def test_all_three_strategies_trade_through_real_process_and_gateway(self):
        self.start()
        catalog = {item["id"] for item in self.client.list_bots()}
        self.assertIn("pine.strategy", catalog)
        cases = {"ma.pine": [*range(100, 108), *([80] * 12)],
                 "rsi.pine": [*range(110, 102, -1), *([120] * 12)],
                 "breakout.pine": [*([100] * 6), 110, *([80] * 12)]}
        receipt = {}
        for script, prices in cases.items():
            room = f"pine-http-{script}-{time.time_ns()}"
            self.deterministic_room(room, script)
            maximum_position, buy_seen, sell_seen = 0, False, False
            for index, price in enumerate(prices):
                saved = self.tick(room, price, index)
                account = self.observation(room, 300)["own_account"]["Spot"]
                maximum_position = max(maximum_position, int(account["position_qty"]))
                trades = self.observation(room, 300)["public_trades"]
                buy_seen |= any(t["taker_account_id"] == 300 and t["taker_side"] == "Buy" for t in trades)
                sell_seen |= any(t["taker_account_id"] == 300 and t["taker_side"] == "Sell" for t in trades)
            self.assertTrue(buy_seen and sell_seen, script)
            self.assertEqual(maximum_position, 1, "only one of two requested units can fill")
            self.assertEqual(int(account["position_qty"]), 0)
            data = saved["agents"][0]["kind_state"]["Plugin"]["data"]
            self.assertEqual(data["observed_position"], 0)
            self.assertEqual(data["evaluated_bars"], len(prices))
            receipt[script] = {"buy": buy_seen, "sell": sell_seen, "max_position": maximum_position,
                              "final_account": account, "bars": data["evaluated_bars"]}
        (self.receipts / "orders.json").write_text(json.dumps(receipt, indent=2), encoding="utf-8")

    def test_automatic_mixed_background_market_with_six_pine_participants(self):
        self.start()
        room = f"pine-mixed-{time.time_ns()}"
        spec = recipe(room, 7, True, pine=True)
        # At 50 ms wall time per simulated second, a 10-second bar gives a
        # process 500 ms between bars. Do not raise the bot timeout for this test.
        spec["agent_interval_ms"] = 50
        self.client._request("POST", "/rooms", spec)
        deadline = time.monotonic() + 30
        while True:
            status = self.client._request("GET", f"/rooms/{room}/agents")
            self.assertFalse(status["bot_errors"], status["bot_errors"])
            saved = self.client.room_bots(room)
            pine = [a["kind_state"]["Plugin"]["data"] for a in saved["agents"]
                    if a["template"]["Plugin"]["plugin_id"] == "pine.strategy"]
            if len(pine) == 6 and all(s and s.get("evaluated_bars", 0) >= 14 for s in pine):
                break
            if time.monotonic() >= deadline:
                self.fail("six Pine participants failed to evaluate closed bars")
            time.sleep(0.1)
        self.client.pause_room(room)
        accounts = [self.observation(room, 300 + i)["own_account"]["Spot"] for i in range(6)]
        self.assertTrue(any(s["last_fill_id"] > 0 for s in pine), "Pine participants must really trade")
        self.assertTrue(all(0 <= int(a["position_qty"]) <= 10 and int(a["available_cash"]) >= 0 for a in accounts))
        (self.receipts / "mixed.json").write_text(json.dumps({"status": status,
            "pine": [{k: s.get(k) for k in ("evaluated_bars", "last_fill_id", "observed_position", "observed_cash")}
                     for s in pine], "accounts": accounts}, indent=2), encoding="utf-8")

    def test_postgres_restart_after_partial_fill_and_step_idempotency(self):
        dsn = os.environ.get("MARKETFORGE_TEST_DATABASE_URL")
        if not dsn:
            if os.environ.get("MARKETFORGE_REQUIRE_POSTGRES_TESTS") == "1":
                self.fail("forced restart tests require a PostgreSQL test database")
            self.skipTest("PostgreSQL test database not set")
        self.env["MARKETFORGE_DATABASE_URL"] = dsn
        self.start()
        room = f"pine-restart-{time.time_ns()}"
        self.deterministic_room(room, "ma.pine")
        for index, price in enumerate(range(100, 108)):
            saved = self.tick(room, price, index)
        self.assertEqual(int(self.observation(room, 300)["own_account"]["Spot"]["position_qty"]), 1)
        before = self.observation(room, 300)
        self.stop()
        self.start()
        self.assertEqual(self.client.room_bots(room), saved)
        self.assertEqual(self.client.step_bots(room, f"{room}-step-7"), saved)
        after = self.observation(room, 300)
        for key in ("own_account", "book", "market_time_ms", "step"):
            self.assertEqual(after[key], before[key])
        resumed = self.tick(room, 80, 8)
        data = resumed["agents"][0]["kind_state"]["Plugin"]["data"]
        self.assertEqual(data["accounts"][-1]["position_size"], 1)
        self.assertEqual(data["accounts"][-1]["position_avg_price"], 108)
        self.assertEqual(data["observed_position"], 1)
        self.assertEqual(data["evaluated_bars"], 9)
        (self.receipts / "restart.json").write_text(json.dumps({"room": room,
            "idempotent_step": True, "position_before_restart": 1,
            "feedback_position": data["observed_position"], "feedback_average": 108,
            "evaluated_bars": 9}, indent=2), encoding="utf-8")


if __name__ == "__main__":
    unittest.main()
