"""A long-run capacity report must not qualify a missing or stalled tail."""
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts"))
from benchmark_strategy_runtime import duration_summary


class DurationGatesTests(unittest.TestCase):
    def summarize(self, rows, seconds=120):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "samples.json"
            path.write_text(json.dumps({"clock":rows}), encoding="utf-8")
            return duration_summary(path, None, 0, seconds, 10)

    def test_early_fast_period_cannot_hide_stalled_tail(self):
        rows = [{"elapsed":i/4, "step":min(i*2.5, 600)} for i in range(480)]
        report = self.summarize(rows)
        self.assertTrue(report["full_requested_duration_observed"])
        self.assertFalse(report["sustained_target"])

    def test_missing_interior_samples_cannot_qualify_with_catchup(self):
        rows = [{"elapsed":i/4, "step":i*2.5} for i in range(480) if not 100 < i < 170]
        report = self.summarize(rows)
        self.assertFalse(report["full_requested_duration_observed"])
        self.assertFalse(report["sustained_target"])

    def test_small_duration_remainder_joins_previous_window(self):
        rows = [{"elapsed":i/4, "step":i*2.5} for i in range(245)]
        report = self.summarize(rows, 61.1)
        self.assertTrue(report["sustained_target"])
        self.assertEqual(len(report["windows"]), 1)


if __name__ == "__main__":
    unittest.main()
