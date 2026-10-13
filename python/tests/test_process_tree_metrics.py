"""Pure checks for benchmark contamination detection and measurement metadata."""
import sys
import threading
import unittest
from pathlib import Path
from unittest.mock import Mock

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts"))
from process_tree_metrics import ProcessTreeMetrics, is_benchmark_server_name


class ProcessTreeMetricsTests(unittest.TestCase):
    def test_frozen_and_prefixed_server_names_are_detected(self):
        for name in ("exchange-server.exe", "baseline-server.exe", "candidate-server.exe",
                     "before-server.exe", "diagnostic-server.exe",
                     "native-pine-release-exchange-server.exe", "NATIVE-PINE-EXCHANGE-SERVER.EXE",
                     r"E:\receipts\baseline-server.exe"):
            with self.subTest(name=name):
                self.assertTrue(is_benchmark_server_name(name))
        for name in ("python.exe", "rustc.exe", "unrelated-server.exe", "exchange-server.exe.old"):
            with self.subTest(name=name):
                self.assertFalse(is_benchmark_server_name(name))

    def test_measurement_bounds_and_competing_process_identity_survive_finish(self):
        # No thread or processes are started by this metadata-only check.
        metrics = ProcessTreeMetrics.__new__(ProcessTreeMetrics)
        metrics.stop = threading.Event()
        metrics.thread = Mock()
        metrics.measurement_window = (5, 10)
        metrics.samples = [{"elapsed":elapsed, "cpu_seconds":elapsed, "rss_bytes":1024**2,
                            "processes":1, "per_process":{}} for elapsed in (1, 5, 8, 10, 12)]
        metrics.known = {1}
        metrics.compiler_activity = [{}, {}]
        metrics.other_servers = [{2:"baseline-server.exe"}, {}]
        metrics.errors = []
        result = metrics.finish()
        self.assertEqual(result["measurement"]["elapsed_bounds"], (5, 10))
        self.assertEqual(result["measurement"]["samples"], 3)
        self.assertEqual(result["measurement"]["average_logical_cores"], 1)
        self.assertEqual(result["other_server_nonempty_samples"], 1)
        self.assertEqual(result["other_server_processes"], {2:"baseline-server.exe"})


if __name__ == "__main__":
    unittest.main()
