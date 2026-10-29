import copy,sys,unittest
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parents[2]/'scripts'))
from strategy_ecosystem import population
from research_market_mechanisms import options
from research_price_formation import recipe_for,PROFILES


class PriceFormationRecipeTests(unittest.TestCase):
    def test_isolation_and_cli_equivalence_on_all_seeds(self):
        for seed in (41,7,19,73):
            opts=dict(**options(seed),maker_quote_mode='model',requote_on_cancel_ack=True,basis_execution='touch-hedge',reservation_innovations=True,market_depth_flow=True,reactive_execution='sliced',style_execution='touch')
            control,_=population(**opts);saved=copy.deepcopy(control)
            for profile,flags in PROFILES.items():
                generated,_=population(**opts,**{k:True for k in flags})
                self.assertEqual(generated,recipe_for(control,profile))
            self.assertEqual(control,saved)

    def test_dependency_guards_and_defaults(self):
        before,_=population(**options(41))
        explicit,_=population(**options(41),persistent_index_anchor=False,book_slope_flow=False)
        self.assertEqual(before,explicit)
        for k in ('persistent_index_anchor','book_slope_flow'):
            with self.assertRaises(ValueError):population(count=300,**{k:True})
            with self.assertRaises(ValueError):population(**options(41),**{k:True})


if __name__=='__main__':unittest.main()
