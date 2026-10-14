"""Fixed bar-by-bar Pine/Rust ports, authoritative feedback and JSON recovery.

Build exchange-server --example native_pine_probe and point
MARKETFORGE_NATIVE_PINE_PROBE at that exact executable. Receipts retain source
hashes, original actions and nullable Pine plot values; only envelope identity
keys and omitted default order fields are normalized for semantic comparison.
"""
from __future__ import annotations

import copy
import hashlib
import json
import os
from pathlib import Path
import random
import subprocess
import unittest
from unittest.mock import patch

from test_pine_bot import ROOT, bot
from test_pine_broker import ack, broker_request

PORTS = {"ma.pine": "NativePineMa", "rsi.pine": "NativePineRsi", "breakout.pine": "NativePineBreakout"}
INDICATORS = {"ma.pine": ("fast_ma", "slow_ma"), "rsi.pine": ("rsi",), "breakout.pine": ("upper", "lower")}
COMMON = ("bar_interval_ms", "history_limit", "max_qty", "inventory_cap", "slippage_ticks", "fee_buffer_ppm", "initial_avg_price_tick")
INPUTS = {"Fast": "fast", "Slow": "slow", "Length": "length", "Low": "low", "High": "high", "Quantity": "qty"}
RECEIPTS = Path(os.environ.get("MARKETFORGE_NATIVE_PINE_RECEIPTS", ROOT / ".local/native-pine-20261013"))


def hashes():
    files = ["exchange-core/src/native_pine.rs", "exchange-core/Cargo.toml", "bot-plugins/pine-strategy/bot.py",
             "bot-plugins/pine-strategy/broker.py", "bot-plugins/pine-strategy/incremental.py",
             "bot-plugins/pine-strategy/bot.json", "exchange-server/examples/native_pine_probe.rs",
             "python/tests/test_native_pine.py", "python/tests/test_native_pine_http.py",
             *[f"bot-plugins/pine-strategy/scripts/{name}" for name in PORTS]]
    return {name: hashlib.sha256((ROOT / name).read_bytes()).hexdigest() for name in files}


def native_request(pine_request, state=None):
    request = copy.deepcopy(pine_request)
    request.update(plugin_id=PORTS[request["config"]["script"]], plugin_version="1.0.0", state=state)
    config = {key: request["config"][key] for key in COMMON}
    config.update({INPUTS[key]: value for key, value in request["config"]["inputs"].items()})
    request["config"] = config
    # Python's broker fixtures omit unused public metadata. The Rust wire type
    # requires these fields, so supply the same fixed values to both callers.
    request["observation"].setdefault("venue_id", "default-venue")
    request["observation"].setdefault("public_trades", [])
    return request


def semantic_actions(actions):
    normalized = []
    for original in actions:
        envelope = original["BotOrder"]
        action = copy.deepcopy(envelope["action"])
        if "PlaceProtected" in action:
            order = action["PlaceProtected"]
            order.setdefault("position_side", "Both")
            order.setdefault("reduce_only", False)
            for name in ("valid_until_market_time_ms", "expires_at_market_time_ms"):
                if order.get(name) is None:
                    order.pop(name, None)
        normalized.append({"instrument_id": envelope["instrument_id"], "action": action})
    return normalized


class NativePineDifferentialTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        import pine_compat
        if pine_compat.__version__ != "0.3.1":
            raise AssertionError("native ports require qualified Pine 0.3.1 for differential checks")
        executable = Path(os.environ.get("MARKETFORGE_NATIVE_PINE_PROBE", ROOT / "target/release/examples/native_pine_probe.exe"))
        if not executable.is_file():
            if os.environ.get("MARKETFORGE_REQUIRE_NATIVE_PINE_TESTS") == "1":
                raise AssertionError("build native_pine_probe before required differential checks")
            raise unittest.SkipTest("native_pine_probe not built")
        cls.process = subprocess.Popen([str(executable)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, text=True, encoding="utf-8")
        cls.addClassCleanup(cls.close)
        RECEIPTS.mkdir(parents=True, exist_ok=True)
        cls.binary_hash = hashlib.sha256(executable.read_bytes()).hexdigest()

    @classmethod
    def close(cls):
        cls.process.stdin.close()
        cls.process.wait(timeout=10)
        cls.process.stdout.close()
        cls.process.stderr.close()

    def rust_result(self, request, **options):
        self.process.stdin.write(json.dumps({"request": request, **options}) + "\n")
        self.process.stdin.flush()
        line = self.process.stdout.readline()
        if not line:
            self.fail(self.process.stderr.read())
        return json.loads(line)

    def rust(self, request, **options):
        result = self.rust_result(request, **options)
        if "error" in result:
            (RECEIPTS / "latest-differential-error.json").write_text(json.dumps(
                {"request": request, "options": options, "result": result}, indent=2), encoding="utf-8")
        self.assertNotIn("error", result, result)
        return result["response"]

    def compare(self, request, native_state=None, **options):
        captured = []
        module = bot.incremental_module()
        evaluate = module.evaluate
        def evaluate_and_capture(*args, **kwargs):
            result = evaluate(*args, **kwargs)
            captured.append(result)
            return result
        frozen = copy.deepcopy(request)
        with patch.object(module, "evaluate", side_effect=evaluate_and_capture):
            expected = bot.decide(request)
        req = native_request(request, native_state)
        actual = self.rust(req, **options)
        (RECEIPTS / "latest-differential-step.json").write_text(json.dumps(
            {"pine_request": request, "native_request": req, "pine_response": expected,
             "native_response": actual, "pine_output": captured[-1] if captured else None}, indent=2), encoding="utf-8")
        self.assertEqual(request, frozen, "decision input must remain immutable")
        self.assertEqual(semantic_actions(actual["actions"]), semantic_actions(expected["actions"]))
        if captured:
            expected_indicators = {key: plot["values"][-1] for key, plot in zip(
                INDICATORS[request["config"]["script"]], captured[-1]["output"]["plots"])}
            for key, value in expected_indicators.items():
                native_value = actual["state"]["indicators"][key]
                if value is None:
                    self.assertIsNone(native_value, key)
                else:
                    self.assertAlmostEqual(native_value, value, places=10, msg=key)
        else:
            expected_indicators = None
        self.assertEqual(actual["state"]["evaluated_bars"], len(request["observation"]["bot_market_data"]["candles"]))
        # A fresh factory restored from JSON must reproduce every field and
        # action of the cached native instance on the same committed input.
        self.assertEqual(self.rust(copy.deepcopy(req), restore=True), actual)
        committed = native_request(request, actual["state"])
        duplicate = self.rust(committed, restore=True)
        self.assertEqual(duplicate["actions"], [])
        expected_state = copy.deepcopy(actual["state"])
        expected_state["last_actions"] = []
        if duplicate["state"] != expected_state:
            (RECEIPTS / "latest-state-roundtrip-error.json").write_text(json.dumps(
                {"request": committed, "expected_state": expected_state,
                 "actual_response": duplicate}, indent=2), encoding="utf-8")
        self.assertEqual(duplicate["state"], expected_state,
            "committed JSON must preserve all indicator and audit state exactly")
        return expected, actual, expected_indicators

    def record(self, cases):
        payload = {"test": self._testMethodName, "source_sha256": hashes(),
            "probe_sha256": self.binary_hash, "cases": cases}
        (RECEIPTS / f"{self._testMethodName}.json").write_text(json.dumps(payload, indent=2), encoding="utf-8")

    def test_warmup_flat_alternating_and_parameter_overrides(self):
        receipt = []
        cases = ([100] * 20, list(range(100, 120)), list(range(120, 100, -1)), [100, 110, 90, 100] * 8)
        for script in PORTS:
            overrides = ({"Fast": 2, "Slow": 5, "Quantity": 3} if script == "ma.pine" else
                {"Length": 3, "Low": 30, "High": 70, "Quantity": 3} if script == "rsi.pine" else
                {"Length": 3, "Quantity": 3})
            for inputs in ({}, overrides):
                for case_index, prices in enumerate(cases):
                    pine_state = native_state = None
                    pending_pine = pending_native = None
                    rows = []
                    for end in range(1, len(prices) + 1):
                        request = broker_request(prices[:end], pine_state)
                        request["config"].update(script=script, inputs=inputs, max_qty=5)
                        request["participant"]["participant_id"] = f"warmup-{script}-{case_index}-{bool(inputs)}"
                        if pending_pine is not None:
                            ack(request, pending_pine, 50 + end)
                            native_next = copy.deepcopy(request)
                            ack(native_next, pending_native, 50 + end)
                            native_state = native_next["state"]
                        expected, actual, indicators = self.compare(request, native_state, reset=end == 1)
                        pine_state, native_state = expected["state"], actual["state"]
                        pending_pine, pending_native = expected, actual
                        rows.append({"bar": end, "indicators": indicators, "pine_actions": expected["actions"], "native_actions": actual["actions"]})
                    receipt.append({"script": script, "inputs": inputs, "prices": prices, "rows": rows})
        self.record(receipt)

    def test_random_bars_realistic_partial_fills_fees_and_duplicates(self):
        receipt = []
        for script in PORTS:
            for seed in range(3):
                rng = random.Random(seed)
                prices = [100 + rng.randrange(-15, 16) for _ in range(64)]
                ranges = [(price + rng.randrange(0, 4), price - rng.randrange(0, 4)) for price in prices]
                pine_state = native_state = None
                cash, position, fees = 10000, 0, 0
                fills, details, rows = [], [], []
                pending_pine = pending_native = None
                for end in range(1, 65):
                    request = broker_request(prices[:end], pine_state)
                    request["config"]["script"] = script
                    request["participant"]["participant_id"] = f"filled-{script}-{seed}"
                    for candle, (high, low) in zip(request["observation"]["bot_market_data"]["candles"], ranges):
                        candle.update(high_tick=high, low_tick=low)
                    if pending_pine is not None:
                        ack(request, pending_pine, 50 + end)
                        native_next = copy.deepcopy(request)
                        ack(native_next, pending_native, 50 + end)
                        native_state = native_next["state"]
                    account = request["observation"]["own_account"]["Spot"]
                    account.update(cash_balance=cash, available_cash=cash, position_qty=position, available_position=position, fees_paid=fees)
                    request["observation"]["bot_market_data"].update(own_fills=copy.deepcopy(fills), fill_details=copy.deepcopy(details))
                    expected, actual, indicators = self.compare(request, native_state, reset=end == 1)
                    duplicate = copy.deepcopy(request)
                    duplicate["state"] = expected["state"]
                    self.assertEqual(bot.decide(duplicate)["actions"], [])
                    self.assertEqual(self.rust(native_request(duplicate, actual["state"]))["actions"], [])
                    rows.append({"bar": end, "account_before": account, "indicators": indicators,
                        "pine_actions": expected["actions"], "native_actions": actual["actions"]})
                    # Model a one-unit fill against an actual book quote. The
                    # separate HTTP suite verifies this through the gateway.
                    for envelope in expected["actions"]:
                        order = envelope["BotOrder"]["action"]["PlaceProtected"]
                        price = prices[end - 1] + (1 if order["side"] == "Buy" else -1)
                        qty = min(1, order["qty"])
                        cash += (-price * qty if order["side"] == "Buy" else price * qty) - 2
                        position += qty if order["side"] == "Buy" else -qty
                        fees += 2
                        trade_id = len(fills) + 1
                        fills.append({"trade_id": trade_id, "maker_account_id": 10, "taker_account_id": 300,
                            "maker_order_id": 1000 + trade_id, "taker_order_id": 50 + end + 1,
                            "taker_side": order["side"], "qty": qty, "price_tick": price})
                        details.append({"trade_id": trade_id, "market_time_ms": end * 1000, "fee_paid": 2})
                    pine_state, native_state = expected["state"], actual["state"]
                    pending_pine, pending_native = expected, actual
                receipt.append({"script": script, "seed": seed, "prices": prices,
                    "fills": fills, "fill_details": details, "final_cash": cash, "final_position": position, "fees": fees, "rows": rows})
        self.record(receipt)

    def test_rolling_history_and_compact_state_resume_match_complete_pine(self):
        receipt = []
        for script in PORTS:
            prices = [100 + ((index * 17) % 31) - 15 for index in range(128)]
            pine_state = native_state = None
            sizes, last_request = [], None
            for end in range(1, len(prices) + 1):
                request = broker_request(prices[:end], pine_state)
                request["config"].update(script=script, history_limit=256)
                request["participant"]["participant_id"] = f"rolling-{script}"
                request["observation"]["own_account"]["Spot"].update(cash_balance=0, available_cash=0)
                captured = []
                module = bot.incremental_module()
                evaluate = module.evaluate
                def capture(*args, **kwargs):
                    result = evaluate(*args, **kwargs)
                    captured.append(result)
                    return result
                with patch.object(module, "evaluate", side_effect=capture):
                    expected = bot.decide(request)
                native = native_request(request, native_state)
                native["config"]["history_limit"] = 16
                data = native["observation"]["bot_market_data"]
                data["candles"] = data["candles"][-17:]
                data["truncated"] = end > 17
                actual = self.rust(native, reset=end == 1, restore=end in (30, 90))
                self.assertEqual(semantic_actions(expected["actions"]), semantic_actions(actual["actions"]))
                self.assertEqual(actual["state"]["evaluated_bars"], end)
                for key, plot in zip(INDICATORS[script], captured[-1]["output"]["plots"]):
                    value = plot["values"][-1]
                    if value is None:
                        self.assertIsNone(actual["state"]["indicators"][key])
                    else:
                        self.assertAlmostEqual(actual["state"]["indicators"][key], value, places=10)
                sizes.append(len(json.dumps(actual["state"], separators=(",", ":")).encode()))
                pine_state, native_state = expected["state"], actual["state"]
                last_request = native
            # Once the audit overlap reaches its cap, serialized state remains
            # bounded while the total evaluated bar counter keeps increasing.
            self.assertLess(max(sizes), 16000)
            self.assertLess(abs(sizes[-1] - sizes[64]), 256)
            cold = copy.deepcopy(last_request)
            cold["state"] = None
            self.assertIn("cold initialization", self.rust_result(cold, restore=True)["error"])
            corrected = copy.deepcopy(last_request)
            corrected["state"] = native_state
            corrected["observation"]["bot_market_data"]["candles"][0]["close_tick"] += 1
            corrected["observation"]["bot_market_data"]["candles"][0]["high_tick"] += 1
            self.assertIn("history changed", self.rust_result(corrected, restore=True)["error"])
            # A failed correction audit must not advance the checkpoint.
            valid = copy.deepcopy(last_request)
            valid["state"] = native_state
            recovered = self.rust(valid, restore=True)
            self.assertEqual(recovered["state"]["evaluated_bars"], len(prices))
            self.assertEqual(recovered["actions"], [])
            receipt.append({"script": script, "prices": prices, "serialized_state_bytes": sizes,
                "total_bars": len(prices), "native_state": native_state,
                "cold_truncated_rejected": True, "corrected_retained_bar_rejected": True})
        self.record(receipt)

    def test_rejected_entry_no_phantom_position_then_next_bar_retry(self):
        receipt = []
        cases = {"ma.pine": list(range(100, 108)), "rsi.pine": list(range(110, 102, -1)), "breakout.pine": [100] * 6 + [110]}
        for script, prices in cases.items():
            request = broker_request(prices)
            request["config"]["script"] = script
            expected, actual, _ = self.compare(request, reset=True)
            self.assertTrue(actual["actions"])
            for response in (expected, actual):
                response["state"]["_host_receipts"] = [{"key": action["BotOrder"]["key"],
                    "instrument_id": action["BotOrder"]["instrument_id"], "action": action["BotOrder"]["action"],
                    "order_id": 50, "command_seq": 50, "market_time_ms": len(prices) * 1000,
                    "accepted": False, "error": "max_order_qty"} for action in response["actions"]]
            prices = [*prices, prices[-1] + (1 if script != "rsi.pine" else -1)]
            following = broker_request(prices, expected["state"])
            following["config"]["script"] = script
            expected_next, actual_next, _ = self.compare(following, actual["state"])
            self.assertTrue(actual_next["actions"], "rejected IOC must retry only on the next bar")
            self.assertEqual(actual_next["state"]["observed_position"], 0)
            receipt.append({"script": script, "rejected_pine": expected, "rejected_native": actual,
                "retry_pine": expected_next, "retry_native": actual_next})
        self.record(receipt)

    def test_existing_inventory_is_not_an_owned_strategy_entry(self):
        receipt = []
        cases = {"ma.pine": list(range(110, 102, -1)), "rsi.pine": list(range(100, 108)),
            "breakout.pine": [100] * 6 + [80]}
        for script, prices in cases.items():
            request = broker_request(prices)
            request["config"].update(script=script, initial_avg_price_tick=90)
            request["participant"]["participant_id"] = f"owned-{script}"
            request["observation"]["own_account"]["Spot"].update(position_qty=3, available_position=3)
            expected, actual, indicators = self.compare(request, reset=True)
            self.assertEqual(actual["actions"], [], "strategy.close(L) cannot close externally owned inventory")
            self.assertEqual(actual["state"]["observed_position"], 3)
            self.assertEqual(actual["state"]["entry_qty"], 0)
            receipt.append({"script": script, "pine": expected, "native": actual, "indicators": indicators})
        self.record(receipt)


if __name__ == "__main__":
    unittest.main()
