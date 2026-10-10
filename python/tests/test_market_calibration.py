import copy
from decimal import Decimal
import hashlib
import io
import csv
from pathlib import Path
import sys
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
from calibrate_behavior_market import fitted_recipe
from marketforge.calibration import ReferenceTrade, Segment, day_start, parse_trades, reference_profile

DAY = "2025-01-02"
START = day_start(DAY)


def trade_rows(leg, future_scale=1):
    rows = []
    for second in range(120):
        price = Decimal("100") + Decimal(second % 7) / 100
        if second >= 60:
            price *= future_scale
        timestamp = (START + second) * (1_000_000 if leg == "spot" else 1000)
        row = [str(second), str(price + (Decimal("0.01") if leg == "perp" else 0)),
               "0.00123456", str(second * 2), str(second * 2 + 1), str(timestamp),
               "true" if second % 4 else "false"]
        if leg == "spot":
            row.append("true")
        rows.append(row)
    return rows


def source(directory, future_scale=1):
    receipts = []
    for leg in ("spot", "perp", "funding"):
        rows = trade_rows(leg, future_scale) if leg != "funding" else [
            ["calc_time", "funding_interval_hours", "last_funding_rate"],
            [str(START * 1000), "8", "0.00010000"],
            [str((START + 28800) * 1000), "8", str(Decimal("0.0001") * future_scale)]]
        text = io.StringIO()
        csv.writer(text).writerows(rows)
        path = Path(directory) / f"{leg}.zip"
        with zipfile.ZipFile(path, "w") as archive:
            archive.writestr("data.csv", text.getvalue())
        receipts.append({"leg": leg, "path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()})
    return receipts


class CalibrationTests(unittest.TestCase):
    def test_sparse_trades_do_not_invent_zero_volatility_or_future_bars(self):
        segment = Segment(0, 60)
        for second, price in [(0, "100"), (20, "150"), (40, "80")]:
            segment.add(ReferenceTrade(second * 1000000, Decimal(price), Decimal("1"), True, 1))
        summary = segment.summary()
        self.assertEqual(summary["active_seconds"], 3)
        self.assertEqual(summary["return_samples"], 0)
        self.assertIsNone(summary["return_std_ppm_1s"])
        self.assertIsNone(summary["absolute_return_ppm_1s"]["p95"])
        segment.add(ReferenceTrade(41000000, Decimal("90"), Decimal("1"), True, 1))
        summary = segment.summary()
        self.assertEqual(summary["return_samples"], 1)
        self.assertIsNone(summary["return_std_ppm_1s"])
        self.assertGreater(summary["absolute_return_ppm_1s"]["p95"], 0)

    def test_units_exact_quantity_and_aggressor_side(self):
        spot = list(parse_trades(trade_rows("spot"), "spot", DAY))
        perp = list(parse_trades(trade_rows("perp"), "perp", DAY))
        self.assertEqual(spot[0].time_us, perp[0].time_us)
        self.assertEqual(spot[0].qty, Decimal("0.00123456"))
        self.assertEqual(spot[0].raw_count, 2)
        self.assertTrue(spot[0].buy)
        self.assertFalse(spot[1].buy)

    def test_wrong_timestamp_units_duplicates_nonfinite_fail_closed(self):
        rows = trade_rows("spot")
        for mutate in (lambda r: r[0].__setitem__(5, str(START * 1000)),
                       lambda r: r[1].__setitem__(0, r[0][0]),
                       lambda r: r[0].__setitem__(1, "NaN"),
                       lambda r: r[0].__setitem__(2, "-1"),
                       lambda r: r[0].__setitem__(6, "maybe")):
            bad = copy.deepcopy(rows)
            mutate(bad)
            with self.assertRaises(ValueError):
                list(parse_trades(bad, "spot", DAY))

    def test_fit_does_not_use_holdout_or_later_funding(self):
        with tempfile.TemporaryDirectory() as a, tempfile.TemporaryDirectory() as b:
            original = reference_profile(source(a), DAY, 120)
            changed = reference_profile(source(b, 5), DAY, 120)
            self.assertEqual(original["fit"], changed["fit"])
            self.assertNotEqual(original["holdout"], changed["holdout"])
            self.assertEqual(fitted_recipe(original), fitted_recipe(changed))
            self.assertFalse(original["fit"]["pair"]["is_executable_basis"])
            self.assertEqual(original["fit"]["spot"]["total_base_quantity"], "0.07407360")
            self.assertEqual(original["fit"]["pair"]["paired_active_seconds"], 60)
            recipe, mapping = fitted_recipe(original)
            self.assertEqual(len(recipe["agents"]), 33)
            self.assertEqual(recipe["scenario"]["extra_markets"][0]["Perp"]["funding"]["interval_ms"], 28800000)
            self.assertFalse(mapping["used_holdout_to_fit"])
            noise = next(a["Plugin"]["config"] for a in recipe["agents"] if a["Plugin"]["plugin_id"] == "AdaptiveNoiseTrader")
            self.assertEqual(noise["fallback_price_tick"], 1000000)
            self.assertEqual(noise["side_persistence_ppm"], round(max(0, 2 * original["fit"]["spot"]["taker_side_persistence"] - 1) * 1000000))
            future_only = copy.deepcopy(original)
            future_only["funding_day"] = original["funding_day"][1:]
            with self.assertRaisesRegex(ValueError, "future funding"):
                fitted_recipe(future_only)

    def test_changed_source_or_missing_leg_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            receipts = source(directory)
            with self.assertRaises(ValueError):
                reference_profile(receipts[:-1], DAY, 120)
            with self.assertRaisesRegex(ValueError, "one source"):
                reference_profile(receipts + [receipts[0]], DAY, 120)
            Path(receipts[0]["path"]).write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "source changed"):
                reference_profile(receipts, DAY, 120)


if __name__ == "__main__":
    unittest.main()
