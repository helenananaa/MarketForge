"""Reproduce an archived anomaly with a separate, non-mutating decision trace."""
from __future__ import annotations
import argparse, json, os, subprocess, time
from datetime import datetime
from pathlib import Path
from analyze_market_mechanisms import ROOT, sha, write


def run(out, name, binary, recipe, steps, windows=None):
    done = out / (name + '.run.json')
    assert not done.exists(), 'Preserve old evidence; choose a fresh run name'
    env = dict(os.environ, PYTHONUTF8='1', RESEARCH_PLUGIN_WAKES='events', RAYON_NUM_THREADS='8', MARKETFORGE_BOT_WORKERS='8')
    for key in tuple(env):
        if key.startswith('MF_RESEARCH_') or key.startswith('RESEARCH_ORDER_'):
            del env[key]
    if windows:
        env.update(MF_RESEARCH_TRACE_PATH=str(out / (name + '.trace.jsonl')), MF_RESEARCH_TRACE_WINDOWS=windows)
    before = dict(recorded_at=datetime.now().astimezone().isoformat(), recipe=str(recipe), recipe_sha256=sha(recipe), binary_sha256=sha(binary), steps=steps, seconds=1, windows=windows)
    write(out / (name + '.inputs.json'), before)
    start = time.monotonic()
    with (out / (name + '.receipt.json')).open('w', encoding='utf-8') as receipt, (out / (name + '.log')).open('w', encoding='utf-8') as log:
        result = subprocess.run([str(binary), str(recipe), str(steps), '1'], cwd=ROOT, env=env, stdout=receipt, stderr=log, timeout=7200)
    value = dict(inputs=before, exit_code=result.returncode, wall_seconds=time.monotonic()-start, complete=result.returncode==0)
    if result.returncode == 0:
        data = json.loads((out / (name + '.receipt.json')).read_text(encoding='utf-8'))
        assert data['steps'] == steps and data['simulation_time_ms'] == steps * 1000
        value.update(receipt_sha256=sha(out / (name + '.receipt.json')), trades=data['trades'])
        if windows:value['trace_sha256'] = sha(out / (name + '.trace.jsonl'))
    write(done, value); print(json.dumps(dict(name=name, **value)), flush=True)
    assert value['complete']
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__); parser.add_argument('--output', type=Path, default=ROOT / '.local/strategy-research-causal-20261025')
    args=parser.parse_args(); out=args.output.resolve(); binary=out/'trace.exe'
    original=ROOT/'.local/strategy-research-reservation-20261024'
    control=original/'native-s41-control.recipe.json'
    off=run(out,'smoke-off',binary,control,600)
    on=run(out,'smoke-on',binary,control,600,'450000:600001')
    assert off['receipt_sha256']==on['receipt_sha256']=='d5c7df4c784110888ca208bf7cca26a4bb0f08be32db225a9f6795973da45c16'
    write(out/'trace-compatibility.json',dict(smoke_byte_identical=True, prior_600_step_control_sha256=off['receipt_sha256']))
    selection=json.loads((out/'selection.json').read_text(encoding='utf-8'))
    row=run(out,'trace-s41-reservation',binary,original/'native-s41-reservation.recipe.json',selection['steps'],f"{selection['start_ms']}:{selection['end_ms']}")
    del row
    traced=json.loads((out/'trace-s41-reservation.receipt.json').read_text(encoding='utf-8'))
    prior=json.loads((original/'native-s41-reservation.receipt.json').read_text(encoding='utf-8'))
    end=traced['simulation_time_ms']; checks={}
    for key in ('trade_receipts','book_snapshots'):
        clock='time_ms'; prefix=[r for r in prior[key] if r[clock]<=end]
        assert prefix==traced[key],key
        checks[key]=dict(equal=True, rows=len(prefix))
    assert traced['assets_before']==prior['assets_before']
    write(out/'prefix-verification.json',dict(prior_receipt_sha256=sha(original/'native-s41-reservation.receipt.json'), traced_receipt_sha256=sha(out/'trace-s41-reservation.receipt.json'), end_ms=end, checks=checks, scope='All archived trades and after-wave book samples through this prefix match exactly. Short on/off smoke also matches all final bot states/assets. This is a diagnostic reproduction, not a new market candidate.'))
    print('ARCHIVED PREFIX MATCHES; exact anomaly trace ready',flush=True)


if __name__=='__main__':main()
