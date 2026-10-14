#!/usr/bin/env python3
"""Reproducible offline strategy compatibility research; never connects to an exchange.

Adaptations are formula-level research probes, not copies of complete public bots.
Public-source captures live outside tracked code; pass --originals to audit them.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import math
from pathlib import Path
import random
import time
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]

# name: (formula, entry predicate, exit predicate, family)
PROBES = {
    "ema_cross": ('fast = ta.ema(close, 12)\nslow = ta.ema(close, 26)', 'fast > slow', 'fast < slow', 'trend'),
    "macd": ('[m, s, h] = ta.macd(close, 12, 26, 9)', 'm > s', 'm < s', 'trend'),
    "rsi_reversion": ('r = ta.rsi(close, 14)', 'r < 30', 'r > 60', 'reversion'),
    "bb_reversion": ('[mid, upper, lower] = ta.bb(close, 20, 2.0)', 'close < lower', 'close > mid', 'reversion'),
    "bb_breakout": ('[mid, upper, lower] = ta.bb(close, 20, 2.0)', 'close > upper', 'close < mid', 'breakout'),
    "donchian": ('upper = ta.highest(high, 20)[1]\nlower = ta.lowest(low, 10)[1]', 'close > upper', 'close < lower', 'breakout'),
    "turtle": ('upper = ta.highest(high, 55)[1]\nlower = ta.lowest(low, 20)[1]', 'close > upper', 'close < lower', 'breakout'),
    "supertrend": ('[line, direction] = ta.supertrend(3.0, 10)', 'direction < 0', 'direction > 0', 'trend'),
    "ut_atr": ('loss = ta.atr(10)\nvar float trail = 0.0\nprev = nz(trail[1], 0.0)\ntrail := close > prev and close[1] > prev ? math.max(prev, close - loss) : close < prev and close[1] < prev ? math.min(prev, close + loss) : close > prev ? close - loss : close + loss', 'ta.crossover(close, trail)', 'ta.crossunder(close, trail)', 'trend'),
    "ssl": ('hi = ta.sma(high, 10)\nlo = ta.sma(low, 10)\nvar int side = 0\nside := close > hi ? 1 : close < lo ? -1 : side', 'side == 1', 'side == -1', 'trend'),
    "wavetrend": ('ap = hlc3\nesa = ta.ema(ap, 10)\nd = ta.ema(math.abs(ap - esa), 10)\nci = d > 0 ? (ap - esa) / (0.015 * d) : 0.0\nwt = ta.ema(ci, 21)\nsig = ta.sma(wt, 4)', 'ta.crossover(wt, sig) and wt < -53', 'ta.crossunder(wt, sig) and wt > 53', 'reversion'),
    "squeeze": ('mid = ta.sma(close, 20)\ndev = 2.0 * ta.stdev(close, 20)\nkrange = 1.5 * ta.sma(ta.tr(true), 20)\nsqueezed = dev < krange\ncenter = ((ta.highest(high, 20) + ta.lowest(low, 20)) / 2.0 + mid) / 2.0\nmom = ta.linreg(close - center, 20, 0)', 'squeezed[1] and not squeezed and mom > 0', 'mom < mom[1]', 'volatility_release'),
    "cci": ('c = ta.cci(hlc3, 20)', 'c < -100', 'c > 50', 'reversion'),
    "stochastic": ('k = ta.sma(ta.stoch(close, high, low, 14), 3)\nd = ta.sma(k, 3)', 'ta.crossover(k, d) and k < 25', 'ta.crossunder(k, d) and k > 75', 'reversion'),
    "stoch_rsi": ('r = ta.rsi(close, 14)\nk = ta.sma(ta.stoch(r, r, r, 14), 3)\nd = ta.sma(k, 3)', 'ta.crossover(k, d) and k < 25', 'ta.crossunder(k, d) and k > 75', 'reversion'),
    "williams": ('w = ta.wpr(14)', 'w < -80', 'w > -20', 'reversion'),
    "adx": ('[plus, minus, adx] = ta.dmi(14, 14)', 'plus > minus and adx > 25', 'plus < minus', 'trend'),
    "vwap_reversion": ('v = ta.vwap(hlc3)\na = ta.atr(14)', 'close < v - a', 'close >= v', 'reversion'),
    "obv": ('flow = ta.cum(close > close[1] ? volume : close < close[1] ? -volume : 0.0)\nf = ta.ema(flow, 10)\ns = ta.ema(flow, 30)', 'f > s', 'f < s', 'volume_trend'),
    "ichimoku": ('tenkan = (ta.highest(high, 9) + ta.lowest(low, 9)) / 2.0\nkijun = (ta.highest(high, 26) + ta.lowest(low, 26)) / 2.0\na = ((tenkan + kijun) / 2.0)[26]\nb = ((ta.highest(high, 52) + ta.lowest(low, 52)) / 2.0)[26]', 'tenkan > kijun and close > a and close > b', 'tenkan < kijun or close < a', 'trend'),
    "roc": ('r = ta.roc(close, 20)', 'r > 2', 'r < 0', 'trend'),
    "keltner": ('mid = ta.ema(close, 20)\na = ta.atr(20)', 'close > mid + 2 * a', 'close < mid', 'breakout'),
}


def source_for(name, formula, enter, leave):
    return ('//@version=6\n// Formula-level research adaptation; long/flat closed-bar execution.\n'
            f'strategy("Research {name}")\n{formula}\n'
            f'enter = {enter}\nleave = {leave}\n'
            'if strategy.position_size == 0 and enter\n'
            '    strategy.entry("L", strategy.long, qty=1)\n'
            'else if strategy.position_size > 0 and leave\n'
            '    strategy.close("L")\n'
            'plot(enter ? 1 : 0, "entry_signal")\n'
            'plot(leave ? 1 : 0, "exit_signal")\n')


def bars_for(regime, n=512, seed=731):
    rng = random.Random(seed)
    bars = []
    prev = 10000
    for i in range(n):
        if regime == 'flat':
            close, width = 10000, 0
        elif regime == 'trend_reversal':
            close, width = round(10000 + (i if i < n // 2 else n - i) * 20 + rng.gauss(0, 30)), 30
        elif regime == 'range':
            close, width = round(10000 + 700 * math.sin(i / 12) + rng.gauss(0, 50)), 40
        elif regime == 'squeeze_release':
            close = round(10000 + (25 if i % 128 < 70 else 600) * math.sin(i / 6))
            width = 20 if i % 128 < 70 else 70
        elif regime == 'jumps':
            close, width = round(10000 + (1000 if (i // 35) % 2 else -1000) + rng.gauss(0, 50)), 60
        else:
            close, width = max(1000, round(prev + rng.gauss(0, 160))), 70
        bars.append(dict(time=1791504000000+i*60000, open=prev, high=max(prev, close)+width,
                         low=min(prev, close)-width, close=close,
                         volume=100 + rng.randrange(500)))
        prev = close
    return bars


def error_diagnostics(output):
    return [d for d in output.get('diagnostics', []) if str(d.get('severity', '')).lower() == 'error']


def summarize(output):
    strategy = output.get('strategy') or {}
    return {'orders': len(strategy.get('orders', [])), 'trades': len(strategy.get('trades', [])),
            'plots': len(output.get('plots', [])), 'alerts': len(output.get('alerts', [])),
            'diagnostics': output.get('diagnostics', []),
            'strategy_diagnostics': strategy.get('diagnostics', [])}


def bot_replay(source, bars, bot, helpers):
    """Controlled host fixture: IOC fills at close, not MarketForge matching proof."""
    state, cash, qty, receipts = None, 10000000, 0, []
    buys, sells = 0, 0
    with patch.object(bot, 'load_source', return_value=source):
        for i in range(1, len(bars)+1):
            req = helpers.request([int(b['close']) for b in bars[:i]], state=state)
            req['config'].update(bar_interval_ms=60000, history_limit=1024, max_qty=1, inventory_cap=1)
            obs = req['observation']
            obs['bot_market_data']['interval_ms'] = 60000
            obs['market_time_ms'] = bars[i-1]['time'] + 60000
            for candle, b in zip(obs['bot_market_data']['candles'], bars[:i]):
                candle.update(open_time_ms=b['time'], close_time_ms=b['time']+60000,
                              open_tick=b['open'], high_tick=b['high'], low_tick=b['low'],
                              close_tick=b['close'], volume=b['volume'])
            obs['own_account']['Spot'].update(cash_balance=cash, available_cash=cash,
                position_qty=qty, available_position=qty)
            for receipt in receipts:
                helpers.fill(req, cash=cash, position=qty, **receipt)
            result = bot.decide(req)
            state = result['state']
            for action in result['actions']:
                a = action['PlaceImmediateOrCancel']
                price = int(bars[i-1]['close'])
                bought = a['side'] == 'Buy'
                cash += (-1 if bought else 1) * price * a['qty']
                qty += (1 if bought else -1) * a['qty']
                buys += int(bought)
                sells += int(not bought)
                receipts.append(dict(side=a['side'], qty=a['qty'], price=price,
                                     trade_id=len(receipts)+1, time=bars[i-1]['time']+60000))
            # Same observation/state must not submit another action.
            req['state'] = json.loads(json.dumps(state))
            if bot.decide(req)['actions']:
                raise AssertionError('duplicate bar submitted another order')
    return dict(buys=buys, sells=sells, final_qty=qty, fixture_only=True)


def load_module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--originals', type=Path)
    parser.add_argument('--bot-replay', action='store_true')
    parser.add_argument('--skip-adaptations', action='store_true')
    args = parser.parse_args()
    import pine_compat
    args.output.mkdir(parents=True, exist_ok=True)
    regimes = ['flat', 'trend_reversal', 'range', 'squeeze_release', 'jumps', 'random_walk']
    datasets = {r: bars_for(r) for r in regimes}
    (args.output / 'datasets.json').write_text(json.dumps(datasets), encoding='utf-8')
    report = dict(runtime_version=pine_compat.__version__, runtime_path=pine_compat.__file__,
                  seed=731, bars_per_regime=512, regimes=regimes,
                  limitations=['Synthetic offline OHLCV; no profitability qualification',
                               'No TradingView numerical parity', 'Bot replay uses controlled fill fixtures'],
                  originals=[], adaptations=[])
    if args.originals:
        for path in sorted(p for p in args.originals.rglob('*') if p.suffix in ('.pine', '.ps')):
            source = path.read_text(encoding='utf-8-sig')
            row = dict(name=path.stem, path=str(path.relative_to(args.originals)),
                       sha256=hashlib.sha256(source.encode()).hexdigest())
            try:
                out = pine_compat.run_script(source, datasets['range'], chart_symbol='V-BTC-SPOT', chart_timeframe='1')
                row.update(status='pass' if not error_diagnostics(out) else 'diagnostic_error', **summarize(out))
            except Exception as e:
                row.update(status='failed', error=str(e))
            report['originals'].append(row)
            print('original', row['name'], row['status'], row.get('error', '')[:200], flush=True)
        (args.output / 'results.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
    if args.bot_replay:
        helpers = load_module('research_helpers', ROOT / 'python/tests/test_pine_bot.py')
        bot = helpers.bot
    for name, (formula, enter, leave, family) in ({} if args.skip_adaptations else PROBES).items():
        source = source_for(name, formula, enter, leave)
        (args.output / f'{name}.pine').write_text(source, encoding='utf-8')
        row = dict(name=name, family=family, kind='formula_adaptation',
                   sha256=hashlib.sha256(source.encode()).hexdigest(), runs={})
        start = time.perf_counter()
        try:
            for regime, bars in datasets.items():
                out = pine_compat.run_script(source, bars, chart_symbol='V-BTC-SPOT', chart_timeframe='1')
                if error_diagnostics(out) or error_diagnostics(out.get('strategy') or {}):
                    raise ValueError(json.dumps(summarize(out)))
                row['runs'][regime] = summarize(out)
                # Fixed-prefix invariance detects future dependence at sampled cutoffs.
                for cut in (128, 257):
                    prefix = pine_compat.run_script(source, bars[:cut], chart_symbol='V-BTC-SPOT', chart_timeframe='1')
                    assert [p['values'] for p in prefix['plots']] == [p['values'][:cut] for p in out['plots']], 'signal prefix changed'
                    assert (prefix.get('strategy') or {}).get('orders', []) == [o for o in (out.get('strategy') or {}).get('orders', []) if o['barIndex'] < cut], 'order prefix changed'
            row['prefix_checks'] = 12
            row['status'] = 'pass'
            row['active_regimes'] = [r for r, s in row['runs'].items() if s['orders'] > 0]
            if args.bot_replay:
                row['bot_fixture'] = bot_replay(source, datasets['range'][:256], bot, helpers)
        except Exception as e:
            row.update(status='failed', error=str(e))
        row['elapsed_seconds'] = round(time.perf_counter()-start, 4)
        report['adaptations'].append(row)
        (args.output / 'results.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
        print('adaptation', name, row['status'], row.get('bot_fixture', {}), row.get('error', '')[:160], flush=True)
    print(args.output / 'results.json', flush=True)


if __name__ == '__main__':
    main()
