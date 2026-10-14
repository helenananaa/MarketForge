"""Paired live HTTP coverage for the 13 frozen native Pine references.

The same bar sequence is sent to a Python Pine room and its Rust native port.
The required PostgreSQL mode checkpoints every pair, restarts the server, and
checks durable responses, account/book/trade parity, and duplicate-step
idempotency before the same sequences continue.
"""
from __future__ import annotations

import hashlib
import json
import os
import shutil
import time
import unittest
from pathlib import Path
from unittest.mock import patch

import test_native_pine_http as pine_http
import test_pine_http as http
import test_pine_broker_http as broker_http
from test_native_pine import RECEIPTS
from native_pine_references import reference_sources


ROOT = http.ROOT
PORTS = {
    "native_ema_cross.pine": "NativePineEmaCross",
    "native_macd.pine": "NativePineMacd",
    "native_bb_reversion.pine": "NativePineBollingerReversion",
    "native_roc.pine": "NativePineRoc",
    "native_stochastic.pine": "NativePineStochastic",
    "native_cci.pine": "NativePineCci",
    "native_williams.pine": "NativePineWilliams",
    "native_adx.pine": "NativePineAdx",
    "native_supertrend.pine": "NativePineSupertrend",
    "native_ut_atr.pine": "NativePineUtAtr",
    "native_ssl.pine": "NativePineSsl",
    "native_wavetrend.pine": "NativePineWaveTrend",
    "native_squeeze.pine": "NativePineSqueeze",
}

FROZEN_REFERENCES = {
    "native_ema_cross.pine": "ema_cross.pine",
    "native_macd.pine": "macd.pine",
    "native_bb_reversion.pine": "bb_reversion.pine",
    "native_roc.pine": "roc.pine",
    "native_stochastic.pine": "stochastic.pine",
    "native_cci.pine": "cci.pine",
    "native_williams.pine": "williams.pine",
    "native_adx.pine": "adx.pine",
    "native_supertrend.pine": "supertrend.pine",
    "native_ut_atr.pine": "ut_atr.pine",
    "native_ssl.pine": "ssl.pine",
    "native_wavetrend.pine": "wavetrend.pine",
    "native_squeeze.pine": "squeeze.pine",
}


class NativePinePopularHttpTests(unittest.TestCase):
    # Reuse only the process and broker primitives. This class intentionally
    # does not inherit the older three-script acceptance tests.
    setUpClass = classmethod(http.PineHttpTests.setUpClass.__func__)
    start = http.PineHttpTests.start
    stop = broker_http.PineBrokerHttpTests.stop
    observation = http.PineHttpTests.observation
    data = broker_http.PineBrokerHttpTests.data
    create_room = pine_http.NativePineHttpTests.create_room
    tick = pine_http.NativePineHttpTests.tick
    register_order = pine_http.NativePineHttpTests.register_order
    normalize_order_ids = pine_http.NativePineHttpTests.normalize_order_ids
    raw_semantic_observation = pine_http.NativePineHttpTests.raw_semantic_observation
    semantic_observation = pine_http.NativePineHttpTests.semantic_observation
    assert_pair = pine_http.NativePineHttpTests.assert_pair

    def setUp(self):
        broker_http.PineBrokerHttpTests.setUp(self)
        self.port_patch = patch.object(pine_http, "PORTS", PORTS)
        self.port_patch.start()
        self.addCleanup(self.port_patch.stop)
        # The Pine oracle is a disposable test plugin. Ignored samples are
        # neither installed into production sources nor required on checkout.
        plugins = Path(self.directory.name) / "reference-plugins"
        package = plugins / "pine-strategy"
        shutil.copytree(ROOT / "bot-plugins/pine-strategy", package,
                        ignore=shutil.ignore_patterns(".runtime", "__pycache__", "native_*.pine"))
        for name, source in reference_sources().items():
            (package / "scripts" / name).write_bytes(source)
        self.env["MARKETFORGE_BOT_PLUGIN_DIR"] = str(plugins)
        self.receipts = RECEIPTS / "http-popular"
        self.receipts.mkdir(parents=True, exist_ok=True)
        self.order_ids = {}

    @staticmethod
    def prices():
        """Compression/release, trend reversal and oversold/overbought crosses."""
        out = []
        for index in range(128):
            if index < 39:
                price = 100 + index % 2
            elif index < 56:
                price = 110 + index - 39
            elif index < 88:
                price = 126 - 2 * (index - 56) + index % 3
            elif index < 112:
                price = 64 + 3 * (index - 88) + index % 3
            else:
                price = 133 - 4 * (index - 112) + index % 3
            out.append(price)
        return out

    def record(self, cases):
        # Keep the base harness's historical source hashes and binary hash,
        # then add all 13 newly exercised sources and their frozen references.
        pine_http.NativePineHttpTests.record(self, cases)
        path = self.receipts / f"{self._testMethodName}.json"
        payload = json.loads(path.read_text(encoding="utf-8"))
        payload["native_port_source_sha256"] = {
            script: hashlib.sha256(source).hexdigest()
            for script, source in reference_sources().items()
        }
        catalog = json.loads((ROOT / "docs/native-pine-popular-catalog.json").read_text(encoding="utf-8"))
        payload["frozen_reference_source_sha256"] = {
            FROZEN_REFERENCES[Path(port["pine_source"]).name]: port["reference_sha256"]
            for port in catalog["ports"]
        }
        payload["http_test_sha256"] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
        for relative in ("python/tests/native_pine_references.py", "docs/native-pine-popular-catalog.json",
                         "scripts/research_strategy_compat.py"):
            payload["source_sha256"][relative] = hashlib.sha256((ROOT / relative).read_bytes()).hexdigest()
        for name in ("mod.rs", "common_a.rs", "common_b.rs", "popular.rs"):
            relative = f"exchange-core/src/native_pine_extended/{name}"
            payload["source_sha256"][relative] = hashlib.sha256((ROOT / relative).read_bytes()).hexdigest()
        path.write_text(json.dumps(payload, indent=2), encoding="utf-8")

    def compact_step(self, index, pair, pine_receipts_before, native_receipts_before, seen_trades):
        pine_data, native_data = pair["pine"], pair["native"]
        observation = pair["observation"]
        trades = observation["public_trades"]
        fresh_trades = []
        for trade in trades:
            key = trade.get("trade_id")
            if key is None:
                key = json.dumps(trade, sort_keys=True, separators=(",", ":"))
            if key not in seen_trades:
                seen_trades.add(key)
                fresh_trades.append(trade)
        account = observation["own_account"]["Spot"]
        pine_receipts = pine_data.get("_host_receipts", [])
        native_receipts = native_data.get("_host_receipts", [])
        return {
            "bar_index": index,
            "evaluated_bars": {"pine": pine_data.get("evaluated_bars"), "native": native_data.get("evaluated_bars")},
            "account": account,
            "own_orders": observation["own_orders"],
            "book": observation["book"],
            "new_public_trades": fresh_trades,
            "public_trade_count": len(trades),
            "new_host_receipts": {
                "pine": pine_receipts,
                "native": native_receipts,
            },
            "last_actions": {
                "pine": pine_data.get("last_actions", []),
                "native": native_data.get("last_actions", []),
            },
        }

    def test_all_13_ports_restart_at_bar_64_and_continue_paired_execution(self):
        if os.environ.get("MARKETFORGE_REQUIRE_POSTGRES_TESTS") != "1":
            self.skipTest("set MARKETFORGE_REQUIRE_POSTGRES_TESTS=1 to require the durable 13-port restart gate")
        dsn = os.environ.get("MARKETFORGE_TEST_DATABASE_URL")
        if not dsn:
            self.fail("required 13-port durable restart needs MARKETFORGE_TEST_DATABASE_URL")

        self.env["MARKETFORGE_DATABASE_URL"] = dsn
        self.start()
        catalog = {item["id"]: item for item in self.client.list_bots()}
        for port in PORTS.values():
            self.assertEqual(catalog[port]["runtime"], "builtin")

        prices = self.prices()
        prefix = f"native-popular-{time.time_ns()}"
        rooms = {}
        evidence = {script: {"rows": [], "bought": False, "sold": False, "trade_count": 0,
                            "accepted_receipt_count": 0, "max_position": 0,
                            "seen_trades": set(), "pine_receipts": 0, "native_receipts": 0}
                    for script in PORTS}
        for script in PORTS:
            pine_room = f"{prefix}-{Path(script).stem}-pine"
            native_room = f"{prefix}-{Path(script).stem}-native"
            self.create_room(pine_room, script)
            self.create_room(native_room, script, native=True)
            rooms[script] = {"pine": pine_room, "native": native_room}

        checkpoint = {}
        for index, price in enumerate(prices):
            for script, pair_rooms in rooms.items():
                pine_saved = self.tick(pair_rooms["pine"], price, index)
                native_saved = self.tick(pair_rooms["native"], price, index)
                pair = self.assert_pair(pair_rooms["pine"], pair_rooms["native"], pine_saved, native_saved)
                item = evidence[script]
                compact = self.compact_step(index, pair, item["pine_receipts"],
                                            item["native_receipts"], item["seen_trades"])
                item["rows"].append(compact)
                pine_receipts = pair["pine"].get("_host_receipts", [])
                native_receipts = pair["native"].get("_host_receipts", [])
                new_native_receipts = compact["new_host_receipts"]["native"]
                item["pine_receipts"] = len(pine_receipts)
                item["native_receipts"] = len(native_receipts)
                item["accepted_receipt_count"] += sum(
                    bool(receipt.get("accepted")) for receipt in new_native_receipts
                )
                for trade in compact["new_public_trades"]:
                    item["trade_count"] += 1
                    if trade.get("taker_account_id") == 300:
                        item["bought"] |= trade.get("taker_side") == "Buy"
                        item["sold"] |= trade.get("taker_side") == "Sell"
                item["max_position"] = max(item["max_position"], int(compact["account"]["position_qty"]))
                if index == 63:
                    checkpoint[script] = {
                        "pine_saved": pine_saved,
                        "native_saved": native_saved,
                        "pine_observation": self.semantic_observation(pair_rooms["pine"]),
                        "native_observation": self.semantic_observation(pair_rooms["native"]),
                    }

            if index == 63:
                # Stop one shared PostgreSQL-backed server after every pair has
                # reached the same closed-bar checkpoint.
                self.stop()
                self.start()
                for script, pair_rooms in rooms.items():
                    before = checkpoint[script]
                    for side in ("pine", "native"):
                        room = pair_rooms[side]
                        saved = before[f"{side}_saved"]
                        persisted = self.client.room_bots(room)
                        self.assertEqual(persisted, saved, f"durable {side} response for {script}")
                        repeated = self.client.step_bots(room, f"{room}-step-63")
                        self.assertEqual(repeated, saved, f"same-step idempotency for {side} {script}")
                        self.assertEqual(self.semantic_observation(room), before[f"{side}_observation"])
                    pair = self.assert_pair(pair_rooms["pine"], pair_rooms["native"],
                                           before["pine_saved"], before["native_saved"])
                    checkpoint[script]["post_restart_pair"] = {
                        "account": pair["observation"]["own_account"],
                        "book": pair["observation"]["book"],
                        "public_trades": pair["observation"]["public_trades"],
                        "receipts": {
                            "pine": pair["pine"].get("_host_receipts", []),
                            "native": pair["native"].get("_host_receipts", []),
                        },
                        "persisted_responses_equal": True,
                        "duplicate_step_equal": True,
                        "observations_equal": True,
                    }

        summaries = []
        for script, item in evidence.items():
            final = item["rows"][-1]
            summaries.append({
                "script": script,
                "native_id": PORTS[script],
                "bars": len(item["rows"]),
                "bought": item["bought"],
                "sold": item["sold"],
                "trade_count": item["trade_count"],
                "accepted_bot_receipts": item["accepted_receipt_count"],
                "max_position": item["max_position"],
                "final_account": final["account"],
                "final_book": final["book"],
                "rows": item["rows"],
                "restart_at_bar": 64,
                "restart_evidence": checkpoint[script]["post_restart_pair"],
                "execution_coverage": "traded" if item["bought"] or item["sold"] else "no_bot_trade_observed",
            })
        self.record({"price_sequence": prices, "bar_interval_ms": 1000,
                     "checkpoint_bar_index": 63, "all_13_restarted": True,
                     "cases": summaries})


if __name__ == "__main__":
    unittest.main()
