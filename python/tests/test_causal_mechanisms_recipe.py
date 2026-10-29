import copy,sys,unittest
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parents[2]/'scripts'))
from research_market_mechanisms import options
from strategy_ecosystem import population
from research_causal_mechanisms import recipe_for


class CausalMechanismRecipeTests(unittest.TestCase):
    def test_paired_policies_change_only_the_declared_mechanism(self):
        for seed in (7,19,41,73):
            kwargs=dict(**options(seed),maker_quote_mode='model',requote_on_cancel_ack=True,reservation_innovations=True)
            base=population(**kwargs)[0]
            self.assertEqual(base,population(**kwargs,basis_execution='midpoint',market_depth_flow=False)[0])
            for profile,depth in (('hedged',False),('depth',True)):
                expected=recipe_for(base,profile)
                actual=population(**kwargs,basis_execution='touch-hedge',market_depth_flow=depth)[0]
                self.assertEqual(expected,actual)
                restored=copy.deepcopy(actual)
                for a in restored['agents']:
                    p=a['Plugin']
                    if p['plugin_id']=='BasisLinkTrader':p['plugin_version']='1.0.0'
                    p['config'].pop('market_depth_flow',None)
                self.assertEqual(base,restored)
        with self.assertRaises(ValueError):population(market_depth_flow=True)


if __name__=='__main__':unittest.main()
