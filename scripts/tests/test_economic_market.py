"""Finite funding, versioned cost contracts and session-bound parent demand."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
from strategy_ecosystem import population

class EconomicMarketTest(unittest.TestCase):
    def recipe(self, economic=False, session_ms=7200000):
        return population(seed=7,count=300,ecology=True,noise_fraction=.1,
            value_model='endogenous',value_fraction=.2,flow_mix='rebalanced',
            switching_fraction=.06,price_scale=10,fast_flow=True,
            periods=[10000,30000,60000,300000],economic_flow=economic,session_ms=session_ms)

    def test_preserves_funding_accounts_and_inventory(self):
        legacy,old=self.recipe();new,meta=self.recipe(True)
        self.assertEqual(legacy['scenario'],new['scenario'])
        self.assertEqual(old['total_cash_pool'],meta['total_cash_pool'])
        self.assertEqual(old['role_counts'],meta['role_counts'])
        self.assertEqual(old['initial_spot_inventory'],meta['initial_spot_inventory'])
        self.assertEqual(self.recipe(True),(new,meta))

    def test_every_parent_fits_inside_the_declared_session(self):
        for horizon in [1200000,7200000,14400000]:
            recipe,_=self.recipe(True,horizon)
            parents=[a['Plugin']['config'] for a in recipe['agents'] if a['Plugin']['plugin_id']=='ExecutionTrader']
            self.assertEqual(len(parents),45)
            for c in parents:
                self.assertGreater(c['target_qty'],0)
                self.assertGreaterEqual(c['start_after_ms'],0)
                self.assertLessEqual(c['start_after_ms']+c['horizon_ms'],horizon)

    def test_fees_are_explicit_and_visual_feedback_is_disabled(self):
        recipe,_=self.recipe(True)
        fee=recipe['scenario']['market']['Spot']['clearing']['maker_fee_ppm']
        for a in recipe['agents']:
            p=a['Plugin'];c=p['config']
            if p['plugin_id']=='AdaptiveLiquidityMaker':
                self.assertEqual(p['plugin_version'],'1.2.0')
                self.assertEqual(c['maker_fee_ppm'],fee)
                self.assertGreater(c['max_half_spread_ticks'],1)
                self.assertGreater(c['jitter_ms'],0)
                self.assertGreater(c['max_qty'],5)
                self.assertLessEqual(c['max_qty']*c['levels'],c['inventory_cap'])
            elif p['plugin_id']=='ReactiveFlowTrader':
                self.assertEqual(c['attention_ppm'],0)
                self.assertEqual(c['excitation_ppm'],0)
            elif p['plugin_id']=='BasisLinkTrader':
                self.assertEqual(p['plugin_version'],'1.1.0')
                self.assertEqual(c['round_trip_fee_ppm'],1200)

if __name__=='__main__':unittest.main()
