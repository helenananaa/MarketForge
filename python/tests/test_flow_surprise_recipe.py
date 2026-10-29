import copy
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'scripts'))
from research_market_mechanisms import options
from research_flow_surprise import recipe_for
from strategy_ecosystem import population


class FlowSurpriseRecipeTests(unittest.TestCase):
    def test_every_fixed_seed_and_new_holdout_change_only_one_boolean(self):
        for seed in (41, 7, 19, 73, 211):
            opts = dict(**options(seed), maker_quote_mode='model', requote_on_cancel_ack=True,
                basis_execution='touch-hedge', reservation_innovations=True, market_depth_flow=True,
                reactive_execution='sliced', style_execution='touch', persistent_index_anchor=True,
                book_slope_flow=True)
            control, _ = population(**opts)
            saved = copy.deepcopy(control)
            candidate, _ = population(**opts, flow_surprise=True)
            self.assertEqual(candidate, recipe_for(control, True))
            self.assertEqual(control, recipe_for(control, False))
            for agent in candidate['agents']:
                if agent['Plugin']['plugin_id'] == 'AdaptiveLiquidityMaker':
                    self.assertIs(agent['Plugin']['config'].pop('flow_surprise'), True)
            self.assertEqual(candidate, control)
            self.assertEqual(control, saved)

    def test_defaults_and_dependency_guard(self):
        control, _ = population(**options(41))
        explicit, _ = population(**options(41), flow_surprise=False)
        self.assertEqual(control, explicit)
        with self.assertRaises(ValueError):
            population(**options(41), flow_surprise=True)
        with self.assertRaises(ValueError):
            population(count=300, flow_surprise=True)


if __name__ == '__main__':
    unittest.main()
