#!/usr/bin/env python3
"""Probe native Pine versus MarketForge adapter capabilities with controlled data."""
from __future__ import annotations

import json
from pathlib import Path
import sys

import pine_compat
from research_strategy_compat import bars_for, load_module, ROOT, summarize


def main():
    helpers = load_module('boundary_helpers', ROOT / 'python/tests/test_pine_bot.py')
    bars = bars_for('range', 128)
    peer = [{**b, 'open': b['open']+100, 'high': b['high']+100,
             'low': b['low']+100, 'close': b['close']+100} for b in bars]
    frames = [dict(time=b['time'], position_size=0, position_avg_price=None,
                   initial_capital=10000000, equity=10000000, netprofit=0, openprofit=0) for b in bars]
    sources = {
        'short': '//@version=6\nstrategy("Short")\nif bar_index == 1\n    strategy.entry("S", strategy.short, qty=1)',
        'limit': '//@version=6\nstrategy("Limit")\nif bar_index == 1\n    strategy.entry("L", strategy.long, qty=1, limit=close)',
        'bracket': '//@version=6\nstrategy("Bracket")\nif bar_index == 1\n    strategy.entry("L", strategy.long, qty=1)\nif bar_index == 2\n    strategy.exit("X", "L", stop=close*0.95, limit=close*1.05)',
        'peer_security': '//@version=6\nstrategy("Peer")\np = request.security("V-BTC-PERP", "1", close)\nplot(p-close)',
        'library': '//@version=6\nimport jdehorty/MLExtensions/2 as ml\nindicator("ML")\nplot(ml.n_rsi(close,14,1))',
    }
    rows = []
    for name, source in sources.items():
        row = dict(name=name, source=source)
        try:
            kwargs = dict(chart_symbol='V-BTC-SPOT', chart_timeframe='1')
            if name == 'peer_security':
                kwargs['request_bars'] = {'V-BTC-PERP:1': peer}
            result = pine_compat.run_script(source, bars, **kwargs)
            row['native'] = dict(status='pass', **summarize(result))
            if name == 'peer_security':
                assert result['plots'][0]['values'] == [100.0]*len(bars)
                row['native']['spread_checked'] = True
        except Exception as e:
            row['native'] = dict(status='rejected', error=str(e))
        try:
            result = pine_compat.compile_script(source).run_external(
                bars, frames, chart_symbol='V-BTC-SPOT', chart_timeframe='1')
            helpers.bot.validate_intents(result['intents'], result['pyramiding'], {})
            row['adapter'] = dict(status='accepted')
        except Exception as e:
            row['adapter'] = dict(status='rejected', error=str(e))
        rows.append(row)
    originals = ROOT / '.local/strategy-research-20261010/originals/ut_alerts.pine'
    if originals.exists():
        source = originals.read_text()
        report = pine_compat.analyze_script(source)
        h_id = next(i['callSiteId'] for i in report['inputs'] if i.get('title') == 'Signals from Heikin Ashi Candles')
        row = dict(name='ut_heikin_ashi_enabled')
        try:
            result = pine_compat.run_script(source, bars, chart_symbol='V-BTC-SPOT',
                chart_timeframe='1', input_overrides={h_id: True})
            row.update(status='pass', **summarize(result))
        except Exception as e:
            row.update(status='rejected', error=str(e))
        rows.append(row)
    out = Path(sys.argv[1])
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(dict(runtime_version=pine_compat.__version__, probes=rows), indent=2), encoding='utf-8')
    for row in rows:
        print(row['name'], json.dumps(row, ensure_ascii=False)[:650])


if __name__ == '__main__':
    main()
