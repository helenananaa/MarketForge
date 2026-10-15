"""Plot actual matching receipts and sampled books; never synthesize candles."""
from __future__ import annotations

import argparse
import collections
import csv
import hashlib
import json
from pathlib import Path

import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
import numpy as np
from matplotlib.font_manager import FontProperties
from matplotlib.patches import Rectangle

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / '.local/strategy-ecosystem-20261014/charts'
FONT = Path('C:/Windows/Fonts/msyh.ttc')
if FONT.exists():
    matplotlib.font_manager.fontManager.addfont(str(FONT))
    plt.rcParams['font.family'] = FontProperties(fname=str(FONT)).get_name()
plt.rcParams.update({'font.size': 11, 'axes.spines.top': False,
                     'axes.spines.right': False, 'axes.grid': True,
                     'grid.alpha': .16, 'figure.facecolor': '#ffffff'})
UP, DOWN = '#c94444', '#13866f'  # Local market convention: red up, green down.


def read(path):
    return json.loads(path.read_text(encoding='utf-8'))


def candles(receipt, instrument, interval_ms=10000):
    grouped = collections.defaultdict(list)
    for t in receipt['trade_receipts']:
        if t['instrument_id'] == instrument:
            # The final boundary trade belongs to the next half-open bucket;
            # disclose its one-second partial candle instead of dropping it.
            grouped[t['time_ms'] // interval_ms].append(t)
    result = []
    for bucket, trades in sorted(grouped.items()):
        prices = [t['price_tick'] for t in trades]
        result.append({'time_ms': bucket * interval_ms, 'open': prices[0],
                       'high': max(prices), 'low': min(prices), 'close': prices[-1],
                       'volume': sum(t['qty'] for t in trades), 'trades': len(trades),
                       'partial': (bucket + 1) * interval_ms > receipt['simulation_time_ms']})
    return result


def draw_candles(ax, bars, interval_ms):
    width = interval_ms / 60000 * .72
    for b in bars:
        x = (b['time_ms'] + interval_ms / 2) / 60000
        color = UP if b['close'] >= b['open'] else DOWN
        ax.vlines(x, b['low'], b['high'], color=color, linewidth=.8)
        bottom = min(b['open'], b['close'])
        height = abs(b['close'] - b['open'])
        if height:
            ax.add_patch(Rectangle((x-width/2, bottom), width, height,
                                   facecolor=color, edgecolor=color, linewidth=.5,
                                   alpha=.4 if b['partial'] else 1))
        else:
            ax.hlines(bottom, x-width/2, x+width/2, color=color, linewidth=1)
    ax.set_xlim(0, 20.25)
    ax.set_xticks([0, 5, 10, 15, 20])
    ax.set_ylabel('价格（内部 tick）')


def export_csv(path, rows):
    with path.open('w', encoding='utf-8-sig', newline='') as f:
        writer = csv.DictWriter(f, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)


def plot_main(receipt):
    fig, axes = plt.subplots(2, 2, figsize=(16, 8), height_ratios=[3, 1],
                             sharex='col', layout='constrained')
    for col, leg in enumerate(('SPOT', 'PERP')):
        bars = candles(receipt, 'V-BTC-' + leg)
        export_csv(OUT / f'n1000-s7-{leg.lower()}-10s.csv', bars)
        draw_candles(axes[0, col], bars, 10000)
        axes[0, col].set_title('现货 · 10 秒 K 线' if leg == 'SPOT' else '永续 · 10 秒 K 线')
        axes[1, col].bar([(b['time_ms'] + 5000)/60000 for b in bars],
                         [b['volume'] for b in bars], width=.12,
                         color=[UP if b['close'] >= b['open'] else DOWN for b in bars])
        axes[1, col].set_ylabel('成交量（内部 lot）')
        axes[1, col].set_xlabel('仿真时间（分钟）')
    lo = min(b['low'] for leg in ('SPOT', 'PERP') for b in candles(receipt, 'V-BTC-'+leg))
    hi = max(b['high'] for leg in ('SPOT', 'PERP') for b in candles(receipt, 'V-BTC-'+leg))
    for ax in axes[0]:
        ax.set_ylim(lo-15, hi+15)
    fig.suptitle('1,000 bot：真实撮合成交生成的 K 线\n种子 7 · 20 分钟 · 策略 70% / 做市 10% / 估值 10% / 拆单 10% · 噪声 0%', fontsize=15)
    fig.supxlabel('红涨绿跌；无成交时段不补造 K 线；最右侧淡色为未闭合 bar。价格、数量均为仿真内部单位。', fontsize=10)
    fig.savefig(OUT / 'n1000-candles.png', dpi=150)
    plt.close(fig)


def plot_comparison(pilot):
    fig, axes = plt.subplots(3, 2, figsize=(16, 11), sharex=True, sharey=True, layout='constrained')
    all_bars = []
    for row, count in enumerate((100, 300, 1000)):
        r = pilot if count == 1000 else read(OUT.parent / f'market-study/n{count}-s7.receipt.json')
        for col, leg in enumerate(('SPOT', 'PERP')):
            bars = candles(r, 'V-BTC-'+leg, 30000)
            all_bars.extend(bars)
            draw_candles(axes[row, col], bars, 30000)
            axes[row, col].set_title(f'{count:,} bot · ' + ('现货' if leg == 'SPOT' else '永续'))
            axes[row, col].set_xlabel('仿真时间（分钟）')
    axes[0, 0].set_ylim(min(b['low'] for b in all_bars)-15, max(b['high'] for b in all_bars)+15)
    fig.suptitle('100 / 300 / 1,000 bot：相同种子下的 30 秒 K 线\n共同价格轴；现金池固定，但参数覆盖、库存及单账户分配也随人数变化。', fontsize=15)
    fig.savefig(OUT / 'population-candles.png', dpi=140)
    plt.close(fig)


def analyze_books(receipt):
    report = {}
    for leg in ('SPOT', 'PERP'):
        rows = []
        snapshots = [s for s in receipt['book_snapshots'] if s['instrument_id'] == 'V-BTC-'+leg]
        for s in snapshots:
            bids, asks = s['book']['bids'], s['book']['asks']
            two_sided = bool(bids and asks)
            bid = max((b['price_tick'] for b in bids), default=None)
            ask = min((b['price_tick'] for b in asks), default=None)
            mid = (bid+ask)/2 if two_sided else None
            depth_bid = sum(b['qty'] for b in bids if mid and b['price_tick'] >= mid*(1-.0025))
            depth_ask = sum(b['qty'] for b in asks if mid and b['price_tick'] <= mid*(1+.0025))
            rows.append({'time_ms': s['time_ms'], 'two_sided': two_sided,
                         'bid': bid, 'ask': ask, 'mid': mid,
                         'spread_bps': (ask-bid)/mid*10000 if mid else None,
                         'bid_depth_25bps': depth_bid if mid else None,
                         'ask_depth_25bps': depth_ask if mid else None,
                         'bid_levels': len(bids), 'ask_levels': len(asks)})
        spreads = [r['spread_bps'] for r in rows if r['two_sided']]
        report[leg] = {'samples': len(rows), 'two_sided_fraction': sum(r['two_sided'] for r in rows)/len(rows),
                       'spread_bps_p10_p50_p90_max': np.quantile(spreads, [.1,.5,.9,1]).tolist(),
                       'bid_depth_25bps_p10_p50_p90': np.quantile([r['bid_depth_25bps'] for r in rows if r['two_sided']], [.1,.5,.9]).tolist(),
                       'ask_depth_25bps_p10_p50_p90': np.quantile([r['ask_depth_25bps'] for r in rows if r['two_sided']], [.1,.5,.9]).tolist(),
                       'min_bid_depth_25bps': min(r['bid_depth_25bps'] for r in rows if r['two_sided']),
                       'min_ask_depth_25bps': min(r['ask_depth_25bps'] for r in rows if r['two_sided']),
                       'nonpositive_spread_samples': sum(v <= 0 for v in spreads)}
        sweep_report = {}
        for quantity in (10, 50, 100):
            for side in ('Buy', 'Sell'):
                costs = []
                observed = 0
                for s, row in zip(snapshots, rows):
                    if not row['two_sided']:
                        continue
                    observed += 1
                    levels = sorted(s['book']['asks' if side == 'Buy' else 'bids'],
                                    key=lambda x:x['price_tick'], reverse=side == 'Sell')
                    remaining, notional = quantity, 0
                    for level in levels:
                        used = min(remaining, level['qty'])
                        notional += used*level['price_tick']
                        remaining -= used
                        if not remaining:
                            break
                    if not remaining:
                        costs.append((notional/quantity-row['mid'])/row['mid']*10000*(1 if side=='Buy' else -1))
                sweep_report[f'{side}_{quantity}'] = {
                    'two_sided_samples': observed,
                    'full_quantity_available_fraction': len(costs)/observed,
                    'cost_from_mid_bps_p50_p90': np.quantile(costs, [.5,.9]).tolist() if costs else None}
        report[leg]['static_sweeps_excluding_fees'] = sweep_report
        report[leg]['static_sweep_limitation'] = 'Static book arithmetic only, no submitted test order, participant response, or replenishment.'
        export_csv(OUT / f'n1000-s7-{leg.lower()}-books.csv', rows)
        fig, axes = plt.subplots(3, 1, figsize=(14, 9), layout='constrained')
        t = [r['time_ms']/60000 for r in rows]
        axes[0].plot(t, [r['spread_bps'] for r in rows], color='#325c9e', linewidth=.8)
        axes[0].set_ylabel('价差（bps）')
        for side, color, label in [('bid',UP,'买方'),('ask',DOWN,'卖方')]:
            axes[1].plot(t, [r[side+'_depth_25bps'] for r in rows], color=color, label=label, linewidth=.8)
        axes[1].set_ylabel('距中价 25 bps 内深度（lot）')
        axes[1].legend(loc='upper right')
        # A fixed, disclosed timestamp rather than selecting the nicest book.
        s = snapshots[599]
        b, a = s['book']['bids'], s['book']['asks']
        mid = rows[599]['mid']
        for levels, color, label in [(b,UP,'买方'),(a,DOWN,'卖方')]:
            levels = sorted(levels, key=lambda x: abs(x['price_tick']-mid))
            distance = [abs(x['price_tick']-mid)/mid*10000 for x in levels]
            axes[2].step(distance, np.cumsum([x['qty'] for x in levels]), where='post', color=color, label=label)
        axes[2].set_xlabel('距中价距离（bps）；固定 10:00 快照')
        axes[2].set_ylabel('累计深度（lot）')
        axes[2].legend()
        # The third panel is spatial; it does not share the temporal axis.
        axes[0].set_xlim(0,20)
        axes[1].set_xlim(0,20)
        axes[1].set_xlabel('仿真时间（分钟）')
        axes[0].set_xticks([0,5,10,15,20])
        axes[1].set_xticks([0,5,10,15,20])
        axes[2].set_xlim(0, max(30, min(100, max(distance))))
        axes[2].set_xticks([0,25,50,75,100] if axes[2].get_xlim()[1] == 100 else [0,10,20,30])
        fig.suptitle('1,000 bot · ' + ('现货' if leg == 'SPOT' else '永续') + '盘口\n每秒完成全部 bot 动作后采样；不反映同秒内的瞬时缺口。', fontsize=15)
        fig.savefig(OUT / f'n1000-{leg.lower()}-book.png', dpi=140)
        plt.close(fig)
    return report


def flow_summary(receipt):
    roster = read(OUT.parent / 'native1000.roster.json')
    owners = {100+i: p for i, p in enumerate(roster['participants'])}
    qty = collections.Counter()
    for trade in receipt['trade_receipts']:
        owner = owners.get(trade['taker_account_id'])
        qty[owner['role'] if owner else 'non_roster'] += trade['qty']
    result = {'taker_qty': dict(qty), 'taker_qty_share': {k:v/sum(qty.values()) for k,v in qty.items()}}
    closes = {}
    for leg in ('SPOT', 'PERP'):
        bars = candles(receipt, 'V-BTC-'+leg)
        closed = [b for b in bars if not b['partial']]
        changes = np.diff([b['close'] for b in closed])
        longest = current = 0
        previous = None
        for change in changes:
            sign = np.sign(change)
            current = current+1 if sign and sign == previous else (1 if sign else 0)
            longest = max(longest, current)
            previous = sign
        result[leg] = {'ten_second_close_change_longest_same_sign_run': longest,
                       'closed_bars': len(closed)}
        closes[leg] = {b['time_ms']:b['close'] for b in closed}
    times = sorted(closes['SPOT'].keys() & closes['PERP'].keys())
    result['spot_perp_10s_close_return_correlation'] = float(np.corrcoef(
        np.diff(np.log([closes['SPOT'][t] for t in times])),
        np.diff(np.log([closes['PERP'][t] for t in times])))[0,1])
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--book-receipt', type=Path)
    args = parser.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)
    pilot_path = OUT.parent / 'native1000-wave-pilot.json'
    pilot = read(pilot_path)
    plot_main(pilot)
    plot_comparison(pilot)
    evidence = {'candle_source': str(pilot_path), 'candle_source_sha256': hashlib.sha256(pilot_path.read_bytes()).hexdigest(),
                'ohlcv_rule': 'half-open time buckets; actual receipts in matching order; no empty-bar fill; final partial bar retained'}
    evidence['flow'] = flow_summary(pilot)
    if args.book_receipt:
        books = read(args.book_receipt)
        evidence['book_source'] = str(args.book_receipt)
        evidence['book_source_sha256'] = hashlib.sha256(args.book_receipt.read_bytes()).hexdigest()
        evidence['trade_path_matches_pilot'] = books['trade_receipts'] == pilot['trade_receipts']
        evidence['books'] = analyze_books(books)
    (OUT / 'evidence.json').write_text(json.dumps(evidence, ensure_ascii=False, indent=2)+'\n', encoding='utf-8')
    print(json.dumps(evidence, ensure_ascii=True, indent=2))


if __name__ == '__main__':
    main()
