"""Serial ABBA comparison of Pine dispatch and full journal checkpoint costs.

Each version receives identical seeded input/history. Asynchronous subsequent
trades may differ: fixed execution/recovery equality is a separate check.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import statistics
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
DRIVER_INPUTS = (
    'scripts/benchmark_pine_dispatch_checkpoint.py',
    'scripts/benchmark_strategy_runtime.py', 'scripts/benchmark_market_load.py',
    'scripts/process_tree_metrics.py', 'scripts/bot_runtime_receipts.py',
    'scripts/background_market.py', 'scripts/microstructure_market.py',
    'scripts/behavior_market.py', 'scripts/linked_market.py',
    'scripts/market_arrival_feedback.py', 'scripts/validate_behavior_market_live.py',
    'scripts/fixtures/background_market.json',
    'python/marketforge/__init__.py', 'python/marketforge/batch.py',
)
MEASUREMENT_VALIDITY = (
    'full_requested_duration_observed', 'process_sampling_complete',
    'no_harness_errors', 'metric_window_captured',
)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def frozen_inputs(binaries, plugin_dirs, root=ROOT):
    """Freeze the driver import closure, recipe fixture, and full plugin file set."""
    def file_hash(path):
        return digest(path) if path.is_file() else None
    return {
        'binary_sha256': {version: file_hash(path) for version, path in binaries.items()},
        'driver_sha256': {name: file_hash(root / name) for name in DRIVER_INPUTS},
        'plugin_sha256': {version: {str(path.relative_to(directory)): digest(path)
            for path in directory.rglob('*') if path.is_file() and '__pycache__' not in path.parts}
            for version, directory in plugin_dirs.items()},
    }


def changed_inputs(frozen, binaries, plugin_dirs, root=ROOT):
    current = frozen_inputs(binaries, plugin_dirs, root)
    return [name for name, values in frozen.items() if current[name] != values]


def comparison_invariants(rows, population):
    """Measurement validity is separate from a valid but inadequate capacity result."""
    checks = {
        'all_reports': bool(rows) and all(not row.get('failure') and row['exit_code'] == 0 for row in rows),
        'same_recipe': len({row.get('recipe_sha256') for row in rows}) == 1,
        'same_complete_initial_history': len({row.get('prewarm_candles_sha256') for row in rows}) == 1,
        'compiler_quiet': all((row.get('process_tree') or {}).get('compiler_nonempty_samples') == 0 for row in rows) if population == 'pine' else None,
        'no_other_benchmark_server': all((row.get('process_tree') or {}).get('other_server_nonempty_samples') == 0 for row in rows) if population == 'pine' else None,
    }
    for name in MEASUREMENT_VALIDITY:
        checks[name] = (bool(rows) and all(isinstance(row.get('qualified'), dict)
            and row['qualified'].get(name) is True for row in rows)) if population == 'pine' else None
    return checks


def pine_measurement_phases(diagnostics, required_stage):
    """Missing scheduled snapshots cannot turn into cumulative phase measurements."""
    phases = diagnostics.get('measurement_deltas')
    if not isinstance(phases, dict) or required_stage not in phases:
        raise ValueError('scheduled measurement phase deltas are unavailable')
    for stage in phases.values():
        if (not isinstance(stage, dict) or type(stage.get('count')) is not int or stage['count'] < 0
                or type(stage.get('total_wall_ms')) not in (int, float)
                or not math.isfinite(stage['total_wall_ms']) or stage['total_wall_ms'] < 0):
            raise ValueError('scheduled measurement phase deltas are invalid or reset')
    if phases[required_stage]['count'] == 0:
        raise ValueError('requested runtime has no scheduled measurement phase samples')
    return phases


def actual_worker_queue(path):
    events = [json.loads(line) for line in path.read_text(encoding='utf-8').splitlines()]
    origin = min(e['market_time_ms'] for e in events if e['stage'] == 'dispatch')
    dispatch = {}
    waits = []
    for event in events:
        if event['stage'] == 'dispatch':
            dispatch[event['id']] = event
        elif event['stage'] in ('worker_acquired', 'in_process_start'):
            pending = dispatch.pop(event['id'], None)
            if pending is None or pending['market_time_ms'] != event['market_time_ms']:
                raise ValueError('worker trace has no matching dispatch')
            if origin + 20000 <= event['market_time_ms'] < origin + 50000:
                waits.append(pending['elapsed_us'] / 1000)
    waits.sort()
    return {'count': len(waits), 'mean_ms': statistics.mean(waits) if waits else None,
            'p95_ms': waits[int((len(waits) - 1) * .95)] if waits else None,
            'max_ms': max(waits) if waits else None, 'origin_market_time_ms': origin,
            'scope': 'FIFO dispatch queue for calls paired with worker acquisition or direct Rust start; excludes subsequent compute-thread/worker waiting. Simulation-time [first_dispatch+20s, first_dispatch+50s). Both versions have trace overhead.'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--before', type=Path, required=True)
    parser.add_argument('--after', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--plugin-dir', type=Path, default=ROOT / 'bot-plugins')
    parser.add_argument('--before-plugin-dir', type=Path, help='frozen baseline plugin code/runtime')
    parser.add_argument('--after-plugin-dir', type=Path, help='candidate plugin code/runtime')
    parser.add_argument('--counts', type=int, default=1000)
    parser.add_argument('--seconds', type=float, default=30)
    parser.add_argument('--interval-ms', type=int, default=1000, help='identical Pine clock target in both versions')
    parser.add_argument('--native', action='store_true', help='also compare the 1000 native bot / 25ms workload')
    parser.add_argument('--host-trace', action='store_true', help='pair actual Pine worker calls with queue timestamps')
    parser.add_argument('--python-trace', action='store_true', help='profile the same worker phases in both versions')
    parser.add_argument('--write-profile', action='store_true', help='record native per-order server spans in both versions')
    parser.add_argument('--in-process-after', action='store_true', help='enable qualified Rust Pine only in candidate runs')
    parser.add_argument('--in-process-before', action='store_true', help='enable qualified Rust Pine in baseline runs')
    parser.add_argument('--pine-vm-checkpoint-after', action='store_true', help='enable compact VM checkpoints in candidate runs')
    args = parser.parse_args()
    if args.pine_vm_checkpoint_after and not args.in_process_after:
        parser.error('VM checkpoints require --in-process-after')
    args.output.mkdir(parents=True, exist_ok=False)
    binaries = {'before': args.before.resolve(), 'after': args.after.resolve()}
    plugin_dirs = {'before': (args.before_plugin_dir or args.plugin_dir).resolve(),
                   'after': (args.after_plugin_dir or args.plugin_dir).resolve()}
    frozen = frozen_inputs(binaries, plugin_dirs)
    if any(value is None for name in ('binary_sha256', 'driver_sha256') for value in frozen[name].values()):
        parser.error('a required frozen binary or driver input is missing')
    hashes, plugin_hashes = frozen['binary_sha256'], frozen['plugin_sha256']
    (args.output / 'frozen-inputs.json').write_text(json.dumps(frozen, indent=2), encoding='utf-8')
    results = []
    from benchmark_strategy_runtime import diagnostic_metrics
    for population in (['pine', 'native'] if args.native else ['pine']):
        for ordinal, version in enumerate(('before', 'after', 'after', 'before'), 1):
            label = f'{population}-{ordinal:02d}-{version}'
            directory = args.output / label
            changes = changed_inputs(frozen, binaries, plugin_dirs)
            if changes:
                results.append({'label': label, 'version': version, 'population': population,
                    'exit_code': None, 'attempted': False,
                    'failure': 'frozen inputs changed before case: ' + ', '.join(changes)})
                (args.output / 'cases.json').write_text(json.dumps(results, indent=2), encoding='utf-8')
                print(json.dumps(results[-1]), flush=True)
                continue
            if population == 'pine':
                flags = ['--population', 'pine', '--pine-profile', 'diverse-short',
                         '--sustained-input', '--prewarm-bars', '64', '--warmup', '20',
                         '--counts', str(args.counts), '--seconds', str(args.seconds),
                         '--interval-ms', str(args.interval_ms), '--require-diagnostics',
                         '--plugin-dir', str(plugin_dirs[version])]
                script = 'benchmark_strategy_runtime.py'
                if (args.in_process_after and version == 'after') or (args.in_process_before and version == 'before'):
                    flags += ['--pine-in-process']
                if version == 'after' and args.pine_vm_checkpoint_after:
                    flags += ['--pine-vm-checkpoint']
            else:
                flags = ['--counts', '1000', '--warmup', '5', '--seconds', str(args.seconds),
                         '--interval-ms', '25', '--max-memory-mib', '4096']
                script = 'benchmark_market_load.py'
                if args.write_profile:
                    flags += ['--write-profile']
            command = [sys.executable, str(ROOT / 'scripts' / script), '--server', str(binaries[version]),
                       '--output', str(directory), *flags]
            env = os.environ.copy()
            env.pop('PINE_BENCH_HOST_TRACE', None)
            env.pop('PINE_BENCH_PY_TRACE_DIR', None)
            trace = args.output.resolve() / f'{label}.jsonl'
            if args.host_trace and population == 'pine':
                env['PINE_BENCH_HOST_TRACE'] = str(trace)
            if args.python_trace and population == 'pine':
                python_trace = args.output.resolve() / f'{label}-python'
                python_trace.mkdir()
                env['PINE_BENCH_PY_TRACE_DIR'] = str(python_trace)
            with (args.output / f'{label}.log').open('w', encoding='utf-8') as log:
                process = subprocess.run(command, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)
            path = directory / f'bots-{args.counts if population == "pine" else 1000}'
            report_path = path / 'report.json'
            result = {'label': label, 'version': version, 'population': population,
                      'exit_code': process.returncode, 'receipt': str(report_path.resolve())}
            if report_path.is_file():
                report = json.loads(report_path.read_text(encoding='utf-8'))
                if 'failure' in report:
                    result['failure'] = report['failure']
                else:
                    diagnostics = report.get('diagnostics') or {}
                    if population == 'pine':
                        required_stage = ('bot_inprocess_call' if
                            ((version == 'after' and args.in_process_after) or
                             (version == 'before' and args.in_process_before)) else 'bot_worker_roundtrip')
                        try:
                            phases = pine_measurement_phases(diagnostics, required_stage)
                        except ValueError as error:
                            phases = {}
                            result['failure'] = str(error)
                    else:
                        diagnostics = diagnostics or diagnostic_metrics(path / 'metrics.txt')
                        phases = diagnostics.get('stages', {})
                    result.update(server_sha256=report['server_sha256'], recipe_sha256=report['recipe_sha256'],
                                  prewarm_candles_sha256=digest(path / 'prewarm-candles.json') if population == 'pine' else None,
                                  steps_per_wall_second=report['steps_per_wall_second'], latency=report['latency'],
                                  errors=report['errors'], qualified=report['qualified'],
                                  language_groups=report.get('language_groups'), process_tree=report.get('process_tree'),
                                  peak_memory_mib=report['peak_memory_mib'], average_logical_cores=report['average_logical_cores'],
                                  final_bot_errors=report.get('final', {}).get('status', {}).get('bot_errors'),
                                  phases={name: {'count': v['count'], 'total_wall_ms': v['total_wall_ms'],
                                                'mean_ms': v['total_wall_ms'] / v['count'] if v['count'] else None}
                                          for name, v in phases.items()},
                                  phase_scope=diagnostics.get('measurement_delta_scope', 'whole run including startup/warmup/quiescence'))
                    if args.host_trace and population == 'pine':
                        result['actual_worker_dispatch_queue'] = actual_worker_queue(trace)
                    if result['server_sha256'] != hashes[version]:
                        result['failure'] = 'measured binary hash changed'
            else:
                result['failure'] = 'no final report'
            changes = changed_inputs(frozen, binaries, plugin_dirs)
            if changes:
                result['failure'] = '; '.join(filter(None, (result.get('failure'),
                    'frozen inputs changed during case: ' + ', '.join(changes))))
            results.append(result)
            (args.output / 'cases.json').write_text(json.dumps(results, indent=2), encoding='utf-8')
            print(json.dumps({k: result.get(k) for k in ('label', 'steps_per_wall_second', 'qualified', 'failure')}), flush=True)
    invariants = {}
    aggregates = {}
    for population in {r['population'] for r in results}:
        rows = [r for r in results if r['population'] == population]
        invariants[population] = comparison_invariants(rows, population)
        aggregates[population] = {}
        for version in binaries:
            group = [r for r in rows if r['version'] == version and not r.get('failure')]
            if not group:
                continue
            metrics = {'step_rate': [r['steps_per_wall_second'] for r in group],
                       'place_p95_ms': [r['latency']['place']['p95_ms'] for r in group],
                       'cancel_p95_ms': [r['latency']['cancel']['p95_ms'] for r in group]}
            if population == 'pine':
                metrics['behind_instances'] = [r['language_groups']['pine']['instances_behind_closed_bars'] for r in group]
                if args.host_trace:
                    metrics['actual_worker_queue_mean_ms'] = [r['actual_worker_dispatch_queue']['mean_ms'] for r in group]
                    metrics['actual_worker_queue_p95_ms'] = [r['actual_worker_dispatch_queue']['p95_ms'] for r in group]
            aggregates[population][version] = {key: {'median': statistics.median(v), 'min': min(v), 'max': max(v)}
                                               for key, v in metrics.items()}
    final_changes = changed_inputs(frozen, binaries, plugin_dirs)
    result = {'cases': results, 'invariants': invariants, 'aggregates': aggregates,
              'binary_sha256': hashes, 'plugin_sha256': plugin_hashes,
              'driver_sha256': frozen['driver_sha256'], 'final_input_changes': final_changes,
              'plugin_dirs': {v: str(p) for v, p in plugin_dirs.items()},
              'scope': 'All attempted serial ABBA runs retained. Per-run percentile medians, not pooled requests. Short diagnostic memory-journal windows; no production, long-duration or PostgreSQL capacity claim.'}
    (args.output / 'summary.json').write_text(json.dumps(result, indent=2), encoding='utf-8')
    print(json.dumps({'invariants': invariants, 'aggregates': aggregates}, indent=2), flush=True)
    if final_changes or not all(all(v is not False for v in checks.values()) for checks in invariants.values()):
        raise SystemExit('comparison provenance or run validity failed; receipts retained')


if __name__ == '__main__':
    main()
