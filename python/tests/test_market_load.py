import copy
from pathlib import Path
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
from benchmark_market_load import load_recipe, observation_freshness, percentiles
from market_arrival_feedback import apply_feedback
from microstructure_market import microstructure_recipe


class MarketLoadTests(unittest.TestCase):
    def test_clock_progress_does_not_hide_stale_or_missing_bot_observations(self):
        scheduler={"agents":[{"kind_state":{"Plugin":{"data":data}}} for data in
                             [{"last_observation":[8,8000]}, {"last_observation":[10,10000]}, None, {}]]}
        result=observation_freshness(scheduler,10000)
        self.assertEqual(result["bots_with_observation"],2)
        self.assertEqual(result["bots_without_observation"],2)
        self.assertEqual(result["lag_sim_ms"]["max_ms"],2000)
        self.assertEqual(result["future_observations"],0)
        self.assertEqual(observation_freshness(scheduler,9000)["future_observations"],1)

    def test_population_tiers_have_separate_finite_accounts_and_two_legs(self):
        for count in (38, 100, 200, 500):
            spec = load_recipe(count)
            accounts = {next(iter(a.values()))["account_id"]: next(iter(a.values()))
                        for a in spec["scenario"]["accounts"]}
            identities = [a["Plugin"]["participant"]["account_id"] for a in spec["agents"]]
            self.assertEqual(len(identities), count)
            legs = [(a["Plugin"]["participant"]["account_id"], a["Plugin"]["participant"]["instrument_id"])
                    for a in spec["agents"]]
            self.assertEqual(len(set(legs)), count)
            self.assertTrue(all(accounts[i]["cash_balance"] > 0 for i in identities))
            self.assertTrue(all(a.get("position_qty", 0) >= 0 for a in accounts.values()))
            self.assertEqual(len(accounts), len(spec["scenario"]["accounts"]))
            self.assertNotIn(900000, identities)
            self.assertTrue(all(a["Plugin"]["participant"]["room_id"] == "load-market" for a in spec["agents"]))
            self.assertEqual(len({a["Plugin"]["participant"]["instrument_id"] for a in spec["agents"]}), 2)

    def test_frozen_recipe_is_not_mutated_or_silently_enabled(self):
        template = microstructure_recipe()
        for a in template["agents"]:
            if a["Plugin"]["plugin_id"] == "AdaptiveNoiseTrader":
                a["Plugin"]["config"]["arrival_mode"] = "Periodic"
        original = copy.deepcopy(template)
        result = load_recipe(100, "comparison", 19, template=template)
        self.assertEqual(template, original)
        self.assertTrue(all(a["Plugin"]["config"].get("arrival_mode", "Periodic") == "Periodic"
                            for a in result["agents"]))
        self.assertEqual(result, load_recipe(100, "comparison", 19, template=template))
        for count in (37, 1001):
            with self.assertRaises(ValueError): load_recipe(count)

    def test_feedback_uses_training_volatility_and_preserves_capital(self):
        spec = microstructure_recipe()
        accounts = copy.deepcopy(spec["scenario"]["accounts"])
        fit = {"spot": {"return_std_ppm_1s": 69.6}, "perp": {"return_std_ppm_1s": 75.99}}
        apply_feedback(spec, fit)
        self.assertEqual(accounts, spec["scenario"]["accounts"])
        for a in spec["agents"]:
            bot = a["Plugin"]
            if bot["plugin_id"] == "DynamicMarketMaker":
                self.assertEqual(bot["config"]["size_volatility_ticks"],
                                 70 if bot["participant"]["instrument_id"].endswith("SPOT") else 76)
            if bot["plugin_id"] == "AdaptiveNoiseTrader":
                self.assertEqual(bot["config"]["arrival_mode"], "Poisson")
                self.assertGreaterEqual(bot["config"]["decision_interval_ms"], 500)
                self.assertLessEqual(bot["config"]["decision_interval_ms"], 1000)
        saved = copy.deepcopy(spec)
        self.assertEqual(apply_feedback(spec, fit), saved)

    def test_percentiles_include_sample_count_and_missing_values(self):
        self.assertEqual(percentiles([]), {"count": 0, "p50_ms": None, "p95_ms": None, "max_ms": None})
        self.assertEqual(percentiles(list(range(1, 21))), {"count": 20, "p50_ms": 10, "p95_ms": 19, "max_ms": 20})


if __name__ == "__main__":
    unittest.main()
