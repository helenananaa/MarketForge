"""Frozen-seed mechanism ablations. Never select parameters from the resulting chart."""
from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
import subprocess
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime
from pathlib import Path

from background_market import child_seed
from strategy_ecosystem import population

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_OUT = ROOT / '.local/strategy-research-mechanisms-20261024'
PROFILES = {
    'baseline': dict(maker_quote_mode='touch', requote_on_cancel_ack=False),
    'model': dict(maker_quote_mode='model', requote_on_cancel_ack=False),
    'model_ack': dict(maker_quote_mode='model', requote_on_cancel_ack=True),
}


def sha(path):
    digest = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def write(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')


def options(seed):
    return dict(seed=seed, count=300, ecology=True, value_model='endogenous', value_fraction=.2,
                flow_mix='rebalanced', switching_fraction=.06, price_scale=10, fast_flow=True,
                periods=[10000, 30000, 60000, 300000])


def assert_isolated(before, after):
    assert before['scenario'] == after['scenario'], 'account/market economics changed'
    assert len(before['agents']) == len(after['agents'])
    for a, b in zip(before['agents'], after['agents']):
        if a['Plugin']['plugin_id'] != 'AdaptiveLiquidityMaker':
            assert a == b, a['Plugin']['participant']['participant_id']
        else:
            x = copy.deepcopy(b)
            for k in ('max_half_spread_ticks', 'refresh_ms', 'jitter_ms', 'requote_on_cancel_ack'):
                x['Plugin']['config'].pop(k, None)
                if k in a['Plugin']['config']:
                    x['Plugin']['config'][k] = a['Plugin']['config'][k]
            assert x == a


def ensure_runtime_inputs(out, rows):
    """A cached receipt also depends on interpreted sources and private runtimes."""
    package=ROOT/'bot-plugins/pine-strategy'
    paths={ROOT/p for p in ('Cargo.lock','exchange-core/examples/strategy_market.rs',
                           'exchange-server/examples/strategy_ecosystem_market.rs',
                           'exchange-core/examples/support/research_output.rs',
                           'exchange-core/examples/support/research_checkpoint.rs',
                           'scripts/research_receipts.py','scripts/research_resources.py')}
    for row in rows:
        recipe=json.loads(Path(row['recipe']).read_text(encoding='utf-8'))
        paths.update(package/'scripts'/a['Plugin']['config']['script'] for a in recipe['agents']
                     if a['Plugin']['plugin_id']=='pine.strategy')
    paths.update(p for p in package.glob('*') if p.is_file())
    paths.update(p for p in (package/'.runtime').rglob('*')
                 if p.is_file() and '__pycache__' not in p.parts)
    files=[dict(path=str(p.relative_to(ROOT)),bytes=p.stat().st_size,sha256=sha(p)) for p in sorted(paths)]
    target=out/'runtime-inputs.json'
    if target.exists():
        prior=json.loads(target.read_text(encoding='utf-8'))
        expected={p['path']:p['sha256'] for p in prior['files']}
        actual={p['path']:p['sha256'] for p in files}
        if actual!=expected:
            changed=sorted(k for k in actual.keys()|expected.keys() if actual.get(k)!=expected.get(k))
            raise RuntimeError('Runtime inputs changed; preserve this experiment and use a new directory: '+', '.join(changed))
    else:
        if any(out.glob('*.run.json')):
            raise RuntimeError('Existing runs lack a runtime input manifest; their dependency identity cannot be reconstructed retroactively')
        write(target,dict(recorded_at=datetime.now().astimezone().isoformat(),files=files,
                          scope='Frozen before execution; validated before cache reuse and after the batch.'))


def plan(out):
    out.mkdir(parents=True, exist_ok=True)
    if (out / 'plan.json').exists():
        raise SystemExit('Plan already frozen; choose a new output directory to change it.')
    runs = []
    for seed in (7, 19, 41, 73):
        baseline, _ = population(**options(seed))
        for profile, changes in PROFILES.items():
            recipe, roster = population(**options(seed), **changes)
            assert_isolated(baseline, recipe)
            name = f'native-s{seed}-{profile}'
            path = out / (name + '.recipe.json')
            write(path, recipe)
            write(out / (name + '.roster.json'), roster)
            runs.append(dict(name=name, suite='native', seed=seed, profile=profile, bots=300,
                             steps=43200, seconds=1, warmup_ms=7200000,
                             recipe=str(path), recipe_sha256=sha(path)))
    # Scale confirmation uses the exact previous 3300-bot allocation and scripts.
    # It is not a pure bot-count comparison against the 300-agent mechanism cohort.
    original = ROOT / '.local/tuning/s3000.json'
    frozen = json.loads(original.read_text(encoding='utf-8'))
    model, _ = population(**options(7), maker_quote_mode='model')
    makers = {a['Plugin']['participant']['participant_id']:a['Plugin']['config']
              for a in model['agents'] if a['Plugin']['plugin_id']=='AdaptiveLiquidityMaker'}
    for profile in PROFILES:
        recipe = copy.deepcopy(frozen)
        if profile != 'baseline':
            for agent in recipe['agents']:
                p = agent['Plugin']
                if p['plugin_id'] == 'AdaptiveLiquidityMaker':
                    source = makers[p['participant']['participant_id']]
                    for key in ('max_half_spread_ticks', 'refresh_ms', 'jitter_ms'):
                        p['config'][key] = source[key]
                    if profile == 'model_ack':
                        p['config']['requote_on_cancel_ack'] = True
        assert_isolated(frozen, recipe)
        name = f'scale-s7-{profile}'
        path = out / (name + '.recipe.json')
        write(path, recipe)
        runs.append(dict(name=name, suite='scale', seed=7, profile=profile, bots=3300,
                         steps=21600, seconds=1, warmup_ms=7200000,
                         recipe=str(path), recipe_sha256=sha(path)))
    write(out / 'plan.json', dict(
        purpose='Mechanism ablation, not visual tuning or production qualification',
        head=subprocess.check_output(['git','-c','safe.directory=E:/projects/MarketForge-tuning',
                                      'rev-parse','HEAD'], cwd=ROOT, text=True).strip(),
        profiles=PROFILES, source_recipe_sha256=sha(original), runs=runs,
        fixed=['accounts and finite assets','fees','strategy sources and input defaults','role allocation',
               'parent-order starts and targets','all non-maker configuration','one-second observation clock'],
        windows='Native: 12h, discard first 2h for primary statistics; scale: 6h, same 2h warmup. '
                'Also report 0-30min and rolling 2h windows; do not treat all Pine scripts as warmed.',
        evaluation=['1m/5m return variance, lag-1 return and absolute-return correlation, zero returns, tails',
                    'nonempty bar coverage, longest same-sign run, volume/absolute-return correlation',
                    'two-sided book availability, spread distribution, depth, largest outage',
                    'actual filled taker-order persistence, role quantity contributions and active accounts',
                    'finite-assets checks, rejection counts and actual strategy warmup coverage'],
        decision='Report per-seed tradeoffs. Do not promote a candidate solely on smoother candles or one '
                 'statistic; worse quote availability, large tails or much wider spreads are regressions. '
                 'No further parameter search in this experiment.',
        limits=['One-second timing is retained; no subsecond or queue realism claim.',
                'Historical reference is one archived BTCUSDT day, not multi-date validation.',
                'The scale cohort is a single frozen seed; 300-agent results do not certify scale.',
                'Price changes are generated by matching; no external price series is injected.']))


def run_one(out, row, binary, timeout):
    name = row['name']
    receipt, log, done = [out / (name + suffix) for suffix in ('.receipt.json','.log','.run.json')]
    fingerprint = dict(recipe_sha256=sha(Path(row['recipe'])), binary_sha256=sha(binary),
                       steps=row['steps'], seconds=row['seconds'])
    assert fingerprint['recipe_sha256'] == row['recipe_sha256']
    if done.exists():
        prior=json.loads(done.read_text(encoding='utf-8'))
        if prior.get('complete') and prior['inputs']==fingerprint and sha(receipt)==prior['receipt_sha256']:
            return prior
        raise RuntimeError(f'{name}: existing incomplete or changed run; preserve it and use a new directory')
    env=dict(os.environ, PYTHONUTF8='1', RESEARCH_PLUGIN_WAKES='events',
             MARKETFORGE_PINE_IN_PROCESS='1', MARKETFORGE_PINE_REPLAY_STATE='1',
             MARKETFORGE_BOT_WORKERS='8', RAYON_NUM_THREADS='8')
    env['PATH']=str(ROOT.parent/'MarketForge/.venv/Scripts')+os.pathsep+env['PATH']
    for key in ('MF_RESEARCH_CHECKPOINT','MF_RESEARCH_STOP_FILE','MF_RESEARCH_DEADLINE_UNIX',
                'RESEARCH_PLUGIN_POLL_MS','RESEARCH_AUDIT','RESEARCH_PROFILE','RESEARCH_SCRIPT_PROFILE'):
        env.pop(key,None)
    start=time.monotonic()
    result=dict(name=name,inputs=fingerprint,complete=False)
    print(f'START {name}',flush=True)
    try:
        with receipt.open('w',encoding='utf-8') as stdout,log.open('w',encoding='utf-8') as stderr:
            p=subprocess.run([str(binary),row['recipe'],str(row['steps']),str(row['seconds'])],
                             cwd=ROOT,env=env,stdout=stdout,stderr=stderr,timeout=timeout)
        result['exit_code']=p.returncode
        if p.returncode==0:
            data=json.loads(receipt.read_text(encoding='utf-8'))
            result.update(complete=data['steps']==row['steps'],receipt_sha256=sha(receipt),
                          trades=data['trades'],simulation_time_ms=data['simulation_time_ms'])
    except subprocess.TimeoutExpired:
        result['failure']='timeout; partial output and log retained'
    result['wall_seconds']=time.monotonic()-start
    write(done,result)
    print(json.dumps(result,ensure_ascii=True),flush=True)
    return result


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('action',choices=['plan','run'])
    p.add_argument('--output',type=Path,default=DEFAULT_OUT)
    p.add_argument('--binary',type=Path)
    p.add_argument('--cases',nargs='+')
    p.add_argument('--workers',type=int,default=1,choices=[1,2])
    p.add_argument('--timeout',type=int,default=7200)
    a=p.parse_args();out=a.output.resolve()
    if a.action=='plan':plan(out);return
    rows=json.loads((out/'plan.json').read_text(encoding='utf-8'))['runs']
    all_rows=rows
    ensure_runtime_inputs(out,all_rows)
    if a.cases:rows=[next(x for x in rows if x['name']==n) for n in a.cases]
    binary=(a.binary or out/'candidate.exe').resolve()
    with ThreadPoolExecutor(max_workers=a.workers) as pool:
        pending=[pool.submit(run_one,out,x,binary,a.timeout) for x in rows]
        results=[x.result() for x in as_completed(pending)]
    ensure_runtime_inputs(out,all_rows)
    if not all(x['complete'] for x in results):raise SystemExit(1)


if __name__=='__main__':main()
