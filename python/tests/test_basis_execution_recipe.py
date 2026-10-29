import copy,sys,unittest
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parents[2]/'scripts'))
from strategy_ecosystem import population
from research_market_mechanisms import options
from research_basis_execution import executable_recipe
from research_quote_reservation import innovation_recipe

class BasisRecipeTests(unittest.TestCase):
    def test_default_and_treatment_preserve_economics_and_numerical_inputs(self):
        for seed in (7,19,41,73):
            kwargs=dict(**options(seed),maker_quote_mode='model',requote_on_cancel_ack=True)
            control,_=population(**kwargs)
            explicit,_=population(**kwargs,basis_execution='midpoint')
            candidate,_=population(**kwargs,basis_execution='executable')
            self.assertEqual(control,explicit)
            self.assertEqual(candidate,executable_recipe(control))
            reverted=copy.deepcopy(candidate)
            for a in reverted['agents']:
                if a['Plugin']['plugin_id']=='BasisLinkTrader':a['Plugin']['plugin_version']='1.0.0'
            self.assertEqual(control,reverted)
        with self.assertRaises(ValueError):population(basis_execution='unknown')

    def test_reservation_ablation_changes_only_maker_policy_flag(self):
        for seed in (7,19,41,73):
            kwargs=dict(**options(seed),maker_quote_mode='model',requote_on_cancel_ack=True)
            control,_=population(**kwargs)
            candidate,_=population(**kwargs,reservation_innovations=True)
            self.assertEqual(candidate,innovation_recipe(control))
            self.assertEqual(control,population(**kwargs,reservation_innovations=False)[0])

if __name__=='__main__':unittest.main()
