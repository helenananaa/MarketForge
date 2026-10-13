"""ABBA validity must not accept incomplete measurements or changed inputs."""
import copy
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts'))
from benchmark_pine_dispatch_checkpoint import (
    DRIVER_INPUTS, MEASUREMENT_VALIDITY, changed_inputs, comparison_invariants,
    frozen_inputs, pine_measurement_phases,
)


def valid_case():
    return {
        'exit_code': 0, 'recipe_sha256': 'same-recipe',
        'prewarm_candles_sha256': 'same-history',
        'process_tree': {'compiler_nonempty_samples': 0, 'other_server_nonempty_samples': 0},
        'qualified': {name: True for name in MEASUREMENT_VALIDITY},
    }


class ComparisonValidityTests(unittest.TestCase):
    def test_one_incomplete_case_invalidates_abba_despite_clean_poll_counts(self):
        for name in MEASUREMENT_VALIDITY:
            for value in (None, False, 1, 'true', 'missing'):
                with self.subTest(name=name, value=value):
                    rows = [valid_case() for _ in range(4)]
                    if value == 'missing':
                        del rows[2]['qualified'][name]
                    else:
                        rows[2]['qualified'][name] = value
                    original = copy.deepcopy(rows)
                    checks = comparison_invariants(rows, 'pine')
                    self.assertTrue(checks['all_reports'])
                    self.assertTrue(checks['compiler_quiet'])
                    self.assertFalse(checks[name])
                    self.assertEqual(rows, original, 'retain the complete original qualification report')

    def test_missing_or_legacy_qualification_is_not_a_valid_pine_measurement(self):
        for qualification in (None, False, {}):
            row = valid_case()
            row['qualified'] = qualification
            checks = comparison_invariants([row], 'pine')
            self.assertTrue(all(checks[name] is False for name in MEASUREMENT_VALIDITY))

    def test_capacity_failure_does_not_invalidate_an_intact_measurement(self):
        rows = [valid_case() for _ in range(4)]
        for row in rows:
            row['qualified'].update(configured_step_rate=False, literal_step_rate=False,
                no_closed_bar_backlog_after_pause=False, order_p95_under_250ms=False)
        self.assertTrue(all(comparison_invariants(rows, 'pine').values()))

    def test_native_legacy_reports_do_not_claim_pine_measurement_coverage(self):
        row = valid_case()
        del row['qualified']
        checks = comparison_invariants([row], 'native')
        self.assertTrue(checks['all_reports'])
        self.assertTrue(all(checks[name] is None for name in MEASUREMENT_VALIDITY))

    def test_missing_window_phases_cannot_fall_back_to_cumulative_stats(self):
        cumulative = {'bot_inprocess_call': {'count': 99, 'total_wall_ms': 1234}}
        for deltas in (None, {}, {'other': {'count': 2, 'total_wall_ms': 3}}):
            with self.subTest(deltas=deltas), self.assertRaisesRegex(ValueError, 'unavailable'):
                pine_measurement_phases({'stages': cumulative, 'measurement_deltas': deltas},
                                        'bot_inprocess_call')

    def test_measured_phases_preserve_delta_and_reject_resets_or_no_runtime_work(self):
        phases = {'bot_inprocess_call': {'count': 2, 'total_wall_ms': 4.5}}
        diagnostics = {'stages': {'bot_inprocess_call': {'count': 99, 'total_wall_ms': 1234}},
                       'measurement_deltas': phases}
        self.assertEqual(pine_measurement_phases(diagnostics, 'bot_inprocess_call'), phases)
        for count, wall in ((-1, 4), (0, 0), (2, -1), (2, float('nan'))):
            with self.subTest(count=count, wall=wall), self.assertRaises(ValueError):
                pine_measurement_phases({'measurement_deltas': {'bot_inprocess_call':
                    {'count': count, 'total_wall_ms': wall}}}, 'bot_inprocess_call')


class FrozenInputTests(unittest.TestCase):
    def test_driver_edits_and_plugin_set_changes_are_detected_between_cases(self):
        for mutation in ('driver-edit', 'driver-delete', 'plugin-edit', 'plugin-add', 'plugin-delete'):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                for name in DRIVER_INPUTS:
                    path = root / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text('original', encoding='utf-8')
                binary = root / 'server.exe'
                binary.write_bytes(b'original-binary')
                plugin = root / 'plugin'
                plugin.mkdir()
                source = plugin / 'strategy.py'
                source.write_text('original', encoding='utf-8')
                binaries, plugins = {'before': binary}, {'before': plugin}
                frozen = frozen_inputs(binaries, plugins, root)
                self.assertEqual(changed_inputs(frozen, binaries, plugins, root), [])
                if mutation == 'driver-edit':
                    (root / DRIVER_INPUTS[1]).write_text('changed', encoding='utf-8')
                elif mutation == 'driver-delete':
                    (root / DRIVER_INPUTS[1]).unlink()
                elif mutation == 'plugin-edit':
                    source.write_text('changed', encoding='utf-8')
                elif mutation == 'plugin-add':
                    (plugin / 'new_strategy.pine').write_text('new', encoding='utf-8')
                else:
                    source.unlink()
                expected = 'driver_sha256' if mutation.startswith('driver') else 'plugin_sha256'
                self.assertEqual(changed_inputs(frozen, binaries, plugins, root), [expected])

    def test_long_run_without_receipts_fails_before_launch(self):
        import benchmark_strategy_runtime
        with patch.object(sys, 'argv', ['benchmark_strategy_runtime.py', '--server', 'unused',
                '--output', 'unused', '--long-run']), patch('argparse.ArgumentParser.error',
                side_effect=ValueError) as error:
            with self.assertRaises(ValueError):
                benchmark_strategy_runtime.main()
            self.assertIn('--long-run requires --require-bot-receipts', error.call_args.args[0])


if __name__ == '__main__':
    unittest.main()
