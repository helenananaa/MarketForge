"""Full response/state differential checks for the optional direct Rust adapter."""
import copy
import json
import os
from pathlib import Path
import random
import subprocess
import sys
import unittest

from test_pine_bot import bot, ROOT
from test_pine_broker import broker_request, perp, ack


class PineInProcessTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        executable = Path(os.environ.get("MARKETFORGE_PINE_PROBE", ROOT / "target/release/examples/pine_inprocess_probe.exe"))
        if not executable.is_file():
            if os.environ.get("MARKETFORGE_REQUIRE_INPROCESS_TESTS") == "1":
                raise AssertionError("build pine_inprocess_probe with pine-in-process first")
            raise unittest.SkipTest("optional direct Rust Pine probe not built")
        cls.process = subprocess.Popen([str(executable), str(ROOT / "bot-plugins/pine-strategy")],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8")
        cls.addClassCleanup(cls.close)

    @classmethod
    def close(cls):
        cls.process.stdin.close()
        cls.process.wait(timeout=10)
        cls.process.stdout.close()
        cls.process.stderr.close()

    def rust(self, req, reset=False, timeout_ms=5000):
        self.process.stdin.write(json.dumps({"request":req, "reset":reset, "timeout_ms":timeout_ms}) + "\n")
        self.process.stdin.flush()
        line = self.process.stdout.readline()
        if not line:
            raise AssertionError(self.process.stderr.read())
        return json.loads(line)

    def compare(self, req, reset=False):
        frozen = copy.deepcopy(req)
        expected = bot.decide(req)
        actual = self.rust(req, reset)
        self.assertNotIn("error", actual, actual)
        self.assertEqual(actual["response"], expected)
        self.assertEqual(req, frozen)
        return expected

    def test_six_sources_cached_rebuild_and_uncommitted_retry(self):
        scripts = json.loads((ROOT / "bot-plugins/pine-strategy/incremental-qualified.json").read_text())["source_sha256"]
        for script in scripts:
            for seed in range(3):
                rng = random.Random(seed)
                prices = [100 + rng.randrange(-8, 9) for _ in range(32)]
                state = None
                for end in range(1, 33):
                    req = broker_request(prices[:end], state=state)
                    req["config"]["script"] = script
                    req["participant"]["participant_id"] = f"策略🐱-{script}-{seed}"
                    if script == "perp.pine":
                        req = perp(req)
                    if script == "basis.pine":
                        req["config"]["related_instruments"] = ["V-BTC-PERP"]
                        req["observation"]["related_markets"] = [copy.deepcopy(perp(broker_request([p+3 for p in prices[:end]]))["observation"])]
                    result = self.compare(req, reset=end in (1,13,24))
                    if end in (12,23):
                        self.assertEqual(self.rust(req)["response"], result, "uncommitted result must rebuild")
                    state = json.loads(json.dumps(result["state"]))

    def test_exact_expanded_request_and_ascii_response_bounds(self):
        req = broker_request([100, 101, 102])
        initial = self.compare(req, reset=True)
        req["state"] = initial["state"]
        req["state"]["padding"] = ""
        expected = bot.decide(copy.deepcopy(req))
        length = len(json.dumps(expected, ensure_ascii=True).encode("ascii"))
        req["state"]["padding"] = "x" * (1048576 - length)
        self.assertNotIn("error", self.rust(req))
        req["state"]["padding"] += "x"
        self.assertIn("action/state limits", self.rust(req)["error"])
        # UTF-8 request fits but Python's ensure_ascii response does not.
        req["state"]["padding"] = "策略🐱" * 50000
        self.assertIn("action/state limits", self.rust(req)["error"])
        req["state"]["padding"] = "x" * (4 * 1024 * 1024)
        self.assertIn("request exceeds size limit", self.rust(req)["error"])
        req["state"].pop("padding")
        self.compare(req)

    def test_realistic_partial_fill_then_close_and_fee_feedback(self):
        prices = list(range(100,108))
        req = broker_request(prices)
        first = self.compare(req, reset=True)
        self.assertTrue(first["actions"])
        req = broker_request(prices+[80])
        ack(req, first)
        req["observation"]["own_account"]["Spot"].update(position_qty=1, available_position=1, cash_balance=9897, available_cash=9897, fees_paid=2)
        data=req["observation"]["bot_market_data"]
        data["own_fills"]=[{"trade_id":1,"maker_account_id":10,"taker_account_id":300,"maker_order_id":10,"taker_order_id":50,"taker_side":"Buy","qty":1,"price_tick":101}]
        data["fill_details"]=[{"trade_id":1,"market_time_ms":8000,"fee_paid":2}]
        second=self.compare(req)
        next_req=broker_request(prices+[80,79])
        ack(next_req,second)
        for key in ("own_fills","fill_details"):
            next_req["observation"]["bot_market_data"][key]=copy.deepcopy(data[key])
        next_req["observation"]["own_account"]=copy.deepcopy(req["observation"]["own_account"])
        self.compare(next_req,reset=True)

    def test_rational_cost_basis_and_frozen_historical_accounts(self):
        req=broker_request([100,101,102,103])
        req["observation"]["own_account"]["Spot"].update(position_qty=2, available_position=2, cash_balance=9804, available_cash=9804)
        data=req["observation"]["bot_market_data"]
        data["own_fills"]=[];data["fill_details"]=[]
        for index,(side,qty,price) in enumerate((("Buy",2,101),("Buy",1,103),("Sell",1,110))):
            data["own_fills"].append({"trade_id":index,"maker_account_id":10,"taker_account_id":300,"maker_order_id":10+index,"taker_order_id":50+index,"taker_side":side,"qty":qty,"price_tick":price})
            data["fill_details"].append({"trade_id":index,"market_time_ms":(index+1)*1000,"fee_paid":0})
        result=self.compare(req,reset=True)
        self.assertEqual(result["state"]["ledgers"]["V-BTC-SPOT"]["cost"],"610/3")

    def test_limit_cancel_acknowledgement_and_replacement(self):
        req=broker_request([100,100,100,100,95]);req["config"]["script"]="limit.pine"
        first=self.compare(req,reset=True)
        self.assertTrue(first["actions"])
        req=broker_request([100,100,100,100,95,96]);req["config"]["script"]="limit.pine";ack(req,first)
        req["observation"]["own_orders"]=[{"order_id":50,"side":"Buy","price_tick":93,"remaining_qty":2}]
        second=self.compare(req)
        self.assertIn("Cancel",second["actions"][0]["BotOrder"]["action"])
        ack(req,second);req["observation"]["own_orders"]=[]
        third=self.compare(req)
        self.assertIn("PlaceProtected",third["actions"][0]["BotOrder"]["action"])

    def test_changed_prefix_and_timeout_fail_without_cached_advancement(self):
        req=broker_request([100,101])
        first=self.compare(req,reset=True)
        changed=broker_request([90,101,102],state=first["state"])
        with self.assertRaises(ValueError):bot.decide(changed)
        self.assertIn("error",self.rust(changed))
        next_req=broker_request([100,101,102],state=first["state"])
        self.assertIn("error",self.rust(next_req,timeout_ms=0))
        self.compare(next_req)


if __name__ == "__main__":unittest.main()
