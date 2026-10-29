"""Reconcile fixed recipes, actual fills, archives and frozen source provenance."""
import argparse
import json
import zipfile
from pathlib import Path
from research_market_mechanisms import ROOT, sha, write
from research_flow_surprise import BASE, PRIOR, recipe_for
from analyze_wick_basis_capacity import one
from analyze_wick_demand import analyze as demand
from analyze_price_formation_links import analyze as links


def read(path):
    return json.loads(path.read_text(encoding='utf-8'))


def winpath(path):
    return Path('E:/' + path[7:]) if path.startswith('/mnt/e/') else Path(path)


def diagnostics(out):
    p = read(out/'plan.json')
    capacity = []
    for r in p['runs']:
        done = out/(r['name']+'.run.json')
        if done.exists() and read(done)['complete']:
            capacity.extend(one(out, r))
    write(out/'basis-capacity.json', dict(records=capacity,
        scope='Actual ordered fills, primary hours2–4, unchanged finite capacity. Durations include exposure carried into the window; event maxima include same-timestamp legging. Capacity occupancy alone does not explain every missed opportunity.'))
    demand(out)
    links(out)


def verify(folders):
    c0 = BASE/'current-control'
    c1 = BASE/'pilot'
    name = 'native-s41-raw-flow'
    old = PRIOR/'native-s41-anchor-slope.receipt.json'
    assert sha(old) == sha(c0/(name+'.receipt.json')) == sha(c1/(name+'.receipt.json'))
    c0_archives = read(c0/(name+'.summary.json'))['execution_archives']
    c1_archives = read(c1/(name+'.summary.json'))['execution_archives']
    assert [(a['records'], a['sha256']) for a in c0_archives] == [(a['records'], a['sha256']) for a in c1_archives]
    # Source changes made by this experiment are restricted to these two files.
    model = read(BASE/'candidate-design/model-inputs.json')
    changed = {f['path']: f['sha256'] for f in model['files']}
    assert set(changed) == {'exchange-core/src/adaptive_liquidity.rs', 'scripts/strategy_ecosystem.py'}
    for f in read(BASE/'control-inputs.json')['files']:
        assert sha(BASE/'control-source'/f['path']) == f['sha256']
        assert sha(ROOT/f['path']) == changed.get(f['path'], f['sha256']), f['path']
    design = BASE/'candidate-design'
    dp = read(design/'plan.json')
    for f in model['files']:
        assert sha(design/'model-source'/f['path']) == f['sha256']
    recipes = {}
    for r in dp['runs']:
        assert sha(Path(r['recipe'])) == r['recipe_sha256']
        recipes[r['seed'], r['profile']] = read(Path(r['recipe']))
    for seed in {r['seed'] for r in dp['runs']}:
        assert recipes[seed, 'surprise'] == recipe_for(recipes[seed, 'raw-flow'], True)
        if seed != 211:
            assert recipes[seed, 'raw-flow'] == read(PRIOR/f'native-s{seed}-anchor-slope.recipe.json')
    checked, external, unique = [], [], {}
    for out in [c0] + folders:
        p = read(out/'plan.json')
        assert sha(out/'candidate.exe') == p['binary_sha256']
        if out != c0:
            assert p['binary_sha256'] == dp['binary_sha256']
        cache = read(out/'cache-provenance.json') if (out/'cache-provenance.json').exists() else {}
        for r in p['runs']:
            done = out/(r['name']+'.run.json')
            if not done.exists():
                continue
            d = read(done)
            assert d['complete'] and d['exit_code'] == 0
            assert d['inputs'] == dict(recipe_sha256=r['recipe_sha256'], binary_sha256=p['binary_sha256'], steps=14400, seconds=1)
            assert sha(Path(r['recipe'])) == r['recipe_sha256']
            raw = out/(r['name']+'.receipt.json')
            assert sha(raw) == d['receipt_sha256']
            if r['profile'] == 'raw-flow' and r['seed'] != 211:
                assert sha(PRIOR/f"native-s{r['seed']}-anchor-slope.receipt.json") == d['receipt_sha256']
            summary = read(out/(r['name']+'.summary.json'))
            assert summary['receipt_sha256'] == d['receipt_sha256']
            assert summary['steps'] == 14400 and summary['simulation_time_ms'] == 14400000
            assert summary['trades'] == d['trades']
            external.append(dict(path=str(raw), bytes=raw.stat().st_size, sha256=d['receipt_sha256']))
            for archive in summary['execution_archives']:
                path = winpath(archive['path'])
                assert sha(path) == archive['sha256']
                with path.open('rb') as f:
                    count = sum(block.count(b'\n') for block in iter(lambda: f.read(1024*1024), b''))
                assert count == archive['records']
                external.append(dict(path=str(path), bytes=path.stat().st_size, sha256=archive['sha256'], records=count))
            reused = r['name'] in cache
            if reused:
                origin = Path(cache[r['name']]['directory'])
                assert sha(origin/(r['name']+'.run.json')) == sha(done) == cache[r['name']]['run_sha256']
                assert sha(origin/(r['name']+'.receipt.json')) == d['receipt_sha256']
            else:
                unique[str(out), r['name']] = {**d, 'directory':str(out)}
            checked.append(dict(directory=str(out), name=r['name'], seed=r['seed'], profile=r['profile'], cached=reused,
                receipt_sha256=d['receipt_sha256'], peak_memory_kib=d['peak_memory_kib'], peak_swap_kib=d['peak_swap_kib']))
        if out == c0:
            continue
        comparison = read(out/'comparison.json')
        assert comparison['complete']
        for m in comparison['metrics']:
            assert m['integrity']['count_matches'] and m['integrity']['self_trades'] == 0
            assert m['asset_checks']['total_deltas']['V'] == 0
            for stage in ('before', 'after'):
                assert not m['asset_checks'][stage]['negative_available']
        for r in read(out/'demand-reconciliation.json')['records']:
            assert r['actual_minus_child_fills'] == 0
            assert all(a['actual_minus_child_fills'] == 0 for a in r['details'])
    archives = {
        '2026-10-24-market-mechanisms-evidence.zip': '2e7df9279d0fcc5d067edb0d050ce0782d9f6a18ce7abf94b2be734cd065f93c',
        '2026-10-24-market-feedback-evidence.zip': '0e000afe851fba640d7f2eb4c0e326ed05865e4990771158c750e0fc5a55024e',
        '2026-10-25-market-causal-evidence.zip': '1b2201748423599962aeb8b7f7cf3fcf821e062fb98050a6a040ea5dc9ede9dd',
        '2026-10-25-market-wicks-evidence.zip': '2a57a2c08aa8f7fb71033ed3b821db485fa1eabc67a517bd792487806c0fa49e',
        '2026-10-25-market-price-formation-evidence.zip': '8e4cd54b73f96180ff76e7c782da975c9fce9c6b4925e1ef75c41c9a872c11c8'}
    for name, digest in archives.items():
        assert sha(ROOT/'docs/validation'/name) == digest
    with zipfile.ZipFile(ROOT/'docs/validation/2026-10-25-market-price-formation-evidence.zip') as z:
        previous = json.loads(z.read('manifest.json'))
        hashes = {f['name']:f['sha256'] for f in previous['files']}
        source = ROOT/'.local/strategy-research-reservation-20261024/reference-calendar/calendar-reference.json'
        assert sha(source) == hashes['reference/cached-calendar-reference.json']
        context = ROOT/'.local/strategy-research-price-formation-20261025/reference-short-context.json'
        assert sha(context) == hashes['study/reference-short-context.json']
        assert read(BASE/'reference-horizon-context.json')['source_sha256'] == sha(source)
        for seed in (41,7,19,73):
            raw = PRIOR/f'native-s{seed}-anchor-slope.receipt.json'
            archived = next(r for r in previous['external_raw_data'] if Path(r['path']) == raw)
            assert sha(raw) == archived['sha256']
    holdout=BASE/'holdout'
    failed=read(holdout/'native-s211-raw-flow.run.json')
    assert not failed['complete'] and failed['stop_reason']=='host_commit_headroom_below_3GiB'
    partial=holdout/'native-s211-raw-flow.receipt.json'
    assert partial.stat().st_size==failed['partial_receipt_bytes']==0
    assert sha(partial)==failed['partial_receipt_sha256']
    assert not (holdout/'native-s211-surprise.run.json').exists()
    assert not (holdout/'native-s211-surprise.receipt.json').exists()
    assert read(BASE/'holdout-closure.json')['candidate_started'] is False
    external.append(dict(path=str(partial),bytes=0,sha256=sha(partial),complete=False))
    for path in (holdout/'native-s211-raw-flow.spool').rglob('*'):
        if path.is_file():
            external.append(dict(path=str(path),bytes=path.stat().st_size,sha256=sha(path),complete=False,
                note='Interrupted native spool, no valid completion summary; no horizon or final record-count qualification.'))
    log=(holdout/'native-s211-raw-flow.log').read_text(encoding='utf-8')
    steps=[int(line.split()[0].split('=')[1]) for line in log.splitlines() if line.startswith('step=')]
    interruption=dict(metadata=failed,last_logged_step=max(steps),exact_stop_step_unknown=True,successful_hours=0,
        scope='Native control211 interrupted by the existing3GiB host guard. Systemd proxy returned0 but the launcher correctly retained failure; empty stdout is not a success. Candidate211 cancelled while unstarted at the8GiB resource gate. Neither profile yields independent holdout statistics.')
    result = dict(native_runs=len(unique), simulated_hours=4*len(unique), cached_artifacts=len(checked)-len(unique),
        checked=checked, native_metadata=list(unique.values()), previous_archives_unchanged=archives,
        raw_control_byte_identity=sha(old), source_and_recipe_checks=True,
        full_execution_control_byte_identity=[dict(records=a['records'], sha256=a['sha256']) for a in c0_archives],
        interrupted_native_attempts=[interruption],cancelled_unstarted_candidates=1,independent_holdout_complete=False,
        external_native_data=external, folders=[str(p) for p in folders],
        scope='Same Linux platform and Rust1.97.1. Current output/archive changes and new-mode-off replay reproduce prior complete raw receipt byte-for-byte. Entire actual tape and endpoint balances checked. No12h, live, intraday full-fee/margin or broad market-realism qualification.')
    write(BASE/'completion.json', result)
    print(json.dumps({k:v for k,v in result.items() if k not in ('native_metadata','external_native_data','checked')}, indent=2))


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('action', choices=['diagnostics', 'verify'])
    p.add_argument('directories', type=Path, nargs='+')
    args = p.parse_args()
    if args.action == 'diagnostics':
        for folder in args.directories:
            diagnostics(folder)
    else:
        verify(args.directories)
