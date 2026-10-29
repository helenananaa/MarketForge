import copy,sys,unittest
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parents[2]/'scripts'))
from strategy_ecosystem import population
from research_market_mechanisms import options
from research_wick_observations import recipe_for,PROFILES
from research_wick_execution import recipe_for as execution_recipe,PROFILES as EXECUTION_PROFILES
from research_wick_style import recipe_for as style_recipe,PROFILES as STYLE_PROFILES


class WickObservationRecipeTests(unittest.TestCase):
    def test_all_four_seeds_change_only_the_declared_observation_flags(self):
        for seed in (41,7,19,73):
            original,_=population(**options(seed),maker_quote_mode='model',requote_on_cancel_ack=True,basis_execution='touch-hedge',reservation_innovations=True,market_depth_flow=True)
            before=copy.deepcopy(original)
            for profile,flags in PROFILES.items():
                candidate=recipe_for(original,profile)
                for a,b in zip(original['agents'],candidate['agents']):
                    if b['Plugin']['plugin_id']=='AdaptiveLiquidityMaker':
                        for key in flags:self.assertTrue(b['Plugin']['config'].pop(key))
                    self.assertEqual(a,b)
                self.assertEqual(candidate,original)
            self.assertEqual(original,before)

    def test_population_defaults_remain_unmodified_and_new_flags_require_fast_flow(self):
        original,_=population(**options(41),maker_quote_mode='model')
        explicit,_=population(**options(41),maker_quote_mode='model',touch_excursion_calibration=False,closed_candle_flow=False)
        self.assertEqual(original,explicit)
        for key in ('touch_excursion_calibration','closed_candle_flow'):
            with self.assertRaises(ValueError):population(count=300,**{key:True})

    def test_execution_ablation_changes_only_version_and_declared_observations(self):
        for seed in (41,7,19,73):
            original,_=population(**options(seed),maker_quote_mode='model',requote_on_cancel_ack=True,basis_execution='touch-hedge',reservation_innovations=True,market_depth_flow=True)
            sliced,_=population(**options(seed),maker_quote_mode='model',requote_on_cancel_ack=True,basis_execution='touch-hedge',reservation_innovations=True,market_depth_flow=True,reactive_execution='sliced')
            self.assertEqual(sliced,execution_recipe(original,'sliced'))
            for profile,observation in EXECUTION_PROFILES.items():
                candidate=execution_recipe(original,profile)
                for a,b in zip(original['agents'],candidate['agents']):
                    p=b['Plugin']
                    if p['plugin_id']=='ReactiveFlowTrader':
                        self.assertEqual(p['plugin_version'],'1.0.0' if profile=='control' else '1.1.0');p['plugin_version']='1.0.0'
                    if p['plugin_id']=='AdaptiveLiquidityMaker':
                        for key in PROFILES[observation or 'control']:self.assertTrue(p['config'].pop(key))
                    self.assertEqual(a,b)
        with self.assertRaises(ValueError):population(count=300,reactive_execution='sliced')

    def test_style_factorial_changes_only_the_two_execution_versions(self):
        for seed in (41,7,19,73):
            opts=dict(**options(seed),maker_quote_mode='model',requote_on_cancel_ack=True,basis_execution='touch-hedge',reservation_innovations=True,market_depth_flow=True)
            original,_=population(**opts)
            for profile,(reactive,style)in STYLE_PROFILES.items():
                generated,_=population(**opts,reactive_execution='sliced'if reactive else'sweep',style_execution='touch'if style else'sweep')
                candidate=style_recipe(original,profile);self.assertEqual(candidate,generated)
                for a in candidate['agents']:
                    if a['Plugin']['plugin_id']in ('ReactiveFlowTrader','StyleSwitchingTrader'):a['Plugin']['plugin_version']='1.0.0'
                self.assertEqual(candidate,original)
        with self.assertRaises(ValueError):population(count=300,style_execution='touch')


if __name__=='__main__':unittest.main()
