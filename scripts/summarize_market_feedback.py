"""Join both frozen ablations with the original default and calendar references."""
from __future__ import annotations
import argparse, csv, json, math
from pathlib import Path
import numpy as np
from analyze_market_mechanisms import ROOT, corr, rebar, sha, write
from analyze_basis_execution import candles, morphology

MARKETS = ('V-BTC-SPOT', 'V-BTC-PERP')
SEEDS = (7, 19, 41, 73)
PROFILES = ('baseline', 'model_ack', 'executable', 'reservation')
LABELS = ('Original touch', 'Model + cancel ack', 'Executable basis', 'Reservation changes')


def market_link(spot, perp, start, end):
    keys = sorted(k for k in spot.keys() & perp.keys() if k * 60000 >= start and (k + 1) * 60000 <= end)
    aligned = set(keys)
    basis = [(perp[k]['close'] / spot[k]['close'] - 1) * 10000 for k in keys]
    rs = [math.log(spot[k]['close'] / spot[k - 1]['close']) for k in keys if k - 1 in aligned]
    rp = [math.log(perp[k]['close'] / perp[k - 1]['close']) for k in keys if k - 1 in aligned]
    return dict(aligned_minutes=len(keys), return_correlation=corr(rs, rp),
                absolute_close_basis_median_bp=float(np.median(np.abs(basis))),
                absolute_close_basis_p99_bp=float(np.percentile(np.abs(basis), 99)),
                absolute_close_basis_max_bp=float(max(map(abs, basis))))


def summarize(out):
    source = ROOT / '.local/strategy-research-mechanisms-20261024'
    basis = ROOT / '.local/strategy-research-basis-20261024'
    reservation = ROOT / '.local/strategy-research-reservation-20261024'
    joined = []; links = []
    for directory in (basis, reservation):
        comparison = json.loads((directory / 'basis-comparison.json').read_text(encoding='utf-8'))
        assert comparison['complete'] and len(comparison['pairs']) == 4
        assert comparison['compatibility']['byte_identical']
    for profile in PROFILES:
        directory = source if profile in PROFILES[:2] else basis if profile == 'executable' else reservation
        for seed in SEEDS:
            name = f'native-s{seed}-{profile}'
            path = directory / (name + '.metrics.json')
            metric = json.loads(path.read_text(encoding='utf-8'))
            assert metric['seed'] == seed and metric['warmup_ms'] == 7200000 and metric['steps'] == 43200
            bars_by_market = {}
            for market in MARKETS:
                m = metric['markets'][market]
                candle_path = directory / (name + '-' + market + '.candles.csv')
                bars = candles(candle_path)
                bars_by_market[market] = bars
                shape = morphology(rebar(bars, 5), 7200000, 43200000, 300000)
                joined.append(dict(profile=profile, seed=seed, market=market,
                    metrics_sha256=sha(path), candle_sha256=sha(candle_path),
                    **{k: m['minute'][k] for k in ('return_std_bp', 'return_acf1', 'abs_return_acf1', 'abs_return_p99_bp', 'abs_return_max_bp', 'volume_cv', 'volume_abs_return_corr')},
                    two_sided_pct=m['two_sided_fraction'] * 100,
                    spread_median_bp=m['spread_median_bp'],
                    basis_taker_qty_share=m['taker_share_by_type'].get('BasisLinkTrader', 0),
                    reactive_taker_qty_share=m['taker_share_by_type'].get('ReactiveFlowTrader', 0),
                    wick_5m=shape['median_wick_fraction'], range_5m_bp=shape['median_range_bp'],
                    range_5m_p99_bp=shape['p99_range_bp']))
            links.append(dict(profile=profile, seed=seed, **market_link(bars_by_market[MARKETS[0]], bars_by_market[MARKETS[1]], 7200000, 43200000)))
    reference_path = reservation / 'reference-calendar/calendar-reference.json'
    reference = json.loads(reference_path.read_text(encoding='utf-8'))
    refs = []; reference_links = []
    for record in reference['records']:
        bars = {b['bucket']: b for b in record['candles']}
        for window in record['ten_hour_windows']:
            h = window['start_hour']
            shape = morphology(rebar(bars, 5), h * 3600000, (h + 10) * 3600000, 300000)
            refs.append(dict(market='V-BTC-' + record['leg'].upper(), date=record['date'], start_hour=h,
                **{k: window[k] for k in ('return_std_bp', 'return_acf1', 'abs_return_acf1', 'abs_return_p99_bp', 'abs_return_max_bp', 'volume_cv', 'volume_abs_return_corr')},
                wick_5m=shape['median_wick_fraction'], range_5m_bp=shape['median_range_bp'], range_5m_p99_bp=shape['p99_range_bp']))
    for date in sorted({r['date'] for r in reference['records']}):
        markets = {r['leg']: {b['bucket']: b for b in r['candles']} for r in reference['records'] if r['date'] == date}
        for h in (0, 12):
            reference_links.append(dict(date=date, start_hour=h, **market_link(markets['spot'], markets['perp'], h * 3600000, (h + 10) * 3600000)))
    aggregate = []
    excluded = {'profile', 'seed', 'market', 'metrics_sha256', 'candle_sha256'}
    for market in MARKETS:
        for profile in PROFILES:
            rows = [r for r in joined if r['market'] == market and r['profile'] == profile]
            assert len(rows) == 4
            aggregate.append(dict(market=market, profile=profile, seeds=SEEDS,
                mean={k: float(np.mean([r[k] for r in rows])) for k in rows[0] if k not in excluded},
                minimum={k: float(min(r[k] for r in rows)) for k in rows[0] if k not in excluded},
                maximum={k: float(max(r[k] for r in rows)) for k in rows[0] if k not in excluded}))
    result = dict(rows=joined, reference_windows=refs, aggregate=aggregate,
        market_links=links, reference_market_links=reference_links,
        market_link_scope='Post hoc cross-market diagnostic added after the seed-7 basis result. Aligns actual traded minute closes, not simultaneous executable quotes; it cannot measure a guaranteed arbitrage profit or causal price transmission.',
        reference_sha256=sha(reference_path),
        scope='All four previously examined seeds; no holdout or confidence interval. Each simulation statistic uses hours 2–12. References: eight disjoint ten-hour windows per market from four calendar-selected dates. Aggregate values are arithmetic means of per-run statistics, not pooled returns. Basis and reservation are independent ablations against model_ack, not a combined policy. Original touch is contextual, not the paired control.')
    out.mkdir(parents=True, exist_ok=True)
    write(out / 'feedback-summary.json', result)
    with (out / 'feedback-summary.csv').open('w', newline='', encoding='utf-8') as f:
        writer = csv.DictWriter(f, fieldnames=list(joined[0])); writer.writeheader(); writer.writerows(joined)
    return result


def charts(out, result):
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    plt.rcParams.update({'font.family': 'DejaVu Sans', 'font.size': 9, 'axes.spines.top': False, 'axes.spines.right': False})
    columns = [('return_std_bp', '1m return standard deviation (bp)'),
        ('return_acf1', '1m return lag-1 correlation'),
        ('abs_return_p99_bp', '1m absolute return P99 (bp)'),
        ('wick_5m', '5m median wick fraction'),
        ('two_sided_pct', 'Two-sided quote availability (%)')]
    fig, axes = plt.subplots(2, 5, figsize=(19, 8))
    colors = ('#6d7379', '#b46b25', '#1f4e79', '#5b7e40')
    for row, market in enumerate(MARKETS):
        for col, (key, title) in enumerate(columns):
            ax = axes[row, col]
            for x, (profile, color) in enumerate(zip(PROFILES, colors)):
                vals = [r[key] for r in result['rows'] if r['market'] == market and r['profile'] == profile]
                ax.scatter(np.arange(4) * .08 + x - .12, vals, color=color, s=30)
                ax.plot([x - .2, x + .2], [np.mean(vals)] * 2, color=color, linewidth=2)
            if key != 'two_sided_pct':
                ref = [r[key] for r in result['reference_windows'] if r['market'] == market]
                ax.axhspan(min(ref), max(ref), color='#777', alpha=.12)
            ax.set_xticks(range(4), ['Touch', 'Model+ack', 'Basis', 'Reservation'], rotation=25, ha='right')
            ax.set_ylabel(market.replace('V-BTC-', ''))
            ax.grid(axis='y', alpha=.18)
            if row == 0: ax.set_title(title)
            if key == 'return_acf1': ax.set_ylim(-1, 1)
            elif key == 'wick_5m': ax.set_ylim(0, 1)
    for col in (0, 2, 4):
        lo = min(ax.get_ylim()[0] for ax in axes[:, col]); hi = max(ax.get_ylim()[1] for ax in axes[:, col])
        for ax in axes[:, col]: ax.set_ylim(lo, hi)
    fig.suptitle('Independent mechanism ablations | all four fixed seeds', fontsize=16)
    fig.text(.5, .025, 'Dots: seeds 7, 19, 41, 73 (left to right); line: per-run mean. Gray: eight historical 10h windows, descriptive only.\nTouch is the original default; Model+ack is the paired control. Basis and Reservation are separate changes. No historical order-book reference.', ha='center', fontsize=10)
    fig.tight_layout(rect=[0, .08, 1, .94]); fig.savefig(out / 'feedback-all-seeds.png', dpi=160); plt.close(fig)


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, default=ROOT / '.local/strategy-research-reservation-20261024/summary')
    args = p.parse_args(); result = summarize(args.output.resolve()); charts(args.output.resolve(), result)
