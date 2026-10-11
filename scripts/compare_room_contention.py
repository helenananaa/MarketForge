"""Same-recipe before/after load, interleaved ABBA; retain interference receipts."""
import argparse
import hashlib
import json
from pathlib import Path
import time
from types import SimpleNamespace

from benchmark_room_contention import process_snapshot, run_case


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--before', type=Path, required=True)
    parser.add_argument('--after', type=Path, required=True)
    parser.add_argument('--load-client', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--concurrency', nargs='+', type=int, default=[1, 8, 32])
    parser.add_argument('--topologies', nargs='+', default=['one_book', 'four_books', 'four_rooms', 'four_venues'])
    parser.add_argument('--kinds', nargs='+', default=['Spot', 'Perp'])
    parser.add_argument('--workloads', nargs='+', default=['rest_cancel', 'roundtrip'])
    parser.add_argument('--cycles', type=int, default=2048)
    parser.add_argument('--compiler-wait-seconds', type=int, default=900)
    parser.add_argument('--max-attempts', type=int, default=4)
    parser.add_argument('--resume', action='store_true')
    options = parser.parse_args()
    options.output.mkdir(parents=True, exist_ok=True)
    assert all(1 <= c <= 64 and options.cycles % c == 0 for c in options.concurrency)
    assert 1 <= options.cycles <= 8192
    assert 1 <= options.max_attempts <= 4
    assert set(options.topologies) <= {'one_book', 'four_books', 'four_rooms', 'four_venues'}
    assert set(options.kinds) <= {'Spot', 'Perp'}
    assert set(options.workloads) <= {'rest_cancel', 'roundtrip'}
    root = Path(__file__).resolve().parents[1]
    hashes = {str(p.resolve()): hashlib.sha256(p.read_bytes()).hexdigest()
              for p in (options.before, options.after, options.load_client,
                        root / 'scripts/benchmark_room_contention.py',
                        root / 'exchange-server/examples/benchmark_room_http.rs')}
    if options.resume and (options.output / 'report.json').exists():
        previous = json.loads((options.output / 'report.json').read_text(encoding='utf-8'))
        assert previous['frozen_hashes'] == hashes, 'resume inputs changed'
    def save(report):
        (options.output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    reports = []
    # Every fixed recipe is run before -> after -> after -> before. Resume only
    # skips exact receipts with the frozen binary hash and recipe parameters.
    for c in options.concurrency:
        for kind in options.kinds:
            for workload in options.workloads:
                for topology in options.topologies:
                    for attempt in range(1, options.max_attempts + 1):
                        quartet = []
                        for index, variant in enumerate(('before', 'after', 'after', 'before'), 1):
                            label = f'{kind.lower()}-{workload}-{topology}-c{c}-{index}-{variant}-a{attempt}'
                            server = getattr(options, variant)
                            receipt = options.output / label / 'report.json'
                            if options.resume and receipt.exists():
                                row = json.loads(receipt.read_text(encoding='utf-8'))
                                assert row['server_sha256'] == hashes[str(server.resolve())]
                                assert row['cycles'] == options.cycles and row['concurrency'] == c
                                assert not row['errors']
                            else:
                                started = time.monotonic()
                                notice = 0
                                while any(p['name'].lower() in ('cargo.exe', 'rustc.exe') for p in process_snapshot()):
                                    if time.monotonic() - started > options.compiler_wait_seconds:
                                        raise RuntimeError('existing compilation still active; receipts retained, resume when idle')
                                    if time.monotonic() - notice > 30:
                                        print('waiting for other compilation; no processes stopped', flush=True)
                                        notice = time.monotonic()
                                    time.sleep(5)
                                args = SimpleNamespace(server=server, load_client=options.load_client,
                                    output=options.output, cycles=options.cycles, warmup_cycles=2,
                                    max_memory_mib=2048, max_seconds=120)
                                row = run_case(args, topology, kind, workload, c, label)
                            row['variant'] = variant
                            row['attempt'] = attempt
                            row['evidence_path'] = str(receipt.resolve())
                            row['compiler_free'] = row['compiler_overlap_samples'] == 0
                            row['eligible_comparison'] = False  # requires a complete clean quartet
                            quartet.append(row)
                            reports.append(row)
                            save({'frozen_hashes': hashes, 'cases': reports, 'ordering': 'ABBA per identical recipe',
                                  'scope': 'same client, same closed-loop fixed work, in-memory journal'})
                            if row['errors']:
                                raise RuntimeError(f'validation failed: {receipt}')
                        clean = all(row['compiler_free'] for row in quartet)
                        for row in quartet:
                            row['eligible_comparison'] = clean
                        save({'frozen_hashes': hashes, 'cases': reports, 'ordering': 'complete compiler-free ABBA quartets',
                              'scope': 'same client, same closed-loop fixed work, in-memory journal'})
                        if clean:
                            break
                        print(f'retrying contaminated quartet: {kind} {workload} {topology} c{c}', flush=True)
                    else:
                        raise RuntimeError('no clean ABBA quartet after bounded attempts; all evidence retained')
    print(json.dumps({'cases': len(reports), 'eligible': sum(r['eligible_comparison'] for r in reports),
                      'requests': sum(r['validation']['completed_commands'] for r in reports)}), flush=True)


if __name__ == '__main__':
    main()
