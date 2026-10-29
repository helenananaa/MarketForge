"""Fixed four-hour mechanism comparison; never retune numerical coefficients."""
import argparse
import copy
import json
import shutil
import subprocess
from datetime import datetime
from pathlib import Path
from research_market_mechanisms import ROOT, sha, write, ensure_runtime_inputs, options
from strategy_ecosystem import population

BASE = ROOT / '.local/strategy-research-flow-surprise-20261025'
PRIOR = ROOT / '.local/strategy-research-price-formation-20261025/short-candidate'
SEEDS = (41, 7, 19, 73)


def recipe_for(control, surprise):
    result = copy.deepcopy(control)
    if surprise:
        for agent in result['agents']:
            p = agent['Plugin']
            if p['plugin_id'] == 'AdaptiveLiquidityMaker':
                assert p['config']['book_slope_flow'] and p['config']['persistent_index_anchor']
                p['config']['flow_surprise'] = True
    reverse = copy.deepcopy(result)
    if surprise:
        for agent in reverse['agents']:
            if agent['Plugin']['plugin_id'] == 'AdaptiveLiquidityMaker':
                assert agent['Plugin']['config'].pop('flow_surprise') is True
    assert reverse == control
    return result


def copy_binary(out, source=None):
    target = out / 'candidate.exe'
    if source:
        shutil.copyfile(source, target)
    else:
        native = '/home/helenanana/.cache/marketforge-flow-surprise-20261025/release/examples/strategy_ecosystem_market'
        unix = '/mnt/e/' + target.relative_to('E:/').as_posix()
        subprocess.run(['wsl', '--exec', 'cp', native, unix], check=True)
    unix = '/mnt/e/' + target.relative_to('E:/').as_posix()
    subprocess.run(['wsl', '--exec', 'chmod', '755', unix], check=True)


def plan(out, rows, source_binary=None):
    out.mkdir(exist_ok=False)
    copy_binary(out, source_binary)
    prepared = []
    for seed, label, recipe in rows:
        name = f'native-s{seed}-{label}'
        path = out / (name + '.recipe.json')
        write(path, recipe)
        prepared.append(dict(name=name, seed=seed, profile=label, bots=300, steps=14400,
                             seconds=1, warmup_ms=7200000, recipe=str(path), recipe_sha256=sha(path)))
    write(out / 'plan.json', dict(frozen_at=datetime.now().astimezone().isoformat(), runs=prepared,
        binary_sha256=sha(out / 'candidate.exe'), minimum_linux_available_gib=16,
        minimum_host_commit_gib=8,
        task_cgroup=dict(MemoryHigh='4G', MemoryMax='8G', MemorySwapMax='8G', User='helenanana'),
        scope='300 native bots,4h,primary[2h,4h),fixed1s clock. Only flow_surprise boolean differs within each pair; index anchor and supply-curve response remain on. All numerical economic/strategy/fee/fund/hedge/TTL inputs fixed. Global caches/WSL settings and other project processes untouched. Task-owned transient units and3GiB guards only. Complete raw receipt and execution archive hashes retained.'))
    ensure_runtime_inputs(out, prepared)
    return prepared


def compatibility():
    control = json.loads((PRIOR / 'native-s41-anchor-slope.recipe.json').read_text(encoding='utf-8'))
    plan(BASE / 'current-control', [(41, 'raw-flow', control)])


def freeze_candidate():
    out = BASE / 'candidate-design'
    designs = []
    for seed in SEEDS:
        control = json.loads((PRIOR / f'native-s{seed}-anchor-slope.recipe.json').read_text(encoding='utf-8'))
        designs += [(seed, 'raw-flow', control), (seed, 'surprise', recipe_for(control, True))]
    control, _ = population(**options(211), maker_quote_mode='model', requote_on_cancel_ack=True,
        basis_execution='touch-hedge', reservation_innovations=True, market_depth_flow=True,
        reactive_execution='sliced', style_execution='touch', persistent_index_anchor=True,
        book_slope_flow=True)
    designs += [(211, 'raw-flow', control), (211, 'surprise', recipe_for(control, True))]
    rows = plan(out, designs)
    files = []
    for source in (ROOT / 'exchange-core/src/adaptive_liquidity.rs', ROOT / 'scripts/strategy_ecosystem.py'):
        relative = source.relative_to(ROOT)
        target = out / 'model-source' / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        files.append(dict(path=relative.as_posix(), sha256=sha(source)))
    write(out / 'model-inputs.json', dict(files=files, binary_sha256=sha(out / 'candidate.exe'),
        mechanism='Forecast expected signed flow from the pre-observation decayed public-flow memory using the existing half-life: expected=(1-decay)*old_flow_signed. Accumulate it over the quote interval and use observed signed flow minus expected in the unchanged inverse visible supply curve. Keep raw-flow toxicity/widening, numerical coefficients, passive execution and finite budgets unchanged. Forecasts use only prior public observations; no full-tape/private/future information. This is a testable model hypothesis, not a market-efficiency certificate.'))
    for label, selected in [('pilot', [r for r in rows if r['seed'] == 41]),
                            ('main', [r for r in rows if r['seed'] in SEEDS]),
                            ('holdout', [r for r in rows if r['seed'] == 211])]:
        folder = BASE / label
        folder.mkdir(exist_ok=False)
        shutil.copyfile(out / 'candidate.exe', folder / 'candidate.exe')
        fresh = []
        for row in selected:
            path = folder / Path(row['recipe']).name
            shutil.copyfile(row['recipe'], path)
            fresh.append({**row, 'recipe': str(path)})
        p = json.loads((out / 'plan.json').read_text(encoding='utf-8'))
        p.update(runs=fresh, source_design_sha256=sha(out / 'plan.json'))
        write(folder / 'plan.json', p)
        ensure_runtime_inputs(folder, fresh)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['compatibility', 'freeze-candidate'])
    args = parser.parse_args()
    compatibility() if args.action == 'compatibility' else freeze_candidate()
