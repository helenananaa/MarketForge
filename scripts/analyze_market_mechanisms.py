"""Receipt-based market diagnostics and reproducible scientific comparison figures."""
from __future__ import annotations

import argparse
import csv
import hashlib
import io
import json
import math
import re
import zipfile
from collections import Counter, defaultdict
from pathlib import Path

import research_numeric  # Configure numerical workers before NumPy loads.
import numpy as np
from research_json import Receipt, compact_quote

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_OUT = ROOT / '.local/strategy-research-mechanisms-20261024'
ANALYSIS_VERSION = 4


def sha(path):
    digest=hashlib.sha256()
    with path.open('rb') as f:
        for block in iter(lambda:f.read(1024*1024), b''):digest.update(block)
    return digest.hexdigest()


def write(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2, allow_nan=False)+'\n',encoding='utf-8')


def corr(a, b):
    if len(a)<3 or np.std(a)==0 or np.std(b)==0:return None
    return float(np.corrcoef(a,b)[0,1])


def q(values, percentile):
    return float(np.percentile(values,percentile)) if len(values) else None


def add_bar(bars, time_ms, price, qty, interval=60000):
    key=time_ms//interval
    if key not in bars:bars[key]=dict(bucket=key,open=price,high=price,low=price,close=price,volume=qty,trades=1)
    else:
        b=bars[key];b.update(high=max(b['high'],price),low=min(b['low'],price),close=price,
                            volume=b['volume']+qty,trades=b['trades']+1)


def rebar(bars, multiple):
    result={}
    for k,b in sorted(bars.items()):
        key=k//multiple
        if key not in result:result[key]={**b,'bucket':key}
        else:
            x=result[key];x.update(high=max(x['high'],b['high']),low=min(x['low'],b['low']),
                close=b['close'],volume=x['volume']+b['volume'],trades=x['trades']+b['trades'])
    return result


def bar_metrics(bars, start_ms, end_ms, interval=60000):
    # Only complete, actually traded buckets. Never fill a gap or include a boundary trade.
    selected={k:b for k,b in bars.items() if k*interval>=start_ms and (k+1)*interval<=end_ms}
    returns={k:math.log(b['close']/selected[k-1]['close'])*10000
             for k,b in selected.items() if k-1 in selected}
    rv=list(returns.values());absrv=list(map(abs,rv))
    acf=lambda lag,absolute=False:corr(
        [abs(v) if absolute else v for k,v in returns.items() if k-lag in returns],
        [abs(returns[k-lag]) if absolute else returns[k-lag] for k in returns if k-lag in returns])
    run=longest=0;prev=0;previous_bucket=None
    for k,v in sorted(returns.items()):
        sign=int(v>0)-int(v<0)
        run=run+1 if sign and sign==prev and previous_bucket==k-1 else int(sign!=0)
        longest=max(longest,run);prev=sign;previous_bucket=k
    vols=[selected[k]['volume'] for k in sorted(selected)]
    return dict(bars=len(selected),expected_bars=(end_ms-start_ms)//interval,return_samples=len(rv),
        return_std_bp=float(np.std(rv,ddof=1)) if len(rv)>1 else None,
        return_acf1=acf(1),abs_return_acf1=acf(1,True),
        return_acf=[acf(lag) for lag in range(1,21)],abs_return_acf=[acf(lag,True) for lag in range(1,21)],
        zero_return_fraction=sum(v==0 for v in rv)/len(rv) if rv else None,
        abs_return_p99_bp=q(absrv,99),abs_return_max_bp=max(absrv,default=None),
        longest_directional_run=longest,
        volume_cv=float(np.std(vols)/np.mean(vols)) if vols and np.mean(vols)>0 else None,
        volume_abs_return_corr=corr([selected[k]['volume'] for k in returns],absrv),
        close_change_pct=(selected[max(selected)]['close']/selected[min(selected)]['close']-1)*100 if selected else None)


def reference(out):
    root=ROOT.parent/'MarketForge/.local/market-calibration/source'
    manifest=json.loads((root/'manifest.json').read_text(encoding='utf-8'))
    result={'source_manifest_sha256':sha(root/'manifest.json'),'date':'2025-01-02',
            'scope':'One historical BTCUSDT day, descriptive reference only; no order-book archive.',
            'markets':{}}
    for item in manifest:
        leg=item['leg'];path=root/f'{leg}-2025-01-02.zip'
        assert sha(path)==item['sha256'],f'{leg}: archive checksum mismatch'
        bars={};last_id=last_time=-1;count=0
        with zipfile.ZipFile(path) as z, z.open(z.namelist()[0]) as f:
            for row in csv.reader(io.TextIOWrapper(f,encoding='utf-8')):
                if not row[0].isdigit():continue
                ident=int(row[0]);time=int(row[5])//(1000 if leg=='spot' else 1)
                assert ident>last_id and time>=last_time
                last_id,last_time=ident,time
                price,qty=float(row[1]),float(row[2]);assert price>0 and qty>0
                relative=time-1735776000000
                assert 0<=relative<86400000
                add_bar(bars,relative,price,qty);count+=1
        result['markets'][leg]=dict(source=item,records=count,
            full_day=bar_metrics(bars,0,86400000),
            two_hour_windows=[dict(start_hour=h,**bar_metrics(bars,h*3600000,(h+2)*3600000)) for h in range(0,24,2)],
            candles=list(bars.values()))
    write(out/'reference-statistics.json',result)
    return result


def state_coverage(data, agents, end_ms):
    configs={x['Plugin']['participant']['participant_id']:x['Plugin'] for x in agents}
    technical=[];makers=[];execution=[]
    for entry in data['states']:
        p=configs[entry['id']];s=entry['state']['Plugin']['data'];kind=p['plugin_id']
        if kind=='TechnicalStrategyTrader':
            technical.append(dict(id=entry['id'],period_ms=p['config']['bar_interval_ms'],
                bars=s.get('bars'),ready=bool(s.get('frame',{}).get('ready'))))
        elif kind=='AdaptiveLiquidityMaker':
            makers.append(dict(id=entry['id'],calibrated=s.get('calibrated'),half_spread=s.get('half_spread'),
                               quoted=s.get('quoted'),cancelled=s.get('cancelled')))
        elif kind=='ExecutionTrader':
            execution.append(dict(id=entry['id'],start_after_ms=p['config']['start_after_ms'],
                scheduled_by_end=p['config']['start_after_ms']<end_ms,
                target_qty=p['config']['target_qty'],completed_qty=s.get('completed_qty'),
                deadline_reached=s.get('deadline_reached')))
    return dict(technical=technical,makers=makers,execution=execution,
                pine_scope='Configured periods and actual fills are reported; a generic all-Pine-ready flag is unavailable.')


def asset_checks(data):
    stages={}
    for stage in ('before','after'):
        totals=Counter();negative=[];reserved=Counter();seen=set()
        for account in data['assets_'+stage]['accounts']:
            for asset in account['assets']:
                key=(account['account_id'],asset['asset_id'])
                assert key not in seen,'duplicate account asset snapshot'
                seen.add(key)
                assert asset['total']==asset['portfolio_total']+asset['venue_total']
                assert asset['available']==asset['total']-asset['portfolio_reserved']-asset['venue_reserved']
                assert asset['portfolio_available']==asset['portfolio_total']-asset['portfolio_reserved']
                assert asset['venue_available']==asset['venue_total']-asset['venue_reserved']
                assert asset['portfolio_reserved']>=0 and asset['venue_reserved']>=0
                totals[asset['asset_id']]+=asset['total']
                reserved[asset['asset_id']]+=asset['portfolio_reserved']+asset['venue_reserved']
                if asset['available']<0:negative.append(dict(account_id=account['account_id'],**asset))
        stages[stage]=dict(totals=dict(totals),reserved=dict(reserved),negative_available=negative,
                           checked_balances=len(seen))
    before,after=stages['before']['totals'],stages['after']['totals']
    return dict(**stages,total_deltas={k:after.get(k,0)-before.get(k,0) for k in before.keys()|after.keys()},
        scope='Endpoint accounting identities and balances only. Settlement cash may decrease through fees; '
              'these receipts do not expose a complete fee/insurance ledger or every intraday reservation.')


def quoted_mid_metrics(snapshots, start, end, step_ms):
    # Diagnostic added after the first paired result, not a treatment-selection
    # objective. A wider spread can alter trade-close returns through bid/ask
    # bounce. Sample only the final scheduled quote observation of each minute;
    # an unavailable two-sided quote stays missing, never carried forward.
    mids={}
    for s in snapshots:
        if s['time_ms']%60000!=60000-step_ms:continue
        bid,ask=s['book']['bids'],s['book']['asks']
        if bid and ask:
            add_bar(mids,s['time_ms'],(bid[0]['price_tick']+ask[0]['price_tick'])/2,0)
    return dict(scope='Last scheduled two-sided quote midpoint per minute; not traded OHLCV. '
                      'Exploratory spread-bounce diagnostic, added after the first baseline/model pair.',
                **bar_metrics(mids,start,end))


def analyze_run(out, row):
    path=out/(row['name']+'.receipt.json');done=out/(row['name']+'.run.json')
    run=json.loads(done.read_text(encoding='utf-8'))
    assert run['complete'] and sha(path)==run['receipt_sha256']
    data=Receipt(path)
    recipe=json.loads(Path(row['recipe']).read_text(encoding='utf-8'))
    assert sha(Path(row['recipe']))==row['recipe_sha256']
    assert data['steps']==row['steps'] and data['clock_step_seconds']==row['seconds']
    assert data['simulation_time_ms']==row['steps']*row['seconds']*1000
    assert data['bots']==row['bots'] and len(data['states'])==row['bots']
    by_account={(a['Plugin']['participant']['account_id'],a['Plugin']['participant']['instrument_id']):a['Plugin']
                for a in recipe['agents']}
    end=data['simulation_time_ms'];start=row['warmup_ms']
    result={**row,'analysis_version':ANALYSIS_VERSION,'receipt_sha256':run['receipt_sha256'],'binary_sha256':run['inputs']['binary_sha256'],
            'wall_seconds':run['wall_seconds'],'markets':{},'coverage':state_coverage(data,recipe['agents'],end),
            'asset_checks':asset_checks(data),
            'rejected_actions':sum(data['rejections'].values()),'rejection_reasons':dict(Counter({k.split('{')[0].strip():0 for k in data['rejections']}))}
    reject=Counter()
    for k,n in data['rejections'].items():
        # Keep full source messages in the receipt; these categories remain diagnostic.
        reject[k.split('reason: ')[-1].split(' }')[0] if 'reason: ' in k else k]+=n
    result['rejection_reasons']=dict(reject)
    seen=set();rows=defaultdict(list);bars=defaultdict(dict);raw=Counter();self_trades=0;last_time={}
    for t in data['trade_receipts']:
        key=(t['instrument_id'],t['trade_id']);assert key not in seen;seen.add(key)
        assert t['time_ms']>=last_time.get(t['instrument_id'],-1)
        last_time[t['instrument_id']]=t['time_ms']
        raw[t['instrument_id']]+=1
        assert t['qty']>0 and t['price_tick']>0 and 0<=t['time_ms']<=end
        self_trades+=t['maker_account_id']==t['taker_account_id']
        if t['time_ms']<end:
            add_bar(bars[t['instrument_id']],t['time_ms'],t['price_tick'],t['qty'])
            if t['time_ms']>=start:rows[t['instrument_id']].append(t)
    assert dict(raw)==data['trades']
    result['integrity']=dict(unique_trades=len(seen),count_matches=True,self_trades=self_trades,
                             comparison_window='[warmup_ms, simulation_time_ms); no invented empty bars')
    snapshots=defaultdict(list)
    for s in data['book_snapshots']:
        if start<=s['time_ms']<end:snapshots[s['instrument_id']].append(compact_quote(s))
    active_pine=set();active_pine_sources=set()
    configured_pine=[a['Plugin'] for a in recipe['agents'] if a['Plugin']['plugin_id']=='pine.strategy']
    for market in raw:
        sample_times=[s['time_ms'] for s in snapshots[market]]
        assert sample_times==list(range(start,end,row['seconds']*1000)),f'{market}: missing/duplicate book samples'
        trades=rows[market];role_qty=Counter();active=set();active_role=defaultdict(set);signs=[];order_ids=set();pine_sources=set()
        for t in trades:
            p=by_account.get((t['taker_account_id'],market));role=p['plugin_id'] if p else 'external'
            role_qty[role]+=t['qty']
            for side in ('maker','taker'):
                owner=by_account.get((t[side+'_account_id'],market))
                if owner:
                    ident=owner['participant']['participant_id'];active.add(ident);active_role[owner['plugin_id']].add(ident)
                    if owner['plugin_id']=='pine.strategy':
                        active_pine.add(ident);active_pine_sources.add(owner['config']['script']);pine_sources.add(owner['config']['script'])
            if t['taker_order_id'] not in order_ids:
                order_ids.add(t['taker_order_id']);signs.append(1 if t['taker_side']=='Buy' else -1)
        spreads=[];bdepth=[];adepth=[];available=0;outage=longest=0
        for s in snapshots[market]:
            bid,ask=s['book']['bids'],s['book']['asks']
            if bid and ask:
                assert bid[0]['price_tick']<ask[0]['price_tick']
                available+=1;outage=0;mid=(bid[0]['price_tick']+ask[0]['price_tick'])/2
                spreads.append((ask[0]['price_tick']-bid[0]['price_tick'])/mid*10000)
                bdepth.append(sum(x['qty'] for x in bid if x['price_tick']>=mid*(1-.0025)))
                adepth.append(sum(x['qty'] for x in ask if x['price_tick']<=mid*(1+.0025)))
            else:outage+=row['seconds'];longest=max(longest,outage)
        total=sum(role_qty.values())
        result['markets'][market]=dict(
            minute=bar_metrics(bars[market],start,end),five_minute=bar_metrics(rebar(bars[market],5),start,end,300000),
            quoted_mid_minute=quoted_mid_metrics(snapshots[market],start,end,row['seconds']*1000),
            startup_30m=bar_metrics(bars[market],0,1800000),
            windows_2h=[dict(start_hour=h,**bar_metrics(bars[market],h*3600000,(h+2)*3600000)) for h in range(0,end//3600000,2)],
            trades=len(trades),volume_lots=total,active_instances=len(active),active_by_type={k:len(v) for k,v in active_role.items()},
            active_pine_sources=len(pine_sources),
            taker_qty_by_type=dict(role_qty),taker_share_by_type={k:v/total for k,v in role_qty.items()} if total else {},
            filled_taker_orders=len(signs),taker_order_same_direction=sum(a==b for a,b in zip(signs,signs[1:]))/(len(signs)-1) if len(signs)>1 else None,
            book_samples=len(snapshots[market]),two_sided_fraction=available/len(snapshots[market]) if snapshots[market] else None,
            spread_median_bp=q(spreads,50),spread_p90_bp=q(spreads,90),bid_depth_25bp_median=q(bdepth,50),ask_depth_25bp_median=q(adepth,50),
            longest_one_sided_outage_seconds=longest)
        candle_path=out/(row['name']+'-'+market+'.candles.csv')
        with candle_path.open('w',newline='',encoding='utf-8') as f:
            writer=csv.DictWriter(f,fieldnames=['bucket','open','high','low','close','volume','trades'])
            writer.writeheader();writer.writerows(bars[market][k] for k in sorted(bars[market]))
    result['pine_activity']=dict(configured_instances=len(configured_pine),
        configured_sources=len({p['config']['script'] for p in configured_pine}),
        active_instances=len(active_pine),active_sources=len(active_pine_sources),
        scope='Any actual fill in the primary window, attributed by account owner; not an all-Pine-ready or strategy-initiated-order flag.')
    write(out/(row['name']+'.metrics.json'),result)
    return result


def chart(out, rows, suite, seed, minute_zoom=False):
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    from matplotlib.patches import Rectangle
    selected=[r for r in rows if r['suite']==suite and r['seed']==seed]
    if len(selected)!=3:return
    selected.sort(key=lambda r:['baseline','model','model_ack'].index(r['profile']))
    plt.rcParams.update({'font.family':'DejaVu Sans','font.size':10,'axes.spines.top':False,'axes.spines.right':False})
    fig,axes=plt.subplots(4,3,figsize=(17,10),sharex=True,gridspec_kw={'height_ratios':[3,1,3,1]})
    for leg,market in enumerate(['V-BTC-SPOT','V-BTC-PERP']):
        all_lows=[];all_highs=[]
        for col,r in enumerate(selected):
            with (out/(r['name']+'-'+market+'.candles.csv')).open() as f:
                bars={int(x['bucket']):{k:(int(v) if k in ('bucket','trades') else float(v)) for k,v in x.items()} for x in csv.DictReader(f)}
            # Windows chosen by the experiment, never by an attractive chart segment.
            multiple=1 if minute_zoom else 5
            interval=60000*multiple
            start=max(r['warmup_ms'],r['steps']*1000-7200000) if minute_zoom else r['warmup_ms']
            values=[b for k,b in sorted(rebar(bars,multiple).items()) if k*interval>=start and (k+1)*interval<=r['steps']*1000]
            ax,volume=axes[leg*2,col],axes[leg*2+1,col]
            for b in values:
                x=b['bucket']*multiple/60;op,hi,lo,cl=[(b[k]/100000-1)*100 for k in ('open','high','low','close')]
                color='#1f4e79' if cl>=op else '#b46b25'
                ax.vlines(x,lo,hi,color=color,lw=.7)
                width=.0112*multiple
                ax.add_patch(Rectangle((x-width/2,min(op,cl)),width,max(abs(cl-op),.00005),
                             edgecolor=color,facecolor='white' if cl>=op else color,lw=.8))
                volume.bar(x,b['volume'],width=width*1.15,color=color,alpha=.7)
                all_lows.append(lo);all_highs.append(hi)
            ax.set_title(f"{market.replace('V-BTC-','')} | {r['profile']}")
            ax.axhline(0,color='#666666',lw=.7,ls=':');ax.grid(axis='y',alpha=.18)
            volume.grid(axis='y',alpha=.18)
            if col==0:ax.set_ylabel('Return from initial price (%)');volume.set_ylabel(f'Lots / {multiple}m')
            volume.set_xlabel('Simulation hour')
        if all_lows:
            low,high=min(all_lows),max(all_highs);pad=max((high-low)*.06,.02)
            for ax in axes[leg*2]:ax.set_ylim(low-pad,high+pad)
        vmax=max(ax.get_ylim()[1] for ax in axes[leg*2+1])
        for ax in axes[leg*2+1]:ax.set_ylim(0,vmax)
    fig.suptitle(f'Fixed-seed mechanism comparison | {suite}, seed {seed} | {multiple}-minute actual OHLCV',fontsize=15)
    fig.text(.5,.015,'Same assets, demand, fees and strategy inputs. First 2h excluded; no empty-bar filling. Common scales within each market.',ha='center',fontsize=10)
    suffix='1m-last2h' if minute_zoom else '5m-full'
    fig.tight_layout(rect=[0,.04,1,.96]);fig.savefig(out/f'{suite}-seed{seed}-candles-{suffix}.png',dpi=150);plt.close(fig)


def summary_csv(out, results):
    records=[]
    for r in results:
        for market,m in r['markets'].items():
            records.append(dict(suite=r['suite'],seed=r['seed'],profile=r['profile'],market=market,
                **{k:m['minute'][k] for k in ('bars','return_std_bp','return_acf1','abs_return_acf1',
                    'zero_return_fraction','abs_return_p99_bp','abs_return_max_bp','longest_directional_run','volume_abs_return_corr')},
                **{k:m[k] for k in ('two_sided_fraction','spread_median_bp','spread_p90_bp','bid_depth_25bp_median',
                    'ask_depth_25bp_median','longest_one_sided_outage_seconds','active_instances','taker_order_same_direction')},
                reactive_taker_share=m['taker_share_by_type'].get('ReactiveFlowTrader',0),
                pine_taker_share=m['taker_share_by_type'].get('pine.strategy',0),
                active_pine_instances=m['active_by_type'].get('pine.strategy',0),active_pine_sources=m['active_pine_sources']))
    if records:
        with (out/'comparison.csv').open('w',encoding='utf-8',newline='') as f:
            writer=csv.DictWriter(f,fieldnames=list(records[0]));writer.writeheader();writer.writerows(records)


def comparison_chart(out, results):
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    from matplotlib.patches import Patch
    from matplotlib.lines import Line2D
    profiles=['baseline','model','model_ack']
    native=[r for r in results if r['suite']=='native']
    seeds=[seed for seed in sorted({r['seed'] for r in native})
           if {r['profile'] for r in native if r['seed']==seed}==set(profiles)]
    if not seeds:return
    by_key={(r['seed'],r['profile']):r for r in native}
    reference_path=out/'reference-statistics.json'
    ref=json.loads(reference_path.read_text()) if reference_path.exists() else None
    columns=[('return_std_bp','1m return standard deviation','bps'),
             ('return_acf1','1m return lag-1 correlation','correlation'),
             ('abs_return_p99_bp','1m absolute return P99','bps'),
             ('two_sided_fraction','Two-sided quote availability','%'),
             ('spread_median_bp','Median quoted spread','bps')]
    colors=['#2166ac','#b35c00','#238443','#7a5195']
    fig,axes=plt.subplots(2,5,figsize=(18,8),sharex=True)
    for row,leg in enumerate(['spot','perp']):
        market='V-BTC-'+leg.upper()
        for col,(key,title,unit) in enumerate(columns):
            ax=axes[row,col]
            if ref and col<3:
                real=ref['markets'][leg]
                windows=[w[key] for w in real['two_hour_windows'] if w[key] is not None]
                ax.axhspan(min(windows),max(windows),color='#dddddd',zorder=0)
                ax.axhline(real['full_day'][key],color='#555555',ls='--',lw=1,zorder=1)
            for color,seed in zip(colors,seeds):
                values=[]
                for profile in profiles:
                    m=by_key[seed,profile]['markets'][market]
                    value=m['minute'][key] if col<3 else m[key]
                    values.append(value*100 if key=='two_sided_fraction' else value)
                ax.plot(range(3),values,'o-',color=color,lw=1.2,ms=5,label=f'Seed {seed}')
            if row==0:ax.set_title(title,fontsize=10)
            ax.set_ylabel(f'{leg.upper()} / {unit}')
            ax.set_xticks(range(3),['Baseline','Model','Model + ack'],rotation=20)
            ax.set_xlim(-.25,2.25);ax.grid(axis='y',alpha=.2)
            if key=='return_acf1':ax.set_ylim(-1,1);ax.axhline(0,color='#aaaaaa',lw=.7)
            elif key=='two_sided_fraction':ax.set_ylim(top=100.2)
            else:ax.set_ylim(bottom=0)
            ax.spines[['top','right']].set_visible(False)
    for col in range(len(columns)):
        low=min(ax.get_ylim()[0] for ax in axes[:,col])
        high=max(ax.get_ylim()[1] for ax in axes[:,col])
        for ax in axes[:,col]:ax.set_ylim(low,high)
    handles,labels=axes[0,0].get_legend_handles_labels()
    if ref:
        handles.extend([Patch(facecolor='#dddddd'),Line2D([0],[0],color='#555555',ls='--')])
        labels.extend(['Historical 2h range (descriptive)','Historical full day'])
    fig.legend(handles,labels,loc='upper center',bbox_to_anchor=(.5,.95),ncol=len(labels),frameon=False)
    fig.suptitle('Native mechanism comparison | completed seed triplets: '+', '.join(map(str,seeds)),fontsize=15)
    fig.text(.5,.018,'Simulation: hours 2-12 per run. Historical reference: one BTCUSDT day; 2h ranges are not confidence intervals or matched-length tests. No historical book archive.',ha='center',fontsize=9)
    fig.tight_layout(rect=[0,.055,1,.89]);fig.savefig(out/'native-all-seeds-statistics.png',dpi=150);plt.close(fig)


def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path,default=DEFAULT_OUT)
    p.add_argument('--reference',action='store_true');p.add_argument('--charts',action='store_true');p.add_argument('--cases',nargs='+')
    a=p.parse_args();out=a.output.resolve();plan=json.loads((out/'plan.json').read_text(encoding='utf-8'))
    if a.reference:reference(out)
    results=[];failures=[]
    for row in plan['runs']:
        done=out/(row['name']+'.run.json')
        if a.cases and row['name'] not in a.cases:continue
        if not done.exists():continue
        run=json.loads(done.read_text(encoding='utf-8'))
        assert sha(Path(row['recipe']))==row['recipe_sha256']
        if not run['complete']:
            log=out/(row['name']+'.log');receipt=out/(row['name']+'.receipt.json')
            lines=log.read_text(encoding='utf-8',errors='replace').splitlines()
            errors=[line for line in lines if line.startswith('Error:')]
            progress=[line for line in lines if line.startswith('step=')]
            error=errors[-1] if errors else run.get('failure','Run exited without a complete receipt')
            match=re.search(r'participant=(\S+) step=(\d+) error=',error)
            failures.append(dict(**row,exit_code=run.get('exit_code'),inputs=run['inputs'],
                error=error,participant=match[1] if match else None,error_step_index=int(match[2]) if match else None,
                last_progress=progress[-1] if progress else None,log_sha256=sha(log),
                partial_receipt_bytes=receipt.stat().st_size,partial_receipt_sha256=sha(receipt),
                usable_market_statistics=False))
            print(row['name'],'FAILED',error,flush=True)
            continue
        target=out/(row['name']+'.metrics.json')
        result=json.loads(target.read_text(encoding='utf-8')) if target.exists() else None
        # Cached summaries never bypass verification of their original evidence.
        assert sha(out/(row['name']+'.receipt.json'))==run['receipt_sha256']
        if (not result or result.get('analysis_version')!=ANALYSIS_VERSION or
            result['recipe_sha256']!=row['recipe_sha256'] or
            result['receipt_sha256']!=run['receipt_sha256'] or
            result['binary_sha256']!=run['inputs']['binary_sha256']):
            result=analyze_run(out,row)
        results.append(result)
        print(row['name'],{k:dict(acf=v['minute']['return_acf1'],spread=v['spread_median_bp'],book=v['two_sided_fraction']) for k,v in result['markets'].items()},flush=True)
    terminal_names={r['name'] for r in results+failures}
    write(out/'comparison.json',dict(plan_sha256=sha(out/'plan.json'),complete=len(results)==len(plan['runs']),
        terminal=len(terminal_names)==len(plan['runs']),planned_runs=len(plan['runs']),
        pending_runs=[r['name'] for r in plan['runs'] if r['name'] not in terminal_names],runs=results,failed_runs=failures))
    summary_csv(out,results)
    if a.charts:
        for suite,seed in sorted({(r['suite'],r['seed']) for r in results}):
            chart(out,results,suite,seed)
            chart(out,results,suite,seed,minute_zoom=True)
        comparison_chart(out,results)


if __name__=='__main__':main()
