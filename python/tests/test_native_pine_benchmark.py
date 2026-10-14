"""Recipe equivalence and accounting checks; no live load is started."""
import copy
import json
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
import benchmark_strategy_runtime as benchmark


class NativePineBenchmarkTests(unittest.TestCase):
    def test_native_recipes_change_only_implementation_and_flat_config(self):
        for profile in ("short", "diverse-short", "mixed-horizon", "slow-horizon"):
            with self.subTest(profile=profile):
                pine = benchmark.pine_recipe(100, "comparison", 19, profile=profile)
                native = benchmark.native_pine_recipe(100, "comparison", 19, profile=profile)
                normalized_pine = benchmark.normalized_strategy_recipe(pine)
                normalized_native = benchmark.normalized_strategy_recipe(native)
                self.assertEqual(normalized_pine, normalized_native)
                # Actual receipt hashes are byte based, so dict equality alone
                # cannot catch a different key insertion order after lowering.
                self.assertEqual(json.dumps(normalized_pine, indent=2),
                                 json.dumps(normalized_native, indent=2))
                self.assertEqual(benchmark.workload_summary(pine, profile=profile),
                                 benchmark.workload_summary(native, profile=profile))
                self.assertEqual(pine["scenario"], native["scenario"])
                self.assertEqual(pine["agents"][:38], native["agents"][:38])
                self.assertEqual(len(native["agents"]), 138)
                for original, replacement in zip(pine["agents"][38:], native["agents"][38:]):
                    source = original["Plugin"]
                    target = replacement["Plugin"]
                    self.assertEqual(source["participant"], target["participant"])
                    self.assertEqual(source["seed"], target["seed"])
                    self.assertEqual(target["plugin_version"], "1.0.0")
                    self.assertEqual(target["plugin_id"], benchmark.NATIVE_PINE_PLUGINS[source["config"]["script"]])
                    self.assertNotIn("inputs", target["config"])
                    self.assertNotIn("script", target["config"])

    def test_defaults_and_overrides_match_bundled_sources(self):
        for index, script in enumerate(benchmark.NATIVE_PINE_PLUGINS):
            with self.subTest(script=script):
                config = benchmark.pine_config(index, 7, "short")
                native = benchmark.native_pine_config(config)
                self.assertEqual(native["qty"], 2)
                self.assertEqual(native["history_limit"], 512)
                self.assertEqual(native["bar_interval_ms"], config["bar_interval_ms"])
                for name, value in benchmark.PINE_INPUT_DEFAULTS[script].items():
                    self.assertEqual(native[benchmark.NATIVE_INPUT_FIELDS[name]], value)
                config["inputs"] = {name: value + 1 for name, value in benchmark.PINE_INPUT_DEFAULTS[script].items()}
                native = benchmark.native_pine_config(config)
                self.assertTrue(all(native[benchmark.NATIVE_INPUT_FIELDS[name]] == value
                                    for name, value in config["inputs"].items()))
        with self.assertRaises(ValueError):
            benchmark.native_pine_config({"script":"ma.pine", "inputs":{"Unmapped": 1}})

    def scheduler(self):
        spec = benchmark.native_pine_recipe(100, "comparison", 19)
        required = {f'V-BTC-SPOT:{config["bar_interval_ms"]}':
                    {"closed_bars": 12, "latest_open_ms": 90000, "market_time_ms": 100000}
                    for config in benchmark.strategy_configs(spec)}
        return {"benchmark_required_inputs": required, "agents": [
            {"template": copy.deepcopy(agent),
             "kind_state":{"Plugin":{"data": {"initialized": True, "evaluated_bars": 12, "_host_observation":[100, 100000]}
                         if benchmark.language_group(agent["Plugin"]) == "native-pine" else {"last_observation":[100, 100000]}}}}
            for agent in spec["agents"]]}

    def test_native_strategy_accounting_is_separate_from_liquidity_bots(self):
        scheduler = self.scheduler()
        groups = benchmark.summarize_language_groups(scheduler, 100000)
        self.assertEqual(set(groups), {"native", "native-pine"})
        self.assertEqual(groups["native"]["instances"], 38)
        self.assertEqual(groups["native-pine"]["instances"], 100)
        self.assertEqual(groups["native-pine"]["initialized"], 100)
        self.assertEqual(groups["native-pine"]["evaluated_bars"], 1200)
        self.assertEqual(groups["native-pine"]["closed_bar_backlog"]["count"], 100)
        self.assertEqual(groups["native-pine"]["instances_behind_closed_bars"], 0)
        qualified = benchmark.input_qualification(scheduler, groups, "native-pine")
        self.assertTrue(qualified["no_closed_bar_backlog"])
        self.assertTrue(qualified["all_native_pine_lookbacks_exercised"])
        self.assertIsNone(qualified["all_pine_lookbacks_exercised"])
        scheduler["agents"][38]["kind_state"]["Plugin"]["data"] = None
        groups = benchmark.summarize_language_groups(scheduler, 100000)
        self.assertEqual(groups["native-pine"]["initialized"], 99)
        self.assertEqual(groups["native-pine"]["evaluated_bars"], 1188)
        self.assertEqual(groups["native-pine"]["instances_behind_closed_bars"], 1)
        self.assertEqual(groups["native-pine"]["closed_bar_backlog"]["max"], 12)
        qualified = benchmark.input_qualification(scheduler, groups, "native-pine")
        self.assertFalse(qualified["no_closed_bar_backlog"])
        self.assertFalse(qualified["all_native_pine_lookbacks_exercised"])
        # A nonempty cold checkpoint must not count as initialization merely
        # because its schema and empty indicator state have been persisted.
        scheduler["agents"][38]["kind_state"]["Plugin"]["data"] = {
            "schema": 1, "initialized": False, "evaluated_bars": 0,
            "last_observation": None, "indicator_state": {"kind": "Ma"}}
        groups = benchmark.summarize_language_groups(scheduler, 100000)
        self.assertEqual(groups["native-pine"]["initialized"], 99)
        self.assertEqual(groups["native-pine"]["instances_behind_closed_bars"], 1)

    def test_empty_inputs_and_missing_native_strategies_do_not_qualify(self):
        scheduler = {"agents":[], "benchmark_required_inputs":{}}
        qualified = benchmark.input_qualification(scheduler, {}, "native-pine")
        for key in ("no_closed_bar_backlog", "all_native_pine_lookbacks_exercised",
                    "all_horizon_groups_exercised", "recent_nonempty_input"):
            self.assertFalse(qualified[key])

    def test_native_diagnostics_require_actual_measured_strategy_evaluations(self):
        diagnostics = {"stages":{"bot_evaluate":{"count":1000},
                                  "bot_native_pine_evaluate":{"count":10}},
                       "measurement_deltas":{"bot_native_pine_evaluate":{"count":5, "total_wall_ms":1}}}
        self.assertTrue(benchmark.required_diagnostics_present(diagnostics, "native-pine"))
        diagnostics["measurement_deltas"]["bot_native_pine_evaluate"]["count"] = 0
        self.assertFalse(benchmark.required_diagnostics_present(diagnostics, "native-pine"))
        del diagnostics["stages"]["bot_native_pine_evaluate"]
        self.assertFalse(benchmark.required_diagnostics_present(diagnostics, "native-pine"))

    def test_prewarming_uses_native_strategy_periods_and_real_gateway_fills(self):
        calls = []
        class Client:
            def _request(self, method, route, body, **kwargs):
                calls.append((body["action"]["PlaceLimit"]["side"], body["action"]["PlaceLimit"]["price_tick"]))
                return {"accepted": True, "events": []}
            def advance_clock(self, room, steps): calls.append(("advance", steps))
            def candles(self, room, instrument, **kwargs):
                return {"candles": [{"is_final": True} for _ in range(2)]}
        spec = benchmark.native_pine_recipe(100, "comparison", 19)
        result = benchmark.prewarm_market(Client(), "comparison", spec, 2)
        self.assertEqual(set(result["closed_bars_by_period"]), {"10000", "15000", "30000"})
        self.assertEqual(result["iterations"], 12)
        self.assertEqual(len([call for call in calls if call[0] == "advance"]), 12)
        self.assertEqual(len([call for call in calls if call[0] in ("Buy", "Sell")]), 24)

    def test_export_only_supports_native_prewarm_and_restores_environment(self):
        for original_env in ({}, {"MARKETFORGE_BOT_PLUGIN_DIR":"prior-path",
                                 "MARKETFORGE_EXPERIMENTAL_EVENT_PREFILTER":"prior-value"}):
            with self.subTest(environment=original_env), tempfile.TemporaryDirectory() as directory:
                args = ["benchmark_strategy_runtime.py", "--server", "unused.exe", "--output", directory,
                        "--counts", "100", "--population", "native-pine", "--export-only",
                        "--sustained-input", "--prewarm-bars", "10", "--event-prefilter"]
                with patch.dict(os.environ, original_env, clear=True), patch.object(sys, "argv", args):
                    before = dict(os.environ)
                    benchmark.main()
                    self.assertEqual(dict(os.environ), before)
                recipe = json.loads((Path(directory) / "bots-100/recipe.json").read_text(encoding="utf-8"))
                self.assertTrue({900001, 900002}.issubset({a["Spot"]["account_id"] for a in recipe["scenario"]["accounts"] if "Spot" in a}))
                self.assertEqual(json.loads((Path(directory) / "bots-100/normalized-recipe.json").read_text(encoding="utf-8")),
                                 benchmark.normalized_strategy_recipe(recipe))

    def test_failed_export_also_restores_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            args = ["benchmark_strategy_runtime.py", "--server", "unused.exe", "--output", directory,
                    "--counts", "99", "--population", "native-pine", "--export-only", "--event-prefilter"]
            with patch.object(sys, "argv", args):
                before = dict(os.environ)
                with self.assertRaises(ValueError): benchmark.main()
                self.assertEqual(dict(os.environ), before)

    def test_event_prefilter_is_explicitly_propagated_to_isolated_server_environment(self):
        for population in ("pine", "native-pine"):
            for enabled in (False, True):
                with self.subTest(population=population, enabled=enabled), tempfile.TemporaryDirectory() as directory:
                    args = ["benchmark_strategy_runtime.py", "--server", "unused.exe", "--output", directory,
                            "--counts", "100", "--population", population]
                    if enabled: args.append("--event-prefilter")
                    def run_case(run_args, count):
                        (run_args.output / f"bots-{count}").mkdir(parents=True)
                        benchmark.load.subprocess.Popen(["unused.exe"], env={"MARKETFORGE_BIND_ADDR":"127.0.0.1:1"})
                        return {"final": {}, "qualified": {}}
                    monitor = SimpleNamespace(finish=lambda: {"scope": "mock process; no server launched"})
                    with patch.object(sys, "argv", args), patch.object(benchmark.load, "run_case", run_case), \
                         patch.object(benchmark.load.subprocess, "Popen", return_value=SimpleNamespace(pid=1)) as process, \
                         patch.object(benchmark, "ProcessTreeMetrics", return_value=monitor), patch("builtins.print"):
                        before = dict(os.environ)
                        benchmark.main()
                        self.assertEqual(dict(os.environ), before)
                        child_env = process.call_args.kwargs["env"]
                        self.assertEqual(child_env["MARKETFORGE_EXPERIMENTAL_EVENT_PREFILTER"], "1" if enabled else "0")
                        self.assertEqual(child_env["MARKETFORGE_PINE_IN_PROCESS"], "0")


if __name__ == "__main__": unittest.main()
