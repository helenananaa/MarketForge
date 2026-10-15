"""Causal Pine formula counterparts for configurable native strategy families.

These are named formula adaptations, not copies of complete community systems.
Original archived strategies retain separate identities and source hashes.
"""
from __future__ import annotations
import json
from pathlib import Path

ROOT=Path(__file__).resolve().parents[1]

FORMULAS={
 'SmaCross':('fast = ta.sma(close, fast_length)\nslow = ta.sma(close, slow_length)','fast > slow','fast < slow','fast < slow','fast > slow',['fast','slow']),
 'EmaCross':('fast = ta.ema(close, fast_length)\nslow = ta.ema(close, slow_length)','fast > slow','fast < slow','fast < slow','fast > slow',['fast','slow']),
 'Macd':('[m, s, hist] = ta.macd(close, fast_length, slow_length, signal_length)','m > s','m < s','m < s','m > s',['fast','slow','signal']),
 'RsiReversion':('r = ta.rsi(close, length)','r < low_gate','r > high_gate','r > high_gate','r < low_gate',['length','low','high']),
 'BollingerReversion':('[mid, upper, lower] = ta.bb(close, length, factor)','close < lower','close > mid','close > upper','close < mid',['length','factor']),
 'BollingerBreakout':('[mid, upper, lower] = ta.bb(close, length, factor)','close > upper','close < mid','close < lower','close > mid',['length','factor']),
 'Donchian':('upper = ta.highest(high, length)[1]\nlower = ta.lowest(low, length)[1]\nexit_low = ta.lowest(low, exit_length)[1]\nexit_high = ta.highest(high, exit_length)[1]','close > upper','close < exit_low','close < lower','close > exit_high',['length','exit_length']),
 'Turtle':('upper = ta.highest(high, length)[1]\nlower = ta.lowest(low, length)[1]\nexit_low = ta.lowest(low, exit_length)[1]\nexit_high = ta.highest(high, exit_length)[1]','close > upper','close < exit_low','close < lower','close > exit_high',['length','exit_length']),
 'Supertrend':('[line, direction] = ta.supertrend(factor, length)','direction < 0','direction > 0','direction > 0','direction < 0',['length','factor']),
 'UtAtr':('loss = factor * ta.atr(length)\nvar float trail = 0.0\nprev = nz(trail[1], 0.0)\ntrail := close > prev and close[1] > prev ? math.max(prev, close - loss) : close < prev and close[1] < prev ? math.min(prev, close + loss) : close > prev ? close - loss : close + loss','ta.crossover(close, trail)','ta.crossunder(close, trail)','ta.crossunder(close, trail)','ta.crossover(close, trail)',['length','factor']),
 'Ssl':('hi = ta.sma(high, length)\nlo = ta.sma(low, length)\nvar int side = 0\nside := close > hi ? 1 : close < lo ? -1 : side','side == 1','side == -1','side == -1','side == 1',['length']),
 'WaveTrend':('ap = hlc3\nesa = ta.ema(ap, fast_length)\nd = ta.ema(math.abs(ap - esa), fast_length)\nci = d > 0 ? (ap - esa) / (0.015 * d) : 0.0\nwt = ta.ema(ci, slow_length)\nsig = ta.sma(wt, signal_length)','ta.crossover(wt, sig) and wt < low_gate','ta.crossunder(wt, sig) and wt > high_gate','ta.crossunder(wt, sig) and wt > high_gate','ta.crossover(wt, sig) and wt < low_gate',['fast','slow','signal','low','high']),
 'Squeeze':('mid = ta.sma(close, length)\ndev = factor * ta.stdev(close, length)\nkrange = channel_factor * ta.sma(ta.tr(true), length)\nsqueezed = dev < krange\ncenter = ((ta.highest(high, length) + ta.lowest(low, length)) / 2.0 + mid) / 2.0\nmom = ta.linreg(close - center, length, 0)','squeezed[1] and not squeezed and mom > 0','mom < mom[1]','squeezed[1] and not squeezed and mom < 0','mom > mom[1]',['length','factor','channel_factor']),
 'Cci':('c = ta.cci(hlc3, length)','c < low_gate','c > high_gate','c > -low_gate','c < -high_gate',['length','low','high']),
 'Stochastic':('k = ta.sma(ta.stoch(close, high, low, length), smooth_length)\nd = ta.sma(k, signal_length)','ta.crossover(k, d) and k < low_gate','ta.crossunder(k, d) and k > high_gate','ta.crossunder(k, d) and k > high_gate','ta.crossover(k, d) and k < low_gate',['length','smooth','signal','low','high']),
 'StochRsi':('r = ta.rsi(close, length)\nk = ta.sma(ta.stoch(r, r, r, length), smooth_length)\nd = ta.sma(k, signal_length)','ta.crossover(k, d) and k < low_gate','ta.crossunder(k, d) and k > high_gate','ta.crossunder(k, d) and k > high_gate','ta.crossover(k, d) and k < low_gate',['length','smooth','signal','low','high']),
 'Williams':('w = ta.wpr(length)','w < low_gate','w > high_gate','w > high_gate','w < low_gate',['length','low','high']),
 'Adx':('[plus, minus, adx] = ta.dmi(length, signal_length)','plus > minus and adx > high_gate','plus < minus','plus < minus and adx > high_gate','plus > minus',['length','signal','high']),
 'VwapReversion':('v = ta.vwap(hlc3)\na = ta.atr(length)','close < v - factor * a','close >= v','close > v + factor * a','close <= v',['length','factor']),
 'Obv':('flow = ta.cum(close > close[1] ? volume : close < close[1] ? -volume : 0.0)\nf = ta.ema(flow, fast_length)\ns = ta.ema(flow, slow_length)','f > s','f < s','f < s','f > s',['fast','slow']),
 'Ichimoku':('tenkan = (ta.highest(high, fast_length) + ta.lowest(low, fast_length)) / 2.0\nkijun = (ta.highest(high, slow_length) + ta.lowest(low, slow_length)) / 2.0\na = ((tenkan + kijun) / 2.0)[displacement]\nb = ((ta.highest(high, length) + ta.lowest(low, length)) / 2.0)[displacement]','tenkan > kijun and close > a and close > b','tenkan < kijun or close < a','tenkan < kijun and close < a and close < b','tenkan > kijun or close > a',['fast','slow','length','displacement']),
 'Roc':('r = ta.roc(close, length)','r > high_gate','r < low_gate','r < -high_gate','r > -low_gate',['length','low','high']),
 'Keltner':('mid = ta.ema(close, length)\na = ta.atr(length)','close > mid + factor * a','close < mid','close < mid - factor * a','close > mid',['length','factor']),
 'HullCross':('fast = ta.hma(close, fast_length)\nslow = ta.hma(close, slow_length)','fast > slow','fast < slow','fast < slow','fast > slow',['fast','slow']),
 'AlmaCross':('fast = ta.alma(close, fast_length, alma_offset, alma_sigma)\nslow = ta.alma(close, slow_length, alma_offset, alma_sigma)','fast > slow','fast < slow','fast < slow','fast > slow',['fast','slow','offset','sigma']),
 'MfiReversion':('m = ta.mfi(hlc3, length)','m < low_gate','m > high_gate','m > high_gate','m < low_gate',['length','low','high']),
}

INPUT_NAMES={'fast':'fast_length','slow':'slow_length','signal':'signal_length','smooth':'smooth_length','low':'low_gate','high':'high_gate','offset':'alma_offset','sigma':'alma_sigma'}

def defaults(family):
    p=dict(fast=12,slow=26,signal=9,length=20,exit_length=10,smooth=3,displacement=26,low=30.,high=70.,factor=2.,channel_factor=1.5,offset=.85,sigma=6.)
    overrides={
      'SmaCross':dict(fast=3,slow=8),'RsiReversion':dict(length=14,high=60.),'MfiReversion':dict(length=14,high=60.),
      'Turtle':dict(length=55,exit_length=20),'Supertrend':dict(length=10,factor=3.),'UtAtr':dict(length=10,factor=1.),'Ssl':dict(length=10),
      'WaveTrend':dict(fast=10,slow=21,signal=4,low=-53.,high=53.),'Cci':dict(low=-100.,high=50.),
      'Stochastic':dict(length=14,low=25.,high=75.,signal=3),'StochRsi':dict(length=14,low=25.,high=75.,signal=3),
      'Williams':dict(length=14,low=-80.,high=-20.),'Adx':dict(length=14,signal=14,low=0.,high=25.),'VwapReversion':dict(length=14,factor=1.),
      'Obv':dict(fast=10,slow=30),'Ichimoku':dict(fast=9,slow=26,length=52),'Roc':dict(low=0.,high=2.),'HullCross':dict(fast=16,slow=40),'AlmaCross':dict(fast=9,slow=21),
    }
    p.update(overrides.get(family,{}));return p

def source_for(family):
    formula,long_entry,long_exit,short_entry,short_exit,keys=FORMULAS[family]
    p=defaults(family)
    declarations=[]
    for key in keys:
        value=p[key];typ='int' if isinstance(value,int) else 'float'
        declarations.append(f'{INPUT_NAMES.get(key,key)} = input.{typ}({value}, title="{key}")')
    return ('//@version=6\n// MarketForge configurable formula adaptation; original authors full scripts remain separate.\n'
      f'strategy("MF {family}", pyramiding=0)\n'+ '\n'.join(declarations)+
      '\norder_qty = input.int(2, title="qty", minval=1)\nallow_short = input.bool(false, title="allow_short")\n'+formula+
      f'\nlong_entry = {long_entry}\nlong_exit = {long_exit}\nshort_entry = {short_entry}\nshort_exit = {short_exit}\n'
      'if strategy.position_size == 0\n    if long_entry\n        strategy.entry("L", strategy.long, qty=order_qty)\n    else if allow_short and short_entry\n        strategy.entry("S", strategy.short, qty=order_qty)\n'
      'else if strategy.position_size > 0 and long_exit\n    strategy.close("L")\nelse if strategy.position_size < 0 and short_exit\n    strategy.close("S")\n'
      'plot(long_entry ? 1 : 0, "long_entry")\nplot(long_exit ? 1 : 0, "long_exit")\nplot(short_entry ? 1 : 0, "short_entry")\nplot(short_exit ? 1 : 0, "short_exit")\n')

def install(directory=None):
    directory=Path(directory or ROOT/'bot-plugins/pine-strategy/scripts/ecosystem')
    directory.mkdir(parents=True,exist_ok=True)
    for family in FORMULAS:
        (directory/f'{family}.pine').write_text(source_for(family),encoding='utf-8')
    return directory

if __name__=='__main__':
    print(json.dumps({'directory':str(install()),'families':list(FORMULAS)}))
