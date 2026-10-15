"""Stylized-fact acceptance checks computed from actual matching receipts.

Reads `trade_receipts` from a research receipt; never fills empty intervals
with synthetic trades. Prices on a fixed grid carry the last actual trade
forward, and the fraction of grid points without a trade is reported.

Targets are the commonly cited empirical regularities of liquid markets
(Cont 2001; Bouchaud et al. 2004/2018). They are acceptance targets for a
synthetic market, not a claim of equivalence to any real venue.
"""
from __future__ import annotations

import argparse
import collections
import json
import math
from pathlib import Path

import numpy as np


def autocorr(x, lag):
    x = np.asarray(x, float)
    if len(x) <= lag + 2 or x.std() == 0:
        return None
    a, b = x[:-lag] - x.mean(), x[lag:] - x.mean()
    return float((a * b).mean() / x.var())


def hill_tail_index(r, tail=0.05):
    a = np.sort(np.abs(np.asarray(r, float)))[::-1]
    a = a[a > 0]
    k = max(10, int(len(a) * tail))
    if len(a) <= k + 1:
        return None
    mean_log = np.mean(np.log(a[:k] / a[k]))
    return float(1 / mean_log) if mean_log > 0 else None  # tied tick-sized tails


def power_law_decay(lags, values):
    """Least-squares exponent gamma for C(l) ~ l^-gamma on positive values."""
    pts = [(math.log(l), math.log(v)) for l, v in zip(lags, values) if v is not None and v > 0]
    if len(pts) < 4:
        return None
    x, y = np.array(pts).T
    return float(-np.polyfit(x, y, 1)[0])


def grid_prices(trades, start_ms, end_ms, interval_ms):
    """Last actual trade price per interval, carried forward; None before the first trade."""
    buckets = collections.OrderedDict()
    volume = collections.Counter()
    for t in trades:
        b = t['time_ms'] // interval_ms
        buckets[b] = t['price_tick']
        volume[b] += t['qty']
    prices, vols, empty, last = [], [], 0, None
    first = -(-start_ms // interval_ms)
    for b in range(first, end_ms // interval_ms):
        if b in buckets:
            last = buckets[b]
        else:
            empty += 1
        if last is not None:
            prices.append(last)
            vols.append(volume[b])
    return np.array(prices, float), np.array(vols, float), empty / max(1, end_ms // interval_ms - first)


def longest_run(r):
    best = cur = 0
    prev = 0
    for x in r:
        s = int(np.sign(x))
        cur = cur + 1 if s != 0 and s == prev else (1 if s != 0 else 0)
        prev = s
        best = max(best, cur)
    return best


def analyze(trades, end_ms, skip_ms=0, roster=None):
    trades = [t for t in trades if t['time_ms'] >= skip_ms]
    out = {'trades': len(trades)}
    # Returns at three horizons.
    for name, interval in (('10s', 10_000), ('1m', 60_000), ('5m', 300_000)):
        p, v, empty = grid_prices(trades, skip_ms, end_ms, interval)
        r = np.diff(np.log(p)) if len(p) > 1 else np.array([])
        row = {'samples': int(len(r)), 'no_trade_interval_fraction': round(empty, 4)}
        if len(r) > 20:
            row.update({
                'zero_return_fraction': float(np.mean(r == 0)),
                'std_bps': float(r.std() * 1e4),
                'ret_acf_lag1': autocorr(r, 1),
                'ret_acf_lag2': autocorr(r, 2),
                'excess_kurtosis': float(((r - r.mean()) ** 4).mean() / r.var() ** 2 - 3) if r.var() > 0 else None,
                'hill_tail_index': hill_tail_index(r),
                'abs_ret_acf': {str(l): autocorr(np.abs(r), l) for l in (1, 5, 10, 20, 50)},
                'volume_absret_corr': float(np.corrcoef(v[1:], np.abs(r))[0, 1]) if v[1:].std() > 0 and np.abs(r).std() > 0 else None,
                'longest_same_direction_run': longest_run(r),
            })
        out[name] = row
    # Order-sign memory, one sign per actual filled taker order.
    signs, last = [], None
    for t in trades:
        if t['taker_order_id'] != last:
            signs.append(1 if t['taker_side'] == 'Buy' else -1)
            last = t['taker_order_id']
    lags = [1, 2, 5, 10, 20, 50, 100]
    acf = {l: autocorr(signs, l) for l in lags}
    out['order_sign'] = {'taker_orders': len(signs), 'acf': {str(k): v for k, v in acf.items()},
                         'power_law_gamma': power_law_decay(lags, [acf[l] for l in lags])}
    if roster:
        role = {p['account_id']: p['role'] for p in roster['participants']}
        qty = collections.Counter()
        for t in trades:
            qty[role.get(t.get('taker_account_id'), 'other')] += t['qty']
        total = sum(qty.values()) or 1
        out['taker_volume_share_by_role'] = {k: round(v / total, 4) for k, v in qty.most_common()}
    return out


# (metric path, predicate, description). Evaluated on 1-minute returns unless named.
TARGETS = [
    (('1m', 'ret_acf_lag1'), lambda x: x is not None and abs(x) < 0.1, '|ACF(1)| of 1m returns < 0.1 (no linear predictability)'),
    (('1m', 'excess_kurtosis'), lambda x: x is not None and x > 3, 'excess kurtosis of 1m returns > 3 (fat tails)'),
    (('1m', 'hill_tail_index'), lambda x: x is not None and 2 <= x <= 5, 'Hill tail index of 1m returns in [2, 5]'),
    (('1m', 'abs_ret_acf', '10'), lambda x: x is not None and x > 0.05, 'ACF(10) of |1m returns| > 0.05 (volatility clustering)'),
    (('1m', 'abs_ret_acf', '50'), lambda x: x is not None and x > 0, 'ACF(50) of |1m returns| > 0 (slow decay)'),
    (('1m', 'zero_return_fraction'), lambda x: x is not None and x < 0.2, 'zero 1m returns < 20%'),
    (('1m', 'volume_absret_corr'), lambda x: x is not None and x > 0.2, 'corr(volume, |return|) > 0.2'),
    (('order_sign', 'acf', '10'), lambda x: x is not None and x > 0.05, 'order-sign ACF(10) > 0.05 (persistent flow)'),
    (('order_sign', 'power_law_gamma'), lambda x: x is not None and 0.2 <= x <= 0.9, 'order-sign ACF power-law gamma in [0.2, 0.9]'),
]


def evaluate(metrics):
    rows = []
    for path, ok, text in TARGETS:
        value = metrics
        for key in path:
            value = value.get(key) if isinstance(value, dict) else None
        rows.append({'target': text, 'value': value, 'pass': bool(ok(value))})
    return rows


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('receipt', type=Path)
    p.add_argument('--roster', type=Path)
    p.add_argument('--skip-minutes', type=float, default=0, help='exclude warmup from the start of the run')
    p.add_argument('--output', type=Path)
    a = p.parse_args()
    receipt = json.loads(a.receipt.read_text(encoding='utf-8'))
    roster = json.loads(a.roster.read_text(encoding='utf-8')) if a.roster else None
    end = receipt['simulation_time_ms']
    skip = int(a.skip_minutes * 60_000)
    report = {'receipt': str(a.receipt), 'simulation_time_ms': end, 'skip_ms': skip, 'instruments': {}}
    by_leg = collections.defaultdict(list)
    for t in receipt['trade_receipts']:
        by_leg[t['instrument_id']].append(t)
    for instrument, trades in sorted(by_leg.items()):
        metrics = analyze(trades, end, skip, roster)
        checks = evaluate(metrics)
        report['instruments'][instrument] = {'metrics': metrics, 'checks': checks,
                                             'passed': sum(c['pass'] for c in checks), 'total': len(checks)}
        print(f'\n{instrument}: {sum(c["pass"] for c in checks)}/{len(checks)} targets, {metrics["trades"]} trades')
        for c in checks:
            v = c['value']
            shown = f'{v:.4f}' if isinstance(v, float) else str(v)
            print(f'  [{"PASS" if c["pass"] else "FAIL"}] {shown:>10}  {c["target"]}')
        if 'taker_volume_share_by_role' in metrics:
            print('  taker volume share:', metrics['taker_volume_share_by_role'])
    legs = sorted(by_leg)
    if len(legs) == 2:
        # Spot/perpetual link: last actual prices on a 1-minute grid.
        (sp, _, _), (pp, _, _) = (grid_prices(by_leg[l], skip, end, 60_000) for l in legs)
        k = min(len(sp), len(pp))
        gap = np.abs(pp[-k:] / sp[-k:] - 1) * 100 if k else np.array([])
        link = {'minutes': int(k), 'median_abs_gap_pct': float(np.median(gap)) if k else None,
                'p99_abs_gap_pct': float(np.percentile(gap, 99)) if k else None,
                'max_abs_gap_pct': float(gap.max()) if k else None}
        link['checks'] = [
            {'target': 'median |perp/spot - 1| < 0.3%', 'value': link['median_abs_gap_pct'],
             'pass': link['median_abs_gap_pct'] is not None and link['median_abs_gap_pct'] < 0.3},
            {'target': 'p99 |perp/spot - 1| < 1.5%', 'value': link['p99_abs_gap_pct'],
             'pass': link['p99_abs_gap_pct'] is not None and link['p99_abs_gap_pct'] < 1.5},
        ]
        report['spot_perp_link'] = link
        print(f'{chr(10)}spot/perp link over {k} minutes (max gap {link["max_abs_gap_pct"]:.2f}%)' if k else f'{chr(10)}spot/perp link: no data')
        for c in link['checks']:
            v = c['value']
            print(f'  [{"PASS" if c["pass"] else "FAIL"}] {str(v) if v is None else format(v, ".4f"):>10}  {c["target"]}')
    if a.output:
        a.output.parent.mkdir(parents=True, exist_ok=True)
        a.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')


if __name__ == '__main__':
    main()
