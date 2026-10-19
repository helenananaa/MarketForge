"""Real Pine compilation and broker translation with authoritative fixture feedback."""
import copy
import json
import unittest
from unittest.mock import patch

from test_pine_bot import bot, request


class StrategyOrderGridTests(unittest.TestCase):
    def test_opt_in_rounding_preserves_discrete_limit_and_trigger_conditions(self):
        module=bot.broker_module();config={"quantize_strategy_orders":True}
        self.assertEqual(module.strategy_price(100.4,"limit",2,"Buy",False,config),100)
        self.assertEqual(module.strategy_price(100.4,"limit",2,"Sell",False,config),102)
        self.assertEqual(module.strategy_price(100.4,"stop",2,"Buy",True,config),102)
        self.assertEqual(module.strategy_price(100.4,"stop",2,"Sell",True,config),100)
        self.assertEqual(module.quantized(5.9,"quantity",2),4)
        self.assertEqual(module.quantized(.8,"quantity",1),0)
        self.assertEqual(module.qty_for_close({"qty":1.8},5,True),1)
        with self.assertRaises(ValueError):module.strategy_price(100.4,"limit",1,"Buy",False,{})
        for value in (True,float('nan'),float('inf'),-1):
            with self.assertRaises(ValueError):module.quantized(value,"quantity",1)


def broker_request(prices, state=None):
    req = request(prices, state=state)
    req["config"]["execution_mode"] = "broker"
    return req


def ack(req, response, order_id=50):
    req["state"] = json.loads(json.dumps(response["state"]))
    req["state"]["_host_receipts"] = [{"key": a["BotOrder"]["key"], "instrument_id": a["BotOrder"]["instrument_id"],
        "action": a["BotOrder"]["action"], "order_id": order_id + index, "command_seq": order_id + index,
        "market_time_ms": req["observation"]["market_time_ms"], "accepted": True, "error": None}
        for index, a in enumerate(response["actions"])]


def perp(req, position=0, avg=0, cash=10000, funding=0, realized=0):
    req["participant"]["instrument_id"] = "V-BTC-PERP"
    req["observation"]["instrument_id"] = "V-BTC-PERP"
    req["observation"]["own_account"] = {"Perp": {"account_id": 300, "cash_balance": cash,
        "available_cash": cash, "position_qty": position, "avg_entry_price_tick": avg,
        "equity": cash, "realized_pnl": realized, "unrealized_pnl": 0, "fees_paid": 0, "funding_pnl": funding,
        "margin_status": "Healthy"}}
    return req


class PineBrokerTests(unittest.TestCase):
    def run_source(self, source, req):
        with patch.object(bot, "load_source", return_value='//@version=6\nstrategy("Broker")\n' + source):
            frozen = copy.deepcopy(req)
            expected = bot.decide(req)
            encoded = []
            actual = bot.decide(req, borrow_history=True, encoded_response=encoded)
            self.assertEqual(actual, expected)
            self.assertEqual(json.loads(encoded[0]), expected)
            self.assertEqual(req, frozen)
            return expected

    def test_limit_stays_live_updates_remaining_quantity_and_cancels_by_id(self):
        source = 'if bar_index < 2\n    strategy.entry("L", strategy.long, qty=2, limit=110-close/5)\nelse\n    strategy.cancel("L")'
        req = broker_request([100])
        first = self.run_source(source, req)
        placed = first["actions"][0]["BotOrder"]["action"]["PlaceProtected"]
        self.assertEqual((placed["order_type"], placed["price_tick"], placed["qty"]), ("Limit", 90, 2))
        req = broker_request([100, 105]); ack(req, first)
        req["observation"]["own_orders"] = [{"order_id": 50, "side": "Buy", "price_tick": 90, "remaining_qty": 1}]
        req["observation"]["own_account"]["Spot"].update(position_qty=1, cash_balance=9910, available_position=1, available_cash=9820)
        req["observation"]["bot_market_data"]["own_fills"] = [{"trade_id": 0, "maker_account_id": 300, "taker_account_id": 10, "maker_order_id": 50, "taker_order_id": 51, "taker_side": "Sell", "qty": 1, "price_tick": 90}]
        req["observation"]["bot_market_data"]["fill_details"] = [{"trade_id": 0, "market_time_ms": 1000, "fee_paid": 0}]
        second = self.run_source(source, req)
        self.assertEqual(second["actions"][0]["BotOrder"]["action"], {"Amend": {"order_id": 50, "price_tick": 89, "qty": 1}})
        req["state"] = second["state"]; req["state"]["_host_receipts"] = []
        self.assertEqual(self.run_source(source, req)["actions"], [])
        third_req = broker_request([100, 105, 110]); ack(third_req, second)
        for key in ("own_orders", "own_account"): third_req["observation"][key] = copy.deepcopy(req["observation"][key])
        for key in ("own_fills", "fill_details"): third_req["observation"]["bot_market_data"][key] = copy.deepcopy(req["observation"]["bot_market_data"][key])
        third_req["observation"]["own_orders"][0]["price_tick"] = 89
        third = self.run_source(source, third_req)
        self.assertEqual(third["actions"][0]["BotOrder"]["action"], {"Cancel": {"order_id": 50}})

    def test_perp_short_actual_funding_and_reduce_only_close(self):
        source = 'if strategy.position_size == 0\n    strategy.entry("S", strategy.short, qty=2)\nelse\n    strategy.close("S")'
        first = self.run_source(source, perp(broker_request([100])))
        self.assertEqual(first["actions"][0]["BotOrder"]["action"]["PlaceProtected"]["side"], "Sell")
        req = perp(broker_request([100, 101]), position=-1, avg=100, cash=10002, funding=2); ack(req, first)
        req["observation"]["bot_market_data"]["own_fills"] = [{"trade_id": 0, "maker_account_id": 10, "taker_account_id": 300, "maker_order_id": 10, "taker_order_id": 50, "taker_side": "Sell", "qty": 1, "price_tick": 100}]
        req["observation"]["bot_market_data"]["fill_details"] = [{"trade_id": 0, "market_time_ms": 1000, "fee_paid": 0}]
        second = self.run_source(source, req)
        closing = second["actions"][0]["BotOrder"]["action"]["PlaceProtected"]
        self.assertEqual((closing["side"], closing["qty"], closing["reduce_only"]), ("Buy", 1, True))
        self.assertEqual(second["state"]["accounts"][-1]["netprofit"], 2)
        self.assertEqual(second["state"]["accounts"][-1]["position_avg_price"], 100)

    def test_repeated_close_in_one_bar_closes_once(self):
        source = 'if strategy.position_size == 0\n    strategy.entry("S", strategy.short, qty=2)\nelse\n    strategy.close("S")\n    strategy.close("S")'
        first = self.run_source(source, perp(broker_request([100])))
        req = perp(broker_request([100, 101]), position=-2, avg=100); ack(req, first)
        req["observation"]["bot_market_data"]["own_fills"] = [{"trade_id": 0, "maker_account_id": 10, "taker_account_id": 300, "maker_order_id": 10, "taker_order_id": 50, "taker_side": "Sell", "qty": 2, "price_tick": 100}]
        req["observation"]["bot_market_data"]["fill_details"] = [{"trade_id": 0, "market_time_ms": 1000, "fee_paid": 0}]
        second = self.run_source(source, req)
        self.assertEqual(len(second["actions"]), 1)
        closing = second["actions"][0]["BotOrder"]["action"]["PlaceProtected"]
        self.assertEqual((closing["side"], closing["qty"], closing["reduce_only"]), ("Buy", 2, True))

    def test_multimarket_security_routes_two_orders_and_freezes_providers(self):
        req = broker_request([100])
        peer = copy.deepcopy(perp(broker_request([110]))["observation"])
        req["observation"]["related_markets"] = [peer]
        req["config"]["related_instruments"] = ["V-BTC-PERP"]
        source = 'p = request.security("V-BTC-PERP", "1S", close)\nif p > close\n    strategy.entry("L", strategy.long, qty=1, limit=90)\n    strategy.entry("V-BTC-PERP::S", strategy.short, qty=1, limit=120)'
        result = self.run_source(source, req)
        self.assertEqual([a["BotOrder"]["instrument_id"] for a in result["actions"]], ["V-BTC-SPOT", "V-BTC-PERP"])
        self.assertEqual(result["state"]["evaluations"][0]["providers"]["V-BTC-PERP:1S"], 1)

    def test_strategy_order_preserves_strings_and_can_sell_owned_spot(self):
        req = broker_request([100])
        req["config"]["initial_avg_price_tick"] = 90
        req["observation"]["own_account"]["Spot"].update(position_qty=2, available_position=2)
        source = '// strategy.order("ignored",strategy.short)\ns = "strategy.order(ignored)"\nstrategy.order("sell", strategy.short, qty=1, limit=110)'
        result = self.run_source(source, req)
        self.assertEqual(result["actions"][0]["BotOrder"]["action"]["PlaceProtected"]["side"], "Sell")

    def test_declared_hedge_uses_actual_exposure_and_unwinds_on_timeout(self):
        req = broker_request([100])
        req["config"].update(initial_avg_price_tick=100, hedge_instrument_id="V-BTC-PERP", related_instruments=["V-BTC-PERP"])
        req["observation"]["own_account"]["Spot"].update(position_qty=1, available_position=1)
        req["observation"]["related_markets"] = [copy.deepcopy(perp(broker_request([100]))["observation"])]
        result = self.run_source('plot(close)', req)
        order = result["actions"][0]["BotOrder"]
        self.assertEqual((order["instrument_id"], order["action"]["PlaceProtected"]["side"], order["action"]["PlaceProtected"]["qty"]), ("V-BTC-PERP", "Sell", 1))
        ack(req, result)
        req["observation"]["market_time_ms"] += 6000
        req["observation"]["related_markets"][0]["market_time_ms"] += 6000
        result = self.run_source('plot(close)', req)
        self.assertTrue(result["state"]["hedge"]["unwinding"])
        self.assertEqual(result["actions"][0]["BotOrder"]["instrument_id"], "V-BTC-SPOT")

    def test_exchange_tick_grid_and_batch_cash_inventory_budget(self):
        req = broker_request([100])
        req["observation"]["instrument"] = {"tick_size": 5, "lot_size": 1}
        req["config"].update(inventory_cap=3)
        result = self.run_source('strategy.order("A",strategy.long,qty=2,limit=close-2*syminfo.mintick)\nstrategy.order("B",strategy.long,qty=2,limit=90)', req)
        orders = [a["BotOrder"]["action"]["PlaceProtected"] for a in result["actions"]]
        self.assertEqual([(o["price_tick"], o["qty"]) for o in orders], [(90, 2), (90, 1)])
        req = broker_request([100])
        req["observation"]["own_account"]["Spot"].update(cash_balance=200, available_cash=200)
        result = self.run_source('strategy.order("A",strategy.long,qty=2,limit=90)\nstrategy.order("B",strategy.long,qty=2,limit=90)', req)
        self.assertEqual([a["BotOrder"]["action"]["PlaceProtected"]["qty"] for a in result["actions"]], [2])

    def test_hedge_unwind_latches_when_balanced_and_margin_status_is_snake_case(self):
        req = broker_request([100])
        req["config"].update(initial_avg_price_tick=100, hedge_instrument_id="V-BTC-PERP", related_instruments=["V-BTC-PERP"])
        req["observation"]["own_account"]["Spot"].update(position_qty=1, available_position=1)
        peer = perp(broker_request([100]), position=-1, avg=100)["observation"]
        peer["own_account"]["Perp"]["margin_status"] = "margin_call"
        req["observation"]["related_markets"] = [peer]
        result = self.run_source('plot(close)', req)
        self.assertTrue(result["state"]["hedge"]["unwinding"])
        self.assertEqual(len(result["actions"]), 2)
        ack(req, result)
        peer["own_account"]["Perp"]["margin_status"] = "healthy"
        next_result = self.run_source('plot(close)', req)
        self.assertTrue(next_result["state"]["hedge"]["unwinding"])
        self.assertEqual(len(next_result["actions"]), 2)

    def test_perpetual_conditional_entry_and_actual_exit_protection(self):
        req = perp(broker_request([100]))
        result = self.run_source('strategy.entry("L",strategy.long,qty=1,stop=110,limit=111)', req)
        condition = result["actions"][0]["BotOrder"]["action"]["SetConditional"]["spec"]
        self.assertEqual((condition["trigger_price_tick"], condition["limit_price_tick"]), (110, 111))
        req = perp(broker_request([100]), position=2, avg=100)
        req["observation"]["public_trades"] = [{"price_tick": 100}]
        result = self.run_source('strategy.exit("X",profit=10,loss=5)', req)
        protection = result["actions"][0]["BotOrder"]["action"]["SetPositionProtection"]["protection"]
        self.assertEqual((protection["take_profit_tick"], protection["stop_loss_tick"], protection["exit_qty"]), (110, 95, 2))

    def test_fractional_original_strategy_exits_quantize_on_each_side(self):
        for position, expected in [(2,(111,94)),(-2,(89,106))]:
            req=perp(broker_request([100]),position=position,avg=100)
            req['config']['quantize_strategy_orders']=True
            req['observation']['public_trades']=[{'price_tick':100}]
            result=self.run_source('strategy.exit("X",profit=10.4,loss=5.3)',req)
            protection=result['actions'][0]['BotOrder']['action']['SetPositionProtection']['protection']
            self.assertEqual((protection['take_profit_tick'],protection['stop_loss_tick']),expected)

    def test_disabled_quantization_preserves_existing_identity(self):
        req=broker_request([100])
        first=self.run_source('plot(close)',req)
        req['config']['quantize_strategy_orders']=False
        second=self.run_source('plot(close)',req)
        self.assertEqual(first,second)

    def test_perpetual_hedge_account_uses_selected_leg(self):
        req = perp(broker_request([100]), position=0)
        req["config"]["position_side"] = "Short"
        req["observation"]["own_account"]["Perp"]["hedge_positions"] = {
            "long": {"qty": 2, "avg_entry_price_tick": 90}, "short": {"qty": 2, "avg_entry_price_tick": 110}}
        result = self.run_source('if strategy.position_size < 0\n    strategy.close_all()', req)
        order = result["actions"][0]["BotOrder"]["action"]["PlaceProtected"]
        self.assertEqual((order["side"], order["position_side"], order["qty"], order["reduce_only"]), ("Buy", "Short", 2, True))


    def test_same_id_side_change_waits_for_cancel_ack_before_replacement(self):
        source = 'if bar_index == 0\n    strategy.order("A",strategy.long,qty=2,limit=90)\nelse\n    strategy.order("A",strategy.short,qty=2,limit=110)'
        first = self.run_source(source, perp(broker_request([100])))
        req = perp(broker_request([100, 100])); ack(req, first)
        req["observation"]["own_orders"] = [{"order_id": 50, "side": "Buy", "price_tick": 90, "remaining_qty": 2}]
        second = self.run_source(source, req)
        self.assertEqual(second["actions"][0]["BotOrder"]["action"], {"Cancel": {"order_id": 50}})
        ack(req, second)
        req["observation"]["own_orders"] = []
        third = self.run_source(source, req)
        order = third["actions"][0]["BotOrder"]["action"]["PlaceProtected"]
        self.assertEqual((order["side"], order["qty"], order["price_tick"]), ("Sell", 2, 110))

    def test_new_bar_cancel_supersedes_queued_replacement(self):
        source = 'if bar_index < 2\n    strategy.entry("L",strategy.long,qty=2,limit=close-10)\nelse\n    strategy.cancel("L")'
        first = self.run_source(source, broker_request([100]))
        req = broker_request([100, 105]); ack(req, first)
        req["observation"]["own_orders"] = [{"order_id": 50, "side": "Buy", "price_tick": 90, "remaining_qty": 2}]
        second = self.run_source(source, req)
        self.assertIn("Cancel", second["actions"][0]["BotOrder"]["action"])
        req = broker_request([100, 105, 110]); ack(req, second)
        third = self.run_source(source, req)
        self.assertEqual(third["actions"], [])
        self.assertFalse(any(o.get("replacement") for o in third["state"]["orders"].values()))

    def test_exit_profit_loss_and_trailing_offset_use_exchange_ticks(self):
        req = perp(broker_request([100]), position=1, avg=100)
        req["observation"].update(instrument={"tick_size": 5, "lot_size": 1}, public_trades=[{"price_tick": 100}])
        result = self.run_source('strategy.exit("X",profit=2,loss=1)', req)
        protection = result["actions"][0]["BotOrder"]["action"]["SetPositionProtection"]["protection"]
        self.assertEqual((protection["take_profit_tick"], protection["stop_loss_tick"]), (110, 95))
        req["observation"]["public_trades"] = [{"price_tick": 110}]
        result = self.run_source('strategy.exit("T",trail_points=2,trail_offset=1)', req)
        self.assertEqual(result["actions"][0]["BotOrder"]["action"]["SetPositionProtection"]["protection"]["trailing_distance_tick"], 5)

    def test_exits_report_the_last_trade_levels_they_compare(self):
        req = perp(broker_request([100]), position=2, avg=100)
        req["observation"]["public_trades"] = [{"price_tick": 100}]
        result = self.run_source('strategy.exit("X",profit=10,loss=5)', req)
        self.assertEqual(result["state"]["_wake_prices"], [{"instrument_id": "V-BTC-PERP", "at_or_above": 110, "at_or_below": 95}])
        # A trailing exit waits for its activation price, then trails on the exchange.
        source = 'strategy.exit("T",trail_price=104,trail_offset=1)'
        result = self.run_source(source, req)
        self.assertEqual(result["state"]["_wake_prices"], [{"instrument_id": "V-BTC-PERP", "at_or_above": 104}])
        ack(req, result)
        req["observation"]["public_trades"] = [{"price_tick": 104}]
        result = self.run_source(source, req)
        self.assertIn("SetPositionProtection", result["actions"][0]["BotOrder"]["action"])
        self.assertNotIn("_wake_prices", result["state"])
        # A short position mirrors the levels; a flat one reports none.
        short = perp(broker_request([100]), position=-2, avg=100)
        short["observation"]["public_trades"] = [{"price_tick": 100}]
        result = self.run_source('strategy.exit("X",profit=10,loss=5)', short)
        self.assertEqual(result["state"]["_wake_prices"], [{"instrument_id": "V-BTC-PERP", "at_or_above": 105, "at_or_below": 90}])
        flat = perp(broker_request([100]))
        self.assertNotIn("_wake_prices", self.run_source('strategy.exit("X",profit=10,loss=5)', flat)["state"])

    def test_shared_account_instances_have_distinct_native_conditional_keys(self):
        req = perp(broker_request([100]))
        source = 'strategy.entry("L",strategy.long,qty=1,stop=110)'
        first = self.run_source(source, req)
        req["participant"]["participant_id"] = "another-pine"
        second = self.run_source(source, req)
        self.assertNotEqual(first["actions"][0]["BotOrder"]["action"]["SetConditional"]["key"], second["actions"][0]["BotOrder"]["action"]["SetConditional"]["key"])

    def test_cancel_does_not_remove_manually_replaced_position_protection(self):
        source = 'if bar_index == 0\n    strategy.exit("X",profit=10,loss=5)\nelse\n    strategy.cancel("X")'
        req = perp(broker_request([100]), position=1, avg=100)
        req["observation"]["public_trades"] = [{"price_tick": 100}]
        first = self.run_source(source, req)
        req = perp(broker_request([100, 101]), position=1, avg=100); ack(req, first)
        req["observation"]["public_trades"] = [{"price_tick": 101}]
        req["observation"]["position_protections"] = [{"position_side": "Both", "status": "armed", "spec": {"take_profit_tick": 120}}]
        self.assertEqual(self.run_source(source, req)["actions"], [])


if __name__ == "__main__": unittest.main()
