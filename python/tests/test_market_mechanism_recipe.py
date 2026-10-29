"""Paired treatment isolation: economics and all non-maker agents stay exact."""
import copy
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'scripts'))
from strategy_ecosystem import population
import research_market_mechanisms as research


class MechanismRecipeTests(unittest.TestCase):
    def test_treatment_changes_only_maker_quote_lifecycle(self):
        for seed in (7, 19, 41, 73):
            options = dict(seed=seed, count=300, ecology=True, value_model='endogenous',
                           value_fraction=.2, flow_mix='rebalanced', switching_fraction=.06,
                           price_scale=10, fast_flow=True)
            baseline, before = population(**options)
            treatment, after = population(**options, maker_quote_mode='model',
                                          requote_on_cancel_ack=True)
            self.assertEqual(baseline['scenario'], treatment['scenario'])
            self.assertEqual(before['role_cash_pools'], after['role_cash_pools'])
            self.assertEqual(before['initial_spot_inventory'], after['initial_spot_inventory'])
            for a, b in zip(baseline['agents'], treatment['agents']):
                if a['Plugin']['plugin_id'] != 'AdaptiveLiquidityMaker':
                    self.assertEqual(a, b)
                    continue
                cleaned = copy.deepcopy(b)
                c = cleaned['Plugin']['config']
                self.assertTrue(c.pop('requote_on_cancel_ack'))
                self.assertGreater(c['max_half_spread_ticks'], c['min_half_spread_ticks'])
                for key in ('max_half_spread_ticks', 'refresh_ms', 'jitter_ms'):
                    c[key] = a['Plugin']['config'][key]
                self.assertEqual(a, cleaned)

    def test_explicit_legacy_is_default_and_invalid_profiles_fail(self):
        self.assertEqual(population(), population(maker_quote_mode='touch'))
        for options in ({'maker_quote_mode':'unknown'}, {'maker_quote_mode':'model'},
                        {'requote_on_cancel_ack':True}):
            with self.assertRaises(ValueError):
                population(**options)

    def test_cached_experiment_rejects_changed_interpreted_runtime(self):
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder);out=root/'evidence';out.mkdir()
            for name in ('Cargo.lock','exchange-core/examples/strategy_market.rs',
                         'exchange-server/examples/strategy_ecosystem_market.rs',
                         'exchange-core/examples/support/research_output.rs',
                         'exchange-core/examples/support/research_checkpoint.rs',
                         'scripts/research_receipts.py','scripts/research_resources.py',
                         'bot-plugins/pine-strategy/bot.py','bot-plugins/pine-strategy/scripts/test.pine',
                         'bot-plugins/pine-strategy/.runtime/pine_compat.pyd'):
                p=root/name;p.parent.mkdir(parents=True,exist_ok=True);p.write_bytes(b'original')
            recipe=out/'recipe.json'
            recipe.write_text(json.dumps(dict(agents=[dict(Plugin=dict(plugin_id='pine.strategy',
                                               config=dict(script='test.pine')))])))
            rows=[dict(recipe=str(recipe))]
            with patch.object(research,'ROOT',root):
                research.ensure_runtime_inputs(out,rows)
                research.ensure_runtime_inputs(out,rows)
                script=root/'bot-plugins/pine-strategy/scripts/test.pine'
                script.write_bytes(b'changed strategy')
                with self.assertRaisesRegex(RuntimeError,'Runtime inputs changed'):
                    research.ensure_runtime_inputs(out,rows)
                script.write_bytes(b'original')
                (root/'bot-plugins/pine-strategy/.runtime/pine_compat.pyd').write_bytes(b'changed runtime')
                with self.assertRaisesRegex(RuntimeError,'Runtime inputs changed'):
                    research.ensure_runtime_inputs(out,rows)
                (out/'runtime-inputs.json').unlink()
                (out/'old.run.json').write_text('{}')
                with self.assertRaisesRegex(RuntimeError,'cannot be reconstructed retroactively'):
                    research.ensure_runtime_inputs(out,rows)


if __name__ == '__main__':
    unittest.main()
