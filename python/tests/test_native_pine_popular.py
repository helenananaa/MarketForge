"""Differential qualification for the thirteen fixed native Pine formula ports.

Build exchange-server --example native_pine_probe first. This suite compares
the existing Pine ``bot.decide`` path with the native probe, including plots,
orders, execution feedback, compact history and JSON factory restoration.
"""
from __future__ import annotations

import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

from test_native_pine import native_request, semantic_actions
from test_pine_bot import ROOT, bot, fill
from test_pine_broker import ack, broker_request
from native_pine_references import reference_sources

sys.path.insert(0, str(ROOT / "scripts"))
from research_strategy_compat import bars_for


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

# Order: the first two plots expose Pine's actual entry/exit predicates. The
# remaining named plots are compared to indicators.extended.plots in Rust.
PLOT_KEYS = {
    "native_ema_cross.pine": ("fast", "slow"),
    "native_macd.pine": ("macd", "signal", "histogram"),
    "native_bb_reversion.pine": ("mid", "upper", "lower"),
    "native_roc.pine": ("roc",),
    "native_stochastic.pine": ("k", "d"),
    "native_cci.pine": ("cci",),
    "native_williams.pine": ("williams",),
    "native_adx.pine": ("plus", "minus", "adx"),
    "native_supertrend.pine": ("supertrend", "direction"),
    "native_ut_atr.pine": ("atr", "trail"),
    "native_ssl.pine": ("high_ma", "low_ma", "side"),
    "native_wavetrend.pine": ("wt", "signal"),
    "native_squeeze.pine": ("mid", "deviation", "k_range", "momentum", "squeezed"),
}

REGIMES = ("flat", "trend_reversal", "range", "squeeze_release", "jumps", "random_walk")
RECEIPTS = Path(os.environ.get(
    "MARKETFORGE_NATIVE_PINE_RECEIPTS", ROOT / ".local/native-pine-20261013"))
PROBE_DEFAULT = ROOT / "target/release/examples/native_pine_probe.exe"
PLOT_ABS_TOL = 1e-9
PLOT_REL_TOL = 1e-9


def source_hashes():
    names = [
        "exchange-core/src/native_pine.rs",
        "exchange-core/src/native_pine_extended/mod.rs",
        "exchange-core/src/native_pine_extended/common_a.rs",
        "exchange-core/src/native_pine_extended/common_b.rs",
        "exchange-core/src/native_pine_extended/popular.rs",
        "exchange-server/examples/native_pine_probe.rs",
        "bot-plugins/pine-strategy/bot.py",
        "bot-plugins/pine-strategy/broker.py",
        "bot-plugins/pine-strategy/incremental.py",
        "bot-plugins/pine-strategy/bot.json",
        "python/tests/test_native_pine_popular.py",
        "python/tests/native_pine_references.py",
        "docs/native-pine-popular-catalog.json",
        "scripts/research_strategy_compat.py",
    ]
    return {
        name: hashlib.sha256((ROOT / name).read_bytes()).hexdigest()
        for name in names if (ROOT / name).is_file()
    }


def popular_native_request(request, state):
    """Reuse the established wire lowering while selecting an extended port."""
    script_name = request["config"]["script"]
    compatible = copy.deepcopy(request)
    # native_request predates the extended port map; its generic configuration
    # lowering is still the shared fixture contract, while plugin identity is
    # supplied by this test's fixed extended-strategy map.
    compatible["config"]["script"] = "ma.pine"
    lowered = native_request(compatible, state)
    lowered["plugin_id"] = PORTS[script_name]
    return lowered


def candles_for(regime, n):
    """Use the shared six-regime recipes, but align every price to integer ticks."""
    bars = bars_for(regime, n=n)
    aligned = []
    previous = bars[0]["open"]
    for index, bar in enumerate(bars):
        close = int(bar["close"])
        open_tick = int(previous)
        aligned.append({
            "schema_version": 1,
            "open_time_ms": index * 1000,
            "close_time_ms": (index + 1) * 1000,
            "open_tick": open_tick,
            "high_tick": max(int(bar["high"]), open_tick, close),
            "low_tick": min(int(bar["low"]), open_tick, close),
            "close_tick": close,
            "volume": int(bar["volume"]),
            "quote_volume": int(bar["volume"]) * close,
            "trades": 1,
            "is_final": True,
        })
        previous = close
    return aligned


def _last_plot_value(plot):
    return plot["values"][-1] if plot.get("values") else None


def _signal(value):
    return value is not None and float(value) != 0.0


def _close(actual, expected):
    if actual is None or expected is None:
        return actual is None and expected is None
    a, e = float(actual), float(expected)
    return abs(a - e) <= PLOT_ABS_TOL + PLOT_REL_TOL * max(abs(a), abs(e))


def _receipt_for(response, req, *, accepted, error):
    receipts = []
    for action in response["actions"]:
        envelope = action["BotOrder"]
        receipts.append({
            "key": envelope["key"],
            "instrument_id": envelope["instrument_id"],
            "action": envelope["action"],
            "order_id": 700 if accepted else None,
            "command_seq": 700,
            "market_time_ms": req["observation"]["market_time_ms"],
            "accepted": accepted,
            "error": error,
        })
    return receipts


class NativePinePopularManifestTests(unittest.TestCase):
    def test_installed_references_match_frozen_catalog_hashes(self):
        catalog = json.loads((ROOT / "docs/native-pine-popular-catalog.json").read_text(encoding="utf-8"))
        self.assertEqual({Path(port["pine_source"]).name: port["bot_id"] for port in catalog["ports"]}, PORTS)
        sources = reference_sources()
        for port in catalog["ports"]:
            source = sources[Path(port["pine_source"]).name]
            self.assertEqual(hashlib.sha256(source).hexdigest(), port["installed_sha256"])
            reference = source.split(b"// Numeric diagnostics; trading rules above are frozen.\n")[0]
            self.assertEqual(hashlib.sha256(reference).hexdigest(), port["reference_sha256"])
            local = ROOT / port["pine_source"]
            if local.is_file():
                self.assertEqual(local.read_bytes(), source, "local sample differs from frozen formula")


class NativePinePopularDifferentialTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            import pine_compat
        except ImportError:
            if os.environ.get("MARKETFORGE_REQUIRE_NATIVE_PINE_TESTS") == "1":
                raise AssertionError("native popular Pine checks require pine-compat-runtime==0.3.1")
            raise unittest.SkipTest("optional Pine runtime not installed")
        if pine_compat.__version__ != "0.3.1":
            raise AssertionError("native popular Pine checks require qualified Pine 0.3.1")
        executable = Path(os.environ.get("MARKETFORGE_NATIVE_PINE_PROBE", PROBE_DEFAULT))
        if not executable.is_file():
            if os.environ.get("MARKETFORGE_REQUIRE_NATIVE_PINE_TESTS") == "1":
                raise AssertionError("build native_pine_probe before required differential checks")
            raise unittest.SkipTest("native_pine_probe not built")
        sources = reference_sources()
        load_source = bot.load_source
        cls.source_patch = patch.object(bot, "load_source", side_effect=lambda name:
            sources[name].decode("utf-8") if name in sources else load_source(name))
        cls.source_patch.start()
        cls.addClassCleanup(cls.source_patch.stop)
        cls.probe = subprocess.Popen(
            [str(executable)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, text=True, encoding="utf-8")
        cls.addClassCleanup(cls.close)
        RECEIPTS.mkdir(parents=True, exist_ok=True)
        cls.probe_sha256 = hashlib.sha256(executable.read_bytes()).hexdigest()

    @classmethod
    def close(cls):
        cls.probe.stdin.close()
        cls.probe.wait(timeout=10)
        cls.probe.stdout.close()
        cls.probe.stderr.close()

    def rust_result(self, req, **options):
        self.probe.stdin.write(json.dumps({"request": req, **options}) + "\n")
        self.probe.stdin.flush()
        line = self.probe.stdout.readline()
        if not line:
            self.fail(self.probe.stderr.read())
        return json.loads(line)

    def rust(self, req, **options):
        result = self.rust_result(req, **options)
        self.assertNotIn("error", result, result)
        return result["response"]

    def _base_request(self, script, candles, end, participant_id, state):
        req = broker_request([c["close_tick"] for c in candles[:end]], state)
        req["config"].update(
            script=script, inputs={}, bar_interval_ms=1000, history_limit=512,
            max_qty=1, inventory_cap=1, slippage_ticks=0, fee_buffer_ppm=0)
        req["participant"]["participant_id"] = participant_id
        req["observation"]["step"] = end
        req["observation"]["market_time_ms"] = end * 1000
        req["observation"]["bot_market_data"]["interval_ms"] = 1000
        req["observation"]["bot_market_data"]["candles"] = copy.deepcopy(candles[:end])
        last = candles[end - 1]["close_tick"]
        req["observation"]["book"] = {
            "bids": [{"price_tick": last - 1, "qty": 100}],
            "asks": [{"price_tick": last + 1, "qty": 100}],
        }
        return req

    @staticmethod
    def _world(req, *, cash, position, fees, fills, details):
        account = req["observation"]["own_account"]["Spot"]
        account.update(cash_balance=cash, available_cash=cash,
                       position_qty=position, available_position=position, fees_paid=fees)
        req["observation"]["bot_market_data"]["own_fills"] = copy.deepcopy(fills)
        req["observation"]["bot_market_data"]["fill_details"] = copy.deepcopy(details)

    def _accept_and_fill(self, req, pending, *, cash, position, fees, fills, details,
                         fill_price, fill_time, trade_id):
        if pending is None:
            self._world(req, cash=cash, position=position, fees=fees, fills=fills, details=details)
            return cash, position, fees, fills, details
        ack(req, pending, order_id=500 + trade_id)
        self._world(req, cash=cash, position=position, fees=fees, fills=fills, details=details)
        self.assertEqual(len(pending["actions"]), 1, "native Pine host emits at most one action per bar")
        order = pending["actions"][0]["BotOrder"]["action"]["PlaceProtected"]
        self.assertEqual(order["qty"], 1, "fixtures must exercise complete quantity-one fills")
        side = order["side"]
        fill_price += 1 if side == "Buy" else -1
        next_cash = cash + (fill_price if side == "Sell" else -fill_price) - 1
        next_position = position + (1 if side == "Buy" else -1)
        next_fees = fees + 1
        fill(req, side=side, qty=1, price=fill_price, trade_id=trade_id,
             cash=next_cash, position=next_position, fees=next_fees,
             time=fill_time, fill_fee=1)
        req["observation"]["bot_market_data"]["own_fills"][-1].update(
            maker_order_id=1000 + trade_id, taker_order_id=500 + trade_id)
        next_fills = copy.deepcopy(req["observation"]["bot_market_data"]["own_fills"])
        next_details = copy.deepcopy(req["observation"]["bot_market_data"]["fill_details"])
        return next_cash, next_position, next_fees, next_fills, next_details

    def _capture_pine(self, req):
        captured = []
        module = bot.incremental_module()
        evaluate = module.evaluate

        def evaluate_and_capture(*args, **kwargs):
            result = evaluate(*args, **kwargs)
            captured.append(result)
            return result

        with patch.object(module, "evaluate", side_effect=evaluate_and_capture):
            response = bot.decide(req)
        self.assertTrue(captured, "bot.decide must evaluate the Pine reference")
        return response, captured[-1]

    def _compare_step(self, script, pine_req, native_req, pine_response, native_response,
                      pine_output, *, script_name, regime, end):
        plots = pine_output["output"]["plots"]
        self.assertEqual(len(plots), len(PLOT_KEYS[script_name]) + 2,
                         f"unexpected reference plot count for {script_name}")
        extended = native_response["state"]["indicators"]["extended"]
        expected_names = set(PLOT_KEYS[script_name])
        self.assertEqual(set(extended["plots"]), expected_names)
        entry_value, exit_value = _last_plot_value(plots[0]), _last_plot_value(plots[1])
        expected_buy, expected_sell = _signal(entry_value), _signal(exit_value)
        self.assertEqual(extended["buy"], expected_buy, "entry predicate differs")
        self.assertEqual(extended["sell"], expected_sell, "exit predicate differs")
        numeric = {}
        for name, plot in zip(PLOT_KEYS[script_name], plots[2:]):
            expected = _last_plot_value(plot)
            actual = extended["plots"][name]
            self.assertTrue(_close(actual, expected),
                            f"{name} differs: native={actual!r} Pine={expected!r}")
            numeric[name] = {"pine": expected, "native": actual}
        self.assertEqual(semantic_actions(native_response["actions"]),
                         semantic_actions(pine_response["actions"]), "bot.decide orders differ")
        return {
            "bar": end, "pine_buy": expected_buy, "pine_sell": expected_sell,
            "native_buy": extended["buy"], "native_sell": extended["sell"],
            "plots": numeric, "pine_actions": pine_response["actions"],
            "native_actions": native_response["actions"],
            "native_state_bytes": len(json.dumps(native_response["state"], separators=(",", ":")).encode()),
            "pine_state_bytes": len(json.dumps(pine_response["state"], separators=(",", ":")).encode()),
        }

    def _reject_without_phantom_position(self, script_name, pine_req, native_req,
                                         pine_response, native_response):
        if not pine_response["actions"]:
            return None
        pine_retry = copy.deepcopy(pine_req)
        pine_retry["state"] = copy.deepcopy(pine_response["state"])
        pine_retry["state"]["_host_receipts"] = _receipt_for(
            pine_response, pine_req, accepted=False, error="fixture_rejected")
        native_retry = copy.deepcopy(native_req)
        native_retry["state"] = copy.deepcopy(native_response["state"])
        native_retry["state"]["_host_receipts"] = _receipt_for(
            native_response, native_req, accepted=False, error="fixture_rejected")
        pine_after = bot.decide(pine_retry)
        native_after = self.rust(native_retry, restore=True)
        self.assertEqual(pine_after["actions"], [])
        self.assertEqual(native_after["actions"], [])
        self.assertEqual(native_after["state"]["observed_position"], 0)
        return {"pine": pine_after, "native": native_after}

    def test_long_volatile_to_flat_large_tick_snapshots_match_pine(self):
        evidence = {}
        for script_name in PORTS:
            if os.environ.get("MARKETFORGE_NATIVE_PINE_CASE") not in (None, script_name):
                continue
            candles = candles_for("random_walk", 576)
            for index, candle in enumerate(candles):
                close = 1_000_000_000_000 + (candle["close_tick"] - 10000 if index < 128 else 0)
                candle.update(open_tick=close, close_tick=close, high_tick=close + 90,
                              low_tick=close - 10, quote_volume=close * candle["volume"])
            pine_state = native_state = None
            rows = []
            # Cold full-history warmup, followed by resumed 64-bar snapshots.
            for end in range(512, 577):
                pine_req = self._base_request(script_name, candles, end, f"large-{script_name}", pine_state)
                pine_req["config"]["history_limit"] = 1024
                self._world(pine_req, cash=0, position=0, fees=0, fills=[], details=[])
                native_req = popular_native_request(pine_req, native_state)
                native_req["config"].update(qty=1, history_limit=1024)
                if native_state is not None:
                    native_req["observation"]["bot_market_data"].update(
                        candles=copy.deepcopy(candles[end - 64:end]), truncated=True)
                pine_response = pine_output = native_response = None
                try:
                    pine_response, pine_output = self._capture_pine(pine_req)
                    native_response = self.rust(native_req, reset=end == 512, restore=True)
                    rows.append(self._compare_step(script_name, pine_req, native_req, pine_response,
                        native_response, pine_output, script_name=script_name, regime="large_flat", end=end))
                    self.assertEqual(self.rust(copy.deepcopy(native_req), restore=True), native_response)
                except Exception as exc:
                    (RECEIPTS / f"popular-large-failure-{script_name}-{end}.json").write_text(
                        json.dumps({"error": repr(exc), "pine_request": pine_req, "native_request": native_req,
                            "pine_response": pine_response, "native_response": native_response,
                            "pine_output": pine_output}, indent=2), encoding="utf-8")
                    raise
                pine_state, native_state = pine_response["state"], native_response["state"]
            evidence[script_name] = rows
        (RECEIPTS / "native-pine-popular-large-snapshots.json").write_text(json.dumps({
            "source_sha256": source_hashes(), "probe_sha256": self.probe_sha256,
            "reference_sha256": {name: hashlib.sha256(data).hexdigest()
                                 for name, data in reference_sources().items()},
            "cash": 0, "trade_coverage": False, "cases": evidence}, indent=2), encoding="utf-8")

    def test_all_formula_ports_match_pine_plots_orders_feedback_and_recovery(self):
        receipt = {
            "test": self._testMethodName,
            "source_sha256": source_hashes(),
            "reference_sha256": {name: hashlib.sha256(data).hexdigest()
                                 for name, data in reference_sources().items()},
            "probe_sha256": self.probe_sha256,
            "pine_runtime": "0.3.1",
            "tolerances": {"signal": "exact", "numeric_abs": PLOT_ABS_TOL,
                           "numeric_rel": PLOT_REL_TOL},
            "bar_contract": {"interval_ms": 1000, "first_close_ms": 1000,
                             "ohlc_aligned_integer_ticks": True,
                             "native_rolling_bars": 64, "quantity": 1, "fill_fee": 1},
            "strategies": {},
        }
        for script_name, plugin_id in PORTS.items():
            if os.environ.get("MARKETFORGE_NATIVE_PINE_CASE") not in (None, script_name):
                continue
            strategy_receipt = {"plugin_id": plugin_id, "regimes": {}, "rejected_receipt": None}
            for regime in REGIMES:
                candles = candles_for(regime, 128)
                participant = f"popular-{script_name}-{regime}"
                pine_state = native_state = None
                pending_pine = pending_native = None
                cash, position, fees = 1_000_000, 0, 0
                fills, details = [], []
                trade_id = 0
                buys = sells = buy_orders = sell_orders = numeric_checks = 0
                rows = []
                rejected_receipt = None
                for end in range(1, len(candles) + 1):
                    base = self._base_request(script_name, candles, end, participant, pine_state)
                    pine_req = copy.deepcopy(base)
                    native_base = copy.deepcopy(base)
                    before_world = (cash, position, fees, fills, details)
                    next_world = self._accept_and_fill(
                        pine_req, pending_pine, cash=cash, position=position, fees=fees,
                        fills=fills, details=details,
                        fill_price=candles[end - 2]["close_tick"] if end > 1 else candles[0]["close_tick"],
                        fill_time=(end - 1) * 1000, trade_id=trade_id + 1)
                    if pending_pine is not None:
                        trade_id += 1
                    native_cash, native_position, native_fees, native_fills, native_details = self._accept_and_fill(
                        native_base, pending_native, cash=before_world[0], position=before_world[1],
                        fees=before_world[2], fills=before_world[3], details=before_world[4],
                        fill_price=candles[end - 2]["close_tick"] if end > 1 else candles[0]["close_tick"],
                        fill_time=(end - 1) * 1000, trade_id=trade_id)
                    self.assertEqual(next_world, (native_cash, native_position, native_fees,
                                                  native_fills, native_details),
                                     "accepted Pine/native order feedback diverged")
                    cash, position, fees, fills, details = next_world

                    pine_response = pine_output = native_response = native_result = None
                    native_req = None
                    try:
                        pine_response, pine_output = self._capture_pine(pine_req)
                        native_req = popular_native_request(
                            native_base, native_base["state"] if pending_native else native_state)
                        native_req["config"].update(qty=1, max_qty=1, inventory_cap=1,
                                                     slippage_ticks=0, fee_buffer_ppm=0,
                                                     history_limit=64)
                        native_data = native_req["observation"]["bot_market_data"]
                        native_data["candles"] = copy.deepcopy(candles[max(0, end - 64):end])
                        native_data["truncated"] = end > 64
                        self._world(native_req, cash=cash, position=position, fees=fees,
                                    fills=fills, details=details)
                        native_result = self.rust_result(native_req, reset=end == 1)
                        self.assertNotIn("error", native_result, native_result)
                        native_response = native_result["response"]
                        cold_result = self.rust_result(copy.deepcopy(native_req), restore=True)
                        self.assertNotIn("error", cold_result, cold_result)
                        cold_response = cold_result["response"]
                        self.assertEqual(cold_response, native_response,
                                         "hot instance and JSON factory restore differ")
                        row = self._compare_step(
                            script_name, pine_req, native_req, pine_response, native_response,
                            pine_output, script_name=script_name, regime=regime, end=end)
                        pine_duplicate = copy.deepcopy(pine_req)
                        pine_duplicate["state"] = copy.deepcopy(pine_response["state"])
                        native_duplicate = copy.deepcopy(native_req)
                        native_duplicate["state"] = copy.deepcopy(native_response["state"])
                        self.assertEqual(bot.decide(pine_duplicate)["actions"], [])
                        duplicate_native = self.rust(native_duplicate, restore=True)
                        self.assertEqual(duplicate_native["actions"], [])
                        extended = native_response["state"]["indicators"]["extended"]
                        buys += int(extended["buy"])
                        sells += int(extended["sell"])
                        numeric_checks += len(PLOT_KEYS[script_name])
                        for action in pine_response["actions"]:
                            side = action["BotOrder"]["action"]["PlaceProtected"]["side"]
                            buy_orders += int(side == "Buy")
                            sell_orders += int(side == "Sell")
                        if rejected_receipt is None and pine_response["actions"]:
                            rejected_receipt = self._reject_without_phantom_position(
                                script_name, pine_req, native_req, pine_response, native_response)
                        pine_state = pine_duplicate["state"]
                        native_state = duplicate_native["state"]
                        pending_pine = pine_response if pine_response["actions"] else None
                        pending_native = native_response if native_response["actions"] else None
                        rows.append(row)
                    except Exception as exc:
                        failure = {
                            "test": self._testMethodName, "script": script_name,
                            "regime": regime, "bar": end, "error": repr(exc),
                            "pine_request": pine_req, "native_request": native_req,
                            "pine_response": pine_response, "native_response": native_response,
                            "pine_output": pine_output, "native_result": native_result,
                        }
                        (RECEIPTS / f"popular-failure-{script_name}-{regime}-{end}.json").write_text(
                            json.dumps(failure, indent=2), encoding="utf-8")
                        raise

                buy_sell_qualified = buys > 0 and sells > 0
                strategy_receipt["regimes"][regime] = {
                    "bars": len(candles), "buy_predicate_bars": buys,
                    "sell_predicate_bars": sells, "buy_orders": buy_orders,
                    "sell_orders": sell_orders, "numeric_plot_checks": numeric_checks,
                    "trade_alignment_qualified": buy_sell_qualified and buy_orders > 0 and sell_orders > 0,
                    "alignment_limit": None if (buys and sells) else
                        "sample did not trigger both buy and sell predicates; no trade-alignment claim",
                    "rows": rows,
                }
                if rejected_receipt is not None:
                    strategy_receipt["rejected_receipt"] = rejected_receipt
            regime_rows = list(strategy_receipt["regimes"].values())
            total_buys = sum(row["buy_predicate_bars"] for row in regime_rows)
            total_sells = sum(row["sell_predicate_bars"] for row in regime_rows)
            total_buy_orders = sum(row["buy_orders"] for row in regime_rows)
            total_sell_orders = sum(row["sell_orders"] for row in regime_rows)
            strategy_receipt["aggregate"] = {
                "buy_predicate_bars": total_buys,
                "sell_predicate_bars": total_sells,
                "buy_orders": total_buy_orders,
                "sell_orders": total_sell_orders,
                "trade_alignment_qualified": bool(
                    total_buys and total_sells and total_buy_orders and total_sell_orders),
                "alignment_limit": None if (total_buys and total_sells) else
                    "six-regime sample did not trigger both predicates; no trade-alignment claim",
            }
            receipt["strategies"][script_name] = strategy_receipt
            print(f"qualified {plugin_id}: {total_buy_orders} buys, {total_sell_orders} sells", flush=True)
            (RECEIPTS / "native-pine-popular-formula-progress.json").write_text(
                json.dumps(receipt, indent=2), encoding="utf-8")
        output = RECEIPTS / "native-pine-popular-formula-matrix.json"
        output.write_text(json.dumps(receipt, indent=2), encoding="utf-8")


if __name__ == "__main__":
    unittest.main()
