"""Pine and native ports through identical live books, fees and durable restart.

Runs paired isolated rooms in an explicitly selected freshly built server.
PostgreSQL restart uses only MARKETFORGE_TEST_DATABASE_URL supplied by caller.
"""
from __future__ import annotations

import copy
import hashlib
import json
import os
import re
import time
import unittest

import test_pine_http as http
import test_pine_broker_http as broker_http
from test_native_pine import PORTS, RECEIPTS, COMMON, hashes, semantic_actions


class NativePineHttpTests(unittest.TestCase):
    setUpClass = classmethod(http.PineHttpTests.setUpClass.__func__)
    start = http.PineHttpTests.start
    stop = broker_http.PineBrokerHttpTests.stop
    observation = http.PineHttpTests.observation
    data = broker_http.PineBrokerHttpTests.data

    def setUp(self):
        broker_http.PineBrokerHttpTests.setUp(self)
        self.receipts = RECEIPTS / "http"
        self.receipts.mkdir(parents=True, exist_ok=True)
        self.order_ids = {}

    def register_order(self, room, raw_id, canonical_id):
        mapping = self.order_ids.setdefault(room, {1: "seed-1", 2: "seed-2"})
        self.assertTrue(raw_id is not None)
        self.assertEqual(mapping.get(raw_id, canonical_id), canonical_id, "one raw order cannot have two identities")
        self.assertFalse(any(value == canonical_id and key != raw_id for key, value in mapping.items()),
            "order identity mapping must remain bijective")
        mapping[raw_id] = canonical_id

    def normalize_order_ids(self, room, value):
        # HTTP allocations share one AtomicU64 across all rooms (see
        # room_runtime::reserve_order_ids and venue_orders::submit). Preserve
        # identities with a verified bijection; every other value stays exact.
        if isinstance(value, list):
            return [self.normalize_order_ids(room, item) for item in value]
        if not isinstance(value, dict):
            return value
        result = {}
        for key, item in value.items():
            if key in ("order_id", "maker_order_id", "taker_order_id") and item is not None:
                mapping = self.order_ids[room]
                self.assertIn(item, mapping, f"unmapped order {item} in {room}")
                result[key] = mapping[item]
            else:
                result[key] = self.normalize_order_ids(room, item)
        return result

    def create_room(self, name, script, native=False, reject=False, bar_interval_ms=1000):
        spec = http.recipe(name, 7, False, pine=True)
        plugin = copy.deepcopy(spec["agents"][20]["Plugin"])
        plugin["config"].update(script=script, inputs={}, bar_interval_ms=bar_interval_ms,
            history_limit=128, execution_mode="broker", max_qty=2, inventory_cap=10,
            slippage_ticks=2, fee_buffer_ppm=1000, initial_avg_price_tick=0)
        if native:
            plugin.update(plugin_id=PORTS[script], plugin_version="1.0.0")
            plugin["config"] = {key: plugin["config"][key] for key in COMMON}
        scenario = spec["scenario"]
        scenario["accounts"] = [
            {"Spot": {"account_id": 10, "cash_balance": 1000000, "position_qty": 10000}},
            {"Spot": {"account_id": 20, "cash_balance": 1000000, "position_qty": 10000}},
            {"Spot": {"account_id": 300, "cash_balance": 10000, "position_qty": 0}},
        ]
        scenario["seed_orders"] = [{"NewOrder": {"order_id": index + 1, "account_id": account,
            "side": side, "kind": {"Limit": {"price_tick": price}}, "qty": 1, "reduce_only": False}}
            for index, (account, side, price) in enumerate(((20, "Buy", 99), (10, "Sell", 101)))]
        scenario["market"]["Spot"]["clearing"].update(maker_fee_ppm=1000, taker_fee_ppm=20000)
        if reject:
            scenario["market"]["Spot"]["risk"]["max_order_qty"] = 1
        scenario["extra_markets"] = []
        self.client.start_training({"run_id": name, "scenario": scenario, "agents": [{"Plugin": plugin}],
            "manual_agents": True, "trainee_account_id": 20, "target_qty": 10000, "horizon_steps": 1000})
        self.order_ids[name] = {1: "seed-1", 2: "seed-2"}
        self.client.pause_room(name)

    def tick(self, room, price, index, reject=False):
        self.client._request("POST", f"/rooms/{room}/resume", {})
        for account in (10, 20):
            for order in self.observation(room, account)["own_orders"]:
                self.client.cancel(room, account, order["order_id"])
        for label, account, side, tick, qty in (("print-ask", 10, "Sell", price, 1),
                ("print-bid", 20, "Buy", price, 1), ("ask", 10, "Sell", price + 1, 1),
                ("bid", 20, "Buy", price - 1, 1 if reject else 100)):
            result = self.client.place(room, account, side, tick, qty)
            self.assertTrue(result["accepted"], result)
            accepted = [event["order_id"] for event in result["events"] if event["type"] == "OrderAccepted"]
            self.assertEqual(len(accepted), 1, result)
            self.register_order(room, accepted[0], f"bar-{index}-{label}")
        self.client.pause_room(room)
        return self.client.step_bots(room, f"{room}-step-{index}")

    def raw_semantic_observation(self, room):
        observation = self.observation(room, 300)
        return {key: observation[key] for key in ("own_account", "own_orders", "book", "public_trades", "market_time_ms", "step")}

    def semantic_observation(self, room):
        return self.normalize_order_ids(room, self.raw_semantic_observation(room))

    def assert_pair(self, pine_room, native_room, pine_saved, native_saved, same_closed_bar=None):
        pine_data, native_data = self.data(pine_saved), self.data(native_saved)
        pine_receipts, native_receipts = pine_data.get("_host_receipts", []), native_data.get("_host_receipts", [])
        self.assertEqual(len(pine_receipts), len(native_receipts))
        for pine_receipt, native_receipt in zip(pine_receipts, native_receipts):
            self.assertEqual(pine_receipt["command_seq"], native_receipt["command_seq"])
            for room, receipt in ((pine_room, pine_receipt), (native_room, native_receipt)):
                if receipt.get("order_id") is not None:
                    self.register_order(room, receipt["order_id"], f"bot-command-{receipt['command_seq']}")
        pine_raw, native_raw = self.raw_semantic_observation(pine_room), self.raw_semantic_observation(native_room)
        pine_observation, native_observation = self.normalize_order_ids(pine_room, pine_raw), self.normalize_order_ids(native_room, native_raw)
        (self.receipts / "latest-http-pair.json").write_text(json.dumps({
            "pine_room": pine_room, "native_room": native_room, "pine": pine_data, "native": native_data,
            "pine_raw_observation": pine_raw, "native_raw_observation": native_raw,
            "pine_observation": pine_observation, "native_observation": native_observation,
            "order_id_mapping": {pine_room: self.order_ids[pine_room], native_room: self.order_ids[native_room]}}, indent=2), encoding="utf-8")
        if same_closed_bar is None:
            self.assertEqual(semantic_actions(pine_data.get("last_actions", [])), semantic_actions(native_data.get("last_actions", [])))
        else:
            # Pine writes last_actions only in its new-closed-bars branch
            # (pine_inprocess.rs and Python broker.py). It remains the previous
            # bar's diagnostic here, while native clears it. Actual submissions
            # are proven by the host receipts, never this diagnostic field.
            self.assertEqual(pine_receipts, [])
            self.assertEqual(native_receipts, [])
            self.assertEqual(pine_data["last_actions"], same_closed_bar["pine"]["last_actions"])
            self.assertEqual(native_data["last_actions"], [])
            for key in ("own_account", "own_orders", "book", "public_trades"):
                self.assertEqual(native_observation[key], same_closed_bar["observation"][key])
            self.assertEqual(pine_data["evaluated_bars"], same_closed_bar["pine"]["evaluated_bars"])
            self.assertEqual(native_data["evaluated_bars"], same_closed_bar["native"]["evaluated_bars"])
            self.assertEqual(pine_data["bars"][-1], same_closed_bar["pine"]["bars"][-1])
            self.assertEqual(native_data["recent_bars"][-1], same_closed_bar["native"]["recent_bars"][-1])
        self.assertEqual(pine_observation, native_observation)
        self.assertEqual(pine_data.get("observed_position", 0), native_data.get("observed_position", 0))
        self.assertEqual(pine_data.get("evaluated_bars", 0), native_data.get("evaluated_bars", 0))
        def normalize_receipt(room, receipt):
            result = {key: value for key, value in receipt.items() if key != "key"}
            result["action"] = semantic_actions([{"BotOrder": receipt}])[0]["action"]
            error = result.get("error")
            if isinstance(error, str):
                # record_bot_execution formats RiskRejected with Debug, which
                # embeds the same globally allocated order ID in error text.
                match = re.fullmatch(r"(RiskRejected \{ order_id: )(\d+)(, reason: [A-Za-z]+ \})", error)
                if match:
                    raw_id = int(match.group(2))
                    self.assertEqual(raw_id, receipt["order_id"], "debug rejection ID must match its receipt")
                    result["error"] = match.group(1) + self.order_ids[room][raw_id] + match.group(3)
            return self.normalize_order_ids(room, result)
        self.assertEqual([normalize_receipt(pine_room, r) for r in pine_receipts], [normalize_receipt(native_room, r) for r in native_receipts])
        return {"pine": pine_data, "native": native_data, "observation": native_observation,
            "pine_raw_observation": pine_raw, "native_raw_observation": native_raw}

    def record(self, cases):
        payload = {"test": self._testMethodName, "source_sha256": hashes(),
            "server_sha256": hashlib.sha256(self.binary.read_bytes()).hexdigest(),
            "pine_in_process": self.env.get("MARKETFORGE_PINE_IN_PROCESS") == "1",
            "order_id_mapping": self.order_ids, "cases": cases}
        (self.receipts / f"{self._testMethodName}.json").write_text(json.dumps(payload, indent=2), encoding="utf-8")

    def cases(self):
        return {"ma.pine": [*range(100, 108), *([80] * 12)],
            "rsi.pine": [*range(110, 102, -1), *([120] * 12)],
            "breakout.pine": [*([100] * 6), 110, *([80] * 12)]}

    def test_all_ports_complete_orders_partial_fills_fees_and_idempotency(self):
        self.start()
        catalog = {item["id"]: item for item in self.client.list_bots()}
        for port in PORTS.values():
            self.assertEqual(catalog[port]["runtime"], "builtin")
        receipt = []
        for script, prices in self.cases().items():
            prefix = f"native-pair-{script}-{time.time_ns()}"
            pine_room, native_room = prefix + "-pine", prefix + "-native"
            self.create_room(pine_room, script)
            self.create_room(native_room, script, native=True)
            rows, max_position, bought, sold = [], 0, False, False
            for index, price in enumerate(prices):
                pine_saved = self.tick(pine_room, price, index)
                native_saved = self.tick(native_room, price, index)
                row = self.assert_pair(pine_room, native_room, pine_saved, native_saved)
                rows.append(row)
                for room, saved in ((pine_room, pine_saved), (native_room, native_saved)):
                    self.assertEqual(self.client.step_bots(room, f"{room}-step-{index}"), saved)
                position = int(row["observation"]["own_account"]["Spot"]["position_qty"])
                max_position = max(max_position, position)
                trades = row["observation"]["public_trades"]
                bought |= any(t["taker_account_id"] == 300 and t["taker_side"] == "Buy" for t in trades)
                sold |= any(t["taker_account_id"] == 300 and t["taker_side"] == "Sell" for t in trades)
            account = rows[-1]["observation"]["own_account"]["Spot"]
            self.assertTrue(bought and sold, script)
            self.assertEqual(max_position, 1, "two-unit entry must partially fill exactly one unit")
            self.assertEqual(int(account["position_qty"]), 0)
            self.assertGreater(int(account["fees_paid"]), 0)
            receipt.append({"script": script, "prices": prices, "max_position": max_position, "rows": rows})
        # Eight 5-second MA bars, with a real partial entry on the last one.
        # Advance only 1 second afterwards so the fill's forming candle stays
        # open. Verify the candle cursor itself rather than inferring it from
        # the lack of external printed trades.
        prefix = f"native-same-bar-{time.time_ns()}"
        pine_room, native_room = prefix + "-pine", prefix + "-native"
        self.create_room(pine_room, "ma.pine", bar_interval_ms=5000)
        self.create_room(native_room, "ma.pine", native=True, bar_interval_ms=5000)
        warmup_rows = []
        for index in range(40):
            price = 100 + index // 5
            pine_saved = self.tick(pine_room, price, index)
            native_saved = self.tick(native_room, price, index)
            warmup_rows.append(self.assert_pair(pine_room, native_room, pine_saved, native_saved))
        before = warmup_rows[-1]
        self.assertEqual(before["native"]["evaluated_bars"], 8)
        self.assertEqual(int(before["observation"]["own_account"]["Spot"]["position_qty"]), 1)
        repeated_pine = self.client.step_bots(pine_room, f"{pine_room}-within-bar")
        repeated_native = self.client.step_bots(native_room, f"{native_room}-within-bar")
        repeated = self.assert_pair(pine_room, native_room, repeated_pine, repeated_native, same_closed_bar=before)
        for key in ("own_account", "own_orders", "book", "public_trades"):
            self.assertEqual(repeated["observation"][key], before["observation"][key])
        self.assertEqual(repeated["observation"]["market_time_ms"], before["observation"]["market_time_ms"] + 1000)
        self.assertEqual(repeated["observation"]["step"], before["observation"]["step"] + 1)
        self.assertEqual(repeated["pine"]["evaluated_bars"], before["pine"]["evaluated_bars"])
        self.assertEqual(repeated["native"]["evaluated_bars"], before["native"]["evaluated_bars"])
        self.assertEqual(repeated["pine"]["bars"][-1], before["pine"]["bars"][-1])
        self.assertEqual(repeated["native"]["recent_bars"][-1], before["native"]["recent_bars"][-1])
        self.assertEqual(repeated["native"]["last_actions"], [])
        receipt.append({"script": "ma.pine", "bar_interval_ms": 5000, "same_closed_bar": True,
            "pine_last_actions_is_previous_bar_diagnostic": True,
            "warmup_rows": warmup_rows, "after_one_second": repeated})
        self.record(receipt)

    def test_gateway_rejection_stays_flat_and_next_bar_retries(self):
        self.start()
        receipt = []
        for script, all_prices in self.cases().items():
            prices = all_prices[:8 if script != "breakout.pine" else 7]
            prefix = f"native-reject-{script}-{time.time_ns()}"
            pine_room, native_room = prefix + "-pine", prefix + "-native"
            self.create_room(pine_room, script, reject=True)
            self.create_room(native_room, script, native=True, reject=True)
            rows = []
            for index, price in enumerate([*prices, prices[-1] + (-1 if script == "rsi.pine" else 1)]):
                pine_saved = self.tick(pine_room, price, index, reject=True)
                native_saved = self.tick(native_room, price, index, reject=True)
                rows.append(self.assert_pair(pine_room, native_room, pine_saved, native_saved))
            for row in rows[-2:]:
                host_receipts = row["native"]["_host_receipts"]
                self.assertTrue(host_receipts)
                self.assertFalse(host_receipts[0]["accepted"])
                self.assertEqual(int(row["observation"]["own_account"]["Spot"]["position_qty"]), 0)
            receipt.append({"script": script, "rows": rows})
        self.record(receipt)

    def test_postgres_restart_after_partial_fills_preserves_subsequent_actions(self):
        dsn = os.environ.get("MARKETFORGE_TEST_DATABASE_URL")
        if not dsn:
            if os.environ.get("MARKETFORGE_REQUIRE_POSTGRES_TESTS") == "1":
                self.fail("required native/Pine durable restart needs caller's test database")
            self.skipTest("PostgreSQL test database not set")
        self.env["MARKETFORGE_DATABASE_URL"] = dsn
        self.start()
        rooms, receipt = [], []
        for script, prices in self.cases().items():
            prefix = f"native-restart-{script}-{time.time_ns()}"
            pine_room, native_room = prefix + "-pine", prefix + "-native"
            self.create_room(pine_room, script)
            self.create_room(native_room, script, native=True)
            warmup = 8 if script != "breakout.pine" else 7
            for index, price in enumerate(prices[:warmup]):
                pine_saved = self.tick(pine_room, price, index)
                native_saved = self.tick(native_room, price, index)
                self.assert_pair(pine_room, native_room, pine_saved, native_saved)
            before = self.semantic_observation(native_room)
            self.assertEqual(int(before["own_account"]["Spot"]["position_qty"]), 1)
            rooms.append((script, prices, warmup, pine_room, native_room, pine_saved, native_saved, before))
        self.stop()
        self.start()
        for script, prices, warmup, pine_room, native_room, pine_saved, native_saved, before in rooms:
            for room, saved in ((pine_room, pine_saved), (native_room, native_saved)):
                self.assertEqual(self.client.room_bots(room), saved)
                self.assertEqual(self.client.step_bots(room, f"{room}-step-{warmup - 1}"), saved)
                self.assertEqual(self.semantic_observation(room), before)
            rows = []
            for index in range(warmup, len(prices)):
                pine_saved = self.tick(pine_room, prices[index], index)
                native_saved = self.tick(native_room, prices[index], index)
                rows.append(self.assert_pair(pine_room, native_room, pine_saved, native_saved))
            self.assertEqual(int(rows[-1]["observation"]["own_account"]["Spot"]["position_qty"]), 0)
            receipt.append({"script": script, "before": before, "restored": True, "rows_after_restart": rows})
        self.record(receipt)


if __name__ == "__main__":
    unittest.main()
