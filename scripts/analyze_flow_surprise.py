"""Joint actual OHLCV evaluation, retaining every seed and market regression."""
import argparse
import csv
import json
from pathlib import Path
import numpy as np
from research_market_mechanisms import sha, write
from analyze_wick_full import enrich
from analyze_basis_execution import candles
from analyze_market_mechanisms import rebar


def chart(out, seed, pair):
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    from matplotlib.patches import Rectangle
    fig, axes = plt.subplots(4, 2, figsize=(16, 10), sharex=True,
                            gridspec_kw={'height_ratios': [3, 1, 3, 1]})
    for leg, market in enumerate(('V-BTC-SPOT', 'V-BTC-PERP')):
        bounds, vmax = [], 0
        for col, profile in enumerate(('raw-flow', 'surprise')):
            row = pair[profile]
            bars = rebar(candles(out / (row['name'] + '-' + market + '.candles.csv')), 5)
            for k, b in sorted(bars.items()):
                if k*300000 < row['warmup_ms'] or (k+1)*300000 > row['steps']*1000:
                    continue
                x = k/12
                op, hi, lo, cl = [(b[z]/100000-1)*100 for z in ('open', 'high', 'low', 'close')]
                color = '#245b80' if cl >= op else '#ac6529'
                axes[2*leg, col].vlines(x, lo, hi, color=color, lw=.7)
                axes[2*leg, col].add_patch(Rectangle((x-.0285, min(op, cl)), .057,
                    max(abs(cl-op), .00001), facecolor='white' if cl >= op else color,
                    edgecolor=color, lw=.7))
                axes[2*leg+1, col].bar(x, b['volume'], width=.07, color=color, alpha=.7)
                bounds.extend((lo, hi)); vmax = max(vmax, b['volume'])
            axes[2*leg, col].set_title(market.replace('V-BTC-', '') + ' | ' + profile)
            axes[2*leg, col].grid(axis='y', alpha=.2)
            axes[2*leg+1, col].set_xlabel('Simulation hour')
        pad = max(.005, (max(bounds)-min(bounds))*.06)
        for ax in axes[2*leg]: ax.set_ylim(min(bounds)-pad, max(bounds)+pad)
        for ax in axes[2*leg+1]: ax.set_ylim(0, vmax*1.08)
        axes[2*leg, 0].set_ylabel('Change from initial price (%)')
        axes[2*leg+1, 0].set_ylabel('Actual lots / 5min')
    fig.suptitle(f'Actual 5min OHLCV | fixed seed {seed} | full primary hours2–4', fontsize=15)
    fig.text(.5, .015, 'Common scales per market; actual trade OHLCV without smoothing, filtering or clipping.\nOnly flow_surprise differs: identical numerical recipe, finite funds, fees, hedge capacity and execution lifecycle.', ha='center')
    fig.tight_layout(rect=[0, .055, 1, .95])
    fig.savefig(out / f'flow-surprise-seed{seed}-5m.png', dpi=145)
    plt.close(fig)


def analyze(out):
    assert np.__version__ == '2.5.3'
    plan = json.loads((out/'plan.json').read_text(encoding='utf-8'))
    rows, links, metrics, complete = [], [], [], []
    for row in plan['runs']:
        path = out / (row['name'] + '.run.json')
        if not path.exists(): continue
        done = json.loads(path.read_text(encoding='utf-8'))
        assert done['complete']
        assert done['inputs']['binary_sha256'] == plan['binary_sha256'] == sha(out/'candidate.exe')
        m, rs, ls = enrich(out, row)
        rows.extend(rs); links.append(ls); metrics.append(m); complete.append(row)
    pairs = 0
    for seed in sorted({r['seed'] for r in plan['runs']}):
        pair = {r['profile']: r for r in complete if r['seed'] == seed}
        if set(pair) == {'raw-flow', 'surprise'}:
            pairs += 1
            chart(out, seed, pair)
    write(out/'comparison.json', dict(rows=rows, market_links=links, metrics=metrics,
        completed_runs=len(complete), completed_pairs=pairs, complete=len(complete)==len(plan['runs']),
        primary_window_ms=[7200000,14400000], plan_sha256=sha(out/'plan.json'),
        analyzer_sha256=sha(Path(__file__)), numpy_version=np.__version__,
        scope='Each profile is an actual complete4h run, primary2–4h. Raw-flow control is the previous index-anchor/supply-curve experimental candidate, not the older dual-execution profile. Numerical recipe inputs unchanged. Previously examined seeds41,7,19,73 remain exploratory;211 is an additional short holdout, not long-duration or broad realism qualification.'))
    for name, records in (('comparison.csv', rows), ('market-links.csv', links)):
        if records:
            with (out/name).open('w', encoding='utf-8', newline='') as f:
                writer=csv.DictWriter(f, fieldnames=list(records[0])); writer.writeheader(); writer.writerows(records)
    print(json.dumps(dict(completed_runs=len(complete), completed_pairs=pairs, rows=rows, links=links), indent=2))


if __name__ == '__main__':
    parser=argparse.ArgumentParser(description=__doc__); parser.add_argument('directory', type=Path)
    analyze(parser.parse_args().directory)
