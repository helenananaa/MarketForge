"""Serial ABBA comparison of Pine dispatch and full journal checkpoint costs.

Each version receives identical seeded input/history. Asynchronous subsequent
trades may differ: fixed execution/recovery equality is a separate check.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import statistics
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


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
    parser.add_argument('--native', action='store_true', help='also compare the 1000 native bot / 25ms workload')
    parser.add_argument('--host-trace', action='store_true', help='pair actual Pine worker calls with queue timestamps')
    parser.add_argument('--python-trace', action='store_true', help='profile the same worker phases in both versions')
    parser.add_argument('--write-profile', action='store_true', help='record native per-order server spans in both versions')
    parser.add_argument('--in-process-after', action='store_true', help='enable qualified Rust Pine only in candidate runs')
    parser.add_argument('--in-process-before', action='store_true', help='enable qualified Rust Pine in baseline runs')
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    binaries = {'before': args.before.resolve(), 'after': args.after.resolve()}
    hashes = {version: digest(path) for version, path in binaries.items()}
    plugin_dirs = {'before': (args.before_plugin_dir or args.plugin_dir).resolve(),
                   'after': (args.after_plugin_dir or args.plugin_dir).resolve()}
    plugin_hashes = {version: {str(p.relative_to(directory)): digest(p)
                             for p in directory.rglob('*') if p.is_file() and '__pycache__' not in p.parts}
                     for version, directory in plugin_dirs.items()}
    results = []
    from benchmark_strategy_runtime import diagnostic_metrics
    for population in (['pine', 'native'] if args.native else ['pine']):
        for ordinal, version in enumerate(('before', 'after', 'after', 'before'), 1):
            label = f'{population}-{ordinal:02d}-{version}'
            directory = args.output / label
            if population == 'pine':
                flags = ['--population', 'pine', '--pine-profile', 'diverse-short',
                         '--sustained-input', '--prewarm-bars', '64', '--warmup', '20',
                         '--counts', str(args.counts), '--seconds', str(args.seconds),
                         '--interval-ms', '1000', '--require-diagnostics',
                         '--plugin-dir', str(plugin_dirs[version])]
                script = 'benchmark_strategy_runtime.py'
                if (args.in_process_after and version == 'after') or (args.in_process_before and version == 'before'):
                    flags += ['--pine-in-process']
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
                    diagnostics = report.get('diagnostics') or diagnostic_metrics(path / 'metrics.txt')
                    phases = diagnostics.get('measurement_deltas') or diagnostics.get('stages', {})
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
            results.append(result)
            (args.output / 'cases.json').write_text(json.dumps(results, indent=2), encoding='utf-8')
            print(json.dumps({k: result.get(k) for k in ('label', 'steps_per_wall_second', 'qualified', 'failure')}), flush=True)
    invariants = {}
    aggregates = {}
    for population in {r['population'] for r in results}:
        rows = [r for r in results if r['population'] == population]
        invariants[population] = {
            'all_reports': all(not r.get('failure') and r['exit_code'] == 0 for r in rows),
            'same_recipe': len({r.get('recipe_sha256') for r in rows}) == 1,
            'same_complete_initial_history': len({r.get('prewarm_candles_sha256') for r in rows}) == 1,
            'compiler_quiet': all((r.get('process_tree') or {}).get('compiler_nonempty_samples') == 0 for r in rows) if population == 'pine' else None,
            'no_other_benchmark_server': all((r.get('process_tree') or {}).get('other_server_nonempty_samples') == 0 for r in rows) if population == 'pine' else None,
        }
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
    assert all(digest(binaries[v]) == hashes[v] for v in binaries), 'binary changed during comparison'
    assert all(digest(plugin_dirs[v] / p) == h for v, files in plugin_hashes.items()
               for p, h in files.items()), 'plugin inputs changed'
    result = {'cases': results, 'invariants': invariants, 'aggregates': aggregates,
              'binary_sha256': hashes, 'plugin_sha256': plugin_hashes,
              'plugin_dirs': {v: str(p) for v, p in plugin_dirs.items()},
              'scope': 'All attempted serial ABBA runs retained. Per-run percentile medians, not pooled requests. Short diagnostic memory-journal windows; no production, long-duration or PostgreSQL capacity claim.'}
    (args.output / 'summary.json').write_text(json.dumps(result, indent=2), encoding='utf-8')
    print(json.dumps({'invariants': invariants, 'aggregates': aggregates}, indent=2), flush=True)
    if not all(all(v is not False for v in checks.values()) for checks in invariants.values()):
        raise SystemExit('comparison provenance or run validity failed; receipts retained')


if __name__ == '__main__':
    main()
