"""Summarize paired same-topology load receipts without averaging percentiles."""
import argparse
from collections import defaultdict
import hashlib
import json
from pathlib import Path
import statistics
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--inputs', type=Path, nargs='+', required=True)
    parser.add_argument('--output', type=Path, required=True)
    options = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    manifests = [json.loads((p / 'report.json').read_text(encoding='utf-8')) for p in options.inputs]
    cases = []
    for manifest in manifests:
        for row in manifest['cases']:
            row = dict(row)
            row.pop('resources', None)
            evidence = Path(row['evidence_path']).parent
            def metrics(name):
                result = {}
                for line in (evidence / name).read_text(encoding='utf-8').splitlines():
                    if line and not line.startswith('#'):
                        key, value = line.rsplit(' ', 1)
                        result[key] = float(value)
                return result
            before, after = metrics('metrics-before.txt'), metrics('metrics-after.txt')
            stages = {}
            for name in ('venue_wait', 'venue_compute', 'venue_commit', 'venue_fallback'):
                prefix = f'marketforge_bench_{name}_'
                if prefix + 'count_total' in after:
                    count = int(after[prefix + 'count_total'] - before.get(prefix + 'count_total', 0))
                    nanos = after[prefix + 'nanos_total'] - before.get(prefix + 'nanos_total', 0)
                    stages[name] = {'count': count, 'mean_ms': nanos / 1e6 / count if count else None}
            row['venue_stages'] = stages
            n = row['validation']['completed_commands']
            assert n == row['validation']['expected_commands']
            for name in ('room_wait', 'room_held', 'order_prepare', 'order_apply', 'order_receipt', 'order_journal', 'order_publish'):
                assert row['stages'][name]['count'] == n, (row['label'], name)
            if row['variant'] == 'after' and row['topology'] == 'four_venues':
                assert stages['venue_compute']['count'] == stages['venue_commit']['count'] == n
                assert stages['venue_fallback']['count'] == 0
            cases.append(row)
    grouped = defaultdict(lambda: defaultdict(list))
    for row in cases:
        if row['eligible_comparison']:
            key = (row['kind'], row['workload'], row['concurrency'], row['topology'], row['cycles'])
            grouped[key][row['variant']].append(row)
    groups = []
    def bounds(rows, extract):
        values = [extract(row) for row in rows]
        return [min(values), max(values)]
    for (kind, workload, c, topology, cycles), variants in sorted(grouped.items()):
        if len(variants['before']) < 2 or len(variants['after']) < 2:
            continue
        group = {'kind': kind, 'workload': workload, 'concurrency': c, 'topology': topology, 'cycles': cycles}
        for variant, rows in variants.items():
            group[variant] = {'runs': len(rows),
                'requests_per_second': bounds(rows, lambda r: r['accepted_commands_per_second']),
                'http_p95_ms': bounds(rows, lambda r: r['http_latency']['p95_ms']),
                'read_p95_ms': bounds(rows, lambda r: r['read_probe_latency']['p95_ms']),
                'room_wait_mean_ms': bounds(rows, lambda r: r['stages']['room_wait']['mean_ms']),
                'room_held_mean_ms': bounds(rows, lambda r: r['stages']['room_held']['mean_ms']),
                'journal_mean_ms': bounds(rows, lambda r: r['stages']['order_journal']['mean_ms']),
                'server_average_cores': bounds(rows, lambda r: r['server_average_cores']),
                'peak_memory_mib': bounds(rows, lambda r: r['peak_memory_mib']),
                'evidence': [r['evidence_path'] for r in rows]}
        before = statistics.median(r['accepted_commands_per_second'] for r in variants['before'])
        after = statistics.median(r['accepted_commands_per_second'] for r in variants['after'])
        group['throughput_ratio_of_medians'] = after / before
        groups.append(group)
    sources = ['exchange-core/src/actor.rs', 'exchange-core/src/room.rs', 'exchange-core/src/simulation.rs',
        'exchange-server/src/lib.rs', 'exchange-server/src/room_runtime.rs', 'exchange-server/src/venue_orders.rs',
        'exchange-server/src/venue_order_tests.rs', 'exchange-server/src/bench_execution_timing.rs',
        'exchange-server/Cargo.toml', 'scripts/compare_room_contention.py', 'scripts/summarize_room_optimization.py']
    result = {'base_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip(),
        'branch': subprocess.check_output(['git', 'branch', '--show-current'], cwd=root, text=True).strip(),
        'sources_sha256': {p: hashlib.sha256((root/p).read_bytes()).hexdigest() for p in sources},
        'frozen_input_manifests': [{'path': str(p.resolve()), 'hashes': m['frozen_hashes']} for p,m in zip(options.inputs, manifests)],
        'case_count': len(cases), 'eligible_case_count': sum(r['eligible_comparison'] for r in cases),
        'excluded_compiler_overlap_count': sum(r['compiler_overlap_samples'] > 0 for r in cases),
        'measured_commands': sum(r['validation']['completed_commands'] for r in cases),
        'actual_trades': sum(r['validation']['trades'] for r in cases),
        'errors': [r['evidence_path'] for r in cases if r['errors']], 'groups': groups, 'cases': cases,
        'limits': ['Same topology and same client, ABBA ordering; shared machine background load retained.',
            'At least two compiler-free repetitions per variant required for comparative groups.',
            'Short closed-loop fixed-work load with in-memory journal, not PostgreSQL or sustained capacity.',
            'Room journal commit remains serial; bot/automation/training/clock/transfers use coordination.',
            'Diagnostic stage boundaries changed: venue wait/compute happen outside room commit lock.']}
    options.output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    print(json.dumps({k: result[k] for k in ('case_count', 'eligible_case_count', 'excluded_compiler_overlap_count', 'measured_commands', 'errors')}), flush=True)


if __name__ == '__main__':
    main()
