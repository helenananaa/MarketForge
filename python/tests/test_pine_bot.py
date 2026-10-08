"""Actual Pine execution, authoritative fills, frozen input and failure boundaries."""
from __future__ import annotations

import copy
import importlib.util
import json
import os
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("pine_bot", ROOT / "bot-plugins/pine-strategy/bot.py")
bot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bot)
sys.path.insert(0, str(ROOT / "scripts"))
from background_market import recipe


def request(prices, script="ma.pine", state=None):
    config = {name: value["default"] for name, value in json.loads(
        (ROOT / "bot-plugins/pine-strategy/bot.json").read_text(encoding="utf-8"))["bot"]["parameters"].items()}
    config.update(script=script, bar_interval_ms=1000)
    candles = [{"schema_version": 1, "open_time_ms": i * 1000, "close_time_ms": (i + 1) * 1000,
                "open_tick": price, "high_tick": price, "low_tick": price, "close_tick": price,
                "volume": 1, "quote_volume": price, "trades": 1, "is_final": True}
               for i, price in enumerate(prices)]
    return {"protocol_version": "bot.v1", "plugin_id": "pine.strategy", "plugin_version": "1.0.0",
            "state_version": 1, "seed": 7, "config": config, "state": state,
            "participant": {"participant_id": "pine", "room_id": "test", "account_id": 300,
                            "instrument_id": "V-BTC-SPOT", "kind": "RuleAgent"},
            "observation": {"version": 1, "room_id": "test", "instrument_id": "V-BTC-SPOT",
                "status": "Running", "step": len(prices), "market_time_ms": len(prices) * 1000,
                "book": {"bids": [{"price_tick": (prices[-1] if prices else 100) - 1, "qty": 100}],
                         "asks": [{"price_tick": (prices[-1] if prices else 100) + 1, "qty": 100}]},
                "own_orders": [], "own_account": {"Spot": {"account_id": 300, "cash_balance": 10000,
                    "position_qty": 0, "fees_paid": 0, "available_cash": 10000, "available_position": 0}},
                "bot_market_data": {"interval_ms": 1000, "candles": candles, "own_fills": [], "fill_details": [], "truncated": False}}}


def fill(req, side="Buy", qty=1, price=100, trade_id=1, cash=None, position=None, fees=0, maker=False, time=1000, fill_fee=None):
    data = req["observation"]["bot_market_data"]
    data["own_fills"].append({"trade_id": trade_id, "maker_account_id": 300 if maker else 10,
        "taker_account_id": 10 if maker else 300, "taker_side": ("Sell" if side == "Buy" else "Buy") if maker else side,
        "qty": qty, "price_tick": price})
    data["fill_details"].append({"trade_id": trade_id, "market_time_ms": time,
                                "fee_paid": fees if fill_fee is None else fill_fee})
    account = req["observation"]["own_account"]["Spot"]
    account.update(cash_balance=cash, available_cash=cash, position_qty=position,
                   available_position=position, fees_paid=fees)


class PineBotTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            import pine_compat
        except ImportError:
            if os.environ.get("MARKETFORGE_REQUIRE_PINE_TESTS") == "1":
                raise AssertionError("forced Pine tests require pine-compat-runtime==0.3.1")
            raise unittest.SkipTest("optional Pine runtime not installed")
        if pine_compat.__version__ != "0.3.1":
            raise AssertionError("install the qualified 0.3.1 runtime for Pine acceptance")

    def test_real_pine_all_three_strategies_enter_and_exit(self):
        cases = {"ma.pine": [100, 101, 102, 103, 104, 105, 106, 107],
                 "rsi.pine": [110, 109, 108, 107, 106, 105, 104, 103],
                 "breakout.pine": [100, 100, 100, 100, 100, 100, 110]}
        for script, prices in cases.items():
            with self.subTest(script=script):
                first = bot.decide(request(prices, script))
                self.assertEqual(first["actions"][0]["PlaceImmediateOrCancel"]["side"], "Buy")
                state, fills = first["state"], []
                # Real feedback is a one-unit partial fill of the two-unit IOC.
                cash, position, last_price = 10000 - prices[-1], 1, prices[-1]
                end_prices = [120] * 10 if script == "rsi.pine" else [80] * 10
                sold = False
                for price in end_prices:
                    prices = [*prices, price]
                    req = request(prices, script, json.loads(json.dumps(state)))
                    fill(req, qty=1, price=last_price, cash=cash, position=position)
                    result = bot.decide(req)
                    self.assertEqual(result["state"]["accounts"][-1]["position_size"], 1)
                    self.assertEqual(result["state"]["accounts"][-1]["position_avg_price"], last_price)
                    state = result["state"]
                    if result["actions"]:
                        action = result["actions"][0]["PlaceImmediateOrCancel"]
                        self.assertEqual((action["side"], action["qty"]), ("Sell", 1))
                        sold = True
                        break
                self.assertTrue(sold)

    def test_duplicate_bar_and_json_recovery_never_repeat_order(self):
        req = request(list(range(100, 108)))
        first = bot.decide(req)
        self.assertTrue(first["actions"])
        req["state"] = json.loads(json.dumps(first["state"]))
        self.assertEqual(bot.decide(req)["actions"], [])
        # Failure to fill does not manufacture position; next bar may retry.
        next_req = request(list(range(100, 109)), state=req["state"])
        next_result = bot.decide(next_req)
        self.assertTrue(next_result["actions"])
        self.assertEqual(next_result["state"]["observed_position"], 0)

    def test_actual_average_price_fees_realized_profit_and_fill_cursor(self):
        source = '//@version=6\nstrategy("Feedback")\nplot(strategy.position_avg_price)\nplot(strategy.netprofit)'
        with patch.object(bot, "load_source", return_value=source):
            first = bot.decide(request([100]))
            req = request([100, 110], state=first["state"])
            fill(req, qty=2, price=100, trade_id=1, cash=9697, position=3, fees=3, fill_fee=2)
            fill(req, qty=1, price=100, trade_id=2, cash=9697, position=3, fees=3, fill_fee=1, maker=True)
            bought = bot.decide(req)
            self.assertEqual(bought["state"]["cost"], "300")
            self.assertEqual(bought["state"]["accounts"][-1]["netprofit"], -3)
            next_req = request([100, 110, 120], state=bought["state"])
            next_req["observation"]["bot_market_data"]["own_fills"] = copy.deepcopy(req["observation"]["bot_market_data"]["own_fills"])
            next_req["observation"]["bot_market_data"]["fill_details"] = copy.deepcopy(req["observation"]["bot_market_data"]["fill_details"])
            fill(next_req, "Sell", qty=1, price=120, trade_id=3, cash=9816, position=2, fees=4, fill_fee=1, time=2000)
            sold = bot.decide(next_req)
            self.assertEqual(sold["state"]["cost"], "200")
            self.assertEqual(sold["state"]["accounts"][-1]["netprofit"], 16)
            self.assertEqual(sold["state"]["accounts"][-1]["openprofit"], 40)

    def test_missed_bars_use_timed_actual_fills_and_submit_only_latest_intent(self):
        source = ('//@version=6\nstrategy("Timed")\nif strategy.position_size == 0\n'
                  '    strategy.entry("L", strategy.long, qty=2)\nelse\n    strategy.close("L")')
        with patch.object(bot, "load_source", return_value=source):
            first = bot.decide(request([100]))
            req = request([100, 101, 102, 103], state=first["state"])
            fill(req, trade_id=0, time=2500, cash=9899, position=1, price=100, fees=1)
            result = bot.decide(req)
            self.assertEqual([frame["position_size"] for frame in result["state"]["accounts"]], [0, 0, 1, 1])
            self.assertEqual(result["state"]["accounts"][-1]["netprofit"], -1)
            self.assertEqual(result["state"]["last_fill_id"], 0)
            self.assertEqual(result["state"]["skipped_bar_decisions"], 2)
            self.assertEqual(len(result["actions"]), 1)
            self.assertEqual(result["actions"][0]["PlaceImmediateOrCancel"]["side"], "Sell")
            req["state"] = json.loads(json.dumps(result["state"]))
            self.assertEqual(bot.decide(req)["actions"], [])
            req["observation"]["bot_market_data"]["fill_details"][0]["fee_paid"] = 2
            with self.assertRaisesRegex(ValueError, "receipt changed"): bot.decide(req)

    def test_reject_changed_history_source_missing_bars_and_unsupported_orders(self):
        first = bot.decide(request([100]))
        for change, message in [("history", "changed"),
                                ("truncated", "truncated"), ("forming", "forming"), ("account", "disagree")]:
            req = request([100, 101], state=first["state"])
            if change == "history": req["observation"]["bot_market_data"]["candles"][0]["close_tick"] = 99
            if change == "truncated": req["observation"]["bot_market_data"]["truncated"] = True
            if change == "forming": req["observation"]["bot_market_data"]["candles"][-1]["is_final"] = False
            if change == "account": req["observation"]["own_account"]["Spot"]["cash_balance"] = 10001
            before = copy.deepcopy(req["state"])
            with self.assertRaisesRegex(ValueError, message): bot.decide(req)
            self.assertEqual(req["state"], before)
        for order in ['strategy.entry("L", strategy.short)', 'strategy.entry("L", strategy.long, limit=close)',
                      'strategy.exit("X", "L", stop=close)',
                      'strategy.entry("L", strategy.long, qty=1.5)']:
            with patch.object(bot, "load_source", return_value='//@version=6\nstrategy("Unsupported")\n' + order):
                with self.assertRaises(ValueError): bot.decide(request([100]))
        with patch.object(bot, "load_source", return_value='//@version=6\nstrategy("Changed")'):
            with self.assertRaisesRegex(ValueError, "changed"): bot.decide(request([100, 101], state=first["state"]))

    def test_no_forming_execution_history_horizon_and_input_overrides(self):
        req = request([])
        result = bot.decide(req)
        self.assertEqual(result["actions"], [])
        req = request([100, 101, 102])
        req["config"]["inputs"] = {"Fast": 1, "Slow": 2, "Quantity": 4}
        req["config"].update(max_qty=3, inventory_cap=2)
        self.assertEqual(bot.decide(req)["actions"][0]["PlaceImmediateOrCancel"]["qty"], 2)
        req["config"]["inputs"] = {"unknown": 1}
        with self.assertRaisesRegex(ValueError, "unknown Pine inputs"): bot.decide(req)
        req = request([100] * 17)
        req["config"]["history_limit"] = 16
        with self.assertRaisesRegex(ValueError, "history_limit"): bot.decide(req)
        with self.assertRaisesRegex(ValueError, "inside scripts"): bot.load_source("../bot.py")

    def test_untitled_defaults_and_changed_historical_intents(self):
        source = ('//@version=6\nstrategy("Defaults")\na = input.int(1)\nb = input.int(2)\n'
                  'if a < b\n    strategy.entry("L", strategy.long, qty=1)')
        with patch.object(bot, "load_source", return_value=source):
            self.assertTrue(bot.decide(request([100]))["actions"])
        last_source = ('//@version=6\nstrategy("Last")\nif barstate.islast\n'
                       '    strategy.entry("L", strategy.long, qty=1)')
        with patch.object(bot, "load_source", return_value=last_source):
            first = bot.decide(request([100]))
            with self.assertRaisesRegex(ValueError, "previous Pine intents changed"):
                bot.decide(request([100, 101], state=first["state"]))

    def test_daily_period_uses_daily_pine_semantics(self):
        req = request([100])
        req["config"]["bar_interval_ms"] = 86400000
        req["observation"]["bot_market_data"]["interval_ms"] = 86400000
        req["observation"]["bot_market_data"]["candles"][0]["close_time_ms"] = 86400000
        req["observation"]["market_time_ms"] = 86400000
        source = ('//@version=6\nstrategy("Daily")\nif timeframe.isdaily\n'
                  '    strategy.entry("L", strategy.long, qty=1)')
        with patch.object(bot, "load_source", return_value=source):
            self.assertTrue(bot.decide(req)["actions"])

    def test_26_participant_recipe_preserves_base_and_isolates_pine_accounts(self):
        original = recipe("one", 7, False)
        mixed = recipe("one", 7, False, pine=True)
        self.assertEqual(mixed["agents"][:20], original["agents"])
        self.assertEqual(len(mixed["agents"]), 26)
        accounts = [a["Plugin"]["participant"]["account_id"] for a in mixed["agents"]]
        self.assertEqual(len(accounts), len(set(accounts)))
        self.assertTrue(all(a["Spot"]["position_qty"] == 0 for a in mixed["scenario"]["accounts"][-6:]))


if __name__ == "__main__":
    unittest.main()
