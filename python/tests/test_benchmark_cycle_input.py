"""Cycle input affects submitted limits; only actual receipts establish fills."""
import sys
from pathlib import Path
from types import SimpleNamespace
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts"))
from benchmark_strategy_runtime import (
    feed_execution_summary, input_price, pine_ledger_fill_summary, sustained_coverage_required,
)


class CycleInputTests(unittest.TestCase):
    def test_flat_remains_fixed_and_cycle_uses_market_time_not_poll_count(self):
        times = (0, 5_000, 200_000, 395_000, 400_000, 405_000)
        self.assertEqual([input_price("flat", time) for time in times], [100]*len(times))
        self.assertEqual([input_price("cycle", time) for time in times], [80, 81, 120, 81, 80, 81])
        self.assertEqual(input_price("cycle", 123_456), input_price("cycle", 523_456))

    def test_window_gate_applies_only_to_cycle_receipt_pilot_or_long_run(self):
        for profile, receipts, seconds, expected in (("flat", True, 120, False),
                ("cycle", False, 120, False), ("cycle", True, 10, False),
                ("cycle", True, 60, True), ("cycle", True, 120, True),
                ("cycle", True, 2100, True)):
            with self.subTest(profile=profile, receipts=receipts, seconds=seconds):
                self.assertEqual(sustained_coverage_required(SimpleNamespace(input_profile=profile,
                    require_bot_receipts=receipts, seconds=seconds)), expected)

    def test_feed_limit_and_admission_do_not_fabricate_execution_price_or_fill(self):
        responses = [{"response":{"action":{"PlaceLimit":{"price_tick":100}},
            "instrument_id":"spot", "market_time_ms":1000, "events":[
                {"type":"TradePrinted", "trade_id":1, "price_tick":106, "qty":1}]}},
            {"response":{"action":{"PlaceLimit":{"price_tick":100}},
                "market_time_ms":1000, "events":[{"type":"OrderRested"}]}}]
        report = feed_execution_summary([{"receipts":[{"orders":responses}]}])
        self.assertEqual(report["requested_limit_ticks"], {"min":100, "max":100})
        self.assertEqual(report["actual_trade_prices"]["min_tick"], 106)
        self.assertEqual(report["orders_with_fills"], 1)
        self.assertEqual(report["orders_without_fills"], 1)
        self.assertEqual(report["unique_trades"], 1)

    def agent(self, participant, source, fills):
        return {"template":{"Plugin":{"plugin_id":"pine.strategy",
            "participant":{"participant_id":participant}, "config":{"script":source}}},
            "kind_state":{"Plugin":{"data":{"ledgers":{"spot":{"fills":fills}}}}}}

    def test_pine_ledgers_count_actual_fills_and_deduplicate_pine_counterparties(self):
        buy = {"id":1, "time":1000, "price":105, "qty":2, "buy":True}
        report = pine_ledger_fill_summary({"agents":[self.agent("a", "ma.pine", [buy]),
            self.agent("b", "rsi.pine", [{**buy, "buy":False}]),
            self.agent("c", "breakout.pine", [])]})
        self.assertTrue(report["complete_final_ledgers"])
        self.assertEqual(report["unique_trades"], 1)
        self.assertEqual(report["matched_qty"], 2)
        self.assertEqual(report["by_source"]["ma.pine"]["participant_filled_qty"], 2)
        self.assertEqual(report["by_source"]["rsi.pine"]["sell_fill_receipts"], 1)
        self.assertEqual(report["by_source"]["breakout.pine"]["fill_receipts"], 0)
        self.assertIn("including warmup and drain", report["scope"])

    def test_malformed_or_duplicate_ledger_does_not_claim_complete_fill_evidence(self):
        fill = {"id":1, "time":1000, "price":105, "qty":2, "buy":True}
        for fills in ([fill, fill], [{**fill, "qty":0}]):
            with self.subTest(fills=fills):
                report = pine_ledger_fill_summary({"agents":[self.agent("a", "ma.pine", fills)]})
                self.assertFalse(report["complete_final_ledgers"])
                self.assertTrue(report["errors"])


if __name__ == "__main__":
    unittest.main()
