"""Post hoc receipt inspection around an actual minimum/maximum traded price."""
from __future__ import annotations
import argparse, json
from collections import Counter
from pathlib import Path
from analyze_market_mechanisms import sha, write


def inspect(receipt, recipe, market, extreme, start_ms=7200000, radius_ms=60000):
    data = json.loads(receipt.read_text(encoding='utf-8-sig'))
    population = json.loads(recipe.read_text(encoding='utf-8'))
    accounts = {(a['Plugin']['participant']['account_id'], a['Plugin']['participant']['instrument_id']): a['Plugin']['plugin_id'] for a in population['agents']}
    trades = [t for t in data['trade_receipts'] if t['instrument_id'] == market and start_ms <= t['time_ms'] < data['simulation_time_ms']]
    event = (min if extreme == 'min' else max)(trades, key=lambda t: t['price_tick'])
    at = event['time_ms']; low, high = at - radius_ms, at + radius_ms
    window = [t for t in trades if low <= t['time_ms'] <= high]
    roles = {}
    for side in ('maker', 'taker'):
        volume = Counter()
        for trade in window:
            volume[accounts.get((trade[side + '_account_id'], market), 'unknown')] += trade['qty']
        roles[side] = dict(volume)
    books = [s for s in data['book_snapshots'] if s['instrument_id'] == market and low <= s['time_ms'] <= high]
    event_books = [s for s in data['book_snapshots'] if s['time_ms'] == at]
    return dict(receipt_sha256=sha(receipt), recipe_sha256=sha(recipe), market=market,
        selection=dict(extreme=extreme, start_ms=start_ms, end_ms=data['simulation_time_ms'], radius_ms=radius_ms),
        event=event, event_books=event_books, window_role_qty=roles, trades=window, book_samples=books,
        scope='Post hoc selection around a realized price extreme, not a predeclared treatment outcome. Final per-second book samples are after the action wave, not immediately before the selected trade. Account-role attribution does not distinguish forced liquidation from voluntary action. This receipt has no intraday account or strategy-state trajectory, so these observations alone do not prove the initiating cause.')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('receipt', type=Path); parser.add_argument('recipe', type=Path); parser.add_argument('output', type=Path)
    parser.add_argument('--market', default='V-BTC-PERP'); parser.add_argument('--extreme', choices=('min', 'max'), default='min')
    args = parser.parse_args()
    result = inspect(args.receipt, args.recipe, args.market, args.extreme); write(args.output, result)
    print(json.dumps({k: result[k] for k in ('event', 'window_role_qty', 'scope')}))
