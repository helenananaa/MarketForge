"""Qualified direct Rust decisions survive durable PostgreSQL restart."""
import json
import os
import re
import time
import unittest

import test_pine_http as http
import test_pine_broker_http as broker_http


class PineInProcessRecoveryTests(unittest.TestCase):
    setUpClass = classmethod(http.PineHttpTests.setUpClass.__func__)
    setUp = broker_http.PineBrokerHttpTests.setUp
    start = broker_http.PineBrokerHttpTests.start
    stop = broker_http.PineBrokerHttpTests.stop
    tick = broker_http.PineBrokerHttpTests.tick
    observation = broker_http.PineBrokerHttpTests.observation
    room = broker_http.PineBrokerHttpTests.room
    data = broker_http.PineBrokerHttpTests.data

    def test_qualified_limit_postgres_restart_and_cancel_confirmation(self):
        dsn = os.environ.get("MARKETFORGE_TEST_DATABASE_URL")
        if not dsn:
            if os.environ.get("MARKETFORGE_REQUIRE_POSTGRES_TESTS") == "1":
                self.fail("direct Rust recovery requires a test database")
            self.skipTest("PostgreSQL test database not set")
        self.env.update(MARKETFORGE_DATABASE_URL=dsn, MARKETFORGE_PINE_IN_PROCESS="1")
        self.start()
        room = f"rust-pine-restart-{time.time_ns()}"
        self.room(room, "limit.pine")
        for index, price in enumerate((104, 103, 102, 101, 100)):
            saved = self.tick(room, price, index)
        order = self.observation(room, 300)["own_orders"][0]
        self.assertEqual((order["price_tick"], order["remaining_qty"]), (98, 2))
        confirmed = self.client.step_bots(room, "rust-confirm-limit")
        durable = self.data(confirmed)
        self.stop()
        self.start()
        self.assertEqual(self.observation(room, 300)["own_orders"], [order])
        self.assertEqual(self.client.step_bots(room, f"{room}-step-4"), saved)
        resumed = self.client.step_bots(room, "rust-restart-observe")
        self.assertEqual(self.observation(room, 300)["own_orders"], [order])
        self.assertEqual(self.data(resumed)["orders"], durable["orders"])
        cancelled = self.tick(room, 110, 5)
        self.assertEqual(self.observation(room, 300)["own_orders"], [])
        self.assertEqual(self.client.step_bots(room, f"{room}-step-5"), cancelled)
        # Metrics is text in the HTTP SDK; fetch directly to verify actual route.
        import urllib.request
        request = urllib.request.Request(self.base + "/metrics", headers={"Authorization":"Bearer pine-test"})
        with urllib.request.urlopen(request, timeout=10) as response:
            metrics = response.read().decode()
        self.assertRegex(metrics, r'marketforge_bench_bot_inprocess_call_count_total [1-9]')
        self.assertRegex(metrics, r'marketforge_bench_bot_worker_acquire_count_total 0')
        (self.receipts / "rust-pine-postgres-restart.json").write_text(json.dumps({"order":order,
            "durable":durable, "restored":self.data(resumed), "cancelled":self.data(cancelled)}, indent=2), encoding="utf-8")

    def test_schema3_postgres_restart_matches_continuous_limit_fill_and_close(self):
        dsn = os.environ.get("MARKETFORGE_TEST_DATABASE_URL")
        if not dsn:
            if os.environ.get("MARKETFORGE_REQUIRE_POSTGRES_TESTS") == "1":
                self.fail("schema-3 restart acceptance requires a test database")
            self.skipTest("PostgreSQL test database not set")
        self.env.update(MARKETFORGE_DATABASE_URL=dsn, MARKETFORGE_PINE_IN_PROCESS="1",
                        MARKETFORGE_PINE_VM_CHECKPOINT="1")
        self.start()

        placements = {}
        order_forward, order_reverse = {}, {}

        def record_decision(room, saved):
            data = self.data(saved)
            created = placements.setdefault(room, [])
            for receipt in data.get("_host_receipts", []):
                action = receipt["action"]
                if not receipt["accepted"] or not any(name.startswith("Place") for name in action):
                    continue
                order = data["orders"][receipt["key"]]
                creation = {"order_id":receipt["order_id"], "instrument_id":receipt["instrument_id"],
                            "name":order["name"], "action":action,
                            "command_seq":receipt["command_seq"], "market_time_ms":receipt["market_time_ms"]}
                self.assertIs(type(creation["order_id"]), int)
                prior = next((row for row in created if row["order_id"] == creation["order_id"]), None)
                if prior is None: created.append(creation)
                else: self.assertEqual(prior, creation, "one order identity cannot describe two creations")
            return saved

        def tick(room, price, index):
            return record_decision(room, self.tick(room, price, index))

        def step(room, key):
            return record_decision(room, self.client.step_bots(room, key))

        def compare_trading(control_room, recovered_room, control, recovered, complete=False):
            original, resumed = placements[control_room], placements[recovered_room]
            self.assertLessEqual(len(resumed), len(original))
            if complete: self.assertEqual(len(resumed), len(original))
            # Only an actual successful creation receipt establishes an ID pair.
            # Cancellation, fill and state references must use that existing pair.
            for left, right in zip(original, resumed):
                self.assertEqual({k:v for k,v in left.items() if k != "order_id"},
                                 {k:v for k,v in right.items() if k != "order_id"})
                a, b = left["order_id"], right["order_id"]
                self.assertEqual(order_forward.setdefault(a, b), b)
                self.assertEqual(order_reverse.setdefault(b, a), a)
            self.assertEqual(len(order_forward), len(order_reverse))
            def references(value, mapping):
                if isinstance(value, list): return [references(item, mapping) for item in value]
                if not isinstance(value, dict): return value
                result = {}
                for key, item in value.items():
                    if key.endswith("order_id") and item is not None:
                        self.assertIs(type(item), int)
                        self.assertIn(item, mapping, f"{key} must reference an authenticated creation")
                        result[key] = mapping[item]
                    elif key == "error" and isinstance(item, str):
                        # The API retains Rust rejection diagnostics as text;
                        # those explicit order references obey the same bijection.
                        def error_order(match):
                            identity = int(match.group(2))
                            self.assertIn(identity, mapping, "error must reference an authenticated creation")
                            return match.group(1) + str(mapping[identity])
                        result[key] = re.sub(r"(\b(?:maker_|taker_|submitted_)?order_id: )(\d+)", error_order, item)
                    else: result[key] = references(item, mapping)
                return result
            self.assertEqual(references(control, {key:key for key in order_forward}),
                             references(recovered, order_reverse))

        def trading_snapshot(room, saved):
            data = self.data(saved)
            # A different room has a different identity and therefore different
            # BotOrder keys. Compare their semantic names and every trading field.
            names = {key:f'{order["instrument"]}::{order["name"]}'
                     for key, order in data["orders"].items()}
            def normalized(value):
                if isinstance(value, dict):
                    return {names.get(key, key):normalized(item) for key,item in value.items()}
                if isinstance(value, list): return [normalized(item) for item in value]
                if isinstance(value, str): return names.get(value, value)
                return value
            view = self.observation(room, 300)
            return normalized({"own_orders":view["own_orders"], "own_account":view["own_account"],
                "state":{key:data.get(key) for key in ("broker_schema", "orders", "entries", "ledgers",
                    "evaluated_bars", "last_bar_time", "last_intents", "last_actions", "last_fill_id",
                    "observed_position", "observed_cash", "candle_audits", "_host_receipts")}})

        def prefix(room):
            self.room(room, "limit.pine", config={"history_limit":128})
            checkpoints = []
            for index in range(64):
                saved = tick(room, 100, index)
                if index in (31, 63):
                    data = self.data(saved)
                    self.assertEqual(data["broker_schema"], 3)
                    self.assertIsNotNone(data["vm_checkpoint"])
                    self.assertEqual(data["vm_checkpoint"]["bars"], index + 1)
                    self.assertEqual(data["vm_tail"], [])
                    checkpoints.append(data["vm_checkpoint"]["bars"])
            for index, price in enumerate((104, 103, 102, 101, 100), 64):
                saved = tick(room, price, index)
            last_tick = saved
            step(room, f"{room}-prefix-replacement")
            saved = step(room, f"{room}-prefix-confirm")
            data = self.data(saved)
            self.assertEqual(data["broker_schema"], 3)
            self.assertGreaterEqual(data["evaluated_bars"], 69)
            self.assertTrue(data["vm_tail"], "restart must also recover post-checkpoint passes")
            orders = self.observation(room, 300)["own_orders"]
            self.assertEqual([(order["price_tick"], order["remaining_qty"]) for order in orders], [(98, 2)])
            return {"last_tick":last_tick, "saved":saved, "orders":orders,
                    "checkpoint_bars":checkpoints, "snapshot":trading_snapshot(room, saved)}

        def continuation(room, before):
            self.assertEqual(self.observation(room, 300)["own_orders"], before["orders"])
            self.assertEqual(step(room, f"{room}-step-68"), before["last_tick"],
                             "durable idempotency must return the original pre-restart receipt")
            observed = step(room, f"{room}-observe")
            self.assertEqual(self.observation(room, 300)["own_orders"], before["orders"])
            self.assertEqual(self.data(observed)["orders"], self.data(before["saved"])["orders"])
            phases = {"observed":trading_snapshot(room, observed)}
            # An upward buy-price revision uses cancel/replacement. A downward
            # revision is a legal same-ID amendment and would not test recovery.
            canceled = tick(room, 101, 69)
            self.assertEqual(self.observation(room, 300)["own_orders"], [])
            phases["replacement_cancel"] = trading_snapshot(room, canceled)
            step(room, f"{room}-replacement")
            replaced = step(room, f"{room}-replacement-confirm")
            order = self.observation(room, 300)["own_orders"][0]
            self.assertEqual((order["price_tick"], order["remaining_qty"]), (99, 2))
            self.assertNotEqual(order["order_id"], before["orders"][0]["order_id"])
            phases["replacement"] = trading_snapshot(room, replaced)

            # Remove the feed's competing orders, then make an actual seller
            # cross precisely one unit of the restored/replaced Pine limit.
            self.client._request("POST", f"/rooms/{room}/resume", {})
            for account in (10, 20):
                for live in self.observation(room, account)["own_orders"]:
                    self.client.cancel(room, account, live["order_id"])
            fill = self.client.place(room, 10, "Sell", 99, 1,
                                     idempotency_key=f"{room}-actual-partial-fill")
            self.assertTrue(fill["accepted"])
            self.client.pause_room(room)
            partial = self.observation(room, 300)
            self.assertEqual(partial["own_account"]["Spot"]["position_qty"], 1)
            self.assertEqual(partial["own_orders"][0]["remaining_qty"], 1)
            filled = step(room, f"{room}-fill-feedback")
            self.assertEqual(self.data(filled)["observed_position"], 1)
            self.assertEqual(self.data(filled)["ledgers"]["V-BTC-SPOT"]["fills"][-1]["qty"], 1)
            phases["partial_fill"] = trading_snapshot(room, filled)
            closed = tick(room, 110, 70)
            self.assertEqual(self.observation(room, 300)["own_orders"], [])
            self.assertEqual(self.observation(room, 300)["own_account"]["Spot"]["position_qty"], 0)
            self.assertEqual(step(room, f"{room}-step-70"), closed)
            confirmed = step(room, f"{room}-close-feedback")
            self.assertEqual(self.data(confirmed)["observed_position"], 0)
            self.assertEqual(len(self.data(confirmed)["ledgers"]["V-BTC-SPOT"]["fills"]), 2)
            phases["closed"] = trading_snapshot(room, confirmed)
            return phases

        control_room = f"vm-continuous-{time.time_ns()}"
        control_prefix = prefix(control_room)
        control = continuation(control_room, control_prefix)
        recovered_room = f"vm-pg-restart-{time.time_ns()}"
        recovered_prefix = prefix(recovered_room)
        compare_trading(control_room, recovered_room, control_prefix["snapshot"], recovered_prefix["snapshot"])
        self.stop()
        self.start()
        recovered = continuation(recovered_room, recovered_prefix)
        receipt = {"checkpoint_bars":recovered_prefix["checkpoint_bars"], "before":recovered_prefix["snapshot"],
                   "control_room":control_room, "recovered_room":recovered_room,
                   "order_identity_forward":order_forward, "order_identity_reverse":order_reverse,
                   "creation_receipts":placements, "continuous":control, "recovered":recovered,
                   "qualified":False}
        receipt_path = self.receipts / f"rust-pine-schema3-postgres-restart-{time.time_ns()}.json"
        receipt_path.write_text(json.dumps(receipt, indent=2), encoding="utf-8")
        compare_trading(control_room, recovered_room, control, recovered, complete=True)
        import urllib.request
        request = urllib.request.Request(self.base + "/metrics", headers={"Authorization":"Bearer pine-test"})
        with urllib.request.urlopen(request, timeout=10) as response:
            metrics = response.read().decode()
        self.assertRegex(metrics, r'marketforge_bot_runtime_evaluation_total\{route="direct",mode="checkpoint_restore",reason="session_missing"\} [1-9]')
        self.assertNotRegex(metrics, r'marketforge_bot_runtime_route_total\{route="(?:process|blocked)"[^\n]*\} [1-9]')
        receipt["qualified"] = True
        receipt_path.write_text(json.dumps(receipt, indent=2), encoding="utf-8")
        (self.receipts / "rust-pine-schema3-postgres-restart.json").write_text(json.dumps(receipt, indent=2), encoding="utf-8")


if __name__ == "__main__":
    unittest.main()
