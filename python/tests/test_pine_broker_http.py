"""Actual process, gateway, books and account acceptance for the full Pine broker."""
import copy
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import unittest

import test_pine_http as http

ROOT = http.ROOT


class PineBrokerHttpTests(unittest.TestCase):
    setUpClass = classmethod(http.PineHttpTests.setUpClass.__func__)
    start = http.PineHttpTests.start
    tick = http.PineHttpTests.tick
    observation = http.PineHttpTests.observation

    def stop(self):
        # This test owns the server and its disposable plugin directory. Windows
        # TerminateProcess does not reap a still-running plugin child.
        if os.name == "nt" and self.process is not None and self.process.poll() is None:
            subprocess.run(["taskkill", "/PID", str(self.process.pid), "/T", "/F"], capture_output=True, timeout=10)
            self.process.wait(timeout=10)
        http.PineHttpTests.stop(self)

    def setUp(self):
        http.PineHttpTests.setUp(self)
        self.plugins = Path(self.directory.name) / "plugins"
        # A shared test journal can also contain the bundled buy-remaining bot.
        # Keep the complete installed catalog when exercising recovery.
        shutil.copytree(ROOT / "bot-plugins", self.plugins)
        package = self.plugins / "pine-strategy"
        manifest = json.loads((package / "bot.json").read_text(encoding="utf-8"))
        manifest["command"][0] = sys.executable
        (package / "bot.json").write_text(json.dumps(manifest), encoding="utf-8")
        scripts = {
            "limit-test.pine": 'if bar_index < 2\n    strategy.entry("L",strategy.long,qty=2,limit=close-10)\nelse\n    strategy.cancel("L")',
            "short-test.pine": 'if strategy.position_size == 0\n    strategy.entry("S",strategy.short,qty=2)\nelse\n    strategy.close("S")',
            "hedge-test.pine": 'if bar_index == 0\n    strategy.entry("L",strategy.long,qty=2)',
            "routes-test.pine": 'p=request.security("V-BTC-PERP","1S",close)\nif p>close\n    strategy.entry("L",strategy.long,qty=1,limit=90)\n    strategy.entry("V-BTC-PERP::S",strategy.short,qty=1,limit=120)',
            "stop-test.pine": 'if bar_index == 0\n    strategy.entry("L",strategy.long,qty=1,stop=110,limit=111)',
            "protection-test.pine": 'strategy.exit("X",profit=10,loss=5)',
            "hedge-side-test.pine": 'if bar_index == 0\n    strategy.entry("S",strategy.short,qty=1)\nelse\n    strategy.close("S")',
        }
        for name, body in scripts.items():
            (package / "scripts" / name).write_text('//@version=6\nstrategy("Acceptance")\n' + body, encoding="utf-8")
        self.env["MARKETFORGE_BOT_PLUGIN_DIR"] = str(self.plugins)

    def room(self, name, script, primary="V-BTC-SPOT", peer_risk=None, config=None, manual=True, position_mode=None):
        spec = http.recipe(name, 7, False, pine=True)
        plugin = spec["agents"][20]["Plugin"]
        plugin["participant"]["instrument_id"] = primary
        plugin["config"].update(script=script, inputs={}, bar_interval_ms=1000, execution_mode="broker")
        plugin["config"].update(config or {})
        spec["scenario"]["accounts"] = [
            {"Spot": {"account_id": 10, "cash_balance": 1000000, "position_qty": 10000}},
            {"Spot": {"account_id": 20, "cash_balance": 1000000, "position_qty": 10000}},
            {"Spot": {"account_id": 300, "cash_balance": 10000, "position_qty": 0}},
        ]
        spec["scenario"]["seed_orders"] = [{"NewOrder": {"order_id": index+1, "account_id": account,
            "side": side, "kind": {"Limit": {"price_tick": price}}, "qty": 1, "reduce_only": False}}
            for index, (account, side, price) in enumerate([(20, "Buy", 99), (10, "Sell", 101)])]
        peer = json.loads((ROOT / "scripts/fixtures/microstructure_market.json").read_text(encoding="utf-8"))["scenario"]["extra_markets"][0]
        peer["Perp"].update(price_link=None, funding=None, risk=peer_risk or {})
        if position_mode: peer["Perp"]["clearing"]["position_mode"] = position_mode
        spec["scenario"]["extra_markets"] = [peer]
        if manual:
            self.client.start_training({"run_id": name, "scenario": spec["scenario"], "agents": [{"Plugin": plugin}],
                "manual_agents": True, "trainee_account_id": 20, "target_qty": 10000, "horizon_steps": 1000})
        else:
            spec["agents"] = [{"Plugin": plugin}]
            self.client._request("POST", "/rooms", spec)
        self.client.pause_room(name)
        return [{"Plugin": plugin}]

    def action(self, room, instrument, account, action):
        return self.client.submit_action(room, instrument, account, action, f"broker-{time.time_ns()}")

    def observe_market(self, room, instrument, account=300):
        result = self.client.observe(room, account, instrument_id=instrument)
        return result.get("observation", result)

    def peer_bar(self, room, price=100):
        self.action(room, "V-BTC-PERP", 10, {"PlaceLimit": {"side": "Sell", "price_tick": price, "qty": 1}})
        self.action(room, "V-BTC-PERP", 20, {"PlaceImmediateOrCancel": {"side": "Buy", "price_tick": price, "qty": 1}})

    def data(self, saved):
        return saved["agents"][0]["kind_state"]["Plugin"]["data"]

    def test_resting_limit_partial_fill_amend_cancel_and_duplicate_step(self):
        self.start()
        room = f"broker-limit-{time.time_ns()}"
        self.room(room, "limit-test.pine")
        first = self.tick(room, 100, 0)
        live = self.observation(room, 300)["own_orders"]
        self.assertEqual([(o["price_tick"], o["remaining_qty"]) for o in live], [(90, 2)])
        order_id = live[0]["order_id"]
        duplicate = self.client.step_bots(room, "same-bar")
        self.assertEqual(self.observation(room, 300)["own_orders"][0]["order_id"], order_id)
        self.client._request("POST", f"/rooms/{room}/resume", {})
        for account in (10, 20):
            for order in self.observation(room, account)["own_orders"]:
                self.client.cancel(room, account, order["order_id"])
        self.client.place(room, 10, "Sell", 90, 1)
        second = self.tick(room, 101, 1)
        live = self.observation(room, 300)["own_orders"]
        self.assertEqual(live, [], "a more aggressive price needs cancel and replacement")
        replaced = self.client.step_bots(room, "replace-confirmed")
        live = self.observation(room, 300)["own_orders"]
        self.assertEqual([(o["price_tick"], o["remaining_qty"]) for o in live], [(91, 1)])
        self.assertNotEqual(live[0]["order_id"], order_id)
        third = self.tick(room, 102, 2)
        self.assertEqual(self.observation(room, 300)["own_orders"], [])
        self.assertEqual(self.observation(room, 300)["own_account"]["Spot"]["position_qty"], 1)
        self.assertEqual(self.client.step_bots(room, f"{room}-step-2"), third)
        (self.receipts / "broker-limit.json").write_text(json.dumps({"order_id": order_id, "first": self.data(first), "second": self.data(second), "last": self.data(third)}, indent=2), encoding="utf-8")

    def test_perpetual_partial_short_and_reduce_only_close(self):
        self.start()
        room = f"broker-short-{time.time_ns()}"
        self.room(room, "short-test.pine", primary="V-BTC-PERP")
        self.client._request("POST", f"/rooms/{room}/resume", {})
        self.peer_bar(room)
        self.action(room, "V-BTC-PERP", 20, {"PlaceLimit": {"side": "Buy", "price_tick": 99, "qty": 1}})
        self.action(room, "V-BTC-PERP", 10, {"PlaceLimit": {"side": "Sell", "price_tick": 101, "qty": 10}})
        self.client.pause_room(room)
        first = self.client.step_bots(room, "short-first")
        account = self.observe_market(room, "V-BTC-PERP")["own_account"]["Perp"]
        self.assertEqual(int(account["position_qty"]), -1)
        second = self.client.step_bots(room, "short-close")
        account = self.observe_market(room, "V-BTC-PERP")["own_account"]["Perp"]
        self.assertEqual(int(account["position_qty"]), 0)
        action = self.data(second)["last_actions"][0]["BotOrder"]["action"]["PlaceProtected"]
        self.assertTrue(action["reduce_only"])
        self.assertEqual(action["qty"], 1)
        (self.receipts / "broker-short.json").write_text(json.dumps({"first": self.data(first), "last": self.data(second), "account": account}, indent=2), encoding="utf-8")

    def test_actual_hedge_rejection_timeout_compensates_primary_leg(self):
        self.start()
        room = f"broker-unwind-{time.time_ns()}"
        self.room(room, "hedge-test.pine", peer_risk={"max_order_qty": 1}, config={"hedge_instrument_id": "V-BTC-PERP", "related_instruments": ["V-BTC-PERP"], "hedge_timeout_ms": 1000})
        self.client._request("POST", f"/rooms/{room}/resume", {})
        self.client.place(room, 10, "Sell", 100, 1)
        self.client.place(room, 20, "Buy", 100, 1)
        self.client.place(room, 10, "Sell", 101, 2)
        self.client.place(room, 20, "Buy", 99, 100)
        self.action(room, "V-BTC-PERP", 20, {"PlaceLimit": {"side": "Buy", "price_tick": 100, "qty": 1}})
        self.client.pause_room(room)
        first = self.client.step_bots(room, "hedge-primary")
        self.assertEqual(self.observation(room, 300)["own_account"]["Spot"]["position_qty"], 2)
        failed = self.client.step_bots(room, "hedge-rejected")
        receipts = self.data(failed)["_host_receipts"]
        self.assertFalse(receipts[0]["accepted"])
        unwind = self.client.step_bots(room, "hedge-unwind")
        self.assertTrue(self.data(unwind)["hedge"]["unwinding"])
        self.assertEqual(self.observation(room, 300)["own_account"]["Spot"]["position_qty"], 0)
        self.assertEqual(int(self.observe_market(room, "V-BTC-PERP")["own_account"]["Perp"]["position_qty"]), 0)
        (self.receipts / "broker-unwind.json").write_text(json.dumps({"first": self.data(first), "failed": self.data(failed), "unwind": self.data(unwind)}, indent=2), encoding="utf-8")

    def test_cross_market_history_routes_orders_and_additional_timeframe(self):
        self.start()
        room = f"broker-routes-{time.time_ns()}"
        self.room(room, "routes-test.pine", config={"related_instruments": ["V-BTC-PERP"], "additional_intervals_ms": [5000]})
        self.client._request("POST", f"/rooms/{room}/resume", {})
        self.peer_bar(room, 110)
        saved = self.tick(room, 100, 0)
        self.assertEqual(self.observation(room, 300)["own_orders"][0]["price_tick"], 90)
        self.assertEqual(self.observe_market(room, "V-BTC-PERP")["own_orders"][0]["price_tick"], 120)
        data = self.data(saved)
        self.assertIn("V-BTC-SPOT:5S", data["providers"])
        self.assertIn("V-BTC-PERP:5S", data["providers"])

    def test_postgres_restart_preserves_resting_reference_and_never_resubmits(self):
        dsn = os.environ.get("MARKETFORGE_TEST_DATABASE_URL")
        if not dsn:
            if os.environ.get("MARKETFORGE_REQUIRE_POSTGRES_TESTS") == "1": self.fail("PostgreSQL restart acceptance requires a test database")
            self.skipTest("PostgreSQL test database not set")
        self.env["MARKETFORGE_DATABASE_URL"] = dsn
        self.start()
        room = f"broker-restart-{time.time_ns()}"
        self.room(room, "limit-test.pine")
        saved = self.tick(room, 100, 0)
        order = self.observation(room, 300)["own_orders"][0]
        self.stop()
        self.start()
        restored = self.observation(room, 300)["own_orders"]
        self.assertEqual(restored, [order])
        duplicate = self.client.step_bots(room, "restart-observe")
        self.assertEqual(self.observation(room, 300)["own_orders"], [order])
        self.assertEqual(self.data(duplicate)["orders"][next(iter(self.data(saved)["orders"]))]["order_id"], order["order_id"])
        self.assertEqual(self.client.step_bots(room, "restart-observe"), duplicate)
        self.tick(room, 100, 1)
        self.tick(room, 100, 2)
        self.assertEqual(self.observation(room, 300)["own_orders"], [])
        (self.receipts / "broker-restart.json").write_text(json.dumps({"order": order, "restored": self.data(duplicate)}, indent=2), encoding="utf-8")

    def test_automatic_routing_and_http_envelope_rejection(self):
        self.start()
        room = f"broker-auto-{time.time_ns()}"
        agents = self.room(room, "routes-test.pine", config={"related_instruments": ["V-BTC-PERP"]}, manual=False)
        self.client._request("POST", f"/rooms/{room}/resume", {})
        self.peer_bar(room, 110)
        for account in (10, 20):
            for order in self.observation(room, account)["own_orders"]: self.client.cancel(room, account, order["order_id"])
        self.client.place(room, 10, "Sell", 100, 1)
        self.client.place(room, 20, "Buy", 100, 1)
        self.client.start_agents(room, agents, interval_ms=100)
        deadline = time.monotonic() + 10
        while True:
            status = self.client._request("GET", f"/rooms/{room}/agents")
            self.assertFalse(status["bot_errors"], status["bot_errors"])
            spot, peer = self.observation(room, 300), self.observe_market(room, "V-BTC-PERP")
            if spot["own_orders"] and peer["own_orders"]: break
            if time.monotonic() >= deadline: self.fail("automatic routed orders were not committed")
            time.sleep(0.05)
        self.client.stop_agents(room)
        self.client.pause_room(room)
        self.assertEqual(spot["own_orders"][0]["price_tick"], 90)
        self.assertEqual(peer["own_orders"][0]["price_tick"], 120)
        self.client._request("POST", f"/rooms/{room}/resume", {})
        with self.assertRaises(http.MarketForgeError) as error:
            self.action(room, "V-BTC-SPOT", 300, {"BotOrder": {"instrument_id": "V-BTC-PERP", "key": "bad-http", "action": {"PlaceLimit": {"side": "Buy", "price_tick": 10, "qty": 1}}}})
        self.assertEqual(error.exception.status, 400)

    def test_partial_two_leg_hedge_and_primary_close_cover_peer(self):
        self.start()
        room = f"broker-hedge-{time.time_ns()}"
        self.room(room, "hedge-test.pine", config={"related_instruments": ["V-BTC-PERP"], "hedge_instrument_id": "V-BTC-PERP"})
        self.client._request("POST", f"/rooms/{room}/resume", {})
        self.action(room, "V-BTC-PERP", 20, {"PlaceLimit": {"side": "Buy", "price_tick": 100, "qty": 1}})
        self.action(room, "V-BTC-PERP", 10, {"PlaceLimit": {"side": "Sell", "price_tick": 102, "qty": 10}})
        self.tick(room, 100, 0)  # Primary requests two; only one can fill.
        self.client.step_bots(room, "actual-hedge")
        self.assertEqual(self.observation(room, 300)["own_account"]["Spot"]["position_qty"], 1)
        self.assertEqual(int(self.observe_market(room, "V-BTC-PERP")["own_account"]["Perp"]["position_qty"]), -1)
        self.client._request("POST", f"/rooms/{room}/resume", {})
        self.client.place(room, 300, "Sell", 99, 1)
        self.client.pause_room(room)
        last = self.client.step_bots(room, "cover-peer")
        self.assertEqual(int(self.observe_market(room, "V-BTC-PERP")["own_account"]["Perp"]["position_qty"]), 0)
        self.assertTrue(self.data(last)["last_actions"][0]["BotOrder"]["action"]["PlaceProtected"]["reduce_only"])

    def test_native_conditional_stop_entry_and_position_protection_fill(self):
        self.start()
        room = f"broker-stop-{time.time_ns()}"
        self.room(room, "stop-test.pine", primary="V-BTC-PERP")
        self.client._request("POST", f"/rooms/{room}/resume", {})
        self.peer_bar(room, 100)
        self.action(room, "V-BTC-PERP", 10, {"PlaceLimit": {"side": "Sell", "price_tick": 111, "qty": 2}})
        self.client.pause_room(room)
        armed = self.client.step_bots(room, "stop-arm")
        receipt = self.data(armed)["_host_receipts"][0]
        self.assertTrue(receipt["accepted"], receipt)
        self.assertIn("SetConditional", receipt["action"])
        self.assertEqual(int(self.observe_market(room, "V-BTC-PERP")["own_account"]["Perp"]["position_qty"]), 0)

        self.client._request("POST", f"/rooms/{room}/resume", {})
        self.peer_bar(room, 110)
        self.client.pause_room(room)
        triggered = self.client.step_bots(room, "stop-trigger")
        self.assertEqual(int(self.observe_market(room, "V-BTC-PERP")["own_account"]["Perp"]["position_qty"]), 1)
        self.assertTrue(any(o.get("order_id") is not None for o in self.data(triggered)["orders"].values()))

        room = f"broker-protection-{time.time_ns()}"
        self.room(room, "protection-test.pine", primary="V-BTC-PERP")
        self.client._request("POST", f"/rooms/{room}/resume", {})
        self.peer_bar(room, 100)
        self.action(room, "V-BTC-PERP", 10, {"PlaceLimit": {"side": "Sell", "price_tick": 100, "qty": 1}})
        self.action(room, "V-BTC-PERP", 300, {"PlaceImmediateOrCancel": {"side": "Buy", "price_tick": 100, "qty": 1}})
        self.action(room, "V-BTC-PERP", 20, {"PlaceLimit": {"side": "Buy", "price_tick": 110, "qty": 10}})
        self.client.pause_room(room)
        saved = self.client.step_bots(room, "protection-arm")
        self.assertTrue(self.data(saved)["_host_receipts"][0]["accepted"])
        protected = self.observe_market(room, "V-BTC-PERP")["position_protections"]
        self.assertEqual(protected[0]["spec"]["take_profit_tick"], 110)
        self.client._request("POST", f"/rooms/{room}/resume", {})
        self.peer_bar(room, 110)
        self.client.pause_room(room)
        self.client.step_bots(room, "protection-trigger")
        self.assertEqual(int(self.observe_market(room, "V-BTC-PERP")["own_account"]["Perp"]["position_qty"]), 0)


    def test_postgres_hedge_side_actual_fill_recovers_and_closes_selected_leg(self):
        dsn = os.environ.get("MARKETFORGE_TEST_DATABASE_URL")
        if not dsn:
            if os.environ.get("MARKETFORGE_REQUIRE_POSTGRES_TESTS") == "1": self.fail("hedge recovery requires PostgreSQL")
            self.skipTest("PostgreSQL test database not set")
        self.env["MARKETFORGE_DATABASE_URL"] = dsn
        self.start()
        room = f"broker-hedge-side-{time.time_ns()}"
        self.room(room, "hedge-side-test.pine", primary="V-BTC-PERP", position_mode="Hedge", config={"position_side": "Short"})
        def place(account, side, leg, price, qty):
            return self.action(room, "V-BTC-PERP", account, {"PlaceProtected": {"side": side, "position_side": leg, "price_tick": price, "qty": qty, "order_type": "Limit", "reduce_only": False}})
        self.client._request("POST", f"/rooms/{room}/resume", {})
        place(10, "Sell", "Short", 100, 1)
        place(20, "Buy", "Long", 100, 1)
        place(20, "Buy", "Long", 99, 1)
        place(10, "Sell", "Short", 101, 10)
        self.client.pause_room(room)
        self.client.step_bots(room, "hedge-side-open")
        account = self.observe_market(room, "V-BTC-PERP")["own_account"]["Perp"]
        self.assertEqual(int(account["hedge_positions"]["short"]["qty"]), 1)
        self.stop(); self.start()
        same = self.client.step_bots(room, "hedge-side-restored")
        self.assertEqual(self.data(same)["observed_position"], -1)
        account = self.observe_market(room, "V-BTC-PERP")["own_account"]["Perp"]
        self.assertEqual(int(account["hedge_positions"]["short"]["qty"]), 0)
        self.assertEqual(int(account["hedge_positions"]["long"]["qty"]), 0)
        order = self.data(same)["last_actions"][0]["BotOrder"]["action"]["PlaceProtected"]
        self.assertEqual((order["position_side"], order["reduce_only"], order["qty"]), ("Short", True, 1))
        self.assertEqual(self.client.step_bots(room, "hedge-side-restored"), same)


if __name__ == "__main__": unittest.main()
