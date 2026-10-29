import sys,unittest
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parents[2]/'scripts'))
from strategy_ecosystem import population
from research_market_mechanisms import options
from research_inventory_level_study import level_recipe


class InventoryLevelRecipeTests(unittest.TestCase):
    def test_removal_changes_only_the_reservation_hypothesis(self):
        for seed in (7,19,41,73):
            kwargs=dict(**options(seed),maker_quote_mode='model',requote_on_cancel_ack=True,basis_execution='touch-hedge',market_depth_flow=True)
            level=population(**kwargs)[0];control=population(**kwargs,reservation_innovations=True)[0]
            self.assertEqual(level,level_recipe(control))


if __name__=='__main__':unittest.main()
