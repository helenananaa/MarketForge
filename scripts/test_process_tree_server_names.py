"""Frozen executable names must not evade concurrent-server detection."""
import unittest
from process_tree_metrics import is_benchmark_server_name


class ServerNames(unittest.TestCase):
    def test_baseline_selected_and_historical_candidates_are_detected(self):
        for name in ["exchange-server.exe", "baseline-server.exe", "candidate-v1-server.exe",
                     "candidate-v6-server.exe", "selected-v11-server.exe", "before-server.exe",
                     "after-exchange-server.exe", r"E:\folder\SELECTED-V11-SERVER.EXE"]:
            self.assertTrue(is_benchmark_server_name(name), name)
        for name in ["python.exe", "postgres.exe", "my-web-server.exe", "selected-report.json"]:
            self.assertFalse(is_benchmark_server_name(name), name)


if __name__ == "__main__":
    unittest.main()
