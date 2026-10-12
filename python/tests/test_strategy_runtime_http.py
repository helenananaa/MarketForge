"""Fixed complete exchange receipts: replay workers versus incremental workers."""
import hashlib
import json
from pathlib import Path
import shutil
import sys
import unittest

import test_pine_http as http


class StrategyRuntimeHttpTests(unittest.TestCase):
    setUpClass = classmethod(http.PineHttpTests.setUpClass.__func__)
    setUp = http.PineHttpTests.setUp
    start = http.PineHttpTests.start
    stop = http.PineHttpTests.stop
    deterministic_room = http.PineHttpTests.deterministic_room
    observation = http.PineHttpTests.observation
    tick = http.PineHttpTests.tick

    def test_fixed_partial_fill_and_close_receipts_equal_replay(self):
        runtime = http.ROOT / "bot-plugins/pine-strategy/.runtime"
        self.assertTrue((runtime / "provenance.json").is_file(), "build the private incremental adapter first")
        self.assertEqual(json.loads((runtime / "provenance.json").read_text())["version"], "0.3.1+mf1")
        cases = {"ma.pine": [*range(100, 108), *([80] * 12)],
                 "rsi.pine": [*range(110, 102, -1), *([120] * 12)],
                 "breakout.pine": [*([100] * 6), 110, *([80] * 12)]}
        paths = {}
        for mode in ("replay", "incremental"):
            path = Path(self.directory.name) / mode
            shutil.copytree(http.ROOT / "bot-plugins", path,
                ignore=shutil.ignore_patterns("__pycache__", *([".runtime"] if mode == "replay" else [])))
            manifest_path = path / "pine-strategy/bot.json"
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            manifest["command"][0] = manifest["worker_pool"]["command"][0] = sys.executable
            # One worker guarantees actual session reuse between committed steps.
            manifest["worker_pool"]["workers"] = 1
            manifest["worker_pool"]["delta_requests"] = mode == "incremental"
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            paths[mode] = path
        actual = {}
        for mode in paths:
            self.env["MARKETFORGE_BOT_PLUGIN_DIR"] = str(paths[mode])
            self.start()
            receipt = []
            try:
                for script, prices in cases.items():
                    room = "fixed-" + script
                    self.deterministic_room(room, script)
                    maximum = 0
                    for index, price in enumerate(prices):
                        saved = self.tick(room, price, index)
                        view = self.observation(room, 300)
                        state = saved["agents"][0]["kind_state"]["Plugin"]["data"]
                        maximum = max(maximum, int(view["own_account"]["Spot"]["position_qty"]))
                        receipt.append({"script":script, "index":index, "state":state,
                            "account":view["own_account"], "orders":view["own_orders"], "trades":view["public_trades"],
                            "book":view["book"]})
                    self.assertEqual(maximum, 1, "real partial fill, not a complete two-unit fill")
                    self.assertEqual(int(view["own_account"]["Spot"]["position_qty"]), 0)
            finally:
                self.stop()
            actual[mode] = receipt
        self.assertEqual(actual["replay"], actual["incremental"])
        payload = json.dumps(actual["incremental"], sort_keys=True, separators=(",", ":"))
        (self.receipts / "fixed-incremental-exchange.json").write_text(json.dumps({
            "scope":"3 exact bundled sources; fixed manual real partial fills and closes; not async throughput",
            "steps":len(actual["incremental"]), "canonical_sha256":hashlib.sha256(payload.encode()).hexdigest(),
            "receipts":actual}, indent=2), encoding="utf-8")


if __name__ == "__main__": unittest.main()
