"""Durable ecosystem replay parity, migration and failed-attempt recovery."""
import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest

from test_pine_bot import bot, ROOT
from test_pine_broker import broker_request, perp, ack
sys.path.insert(0, str(ROOT / "scripts"))
from pine_receipt_projection import trading_projection


class PineReplayTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        executable = Path(os.environ.get("MARKETFORGE_PINE_PROBE", ROOT / "target/release/examples/pine_inprocess_probe.exe"))
        if not executable.is_file():
            if os.environ.get("MARKETFORGE_REQUIRE_INPROCESS_TESTS") == "1":
                raise AssertionError("build pine_inprocess_probe first")
            raise unittest.SkipTest("optional direct Rust probe not built")
        cls.process = subprocess.Popen([str(executable), str(ROOT / "bot-plugins/pine-strategy")],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8",
            env={**os.environ, "MARKETFORGE_PINE_REPLAY_STATE":"1"})
        cls.addClassCleanup(cls.close)

    @classmethod
    def close(cls):
        cls.process.stdin.close()
        cls.process.wait(timeout=10)
        cls.process.stdout.close()
        cls.process.stderr.close()

    def rust(self, req, reset=False, timeout=10000):
        self.process.stdin.write(json.dumps({"request":req,"reset":reset,"timeout_ms":timeout}) + "\n")
        self.process.stdin.flush()
        return json.loads(self.process.stdout.readline())

    def request(self, count, previous=None):
        req = perp(broker_request([100000 + (i % 37) * 100 for i in range(count)]), cash=100000000)
        req["config"].update(script="ecosystem/library/e9a07449adc7b0ba.pine",
                             quantize_strategy_orders=True, history_limit=2048)
        req["observation"]["bot_market_data"]["private_history_complete"] = True
        req["observation"]["instrument"] = {"tick_size":1,"lot_size":1}
        if previous:
            ack(req, previous)
            for receipt in req["state"]["_host_receipts"]:
                receipt.update(accepted=False, order_id=None, error="test rejection")
        return req

    def test_archives_append_retry_and_cold_restore_match_python(self):
        previous = expected = None
        for count in (1,31,32,33,64,65,128,129,512):
            req = self.request(count, previous)
            expected = bot.decide(self.request(count, expected))
            result = self.rust(req, reset=count in (1,33,129))
            self.assertNotIn("error", result, result)
            previous = result["response"]
            self.assertEqual(previous["state"]["broker_schema"], 4)
            self.assertEqual(trading_projection(previous), trading_projection(expected))
            self.assertEqual(self.rust(req)["response"], previous)
            self.assertEqual(self.rust(req, reset=True)["response"], previous)
        self.assertTrue(previous["state"]["vm_checkpoint"]["chunks"])
        self.assertNotIn("bars", previous["state"])

    def test_python_state_migration_checks_old_intents_and_continues(self):
        old = bot.decide(self.request(128))
        req = self.request(129, old)
        actual = self.rust(req, reset=True)
        self.assertNotIn("error", actual, actual)
        expected = bot.decide(copy.deepcopy(req))
        self.assertEqual(trading_projection(actual["response"]), trading_projection(expected))
        self.assertEqual(actual["response"]["state"]["broker_schema"], 4)
        self.assertEqual(self.rust(req, reset=True), actual)
        next_req = self.request(256, actual["response"])
        next_result = self.rust(next_req, reset=True)
        self.assertNotIn("error", next_result, next_result)
        expected = bot.decide(self.request(256, expected))
        self.assertEqual(trading_projection(next_result["response"]), trading_projection(expected))
        broken = copy.deepcopy(req)
        broken["state"]["intents_digest"] = "0" * 64
        self.assertIn("intents changed", self.rust(broken, reset=True)["error"])

    def test_corrupt_history_archive_timeout_and_failed_response_do_not_advance(self):
        first = self.rust(self.request(32), reset=True)["response"]
        valid = self.request(33, first)
        expected = self.rust(valid, reset=True)
        broken = copy.deepcopy(valid)
        broken["state"]["vm_checkpoint"]["chunks"][0] = "ffffffff00"
        self.assertIn("execution seal", self.rust(broken)["error"])
        state = broken["state"]
        payload = {"protocol":"marketforge.pine-execution/1", "vm":{
            "checkpoint":state["vm_checkpoint"],"tail":state["vm_tail"],"intents":state["vm_tail_intents"]},
            "evaluated":state["evaluated_bars"],"last_time":state["last_bar_time"],"audits":state["candle_audits"]}
        state["execution_seal"] = hashlib.sha256(json.dumps(payload, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
        self.assertIn("decoded size limit", self.rust(broken, reset=True)["error"])
        broken = copy.deepcopy(valid)
        broken["observation"]["bot_market_data"]["candles"][0]["close_tick"] += 1
        self.assertIn("prefix changed", self.rust(broken)["error"])
        broken = copy.deepcopy(valid)
        broken["observation"]["bot_market_data"]["private_history_complete"] = False
        self.assertIn("private history required", self.rust(broken)["error"])
        self.assertIn("error", self.rust(valid, timeout=0))
        oversized = copy.deepcopy(valid)
        oversized["state"]["padding"] = "x" * 1048576
        self.assertIn("action/state limits", self.rust(oversized)["error"])
        self.assertEqual(self.rust(valid), expected)
        self.assertEqual(self.rust(valid, reset=True), expected)

    def test_duplicate_primary_timeframe_coalesces_only_identical_history(self):
        req = self.request(32)
        req["config"]["additional_intervals_ms"] = [1000]
        data = req["observation"]["bot_market_data"]
        data["streams"] = [{"instrument_id":req["observation"]["instrument_id"],
                            "interval_ms":data["interval_ms"], "truncated":False,
                            "candles":copy.deepcopy(data["candles"])}]
        expected = bot.decide(copy.deepcopy(req))
        result = self.rust(req, reset=True)
        self.assertNotIn("error", result, result)
        self.assertEqual(trading_projection(result["response"]), trading_projection(expected))
        # Also migrate a legacy state carrying the same deduplicated provider.
        req["state"] = expected["state"]
        migrated = self.rust(req, reset=True)
        self.assertNotIn("error", migrated, migrated)
        self.assertEqual(migrated["response"]["state"]["broker_schema"], 4)
        broken = copy.deepcopy(req)
        broken["state"] = result["response"]["state"]
        broken["observation"]["bot_market_data"]["streams"][0]["candles"][0]["close_tick"] += 1
        self.assertIn("conflicting duplicate", self.rust(broken)["error"])


if __name__ == "__main__":
    unittest.main()
